//! Platform-independent, GUI-free core of FlickFlock.
//!
//! Everything in this crate is plain logic (folder indexing, navigation, key
//! bindings, decoding, caching, animation timing, metadata, editing and
//! configuration) so it can be unit-tested on every OS without a window.

pub mod anim;
pub mod cache;
pub mod config;
pub mod decode;
pub mod edit;
pub mod folder;
pub mod input;
pub mod jobs;
pub mod loader;
pub mod meta;
pub mod natural;
pub mod nav;
pub mod thumbs;

/// `true` when compiled for macOS (where `Mod` means ⌘ instead of Ctrl).
pub const IS_MAC: bool = cfg!(target_os = "macos");
