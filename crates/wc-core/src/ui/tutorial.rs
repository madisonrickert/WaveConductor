//! Attract-mode hand-tracking tutorial overlay (statue + waving hands).
//!
//! Port of v4's screensaver "head statue with animated hands above it"
//! graphic, recomposed for the kiosk deployment: the composition now rests at
//! the **bottom** of the screen (v4 centered it), and the head and hands are
//! **independently toggleable** via
//! [`ScreensaverSettings::tutorial_head`] / [`ScreensaverSettings::tutorial_hands`].
//! It is drawn only while the screensaver is showing *and* hand tracking is
//! actually available — the overlay is an invitation to raise your hands, so
//! it hides entirely when no working sensor could answer that invitation.
//!
//! ## Data flow
//!
//! `load_tutorial_assets` (`Startup`, private) kicks off the async PNG loads
//! for `assets/tutorial/statue.png` and `assets/tutorial/hand.png` (white
//! silhouettes on transparency, rasterized from the v4 SVGs) into
//! [`TutorialAssets`]. [`draw_tutorial_overlay`]
//! (`EguiPrimaryContextPass`, gated on [`ScreensaverActive`])
//! lazily registers each handle with `EguiUserTextures` exactly once, caches
//! the resulting `egui::TextureId`s back into [`TutorialAssets`] (no per-frame
//! registration or allocation beyond egui's own immediate-mode shape
//! tessellation), then paints:
//!
//! - the statue bottom-center (`tutorial_layout`),
//! - two hands above it drifting on v4's 8 s ease-in-out cycle
//!   (`drift_phase` / `hand_drift`), the right hand mirrored,
//! - a procedural glow (each image painted at `GLOW_PASSES`' increasing
//!   scales / decreasing alphas beneath the crisp pass) standing in for v4's
//!   triple CSS drop-shadow,
//! - everything multiplied by the framework [`ScreensaverFade`]
//!   envelope (v4 faded the overlay in over 500 ms; v5 rides the shared 1.5 s
//!   attract fade).
//!
//! The pass runs in every `AppState`, so the draw system is `run_if`-gated on
//! the `ScreensaverActive` marker resource: outside attract mode it is not
//! scheduled at all, keeping it a true no-op per the idle-performance rule.

use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts, EguiTextureHandle};

use crate::input::state::PrimaryState;
use crate::lifecycle::screensaver::fade::ScreensaverFade;
use crate::lifecycle::screensaver::{ScreensaverActive, ScreensaverSettings};

/// Width / height of the statue silhouette. Derived from the v4 SVG viewBox
/// (`0 0 751.1 1371.6`); the rasterized `statue.png` preserves it (600×1096),
/// so hardcoding the source ratio avoids waiting on the async image load to
/// lay out.
const STATUE_ASPECT: f32 = 751.1 / 1371.6;

/// Width / height of the hand silhouette, from the v4 SVG viewBox
/// (`0 0 487.9 679.3`; `hand.png` is 400×557).
const HAND_ASPECT: f32 = 487.9 / 679.3;

/// Statue height as a fraction of window height. "Tastefully small": the
/// tutorial reads as a caption strip along the bottom edge, not a centerpiece
/// competing with the attract visual.
const STATUE_HEIGHT_FRAC: f32 = 0.22;

/// Cap on statue width as a fraction of window width. The statue is tall and
/// narrow (aspect ≈ 0.55) so this only engages on extremely narrow windows,
/// where it shrinks the whole composition proportionally.
const STATUE_MAX_WIDTH_FRAC: f32 = 0.30;

/// Gap between the statue's bottom edge and the window's bottom edge, as a
/// fraction of window height.
const BOTTOM_MARGIN_FRAC: f32 = 0.025;

/// Hand height relative to statue height. In v4's composition the hand SVG
/// (679 units tall) stood at roughly half the statue's 1371-unit height.
const HAND_HEIGHT_RATIO: f32 = 0.5;

/// Lateral offset of each hand's center from the statue's center line, in
/// statue widths. Reproduces v4's 25% / 75% container placement: hands
/// clearly to either side, above the statue's shoulders.
const HAND_SPREAD_RATIO: f32 = 0.75;

/// How far the hands' centers sit above the statue's top edge, in hand
/// heights, so they hover just over the head like v4.
const HAND_RAISE_RATIO: f32 = 0.35;

/// When the head is toggled off, drop the hands by this many statue heights
/// so they hover near where they would naturally be, just slightly lower
/// (they no longer need to clear the statue's crown).
const HANDS_ONLY_DROP_RATIO: f32 = 0.4;

