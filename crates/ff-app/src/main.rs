//! FlickFlock — a fast, keyboard-driven image viewer.

// No console window on Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod browse;
mod canvas;
mod i18n_check;
mod images;
mod ipc;
mod ops;
mod router;
mod selftest;
mod ui_canvas;
mod ui_hud;
mod ui_panels;
mod ui_strip;

use eframe::egui;
use ff_core::config::Settings;
use ff_core::decode::{DecodeError, DecodeOptions, Decoded, Decoder, Decoders, Format};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// Adapter exposing the operating system's image codecs (ImageIO, WIC,
/// libheif) as an `ff-core` decoder for HEIF/HEIC and AVIF.
struct OsCodec;

impl Decoder for OsCodec {
    fn name(&self) -> &'static str {
        ff_platform::decoder().name()
    }

    fn handles(&self, format: Format) -> bool {
        matches!(format, Format::Heif | Format::Avif)
    }

    fn decode(
        &self,
        path: &Path,
        format: Format,
        opts: &DecodeOptions,
    ) -> Result<Decoded, DecodeError> {
        let platform = ff_platform::decoder();
        let raw = match opts.max_dim {
            // Thumbnails can use the OS's cheap reduced-size decode.
            Some(max) if max <= ff_core::thumbs::THUMB_EDGE => platform.decode_scaled(path, max),
            _ => platform.decode(path),
        }
        .map_err(|e| match e {
            ff_platform::PlatformError::Unsupported(msg) => DecodeError::Unsupported(msg),
            ff_platform::PlatformError::Decode(msg) => DecodeError::Failed(msg),
            ff_platform::PlatformError::Io(io) => DecodeError::Io(io),
        })?;
        let image =
            image::RgbaImage::from_raw(raw.width, raw.height, raw.rgba).ok_or_else(|| {
                DecodeError::Failed("the system codec returned a malformed image".into())
            })?;
        Ok(Decoded::still(format, image, self.name()))
    }
}

/// Activate the interface language for a `language` setting ("auto" or a code).
pub(crate) fn apply_language(setting: &str) {
    let os_locales: Vec<String> = sys_locale::get_locales().collect();
    ff_core::i18n::set_language(ff_core::i18n::Lang::resolve(setting, &os_locales));
}

fn app_icon() -> Option<egui::IconData> {
    let img = image::load_from_memory(include_bytes!("../../../assets/icon-256.png"))
        .ok()?
        .into_rgba8();
    let (width, height) = img.dimensions();
    Some(egui::IconData {
        rgba: img.into_raw(),
        width,
        height,
    })
}

struct Args {
    paths: Vec<PathBuf>,
    new_window: bool,
    /// `--slideshow[=SECONDS]`: start a slideshow, optionally with this interval.
    slideshow: Option<Option<f32>>,
}

fn parse_args() -> Option<Args> {
    let mut args = Args {
        paths: Vec::new(),
        new_window: false,
        slideshow: None,
    };
    let mut only_paths = false;
    for arg in std::env::args_os().skip(1) {
        match arg.to_str() {
            Some("--") if !only_paths => only_paths = true,
            Some("--new-window" | "-n") if !only_paths => args.new_window = true,
            Some("--slideshow" | "-s") if !only_paths => args.slideshow = Some(None),
            Some(s) if !only_paths && s.starts_with("--slideshow=") => {
                args.slideshow = Some(
                    s["--slideshow=".len()..]
                        .parse()
                        .ok()
                        .filter(|v: &f32| v.is_finite() && *v > 0.0),
                );
            }
            Some("--version" | "-V") if !only_paths => {
                println!("flickflock {}", env!("CARGO_PKG_VERSION"));
                return None;
            }
            Some("--help" | "-h") if !only_paths => {
                println!(
                    "FlickFlock {} — a fast image viewer\n\nUsage: flickflock [OPTIONS] [FILE or FOLDER]...\n\nOptions:\n  -n, --new-window  Open a new window even if FlickFlock is already running\n  -s, --slideshow[=SECONDS]\n                    Start a slideshow, optionally with SECONDS per image\n  -V, --version     Print the version\n  -h, --help        Print this help",
                    env!("CARGO_PKG_VERSION")
                );
                return None;
            }
            // macOS passes a process serial number when launched from Finder on old systems.
            Some(s) if s.starts_with("-psn_") => {}
            _ => args
                .paths
                .push(std::path::absolute(&arg).unwrap_or_else(|_| PathBuf::from(&arg))),
        }
    }
    Some(args)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("FLICKFLOCK_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let Some(args) = parse_args() else {
        return Ok(());
    };

    let settings_path = ff_core::config::config_file();
    let (settings, settings_warnings) = match &settings_path {
        Some(path) => Settings::load(path, ff_core::IS_MAC),
        None => (Settings::default(), Vec::new()),
    };

    apply_language(&settings.general.language);

    // Single instance: hand our paths to a running window and exit.
    let single = settings.general.single_window && !args.new_window && args.slideshow.is_none();
    if single && ipc::send_to_running_instance(&args.paths) {
        return Ok(());
    }

    let (inbox_tx, inbox) = crossbeam_channel::unbounded::<Vec<PathBuf>>();
    let waker: Arc<OnceLock<egui::Context>> = Arc::new(OnceLock::new());
    let deliver = {
        let waker = waker.clone();
        move |paths: Vec<PathBuf>| {
            let _ = inbox_tx.send(paths);
            if let Some(ctx) = waker.get() {
                ctx.request_repaint();
            }
        }
    };
    if single {
        ipc::listen(deliver.clone());
    }

    let mut decoders = Decoders::builtin();
    decoders.register(Arc::new(OsCodec));

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("FlickFlock")
        .with_app_id("flickflock")
        .with_inner_size([1100.0, 760.0])
        .with_min_inner_size([360.0, 260.0])
        .with_drag_and_drop(true);
    if let Some(icon) = app_icon() {
        viewport = viewport.with_icon(icon);
    }
    if ff_core::IS_MAC {
        // Maximum image area: content extends under a transparent title bar.
        viewport = viewport
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false);
    } else {
        viewport = viewport.with_decorations(!settings.general.borderless);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    let startup = app::Startup {
        settings,
        settings_path,
        settings_warnings,
        decoders,
        initial: args.paths,
        slideshow: args.slideshow,
        inbox,
        waker,
    };

    // The event loop is built here (instead of `eframe::run_native`) so the
    // macOS "open file" handler can be installed between building and running it.
    let event_loop =
        winit::event_loop::EventLoop::<eframe::UserEvent>::with_user_event().build()?;
    ff_platform::integration().install_open_handler(Box::new(deliver));
    let mut winit_app = eframe::create_native(
        "FlickFlock",
        options,
        Box::new(move |cc| Ok(Box::new(app::FlickApp::new(cc, startup)?) as Box<dyn eframe::App>)),
        &event_loop,
    );
    event_loop.run_app(&mut winit_app)?;
    Ok(())
}
