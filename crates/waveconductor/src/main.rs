// Release builds are a GUI app: detach from the console so double-clicking the
// installed exe doesn't spawn a stray console window. Gated on
// `not(debug_assertions)` (not bare `windows`) so `cargo rund`, the visual
// capture harness, and any debug run keep their stderr/console. Inert on
// non-Windows targets. Logs still land on disk via `logging` (Task 1).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! `WaveConductor` v5 binary entry point.
//!
//! Constructs the Bevy [`App`], registers core plugins, and runs the event loop.
//! In Plan 2 this opens a window and exercises the lifecycle plugin (state
//! machine + in-house keyboard actions). Sketch plugins land in Plan 6 onward.

use bevy::camera::Hdr;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::diagnostic::FrameTimeDiagnosticsPlugin;
use bevy::post_process::bloom::{Bloom, BloomPrefilter};
use bevy::prelude::*;
#[cfg(target_os = "windows")]
use bevy::render::settings::Backends;
use bevy::render::settings::WgpuSettings;
use bevy::render::view::Msaa;
use bevy::render::RenderPlugin;
use tracing_subscriber::EnvFilter;
use wc_core::audio::background::{EncodedSample, SampleAssets};
use wc_core::CorePlugin;
use wc_sketches::SketchesPlugin;

mod hand_providers;
mod logging;

fn main() {
    // `init_tracing` returns the in-app log buffer the capture layer feeds; the
    // dev panel's Log view reads it as a resource.
    // `_log_guard` keeps the non-blocking file-log writer alive for the whole
    // process; dropping it would flush and stop on-disk logging.
    let (log_buffer, _log_guard) = init_tracing();
    let mut app = App::new();
    app.insert_resource(log_buffer)
        // v4 Line renders against a black background; Bevy defaults to gray.
        // Setting the clear color globally is the simplest way to match —
        // future sketches can override per-state via `OnEnter`/`OnExit` if
        // they want a different backdrop.
        .insert_resource(ClearColor(Color::BLACK))
        .insert_resource(load_sample_assets())
        .add_plugins((
            DefaultPlugins
                .set(RenderPlugin {
                    render_creation: wgpu_settings().into(),
                    ..default()
                })
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: "WaveConductor".into(),
                        resolution: (1280_u32, 720_u32).into(),
                        ..default()
                    }),
                    ..default()
                })
                .set(AssetPlugin {
                    // Resolved at runtime via `wc_core::platform::assets::asset_root`
                    // so dev, release, and macOS `.app` bundle deployments all
                    // find shaders and other assets without environment-specific
                    // compile-time paths.
                    file_path: wc_core::platform::assets::asset_root()
                        .to_string_lossy()
                        .into_owned(),
                    ..default()
                })
                // We initialize tracing-subscriber in init_tracing() above;
                // Bevy's LogPlugin would clobber that with its own subscriber
                // and emit `ERROR Could not set global logger…` at startup.
                .disable::<bevy::log::LogPlugin>(),
            bevy_egui::EguiPlugin {
                // egui's bindless texture path needs wgpu binding-array support.
                // Windows AMD integrated GPUs can expose a Vulkan/DX12-class adapter
                // without that optional feature, and our UI does not need the bindless
                // path. Gate the request on targets where it has already shown up as
                // startup noise or a portability risk.
                bindless_mode_array_size: egui_bindless_mode_array_size(),
                ..Default::default()
            },
            CorePlugin,
            SketchesPlugin,
            FrameTimeDiagnosticsPlugin::default(),
        ))
        // `install_hand_tracking_providers` runs as a Startup system so it can
        // read `Res<HandTrackingSettings>` after SettingsPlugin has loaded the
        // persisted value. Running it pre-`App::run()` (the old approach) would
        // force the setting to be read before persistence loads.
        .add_systems(
            Startup,
            (
                spawn_camera,
                hand_providers::install_hand_tracking_providers,
                apply_startup_sketch_override,
            ),
        );

    // Live "Tracking provider" switch: applies dropdown changes to the
    // running provider registry without a restart, and resolves Auto's
    // asynchronous MediaPipe camera verdict. See the `hand_providers`
    // module docs for the full signal flow. `publish_hand_activation` runs
    // chained after it so the settings panel's activation cue reflects the
    // post-rebuild registry/watch state on the same frame.
    #[cfg(feature = "hand-tracking-gestures")]
    app.add_systems(
        Update,
        (
            hand_providers::apply_provider_choice,
            hand_providers::publish_hand_activation,
        )
            .chain(),
    );

    // OS display-sleep inhibitor, driven by the persisted "Keep display
    // awake" setting (default on). NonSend: the keepawake handle wraps
    // platform power APIs with no Send guarantee.
    app.insert_non_send(DisplayKeepAwake::default());
    app.add_systems(Update, apply_display_keepawake);

    // Debug-only: `WC_DEBUG_DISABLE_BLOOM` zeroes the main camera bloom for
    // render-stage isolation. Compiled out of release (relies on
    // `debug-assertions = false` in the release/soak profiles).
    #[cfg(debug_assertions)]
    app.add_systems(Update, apply_debug_bloom_toggle);

    app.run();
}

