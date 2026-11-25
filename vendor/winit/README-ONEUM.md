# winit 0.30.13, with one change

This is the source of the `winit` crate, version 0.30.13, exactly as published on
crates.io (license: Apache-2.0, see `LICENSE` and `Cargo.toml`), without its `docs`
folder (pictures for the documentation, which nothing in the source reads), and with
one change:

- `src/platform_impl/macos/ffi.rs`: the declarations of `CGSMainConnectionID` and
  `CGSSetWindowBackgroundBlurRadius` are gone, with the two imports only they used.
- `src/platform_impl/macos/window_delegate.rs`, `set_blur`: it called those two to blur
  what is behind a see-through window. It now does nothing.

Why it is here: the two functions are not public API of macOS. A program that only
names them, called or not, is refused by the Mac App Store: on 2026-11-26 App Review's
automated check returned OnEum 0.1.0 (174) under guideline 2.5.1 for
`_CGSSetWindowBackgroundBlurRadius`. The declaration alone puts the name into the
program, because `set_blur` is part of the window type every program gets.

OnEum never asks for a blurred window (neither does egui), so nothing changes on the
screen. The other systems' code is untouched.

`Cargo.toml` of OnEum points here with `[patch.crates-io]`. `scripts/mac-app-store.sh`
checks that the store's program names neither function. When a winit release no longer
names them (or puts them behind a feature that is off), delete this folder and the
patch entry.
