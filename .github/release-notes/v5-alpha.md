Third alpha of the v5 Rust + Bevy rewrite. Unsigned pre-release binaries for
macOS (Apple Silicon), Windows (x86_64), and Linux (x86_64).

Downloads:
- macOS (Apple Silicon): WaveConductor-macos-arm64.zip
- Windows (x86_64): WaveConductor-windows-x86_64.zip
- Linux (x86_64): WaveConductor-linux-x86_64.tar.gz

Requirements: a WebGPU-capable GPU (Metal / DX12 / Vulkan 1.2+). Hand tracking
is optional and needs the Ultraleap tracking service running.

Changed since v5.0.0-alpha.2:
- Windows now defaults the renderer to DX12 unless WGPU_BACKEND is explicitly
  set, avoiding older AMD Vulkan-driver startup paths.
- Disabled egui's optional bindless-texture request on Windows so GPUs without
  TEXTURE_BINDING_ARRAY no longer hit that startup warning/fallback path.
- DirectML-backed MediaPipe sessions now apply the ONNX Runtime session options
  DirectML requires and report DirectML/CPU mixed states accurately in the dev
  panel.
- Refreshed the Cargo.lock advisory surface by updating crossbeam-epoch to
  0.9.20.

Notes:
- Unsigned builds. macOS: right-click the app and choose Open (or run
  xattr -dr com.apple.quarantine). Windows: SmartScreen may warn, so click More
  info, then Run anyway.
- GPU inference acceleration: CoreML on macOS, DirectML on Windows (AMD/Intel
  integrated GPUs), CPU on Linux.
- Windows temperature telemetry reads the WDDM adapter temperature as a coarse
  throttle proxy and may be unavailable on some integrated GPUs.
- The 8-hour thermal soak gate is deferred for this alpha. This is a pre-release
  for testing, not a soak-verified release.
- Intel Mac and web/WASM targets are not part of this alpha.