#[cfg(target_os = "windows")]
fn wgpu_settings() -> WgpuSettings {
    let mut settings = WgpuSettings::default();
    if Backends::from_env().is_none() {
        settings.backends = Some(Backends::DX12);
    }
    settings
}

#[cfg(not(target_os = "windows"))]
fn wgpu_settings() -> WgpuSettings {
    WgpuSettings::default()
}

fn egui_bindless_mode_array_size() -> Option<std::num::NonZero<u32>> {
    if cfg!(any(
        target_os = "macos",
        target_os = "windows",
        target_arch = "wasm32"
    )) {
        None
    } else {
        bevy_egui::EguiPlugin::default().bindless_mode_array_size
    }
}

/// Read sketch sample assets into a `SampleAssets` resource.
///
/// The audio engine runs in a separate cpal thread that can't reach Bevy's
/// `AssetServer`, so we load files synchronously here on the main thread
/// before `App::run()` and stash the raw bytes in a resource the engine's
/// `Startup` system reads. Per-file load failures are logged as warnings and
/// skipped; the engine always starts even if assets are missing.
///
/// Paths are resolved at runtime via [`wc_core::platform::assets::asset_root`]
/// so dev, release, and macOS `.app` bundle deployments all locate samples
/// without environment-specific compile-time paths or cwd assumptions.
///
/// Includes line background and the three Cymatics samples (kick, risingbass,
/// blub) converted from the v4 audio assets.
fn load_sample_assets() -> SampleAssets {
    let root = wc_core::platform::assets::asset_root();
    let load = |name: &'static str, rel: &str| -> Option<EncodedSample> {
        let path = root.join(rel);
        match std::fs::read(&path) {
            Ok(bytes) => {
                tracing::info!(name, size = bytes.len(), "loaded sample");
                Some(EncodedSample { name, bytes })
            }
            Err(err) => {
                tracing::warn!(
                    name,
                    path = %path.display(),
                    ?err,
                    "sample not found; skipping"
                );
                None
            }
        }
    };
    let mut samples = Vec::new();
    samples.extend(load("line_background", "sketches/line/line_background.ogg"));
    samples.extend(load("cymatics_kick", "sketches/cymatics/kick.ogg"));
    samples.extend(load(
        "cymatics_risingbass",
        "sketches/cymatics/risingbass.ogg",
    ));
    samples.extend(load("cymatics_blub", "sketches/cymatics/blub.ogg"));
    SampleAssets { samples }
}

