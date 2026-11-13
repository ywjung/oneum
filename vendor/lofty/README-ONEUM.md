# lofty 0.25.5, with one change

This is the source of the `lofty` crate, version 0.25.5, exactly as published on
crates.io (license: MIT OR Apache-2.0, see `Cargo.toml`), with one change:

- `src/id3/v2/frame/read.rs`, `handle_compression` (the variant compiled without the
  `id3v2_compression_support` feature): it built its error from a type and an enum
  variant that no longer exist, so upstream 0.25.5 does not compile without that
  feature. It now returns the same error through `FrameParseError::message`.

Why it is here: OnEum builds lofty **without** `id3v2_compression_support`. With it, a
compressed ID3v2 frame is unpacked without any limit, so a file of a few kilobytes can
ask for gigabytes of memory while the library is scanned (see docs/TESTING.md,
"보안 점검"). Without it such a frame is refused.

`Cargo.toml` of OnEum points here with `[patch.crates-io]`. When a lofty release builds
without the feature (or limits what it unpacks), delete this folder and the patch entry.