/// Period of the hand drift cycle, seconds — v4's `hand-drift 8s ease-in-out
/// infinite`.
const DRIFT_PERIOD_SECS: f32 = 8.0;

/// Procedural stand-in for v4's triple CSS drop-shadow glow
/// (20/40/60 px at 0.8/0.6/0.4 alpha): the silhouette is repainted at these
/// `(scale, alpha)` pairs, widest-and-faintest first, beneath the crisp
/// full-alpha pass. Cheap (three extra textured quads per element, no
/// shader), and on a white-on-transparent silhouette it reads as the same
/// luminous halo.
const GLOW_PASSES: [(f32, f32); 3] = [(1.18, 0.10), (1.12, 0.16), (1.06, 0.24)];

/// Handles and cached egui texture ids for the two tutorial images.
///
/// The `Handle`s are strong, loaded once at `Startup`; the `egui::TextureId`s
/// are registered lazily on first draw and cached here so the draw path never
/// re-registers (a per-frame `HashMap` insert) or clones handles per frame.
#[derive(Resource, Default)]
pub struct TutorialAssets {
    /// Strong handle to `assets/tutorial/statue.png`.
    statue: Handle<Image>,
    /// Strong handle to `assets/tutorial/hand.png`.
    hand: Handle<Image>,
    /// Egui texture id for the statue, registered on first draw.
    statue_tex: Option<egui::TextureId>,
    /// Egui texture id for the hand, registered on first draw.
    hand_tex: Option<egui::TextureId>,
}

/// Plugin: loads the tutorial images and registers the attract-mode overlay
/// draw system. See the module docs for the full signal flow.
pub struct TutorialOverlayPlugin;

impl Plugin for TutorialOverlayPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TutorialAssets>();
        app.add_systems(Startup, load_tutorial_assets);
        // `EguiPrimaryContextPass` runs in every AppState; the
        // `ScreensaverActive` gate keeps this system unscheduled (zero cost)
        // outside attract mode.
        app.add_systems(
            bevy_egui::EguiPrimaryContextPass,
            draw_tutorial_overlay.run_if(resource_exists::<ScreensaverActive>),
        );
    }
}

/// `Startup`: kick off the async PNG loads. `AssetServer` is optional so
/// `MinimalPlugins`-based tests can install the plugin without asset plugins.
fn load_tutorial_assets(
    server: Option<Res<'_, AssetServer>>,
    mut assets: ResMut<'_, TutorialAssets>,
) {
    let Some(server) = server else { return };
    assets.statue = server.load("tutorial/statue.png");
    assets.hand = server.load("tutorial/hand.png");
}

/// Whether hand tracking is available enough to honor the tutorial's
/// invitation. Mirrors how `leap_led_color_and_tooltip` categorizes:
///
/// - `DeviceAttached` / `Streaming` / `DeviceDegraded` → the sensor exists
///   and will (or already does) deliver frames, so inviting a wave is
///   honest. Degraded tracking still tracks.
/// - `DeviceWedged` → **hidden.** The device is attached but its frame
///   stream is sustained-dead; a visitor waving at a frozen sensor gets no
///   response, which is worse than showing nothing.
/// - Everything else (not started / service missing / disconnected /
///   service-only / failed) → hidden; there is no device to wave at.
pub(crate) fn hand_tracking_available(state: PrimaryState) -> bool {
    matches!(
        state,
        PrimaryState::DeviceAttached | PrimaryState::Streaming | PrimaryState::DeviceDegraded
    )
}

/// Resolve the operator toggles against tracking availability into
/// `(show_head, show_hands)`. Both are forced off when tracking is
/// unavailable — the overlay hides entirely rather than showing a statue
/// with no invitation attached.
pub(crate) fn shown_elements(
    head_toggle: bool,
    hands_toggle: bool,
    tracking_available: bool,
) -> (bool, bool) {
    if !tracking_available {
        return (false, false);
    }
    (head_toggle, hands_toggle)
}

/// Geometry for one frame of the overlay, in egui screen points.
///
/// Produced by [`tutorial_layout`]; `None` fields mean "element not shown".
/// The hand rects are the **un-drifted** rest rectangles — [`hand_drift`]'s
/// per-frame offset and rotation are applied at paint time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TutorialLayout {
    /// Statue rectangle, bottom-center of the screen.
    pub statue: Option<egui::Rect>,
    /// Left hand rest rectangle (palm shown as-authored).
    pub left_hand: Option<egui::Rect>,
    /// Right hand rest rectangle (painted mirrored, v4's `scaleX(-1)`).
    pub right_hand: Option<egui::Rect>,
}