/// Spawn the primary 2D camera. Required by `bevy_egui`, whose render pass
/// is attached per camera — without one, the settings panels never reach the
/// surface. Sketches in Plan 6+ keep this camera and project into it.
///
/// ## Internal-HDR rendering pipeline
///
/// The camera is configured for **internal HDR rendering, tonemapped to SDR
/// at the end**. This is the same pipeline modern PBR games use on SDR
/// displays: the scene renders into an `Rgba16Float` view target so
/// accumulated brightness above 1.0 survives, a bloom pass scatters bright
/// pixels into a soft halo, then a tonemapping pass rolls highlights off
/// into the displayable range before the SDR swapchain present.
///
/// This is **not** HDR display output — `WaveConductor` targets SDR LCD TVs
/// and projectors. We never write HDR signals to the cable.
///
/// ### Why HDR is necessary
///
/// The Line sketch's gravity post-process (see
/// `crates/wc-sketches/src/line/post_process.rs`) accumulates 22 chromatic
/// samples in a ray-march and routinely produces RGB values well above
/// 1.0. Without HDR, those values clip in the view target before
/// tonemapping has any chance to roll off highlights softly — the gravity
/// rings render as dim instead of glowy. With HDR, the bright accumulator
/// values flow into bloom (soft scatter) and `AgX` (perceptual rolloff)
/// before being clamped to display range.
///
/// ### Tonemapping choice: `AgX`
///
/// `AgX` is a Sobotka tonemap that desaturates highlights as they brighten,
/// matching how film and the human eye respond to overexposure. Compared
/// to `TonyMcMapface` (the Bevy default) `AgX` has slightly more aggressive
/// desaturation and a more film-like response curve, which suits the
/// Line sketch's saturated chromatic samples. The `tonemapping_luts`
/// Bevy feature is required (enabled in the workspace `Cargo.toml`) to
/// supply the `AgX` LUT KTX2 asset.
///
/// ### Bloom parameters
///
/// - `intensity: 0.15` — Subtle lift. The Line gravity post-process already
///   does most of the glow work; bloom only needs to scatter the over-1.0
///   pixels into a soft halo, not blow the whole image out.
/// - `low_frequency_boost: 0.7` — Bevy default; biases the multi-scale
///   blur toward broader halos.
/// - `prefilter.threshold: 0.0`, `prefilter.threshold_softness: 0.0` —
///   Bloom everything, no thresholding. Our content is artistic; a
///   non-zero threshold would clip dark detail that we want preserved.
fn spawn_camera(mut commands: Commands<'_, '_>) {
    commands.spawn((
        Camera2d,
        // `Hdr` is the Bevy-0.18 marker component that switches the view
        // target's main texture from `Rgba8UnormSrgb` to `Rgba16Float`.
        // In earlier Bevy versions this was a `Camera.hdr: bool` field;
        // 0.18 moved it to a separate component to make HDR opt-in per
        // camera entity without touching the `Camera` struct.
        Hdr,
        // Pinned explicitly: the Line hand-mesh overlay `Camera3d` is also HDR
        // and targets this same window, so its MSAA MUST differ from this one
        // (it uses `Msaa::Sample4`) — otherwise Bevy gives the two cameras a
        // *shared* intermediate texture (keyed on `(target, usage, hdr, msaa)`)
        // and the overlay's tonemapping corrupts this camera's gravity-smear
        // post-process. See `wc_sketches::hand_mesh`. Keep this `Off`
        // (it was the implicit 2D default anyway) unless you also change the
        // overlay's MSAA to stay distinct.
        Msaa::Off,
        // SDR base: Home/picker render un-tonemapped (their art is already SDR).
        // Each sketch overrides this on enter via its render-profile apply
        // system (see `wc_core::render`); `WC_DEBUG_TONEMAP` still overrides for
        // auditioning (debug builds only).
        debug_tonemapping(),
        Bloom {
            intensity: wc_core::render::BASE_BLOOM_INTENSITY,
            low_frequency_boost: 0.7,
            prefilter: BloomPrefilter {
                threshold: wc_core::render::BASE_BLOOM_THRESHOLD,
                threshold_softness: 0.0,
            },
            composite_mode: wc_core::render::BASE_BLOOM_COMPOSITE.to_bevy(),
            ..Bloom::NATURAL
        },
    ));
}

/// Apply `WC_DEBUG_DISABLE_BLOOM`: zero the main camera's bloom intensity for
/// render-stage isolation (debug builds only).
///
/// Runs each `Update`; cheap because it early-returns when no `DebugToggles`
/// resource is present (the normal-run case) or the toggle is off, and only
/// writes `Bloom.intensity` when it is non-zero. The override never restores a
/// non-default value because nothing else writes bloom intensity at runtime in
/// this app.
#[cfg(debug_assertions)]
fn apply_debug_bloom_toggle(
    toggles: Option<Res<'_, wc_core::debug::DebugToggles>>,
    mut query: Query<'_, '_, &mut Bloom, With<Camera2d>>,
) {
    let Some(toggles) = toggles else {
        return;
    };
    if !toggles.disable_bloom {
        return;
    }
    for mut bloom in &mut query {
        if bloom.intensity != 0.0 {
            bloom.intensity = 0.0;
        }
    }
}

