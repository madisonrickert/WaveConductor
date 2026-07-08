Fourth alpha of the v5 Rust + Bevy rewrite. Unsigned pre-release binaries for
macOS (Apple Silicon), Windows (x86_64), and Linux (x86_64).

New this release: a Windows MSI installer alongside the portable zip.

Downloads:
- macOS (Apple Silicon): WaveConductor-macos-arm64.zip
- Windows (x86_64), installer: WaveConductor-v5.0.0-alpha.4-x86_64.msi
- Windows (x86_64), portable: WaveConductor-windows-x86_64.zip
- Linux (x86_64): WaveConductor-linux-x86_64.tar.gz

Requirements: a WebGPU-capable GPU (Metal / DX12 / Vulkan 1.2+). Hand tracking
is optional and needs the Ultraleap tracking service running.

Changed since v5.0.0-alpha.3:
- Windows now ships an MSI installer: it installs to Program Files, adds a Start
  Menu shortcut, bundles the Microsoft Visual C++ runtime, and supports clean
  uninstall and in-place upgrades. The portable zip is still available for a
  no-install option.
- Windows release builds no longer open a console window. Diagnostics are written
  to an on-disk log under %LOCALAPPDATA%\WaveConductor\logs (with a matching
  panic log), so crashes remain diagnosable without a console.
- The Windows executable now carries an application icon and version metadata,
  visible in Explorer, the Start Menu, and Add/Remove Programs.

Notes:
- Unsigned builds. macOS: right-click the app and choose Open (or run
  xattr -dr com.apple.quarantine). Windows: SmartScreen may warn, so click More
  info, then Run anyway.
- The portable Windows zip needs the Microsoft Visual C++ 2015-2022 x64
  Redistributable. The MSI installer bundles it automatically.
- GPU inference acceleration: CoreML on macOS, DirectML on Windows (AMD/Intel
  integrated GPUs), CPU on Linux.
- Windows temperature telemetry reads the WDDM adapter temperature as a coarse
  throttle proxy and may be unavailable on some integrated GPUs.
- The 8-hour thermal soak gate is deferred for this alpha. This is a pre-release
  for testing, not a soak-verified release.
- Intel Mac and web/WASM targets are not part of this alpha.
