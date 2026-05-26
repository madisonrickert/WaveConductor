//! Tracks whether the egui UI is capturing the pointer this frame.
//!
//! Sketches consume pointer input directly via Bevy's
//! `ButtonInput<MouseButton>` and `Touches` resources. Without coordination,
//! a click that lands inside the Settings panel both moves the egui slider
//! AND fires the sketch's click handler — for the Line sketch, this means
//! tweaking a slider also spawns an attractor at the slider's screen position.
//!
//! This module exposes [`EguiPointerCaptured`], a thin boolean wrapper that
//! reflects `bevy_egui`'s `EguiWantsInput::wants_any_pointer_input()` state.
//! Sketches read it and suppress their press-edge handling when `true`.
//!
//! [`update_egui_pointer_capture`] copies the value out of `bevy_egui`'s
//! resource each frame. It uses `Option<Res<EguiWantsInput>>` so test
//! harnesses running without `EguiPlugin` (e.g., the `MinimalPlugins`-based
//! `core_plugin_builds_without_panicking` test) don't crash on a missing
//! resource — when `bevy_egui` isn't loaded, the wrapper stays `false`.
//!
//! ## Scheduling
//!
//! `bevy_egui` populates `EguiWantsInput` in `PostUpdate` (via
//! `EguiPostUpdateSet::ProcessOutput::write_egui_wants_input_system`), so a
//! mirror read in the next frame's `Update` carries a one-frame lag against
//! the on-screen UI. That lag is invisible in practice: the panel doesn't
//! move between frames, and the cursor must already be over the panel before
//! the user can click it.
//!
//! ## What sketches do with it
//!
//! Gate press-edge handlers (e.g., "mouse just pressed → spawn attractor")
//! on `!captured.0`. Release events and continuous position updates can
//! still fire regardless, so an attractor that was activated outside the
//! panel and then dragged over it still releases cleanly.

use bevy::prelude::*;

/// Mirror of `bevy_egui::EguiWantsInput::wants_any_pointer_input()`. `true`
/// when the egui UI wants the pointer (hovering over a panel, dragging a
/// widget, popup open). Sketches gate their press-edge handlers on this.
///
/// Default is `false` so the resource is safe to initialize at plugin build
/// time even before [`update_egui_pointer_capture`] has run for the first
/// time.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct EguiPointerCaptured(pub bool);

/// Reflect `bevy_egui`'s pointer-capture state into [`EguiPointerCaptured`].
///
/// Reads `Option<Res<bevy_egui::input::EguiWantsInput>>` so that test
/// harnesses running without `EguiPlugin` don't crash on a missing resource.
/// When `bevy_egui` isn't initialized, the wrapper resets to `false`.
pub fn update_egui_pointer_capture(
    egui_wants: Option<Res<'_, bevy_egui::input::EguiWantsInput>>,
    mut captured: ResMut<'_, EguiPointerCaptured>,
) {
    captured.0 = egui_wants.is_some_and(|w| w.wants_any_pointer_input());
}