/// Compute the composition layout for a window of `screen` size.
///
/// The statue stands bottom-center at [`STATUE_HEIGHT_FRAC`] of the window
/// height (capped by [`STATUE_MAX_WIDTH_FRAC`] of the width for degenerate
/// aspect ratios — the whole composition shrinks together), resting
/// [`BOTTOM_MARGIN_FRAC`] above the bottom edge. Hands sit above and to
/// either side of the head; with the head toggled off they drop slightly
/// ([`HANDS_ONLY_DROP_RATIO`]) into the vacated space. All quantities are
/// fractions of the window or of the statue, so the layout scales with any
/// window size and works in both portrait (the deployment) and landscape.
pub(crate) fn tutorial_layout(
    screen: egui::Rect,
    show_head: bool,
    show_hands: bool,
) -> TutorialLayout {
    if !show_head && !show_hands {
        return TutorialLayout {
            statue: None,
            left_hand: None,
            right_hand: None,
        };
    }

    // Statue size drives everything, even when the statue itself is hidden.
    let mut statue_h = STATUE_HEIGHT_FRAC * screen.height();
    let mut statue_w = statue_h * STATUE_ASPECT;
    let max_w = STATUE_MAX_WIDTH_FRAC * screen.width();
    if statue_w > max_w {
        // Extremely narrow window: shrink proportionally to the width cap.
        statue_w = max_w;
        statue_h = statue_w / STATUE_ASPECT;
    }
    let center_x = screen.center().x;
    let statue_bottom = screen.bottom() - BOTTOM_MARGIN_FRAC * screen.height();
    let statue_rect = egui::Rect::from_min_max(
        egui::pos2(center_x - statue_w / 2.0, statue_bottom - statue_h),
        egui::pos2(center_x + statue_w / 2.0, statue_bottom),
    );

    let hands = show_hands.then(|| {
        let hand_h = HAND_HEIGHT_RATIO * statue_h;
        let hand_w = hand_h * HAND_ASPECT;
        let mut hand_center_y = statue_rect.top() - HAND_RAISE_RATIO * hand_h;
        if !show_head {
            // Hands-only: settle slightly lower into the space the statue
            // would have occupied.
            hand_center_y += HANDS_ONLY_DROP_RATIO * statue_h;
        }
        let spread = HAND_SPREAD_RATIO * statue_w;
        let size = egui::vec2(hand_w, hand_h);
        (
            egui::Rect::from_center_size(egui::pos2(center_x - spread, hand_center_y), size),
            egui::Rect::from_center_size(egui::pos2(center_x + spread, hand_center_y), size),
        )
    });

    TutorialLayout {
        statue: show_head.then_some(statue_rect),
        left_hand: hands.map(|(l, _)| l),
        right_hand: hands.map(|(_, r)| r),
    }
}

/// Drift envelope in `0..=1` for wall-clock time `t` seconds.
///
/// Reproduces v4's `hand-drift 8s ease-in-out infinite` CSS keyframes: a
/// triangle wave over [`DRIFT_PERIOD_SECS`] (0 → 1 over the first half,
/// back to 0 over the second), shaped by smoothstep (`3u² − 2u³`) as a close
/// stand-in for CSS `ease-in-out` — zero velocity at both keyframe endpoints.
pub(crate) fn drift_phase(t: f32) -> f32 {
    let cycle = (t / DRIFT_PERIOD_SECS).rem_euclid(1.0);
    let tri = if cycle < 0.5 {
        cycle * 2.0
    } else {
        2.0 - cycle * 2.0
    };
    // Smoothstep: eases in and out of each keyframe like CSS ease-in-out.
    tri * tri * (3.0 - 2.0 * tri)
}

/// Per-frame drift `(offset, rotation-in-radians)` for a hand of `size`, at
/// envelope value `phase` (from [`drift_phase`]).
///
/// v4 keyframes, expressed relative to the rest pose:
/// - `translate` x: −50% → −20% of the hand's own width ⇒ `+30% · phase`;
/// - `translate` y: −50% → −55% of its height ⇒ `−5% · phase` (upward);
/// - `rotate`: −5° → +5° ⇒ lerp by `phase`.
///
/// The right hand (`mirrored = true`) is v4's `scaleX(-1)` copy: its lateral
/// drift and rotation are negated so the pair sway symmetrically; the small
/// vertical bob is shared.
pub(crate) fn hand_drift(phase: f32, size: egui::Vec2, mirrored: bool) -> (egui::Vec2, f32) {
    let dx = 0.30 * size.x * phase;
    let dy = -0.05 * size.y * phase;
    let rot = (-5.0 + 10.0 * phase).to_radians();
    if mirrored {
        (egui::vec2(-dx, dy), -rot)
    } else {
        (egui::vec2(dx, dy), rot)
    }
}