/// Resolve the main camera's tonemapping operator.
///
/// Production/unset default is [`Tonemapping::None`] (the SDR base; sketches
/// override per-sketch via their render-profile apply systems). Debug builds
/// honour the `WC_DEBUG_TONEMAP` spike-test override so a different operator
/// can be auditioned at launch without recompiling:
///
/// - `none` — [`Tonemapping::None`]: no highlight rolloff (bright values clip).
///   The SDR base; same as the default.
/// - `tony` — [`Tonemapping::TonyMcMapface`]: a filmic curve that desaturates
///   highlights and shadows.
/// - `reinhard` — [`Tonemapping::ReinhardLuminance`]: chroma-preserving "neon
///   glow" curve.
/// - anything else / unset — [`Tonemapping::None`] (SDR base; sketches apply
///   their own operator on enter).
///
/// The override is global to this camera, so it also affects the base look of
/// Home and the picker for the duration of the test. Revert by unsetting the
/// variable.
#[cfg(debug_assertions)]
fn debug_tonemapping() -> Tonemapping {
    match std::env::var("WC_DEBUG_TONEMAP")
        .ok()
        .as_deref()
        .map(str::trim)
    {
        Some("none") => {
            tracing::warn!("WC_DEBUG_TONEMAP=none — tonemapping disabled (highlights clip)");
            Tonemapping::None
        }
        Some("tony") => {
            tracing::warn!("WC_DEBUG_TONEMAP=tony — TonyMcMapface tonemap (spike test)");
            Tonemapping::TonyMcMapface
        }
        Some("reinhard") => {
            tracing::warn!("WC_DEBUG_TONEMAP=reinhard — ReinhardLuminance tonemap (spike test)");
            Tonemapping::ReinhardLuminance
        }
        _ => Tonemapping::None,
    }
}

/// Release build: always [`Tonemapping::None`] (SDR base; the spike-test override compiles out).
#[cfg(not(debug_assertions))]
fn debug_tonemapping() -> Tonemapping {
    Tonemapping::None
}

/// Apply the optional `WAVECONDUCTOR_START_SKETCH` override: when set to a
/// sketch name (`line`, `dots`, `cymatics`, case-insensitive) the app
/// navigates straight into that sketch at startup instead of showing the
/// Home picker. Unset (the default) starts at Home.
///
/// This is a deployment + testing convenience: kiosk installs can boot directly
/// into a fixed sketch, and automated screenshot/verification runs can land in
/// the sketch under test without driving the keyboard. An unrecognised value
/// — including `flame`/`waves`, whose `AppState` seams have no implemented
/// sketch behind them yet (`AUDIT.md` T5) — logs a warning and falls back to
/// Home, so no kiosk launch config can boot into a black screen.
///
/// Setting `NextState` in `Startup` triggers the matching `OnEnter` on the
/// first frame.
fn apply_startup_sketch_override(
    mut next: ResMut<'_, NextState<wc_core::lifecycle::state::AppState>>,
) {
    let Ok(name) = std::env::var("WAVECONDUCTOR_START_SKETCH") else {
        return;
    };
    match wc_core::lifecycle::state::AppState::from_name(&name) {
        Some(state) => {
            tracing::info!(sketch = %name, "WAVECONDUCTOR_START_SKETCH: starting in sketch");
            next.set(state);
        }
        None => {
            tracing::warn!(
                value = %name,
                "WAVECONDUCTOR_START_SKETCH: unknown sketch name; starting at Home"
            );
        }
    }
}

/// Holder for the OS display-sleep assertion. `None` = no assertion held
/// (setting off, or acquisition failed). `NonSend` resource: the platform
/// handle has no `Send` guarantee.
#[derive(Default)]
struct DisplayKeepAwake(Option<keepawake::KeepAwake>);

