//! Manual end-to-end check for [`ff_platform::PlatformIntegration::install_open_handler`].
//!
//! Opens an empty window and appends every batch of paths received from the OS "open file"
//! event to a log file (`$FF_OPEN_HANDLER_LOG`, or `<executable>.log`). To exercise it on
//! macOS, wrap the built binary in a minimal `.app` bundle whose `Info.plist` declares
//! `CFBundleDocumentTypes` for `public.image`, then run `open -a Test.app some.png` both while
//! the app is not running (launch-time delivery) and while it is.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowId};

fn log_path() -> PathBuf {
    if let Some(path) = std::env::var_os("FF_OPEN_HANDLER_LOG") {
        return PathBuf::from(path);
    }
    let mut path = std::env::current_exe()
        .unwrap_or_else(|_| PathBuf::from("open_handler"))
        .into_os_string();
    path.push(".log");
    PathBuf::from(path)
}

fn log(line: &str) {
    let path = log_path();
    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(mut file) => {
            let _ = writeln!(file, "{line}");
        }
        Err(err) => eprintln!("cannot write {}: {err}", path.display()),
    }
    println!("{line}");
}

#[derive(Default)]
struct App {
    window: Option<Window>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            let attributes = Window::default_attributes().with_title("ff-platform open handler");
            match event_loop.create_window(attributes) {
                Ok(window) => {
                    log("window created");
                    self.window = Some(window);
                }
                Err(err) => {
                    log(&format!("could not create a window: {err}"));
                    event_loop.exit();
                }
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if matches!(event, WindowEvent::CloseRequested) {
            event_loop.exit();
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    log(&format!(
        "started; argv = {:?}",
        std::env::args().collect::<Vec<_>>()
    ));

    // Order matters: build the event loop, install the handler, then run.
    let event_loop = EventLoop::new()?;
    ff_platform::integration().install_open_handler(Box::new(|paths| {
        log(&format!("open: {paths:?}"));
    }));
    log("handler installed; running event loop");

    event_loop.run_app(&mut App::default())?;
    Ok(())
}
