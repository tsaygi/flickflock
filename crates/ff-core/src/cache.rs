//! Caches: a byte-budgeted LRU for decoded images and the on-disk thumbnail cache.

use image::RgbaImage;
use std::collections::HashMap;
use std::hash::Hash;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Least-recently-used cache bounded by the total *weight* (bytes) of its values.
pub struct ByteLru<K, V> {
    budget: usize,
    used: usize,
    tick: u64,
    map: HashMap<K, Slot<V>>,
}

struct Slot<V> {
    value: V,
    bytes: usize,
    last_used: u64,
}

impl<K: Eq + Hash + Clone, V> ByteLru<K, V> {
    pub fn new(budget: usize) -> Self {
        Self {
            budget,
            used: 0,
            tick: 0,
            map: HashMap::new(),
        }
    }

    pub fn budget(&self) -> usize {
        self.budget
    }

    pub fn used(&self) -> usize {
        self.used
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn contains(&self, key: &K) -> bool {
        self.map.contains_key(key)
    }

    /// Look up and mark as most recently used.
    pub fn get(&mut self, key: &K) -> Option<&V> {
        self.tick += 1;
        let tick = self.tick;
        self.map.get_mut(key).map(|slot| {
            slot.last_used = tick;
            &slot.value
        })
    }

    /// Look up without touching the LRU order.
    pub fn peek(&self, key: &K) -> Option<&V> {
        self.map.get(key).map(|s| &s.value)
    }

    /// Mark as most recently used without borrowing the value.
    pub fn touch(&mut self, key: &K) {
        self.tick += 1;
        if let Some(slot) = self.map.get_mut(key) {
            slot.last_used = self.tick;
        }
    }

    /// Insert (or replace) a value of the given weight, then evict
    /// least-recently-used entries until the budget is met. The entry just
    /// inserted and entries for which `pinned` returns true are never evicted,
    /// so the cache may temporarily exceed its budget. Returns what was evicted.
    pub fn insert(
        &mut self,
        key: K,
        value: V,
        bytes: usize,
        pinned: impl Fn(&K) -> bool,
    ) -> Vec<(K, V)> {
        self.tick += 1;
        if let Some(old) = self.map.insert(
            key.clone(),
            Slot {
                value,
                bytes,
                last_used: self.tick,
            },
        ) {
            self.used -= old.bytes;
        }
        self.used += bytes;
        self.evict(|k| *k == key || pinned(k))
    }

    /// Evict least-recently-used, unpinned entries until within budget.
    pub fn evict(&mut self, pinned: impl Fn(&K) -> bool) -> Vec<(K, V)> {
        let mut evicted = Vec::new();
        while self.used > self.budget {
            let victim = self
                .map
                .iter()
                .filter(|(k, _)| !pinned(k))
                .min_by_key(|(_, s)| s.last_used)
                .map(|(k, _)| k.clone());
            let Some(key) = victim else { break };
            if let Some(slot) = self.map.remove(&key) {
                self.used -= slot.bytes;
                evicted.push((key, slot.value));
            }
        }
        evicted
    }

    pub fn remove(&mut self, key: &K) -> Option<V> {
        let slot = self.map.remove(key)?;
        self.used -= slot.bytes;
        Some(slot.value)
    }

    /// Drop every entry for which `keep` returns false.
    pub fn retain(&mut self, mut keep: impl FnMut(&K) -> bool) {
        let used = &mut self.used;
        self.map.retain(|k, slot| {
            let k = keep(k);
            if !k {
                *used -= slot.bytes;
            }
            k
        });
    }

    pub fn set_budget(&mut self, budget: usize, pinned: impl Fn(&K) -> bool) -> Vec<(K, V)> {
        self.budget = budget;
        self.evict(pinned)
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.used = 0;
    }
}

/// Thumbnails persisted in the OS cache directory, keyed by path, file size
/// and modification time, so folders on slow network shares reopen instantly.
#[derive(Debug, Clone)]
pub struct ThumbDiskCache {
    dir: PathBuf,
}

impl ThumbDiskCache {
    /// Cache inside `dir` (created on first write).
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The default location: `<OS cache dir>/flickflock/thumbs`.
    pub fn default_location() -> Option<Self> {
        Some(Self::new(crate::config::cache_dir()?.join("thumbs")))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn file_for(&self, source: &Path, edge: u32) -> Option<PathBuf> {
        let md = std::fs::metadata(source).ok()?;
        let mtime = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let mut h = Fnv::default();
        h.write(source.as_os_str().as_encoded_bytes());
        h.write(&md.len().to_le_bytes());
        h.write(&mtime.to_le_bytes());
        h.write(&edge.to_le_bytes());
        let name = format!("{:016x}{:016x}.qoi", h.a, h.b);
        // Two-level layout keeps directories small.
        Some(self.dir.join(&name[..2]).join(name))
    }

    /// Load a cached thumbnail for `source` at `edge` pixels, if present and current.
    pub fn get(&self, source: &Path, edge: u32) -> Option<RgbaImage> {
        let bytes = std::fs::read(self.file_for(source, edge)?).ok()?;
        let (header, data) = qoi::decode_to_vec(&bytes).ok()?;
        let data = match header.channels {
            qoi::Channels::Rgba => data,
            qoi::Channels::Rgb => data
                .chunks_exact(3)
                .flat_map(|p| [p[0], p[1], p[2], 255])
                .collect(),
        };
        RgbaImage::from_raw(header.width, header.height, data)
    }

    /// Store a thumbnail. Failures are logged and otherwise ignored.
    pub fn put(&self, source: &Path, edge: u32, thumb: &RgbaImage) {
        let Some(file) = self.file_for(source, edge) else {
            return;
        };
        let Ok(bytes) = qoi::encode_to_vec(thumb.as_raw(), thumb.width(), thumb.height()) else {
            return;
        };
        let write = || -> std::io::Result<()> {
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // Write-then-rename so a concurrent reader never sees a partial file.
            let tmp = file.with_extension(format!("tmp{}", std::process::id()));
            std::fs::write(&tmp, &bytes)?;
            std::fs::rename(&tmp, &file)
        };
        if let Err(e) = write() {
            tracing::debug!("thumbnail cache write failed: {e}");
        }
    }

    /// Delete everything in the cache. Returns the number of bytes freed.
    pub fn clear(&self) -> u64 {
        let size = self.size();
        let _ = std::fs::remove_dir_all(&self.dir);
        size
    }

    /// Total size on disk in bytes.
    pub fn size(&self) -> u64 {
        fn walk(dir: &Path) -> u64 {
            let Ok(rd) = std::fs::read_dir(dir) else {
                return 0;
            };
            rd.flatten()
                .map(|e| match e.metadata() {
                    Ok(md) if md.is_dir() => walk(&e.path()),
                    Ok(md) => md.len(),
                    Err(_) => 0,
                })
                .sum()
        }
        walk(&self.dir)
    }
}

/// Two independent FNV-1a streams (128 bits total). Stable across Rust
/// versions and platforms, unlike `DefaultHasher`.
struct Fnv {
    a: u64,
    b: u64,
}

impl Default for Fnv {
    fn default() -> Self {
        Self {
            a: 0xcbf2_9ce4_8422_2325,
            b: 0x6c62_272e_07bb_0142,
        }
    }
}

impl Fnv {
    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.a = (self.a ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
            self.b = (self.b ^ u64::from(byte ^ 0x5a))
                .wrapping_mul(0x0000_0100_0000_01b3)
                .rotate_left(5);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    #[test]
    fn evicts_least_recently_used_first() {
        let mut c: ByteLru<&str, u32> = ByteLru::new(100);
        assert!(c.insert("a", 1, 40, |_| false).is_empty());
        assert!(c.insert("b", 2, 40, |_| false).is_empty());
        assert_eq!(c.get(&"a"), Some(&1)); // a is now more recent than b
        let evicted = c.insert("c", 3, 40, |_| false);
        assert_eq!(evicted, [("b", 2)]);
        assert_eq!(c.used(), 80);
        assert!(c.contains(&"a") && c.contains(&"c") && !c.contains(&"b"));
    }

    #[test]
    fn peek_does_not_refresh() {
        let mut c: ByteLru<&str, u32> = ByteLru::new(100);
        c.insert("a", 1, 50, |_| false);
        c.insert("b", 2, 50, |_| false);
        assert_eq!(c.peek(&"a"), Some(&1));
        assert_eq!(c.insert("c", 3, 50, |_| false), [("a", 1)]);
    }

    #[test]
    fn pinned_and_new_entries_survive_even_over_budget() {
        let mut c: ByteLru<&str, u32> = ByteLru::new(100);
        c.insert("current", 1, 90, |_| false);
        let evicted = c.insert("huge", 2, 500, |k| *k == "current");
        assert!(evicted.is_empty());
        assert_eq!(c.used(), 590);
        // Once nothing is pinned the oldest goes first, then the cache is within budget or empty.
        let evicted = c.insert("small", 3, 10, |_| false);
        assert_eq!(evicted, [("current", 1), ("huge", 2)]);
        assert_eq!((c.len(), c.used()), (1, 10));
    }

    #[test]
    fn replace_remove_retain_and_budget_changes() {
        let mut c: ByteLru<u32, &str> = ByteLru::new(100);
        c.insert(1, "one", 30, |_| false);
        c.insert(1, "uno", 50, |_| false);
        assert_eq!((c.len(), c.used()), (1, 50));
        c.insert(2, "two", 20, |_| false);
        c.insert(3, "three", 20, |_| false);
        assert_eq!(c.remove(&2), Some("two"));
        assert_eq!(c.remove(&2), None);
        assert_eq!(c.used(), 70);
        let evicted = c.set_budget(25, |_| false);
        assert_eq!(evicted, [(1, "uno")]);
        c.retain(|k| *k != 3);
        assert!(c.is_empty());
        assert_eq!(c.used(), 0);
        c.insert(9, "nine", 5, |_| false);
        c.clear();
        assert_eq!((c.len(), c.used()), (0, 0));
    }

    #[test]
    fn disk_cache_round_trip_and_invalidation() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = ThumbDiskCache::new(tmp.path().join("cache"));
        let src = tmp.path().join("写真 1.png");
        std::fs::write(&src, b"original").unwrap();
        assert!(cache.get(&src, 256).is_none());

        let thumb = RgbaImage::from_fn(8, 6, |x, y| {
            Rgba([x as u8 * 30, y as u8 * 40, 9, 200 + x as u8])
        });
        cache.put(&src, 256, &thumb);
        assert_eq!(cache.get(&src, 256).as_ref(), Some(&thumb));
        assert!(
            cache.get(&src, 128).is_none(),
            "different size, different entry"
        );
        assert!(cache.size() > 0);

        // Changing the file (size here) invalidates the entry.
        std::fs::write(&src, b"modified content").unwrap();
        assert!(cache.get(&src, 256).is_none());
        assert!(cache.get(&tmp.path().join("missing.png"), 256).is_none());

        assert!(cache.clear() > 0);
        assert_eq!(cache.size(), 0);
    }

    #[test]
    fn hash_is_stable() {
        let mut h = Fnv::default();
        h.write(b"flickflock");
        let first = (h.a, h.b);
        let mut h = Fnv::default();
        h.write(b"flickflock");
        assert_eq!((h.a, h.b), first);
        let mut h2 = Fnv::default();
        h2.write(b"flickflocl");
        assert_ne!((h2.a, h2.b), first);
        assert_ne!(first.0, first.1);
    }
}
