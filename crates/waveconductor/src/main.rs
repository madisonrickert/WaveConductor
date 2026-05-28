//! `WaveConductor` v5 binary entry point.
//!
//! Constructs the Bevy [`App`], registers core plugins, and runs the event loop.
//! In Plan 2 this opens a window and exercises the lifecycle plugin (state
//! machine + leafwing keyboard actions). Sketch plugins land in Plan 6 onward.

use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::post_process::bloom::{Bloom, BloomPrefilter};
use bevy::prelude::*;
use bevy::render::view::Hdr;
use tracing_subscriber::EnvFilter;
use wc_core::audio::background::BackgroundSampleAsset;
use wc_core::CorePlugin;
use wc_sketches::SketchesPlugin;

/// Relative path to the Line sketch's background sample, resolved against
/// the cwd the binary was launched in. `cargo run -p waveconductor` runs
/// In debug builds we resolve against `CARGO_MANIFEST_DIR` (the binary
/// crate's directory at compile time) so the path works regardless of the
/// shell's cwd when `cargo run -p waveconductor` is invoked. Release bundles
/// ship `assets/` next to the binary, so the cwd-relative path is correct
/// there.
///
/// Bevy's `AssetPlugin.file_path = "../../assets"` works by a separate
/// mechanism: Bevy's `FileAssetReader` already resolves against
/// `CARGO_MANIFEST_DIR` in debug builds. `std::fs::read` does not, hence
/// the explicit `concat!`.
#[cfg(debug_assertions)]
const LINE_BACKGROUND_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/sketches/line/line_background.ogg"
);
#[cfg(not(debug_assertions))]
const LINE_BACKGROUND_PATH: &str = "assets/sketches/line/line_background.ogg";

fn main() {
    init_tracing();
    App::new()
        // v4 Line renders against a black background; Bevy defaults to gray.
        // Setting the clear color globally is the simplest way to match —
        // future sketches can override per-state via `OnEnter`/`OnExit` if
        // they want a different backdrop.
        .insert_resource(ClearColor(Color::BLACK))
        .insert_resource(load_line_background())
        .add_plugins((
            DefaultPlugins
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: "WaveConductor".into(),
                        resolution: (1280_u32, 720_u32).into(),
                        ..default()
                    }),
                    ..default()
                })
                .set(AssetPlugin {
                    // Dev builds: shaders live at the workspace root, two levels
                    // above the binary crate. Release bundles: the bundler
                    // copies `assets/` next to the binary, so the default
                    // `"assets"` is correct.
                    #[cfg(debug_assertions)]
                    file_path: "../../assets".into(),
                    ..default()
                })
                // We initialize tracing-subscriber in init_tracing() above;
                // Bevy's LogPlugin would clobber that with its own subscriber
                // and emit `ERROR Could not set global logger…` at startup.
                .disable::<bevy::log::LogPlugin>(),
            bevy_egui::EguiPlugin::default(),
            CorePlugin,
            SketchesPlugin,
        ))
        .add_systems(Startup, spawn_camera)
        .run();
}

/// Read the Line background OGG into a `BackgroundSampleAsset` resource.
///
/// The audio engine runs in a separate cpal thread that can't reach Bevy's
/// `AssetServer`, so we load the file synchronously here on the main
/// thread before `App::run()` and stash the raw bytes in a resource the
/// engine's `Startup` system reads. Failure to read the file logs a
/// warning and yields an empty asset; the engine treats that as "no
/// background mix" and proceeds normally.
fn load_line_background() -> BackgroundSampleAsset {
    match std::fs::read(LINE_BACKGROUND_PATH) {
        Ok(bytes) => {
            tracing::info!(
                path = LINE_BACKGROUND_PATH,
                size = bytes.len(),
                "loaded Line background sample"
            );
            BackgroundSampleAsset::new(bytes)
        }
        Err(err) => {
            tracing::warn!(
                path = LINE_BACKGROUND_PATH,
                ?err,
                "Line background sample not found; audio engine will run without it"
            );
            BackgroundSampleAsset::default()
        }
    }
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
        // See module-level comment for why AgX over TonyMcMapface.
        Tonemapping::AgX,
        Bloom {
            intensity: 0.15,
            low_frequency_boost: 0.7,
            prefilter: BloomPrefilter {
                threshold: 0.0,
                threshold_softness: 0.0,
            },
            ..Bloom::NATURAL
        },
    ));
}

/// Initialize the global tracing subscriber.
///
/// Honors `RUST_LOG` (e.g. `RUST_LOG=info,wc_core=debug`). When unset, defaults
/// to `info` for the application crates so users can see navigation and idle
/// state transitions in the terminal during manual testing.
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,waveconductor=info,wc_core=info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}