/// `EguiPrimaryContextPass` (gated on `ScreensaverActive`): paint the tutorial
/// overlay. All non-egui inputs are `Option`al so headless tests (and builds
/// without the hand-tracking provider) never panic; a missing registry reads
/// as "tracking unavailable" and the overlay stays hidden.
pub fn draw_tutorial_overlay(
    mut contexts: EguiContexts<'_, '_>,
    mut assets: ResMut<'_, TutorialAssets>,
    settings: Option<Res<'_, ScreensaverSettings>>,
    fade: Option<Res<'_, ScreensaverFade>>,
    registry: Option<Res<'_, crate::input::provider::ProviderRegistry>>,
) {
    let Some(settings) = settings else { return };

    let available = registry.is_some_and(|r| hand_tracking_available(r.primary_status().primary()));
    let (show_head, show_hands) =
        shown_elements(settings.tutorial_head, settings.tutorial_hands, available);
    if !show_head && !show_hands {
        return;
    }

    // Global opacity rides the shared attract fade envelope (v5's version of
    // v4's 500 ms overlay fade-in). Nothing to paint while fully faded out.
    let fade_alpha = fade.map_or(1.0, |f| f.alpha());
    if fade_alpha <= 0.0 {
        return;
    }

    // Lazily register the egui textures once; cached ids thereafter (no
    // per-frame map inserts / handle clones on the draw path).
    if assets.statue_tex.is_none() {
        let handle = EguiTextureHandle::Strong(assets.statue.clone());
        assets.statue_tex = Some(contexts.add_image(handle));
    }
    if assets.hand_tex.is_none() {
        let handle = EguiTextureHandle::Strong(assets.hand.clone());
        assets.hand_tex = Some(contexts.add_image(handle));
    }
    let (Some(statue_tex), Some(hand_tex)) = (assets.statue_tex, assets.hand_tex) else {
        return;
    };

    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    let screen = ctx.content_rect();
    let layout = tutorial_layout(screen, show_head, show_hands);

    // Foreground layer painter: pure painting, no interaction surface (the
    // screensaver dismisses on any input anyway).
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("attract_tutorial_overlay"),
    ));

    if let Some(statue) = layout.statue {
        paint_silhouette(&painter, statue_tex, statue, 0.0, false, fade_alpha);
    }

    if let (Some(left), Some(right)) = (layout.left_hand, layout.right_hand) {
        // egui repaints every frame in this app, so `i.time` advances
        // continuously; no explicit repaint request is needed.
        #[allow(
            clippy::as_conversions,
            clippy::cast_possible_truncation,
            reason = "egui time is f64 seconds; f32 precision is ample for an 8 s cycle"
        )]
        let phase = drift_phase(ctx.input(|i| i.time) as f32);
        let (offset, rot) = hand_drift(phase, left.size(), false);
        paint_silhouette(
            &painter,
            hand_tex,
            left.translate(offset),
            rot,
            false,
            fade_alpha,
        );
        let (offset, rot) = hand_drift(phase, right.size(), true);
        paint_silhouette(
            &painter,
            hand_tex,
            right.translate(offset),
            rot,
            true,
            fade_alpha,
        );
    }
}

/// Paint one white silhouette with its procedural glow.
///
/// The image is drawn once per [`GLOW_PASSES`] entry (scaled about its
/// center, faint) and then once crisp at full `alpha`. `rot` rotates the quad
/// about its center; `mirror` flips the texture horizontally by swapping the
/// UV x range (v4's `scaleX(-1)` for the right hand).
fn paint_silhouette(
    painter: &egui::Painter,
    tex: egui::TextureId,
    rect: egui::Rect,
    rot: f32,
    mirror: bool,
    alpha: f32,
) {
    let uv = if mirror {
        egui::Rect::from_min_max(egui::pos2(1.0, 0.0), egui::pos2(0.0, 1.0))
    } else {
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0))
    };
    for (scale, glow_alpha) in GLOW_PASSES {
        let glow_rect = egui::Rect::from_center_size(rect.center(), rect.size() * scale);
        paint_quad(painter, tex, glow_rect, uv, rot, glow_alpha * alpha);
    }
    paint_quad(painter, tex, rect, uv, rot, alpha);
}

