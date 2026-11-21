# Contributing

Thanks for helping out!

- Keep `ff-core` free of GUI and OS-specific code; OS bindings belong in `ff-platform` behind its traits.
- Nothing may block the UI thread: decoding and disk or network I/O go through the loader, the thumbnail service or `FlickApp::spawn_bg`.
- UI code never reads keys directly. Add an `Action` in `ff-core/src/input.rs`, give it a default binding and handle it in `FlickApp::perform`.
- A bad file must never crash the app: return an error and let the user keep flicking.
- Before opening a pull request:

  ```sh
  cargo fmt --all
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  ```

- Record non-obvious decisions in `DECISIONS.md`.
