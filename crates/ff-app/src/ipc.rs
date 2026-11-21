//! Single-instance support: a second launch hands its file paths to the
//! running instance over a local socket (Unix domain socket / named pipe).

use interprocess::local_socket::{prelude::*, ListenerOptions, Name, Stream};
use std::io::{Read, Write};
use std::path::PathBuf;

/// Refuse absurd messages instead of allocating for them.
const MAX_PATHS: u32 = 4096;
const MAX_PATH_BYTES: u32 = 1 << 16;

#[cfg(unix)]
fn socket_name() -> std::io::Result<(Name<'static>, Option<PathBuf>)> {
    use interprocess::local_socket::GenericFilePath;
    // A per-user location: the cache dir is private to the user.
    let dir = ff_core::config::cache_dir().unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("flickflock.sock");
    Ok((
        path.clone().to_fs_name::<GenericFilePath>()?.into_owned(),
        Some(path),
    ))
}

#[cfg(windows)]
fn socket_name() -> std::io::Result<(Name<'static>, Option<PathBuf>)> {
    use interprocess::local_socket::GenericNamespaced;
    let user = std::env::var("USERNAME").unwrap_or_else(|_| "user".into());
    let safe: String = user.chars().filter(|c| c.is_alphanumeric()).collect();
    Ok((
        format!("flickflock-{safe}.sock")
            .to_ns_name::<GenericNamespaced>()?
            .into_owned(),
        None,
    ))
}

#[cfg(unix)]
fn encode(path: &std::path::Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(unix)]
fn decode(bytes: Vec<u8>) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(std::ffi::OsString::from_vec(bytes))
}

#[cfg(windows)]
fn encode(path: &std::path::Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[cfg(windows)]
fn decode(bytes: Vec<u8>) -> PathBuf {
    use std::os::windows::ffi::OsStringExt;
    let wide: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    PathBuf::from(std::ffi::OsString::from_wide(&wide))
}

fn write_message(mut w: impl Write, paths: &[PathBuf]) -> std::io::Result<()> {
    w.write_all(&(paths.len() as u32).to_le_bytes())?;
    for p in paths {
        let bytes = encode(p);
        w.write_all(&(bytes.len() as u32).to_le_bytes())?;
        w.write_all(&bytes)?;
    }
    w.flush()
}

fn read_message(mut r: impl Read) -> std::io::Result<Vec<PathBuf>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let count = u32::from_le_bytes(len);
    if count > MAX_PATHS {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "too many paths",
        ));
    }
    let mut paths = Vec::with_capacity(count as usize);
    for _ in 0..count {
        r.read_exact(&mut len)?;
        let n = u32::from_le_bytes(len);
        if n > MAX_PATH_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "path too long",
            ));
        }
        let mut bytes = vec![0u8; n as usize];
        r.read_exact(&mut bytes)?;
        paths.push(decode(bytes));
    }
    Ok(paths)
}

/// Try to hand `paths` to an already running instance. Returns `true` when
/// one accepted them (the caller should then exit).
pub fn send_to_running_instance(paths: &[PathBuf]) -> bool {
    let Ok((name, _)) = socket_name() else {
        return false;
    };
    match Stream::connect(name) {
        Ok(stream) => write_message(stream, paths).is_ok(),
        Err(_) => false,
    }
}

/// Start listening for paths from later launches. `on_paths` is called from
/// a background thread (with an empty list when a launch had no arguments).
pub fn listen(on_paths: impl Fn(Vec<PathBuf>) + Send + 'static) {
    let (name, file) = match socket_name() {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("single-instance socket unavailable: {e}");
            return;
        }
    };
    // Nobody answered on this socket (we tried to connect first), so a
    // leftover file from a crashed instance can be removed.
    if let Some(file) = &file {
        let _ = std::fs::remove_file(file);
    }
    let listener = match ListenerOptions::new().name(name).create_sync() {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!("could not create single-instance socket: {e}");
            return;
        }
    };
    let spawned = std::thread::Builder::new()
        .name("ff-ipc".into())
        .spawn(move || {
            for conn in listener.incoming() {
                match conn.and_then(read_message) {
                    Ok(paths) => on_paths(paths),
                    Err(e) => tracing::debug!("ignored malformed IPC message: {e}"),
                }
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("could not start IPC thread: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_round_trip() {
        let paths = vec![
            PathBuf::from("/photos/写真 1.jpg"),
            PathBuf::from("relative/b.png"),
            PathBuf::new(),
        ];
        let mut buf = Vec::new();
        write_message(&mut buf, &paths).unwrap();
        assert_eq!(read_message(buf.as_slice()).unwrap(), paths);
        let mut empty = Vec::new();
        write_message(&mut empty, &[]).unwrap();
        assert!(read_message(empty.as_slice()).unwrap().is_empty());
    }

    #[test]
    fn malformed_messages_are_rejected() {
        assert!(read_message(&[1u8, 2][..]).is_err());
        assert!(read_message(&u32::MAX.to_le_bytes()[..]).is_err());
        let mut msg = 1u32.to_le_bytes().to_vec();
        msg.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(read_message(msg.as_slice()).is_err());
        let mut msg = 1u32.to_le_bytes().to_vec();
        msg.extend_from_slice(&10u32.to_le_bytes());
        msg.extend_from_slice(b"short");
        assert!(read_message(msg.as_slice()).is_err());
    }
}
