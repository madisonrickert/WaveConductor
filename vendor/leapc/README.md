# vendor/leapc

Vendored Ultraleap Gemini Tracking SDK runtime libraries and C headers
needed to build and run WaveConductor with native Leap Motion support.

Mirroring v4's `bin/` archival pattern so a fresh `git clone` + `cargo build`
produces a working binary without separately installing the Ultraleap SDK
on the build host.

## Version

Ultraleap Gemini Tracking SDK 6.2.0.

## Layout

- `include/` — C headers shared across all platforms. Consumed by
  `leap-sys`'s build script at compile time.
- `macos-aarch64/libLeapC.6.dylib` — Apple Silicon runtime.
- `macos-x86_64/libLeapC.6.dylib` — Intel Mac runtime.
- `linux-x86_64/libLeapC.so.6` — Linux x86_64 runtime (Ubuntu 22.04 build).
- `windows-x86_64/LeapC.dll` + `LeapC.lib` — Windows x86_64 runtime + MSVC
  import library.

## Refresh procedure

When Ultraleap ships a new SDK and you want to update the vendored copy:

1. Download the SDK installers for all four platforms from
   `https://developer.leapmotion.com/`.
2. Extract `LeapC.h` and companion headers into `include/`:
   ```bash
   pkgutil --expand-full <macos-pkg> /tmp/extract
   find /tmp/extract -name "LeapC.h" -exec cp {} vendor/leapc/include/ \;
   ```
3. Extract platform runtimes into the corresponding subdirectory.
4. Bump the version string at the top of this file.
5. Commit. CI will catch any ABI breakage.

See `docs/superpowers/specs/2026-05-27-plan-11.6-hand-tracking-leap-design.md`
for the design rationale and integration architecture.