/// Push one textured quad (optionally rotated about its center) tinted white
/// at `alpha`.
fn paint_quad(
    painter: &egui::Painter,
    tex: egui::TextureId,
    rect: egui::Rect,
    uv: egui::Rect,
    rot: f32,
    alpha: f32,
) {
    let tint = egui::Color32::WHITE.gamma_multiply(alpha);
    let mut mesh = egui::Mesh::with_texture(tex);
    mesh.add_rect_with_uv(rect, uv, tint);
    if rot != 0.0 {
        mesh.rotate(egui::emath::Rot2::from_angle(rot), rect.center());
    }
    painter.add(egui::Shape::mesh(mesh));
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test-only: panicking on a layout element the test just asked to show \
              is the intended failure mode"
)]
mod tests {
    use super::*;

    /// A portrait deployment-shaped screen (the Priceless kiosk is portrait).
    fn portrait() -> egui::Rect {
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1080.0, 1920.0))
    }

    fn landscape() -> egui::Rect {
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1920.0, 1080.0))
    }

    #[test]
    fn drift_phase_hits_v4_keyframes() {
        // 0%, 50%, 100% of the 8 s cycle land exactly on the CSS keyframes.
        assert!((drift_phase(0.0) - 0.0).abs() < 1e-6);
        assert!((drift_phase(4.0) - 1.0).abs() < 1e-6);
        assert!((drift_phase(8.0) - 0.0).abs() < 1e-6);
        // Periodicity: t = 12 s is the same pose as t = 4 s.
        assert!((drift_phase(12.0) - drift_phase(4.0)).abs() < 1e-6);
        // Quarter cycle: smoothstep(0.5) = 0.5 exactly, and the envelope is
        // symmetric about the half-cycle peak.
        assert!((drift_phase(2.0) - 0.5).abs() < 1e-6);
        assert!((drift_phase(2.0) - drift_phase(6.0)).abs() < 1e-6);
        // Negative time (a clock that starts before zero) must not escape 0..=1.
        let p = drift_phase(-3.0);
        assert!((0.0..=1.0).contains(&p));
    }

    #[test]
    fn hand_drift_matches_v4_translation_and_mirrors() {
        let size = egui::vec2(100.0, 200.0);
        // Rest pose (phase 0): no offset, −5° rotation.
        let (off, rot) = hand_drift(0.0, size, false);
        assert_eq!(off, egui::Vec2::ZERO);
        assert!((rot - (-5.0_f32).to_radians()).abs() < 1e-6);
        // Peak pose (phase 1): +30% width right, −5% height up, +5°.
        let (off, rot) = hand_drift(1.0, size, false);
        assert!((off.x - 30.0).abs() < 1e-4);
        assert!((off.y - (-10.0)).abs() < 1e-4);
        assert!((rot - 5.0_f32.to_radians()).abs() < 1e-6);
        // Mirrored (right) hand: lateral drift and rotation negated, vertical
        // bob shared — the pair sway symmetrically.
        let (m_off, m_rot) = hand_drift(1.0, size, true);
        assert!((m_off.x - (-off.x)).abs() < 1e-6);
        assert!((m_off.y - off.y).abs() < 1e-6);
        assert!((m_rot - (-rot)).abs() < 1e-6);
    }

    #[test]
    fn availability_predicate_matches_led_categories() {
        use PrimaryState as S;
        // Available: sensor present and (about to be) delivering frames.
        assert!(hand_tracking_available(S::DeviceAttached));
        assert!(hand_tracking_available(S::Streaming));
        assert!(hand_tracking_available(S::DeviceDegraded));
        // Hidden: no sensor, no service, or a sensor that cannot respond.
        assert!(!hand_tracking_available(S::NotStarted));
        assert!(!hand_tracking_available(S::ServiceMissing));
        assert!(!hand_tracking_available(S::Disconnected));
        assert!(!hand_tracking_available(S::ServiceOnly));
        assert!(!hand_tracking_available(S::DeviceWedged));
        assert!(!hand_tracking_available(S::DeviceFailed));
    }

    #[test]
    fn shown_elements_respects_toggles_and_availability() {
        // Tracking available: toggles pass through in every combination.
        assert_eq!(shown_elements(true, true, true), (true, true));
        assert_eq!(shown_elements(true, false, true), (true, false));
        assert_eq!(shown_elements(false, true, true), (false, true));
        assert_eq!(shown_elements(false, false, true), (false, false));
        // Tracking unavailable: everything hides regardless of toggles.
        assert_eq!(shown_elements(true, true, false), (false, false));
        assert_eq!(shown_elements(true, false, false), (false, false));
        assert_eq!(shown_elements(false, true, false), (false, false));
    }

    #[test]
    fn layout_rests_statue_at_bottom_center() {
        for screen in [portrait(), landscape()] {
            let layout = tutorial_layout(screen, true, true);
            let statue = layout.statue.expect("head shown");
            // Bottom-center, margin above the bottom edge.
            assert!((statue.center().x - screen.center().x).abs() < 1e-3);
            let expected_bottom = screen.bottom() - BOTTOM_MARGIN_FRAC * screen.height();
            assert!((statue.bottom() - expected_bottom).abs() < 1e-3);
            // Tastefully small: exactly the configured height fraction (the
            // width cap does not engage at these aspect ratios)…
            assert!((statue.height() - STATUE_HEIGHT_FRAC * screen.height()).abs() < 1e-3);
            // …with the source aspect preserved.
            assert!((statue.width() / statue.height() - STATUE_ASPECT).abs() < 1e-4);
        }
    }

    #[test]
    fn layout_places_hands_above_and_astride_the_statue() {
        let screen = portrait();
        let layout = tutorial_layout(screen, true, true);
        let statue = layout.statue.expect("head shown");
        let left = layout.left_hand.expect("hands shown");
        let right = layout.right_hand.expect("hands shown");
        // Above the statue's top edge (rest pose)…
        assert!(left.center().y < statue.top());
        assert!(right.center().y < statue.top());
        // …symmetric about the center line, left on the left.
        assert!(left.center().x < statue.center().x);
        assert!(right.center().x > statue.center().x);
        let l_off = statue.center().x - left.center().x;
        let r_off = right.center().x - statue.center().x;
        assert!((l_off - r_off).abs() < 1e-3);
        // Hand scale follows the v4 composition: half the statue's height.
        assert!((left.height() - HAND_HEIGHT_RATIO * statue.height()).abs() < 1e-3);
        assert!((left.width() / left.height() - HAND_ASPECT).abs() < 1e-4);
    }

    #[test]
    fn hands_only_layout_drops_hands_slightly_lower() {
        let screen = portrait();
        let with_head = tutorial_layout(screen, true, true);
        let hands_only = tutorial_layout(screen, false, true);
        assert!(hands_only.statue.is_none(), "head toggled off");
        let full = with_head.left_hand.expect("hands shown");
        let alone = hands_only.left_hand.expect("hands shown");
        // Same x, same size; lower y (dropped into the vacated space) but
        // still fully above the bottom margin.
        assert!((alone.center().x - full.center().x).abs() < 1e-3);
        assert_eq!(alone.size(), full.size());
        assert!(alone.center().y > full.center().y);
        assert!(alone.bottom() < screen.bottom());
    }

    #[test]
    fn head_only_layout_has_no_hands() {
        let layout = tutorial_layout(portrait(), true, false);
        assert!(layout.statue.is_some());
        assert!(layout.left_hand.is_none());
        assert!(layout.right_hand.is_none());
    }

    #[test]
    fn nothing_shown_yields_empty_layout() {
        let layout = tutorial_layout(portrait(), false, false);
        assert_eq!(
            layout,
            TutorialLayout {
                statue: None,
                left_hand: None,
                right_hand: None,
            }
        );
    }

    #[test]
    fn width_cap_engages_on_degenerate_narrow_windows() {
        // A sliver of a window: 200 wide, 2000 tall. Uncapped the statue
        // would be 0.22 · 2000 · 0.5477 ≈ 241 px wide — wider than the
        // window. The cap shrinks it to 30% of the width, aspect preserved.
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(200.0, 2000.0));
        let layout = tutorial_layout(screen, true, true);
        let statue = layout.statue.expect("head shown");
        assert!((statue.width() - STATUE_MAX_WIDTH_FRAC * screen.width()).abs() < 1e-3);
        assert!((statue.width() / statue.height() - STATUE_ASPECT).abs() < 1e-4);
        // The whole composition shrinks together: hands scale off the capped
        // statue and stay inside the window laterally.
        let left = layout.left_hand.expect("hands shown");
        assert!(left.center().x > screen.left());
    }
}
