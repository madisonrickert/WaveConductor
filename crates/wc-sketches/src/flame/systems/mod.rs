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
//!   re-encode the GPU branch/level tables, and reseed the node buffer. Runs on
//!   every name/point-budget change (including the screensaver carousel).
//! - [`sim_params`] — the per-frame writer: virtual-time `cX` oscillation,
//!   pointer/hand warp, and the single [`sim_params::bake_flame_sim`] baker.
//!   Also the `OnEnter(SketchActivity::Idle)` freeze that zeroes dispatches.
//! - [`camera`] — the [`camera::FlameCamera`] CPU orbit resource: autorotate,
//!   drag, wheel zoom, and decaying fling momentum (F10 sets the momentum on
//!   hand-grab release). [`crate::flame::render::drive_flame_material`] reads
//!   it each frame to build the material's view/projection uniforms.

pub mod camera;
pub mod name_change;
pub mod sim_params;
pub mod spawn;