/// Reconcile the OS display-sleep assertion with the persisted
/// "Keep display awake" setting (`ScreensaverSettings::keep_display_awake`,
/// default on): acquire when enabled, drop (releasing the assertion) when
/// disabled. A gallery install idles into attract mode for hours with no
/// input; without the assertion the OS dims and eventually sleeps the panel.
///
/// Steady-state cost is one bool compare. Acquisition failure is non-fatal —
/// the app runs, the operator lengthens the OS display-sleep timeout by hand
/// — and is retried only when the setting is toggled (the `Option` stays
/// `None`, but we only log on transitions, keyed off the setting flip).
fn apply_display_keepawake(
    settings: Res<'_, wc_core::lifecycle::screensaver::ScreensaverSettings>,
    mut holder: NonSendMut<'_, DisplayKeepAwake>,
    mut last_wanted: Local<'_, Option<bool>>,
) {
    let wanted = settings.keep_display_awake;
    if *last_wanted == Some(wanted) {
        return;
    }
    *last_wanted = Some(wanted);
    if wanted {
        // `keepawake` maps to IOPMAssertionCreateWithName on macOS,
        // SetThreadExecutionState on Windows, and the D-Bus inhibitor portals
        // on Linux — covering both the dev laptop and the deployment NUC.
        match keepawake::Builder::default()
            .display(true)
            .reason("Interactive art installation; attract mode must stay visible")
            .app_name("WaveConductor")
            .app_reverse_domain("dev.waveconductor.app")
            .create()
        {
            Ok(handle) => {
                holder.0 = Some(handle);
                tracing::info!("display-sleep inhibitor active (kiosk display stays awake)");
            }
            Err(err) => {
                tracing::warn!(
                    ?err,
                    "could not inhibit display sleep; the OS may dim the display while idle"
                );
            }
        }
    } else if holder.0.take().is_some() {
        tracing::info!("display-sleep inhibitor released (OS power management back in charge)");
    }
}

/// Initialize the global tracing subscriber.
///
/// Honors `RUST_LOG` (e.g. `RUST_LOG=info,wc_core=debug`). When unset, defaults
/// to `info` for the application crates so users can see navigation and idle
/// state transitions in the terminal during manual testing.
/// Captures the reserved `message` field of a tracing event into a string.
///
/// Structured key/value fields are intentionally dropped — the in-app viewer
/// shows the human message; the full structured record still goes to stderr via
/// the fmt layer.
#[derive(Default)]
struct LogMessageVisitor(String);

impl tracing::field::Visit for LogMessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        // tracing records the event message under the reserved `message` field
        // via `record_debug`; the value is `format_args!`, whose Debug renders
        // as the text itself (Debug == Display), so no surrounding quotes.
        if field.name() == "message" {
            use std::fmt::Write as _;
            let _ = write!(self.0, "{value:?}");
        }
    }
}

/// A `tracing` layer that mirrors each event into the shared
/// [`wc_core::diagnostics::LogBuffer`] for the dev panel's Log view.
struct LogCaptureLayer {
    buffer: wc_core::diagnostics::LogBuffer,
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for LogCaptureLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = LogMessageVisitor::default();
        event.record(&mut visitor);
        let meta = event.metadata();
        self.buffer.push(wc_core::diagnostics::LogLine {
            level: *meta.level(),
            target: meta.target().to_owned(),
            message: visitor.0,
        });
    }
}

/// Initialize the tracing subscriber: env-filtered fmt to stderr, a capture
/// layer feeding the in-app [`wc_core::diagnostics::LogBuffer`], and (best
/// effort) a non-blocking rolling on-disk log. Also installs the panic hook.
///
/// Returns the log buffer (inserted as a resource for the dev panel) and the
/// file writer's `WorkerGuard`, which `main` must hold for the process
/// lifetime so buffered log lines are flushed.
fn init_tracing() -> (
    wc_core::diagnostics::LogBuffer,
    Option<tracing_appender::non_blocking::WorkerGuard>,
) {
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;

    let buffer = wc_core::diagnostics::LogBuffer::new(500);
    // `ort=warn`: the `ort` crate creates the ONNX Runtime environment at VERBOSE
    // and bridges every ORT message into `tracing` under the `ort` target, relying
    // on this filter to gate it. At `info` the graph-transformer / initializer /
    // model-cache chatter (hundreds of lines per session init) floods the log;
    // `warn` keeps the meaningful ORT warnings (partition counts, EP assignment)
    // and drops the noise. Overridable: `RUST_LOG=ort=trace` restores the full
    // node-placement dump for debugging (see `inference_ort::backend`).
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,waveconductor=info,wc_core=info,ort=warn"));

    // Best-effort on-disk layer. `.with(Option<Layer>)` is a no-op when `None`.
    let (file_layer, guard) = match logging::file_writer() {
        Some((writer, guard)) => {
            let layer = tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_target(false)
                .with_writer(writer);
            (Some(layer), Some(guard))
        }
        None => (None, None),
    };

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_target(false))
        .with(LogCaptureLayer {
            buffer: buffer.clone(),
        })
        .with(file_layer)
        .init();

    logging::install_panic_hook();
    (buffer, guard)
}
