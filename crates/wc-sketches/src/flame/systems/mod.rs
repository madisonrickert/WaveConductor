//! Flame sketch main-world driver systems.
//!
//! Split into focused submodules under the ~300-line ceiling:
//!
//! - [`spawn`] — `OnEnter(AppState::Flame)` allocation of the node storage
//!   buffer, plus the [`spawn::FlameRoot`] marker component and the `OnExit`
//!   resource-removal companion. Inserts [`crate::flame::compute::sim_params::FlameSimParams`]
//!   and [`sim_params::FlameState`] for the render/audio/interaction stages.
//! - [`name_change`] — the settings watcher: normalize the name, rebuild the
//!   [`crate::flame::branches::FlameSpec`] + [`crate::flame::levels::LevelLayout`],
//!   and re-encode the GPU branch/level tables. The node buffer is never
//!   reseeded here — the compute morphs the live shape into the new attractor.
//!   Runs on every name/point-budget change (including the screensaver carousel).
//! - [`sim_params`] — the per-frame writer: virtual-time `cX` oscillation,
//!   pointer/hand warp, and the single [`sim_params::bake_flame_sim`] baker.
//!   Also the `OnEnter(SketchActivity::Idle)` freeze that zeroes dispatches.
//! - [`camera`] — the [`camera::FlameCamera`] CPU orbit resource: autorotate,
//!   mouse drag (the operator's orbit), wheel zoom, and the decaying fling
//!   momenta the hand layer leaves on release (yaw from two-hand twist, pan
//!   from a one-hand throw). [`crate::flame::render::drive_flame_material`]
//!   reads it each frame to build the material's view/projection uniforms.
//! - [`hands`] — VR-style "grab space" navigation: gathers grabbing
//!   [`wc_core::input::entity::TrackedHand`]s (with per-hand engage/release
//!   hysteresis); one hand pans the scene with it, two hands zoom (spread) +
//!   rotate (twist) + pan (midpoint) about the grip. Writes
//!   [`hands::FlameGrabState::warp_px`], the pixel-space source
//!   `sim_params::update_flame_sim` maps into the fractal warp. Also owns the
//!   idle veto that keeps the sketch `Active` through a fling's coast-down.

pub mod camera;
pub mod hands;
pub mod name_change;
pub mod sim_params;
pub mod spawn;
