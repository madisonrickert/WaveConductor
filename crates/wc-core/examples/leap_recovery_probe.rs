//! Hand-free Leap **service liveness + recovery-rung-1** probe
//! (`leap-deep-idle-state`).
//!
//! Unlike [`leap_pause_probe`] and [`leap_duty_stress`] — which assert a hand is
//! present during warm-up — this probe assumes **no hand** is over the sensor and
//! that the service may be **wedged** (alive-but-frozen: control path ack's,
//! data path dead). With zero OS privilege it answers:
//!
//!   1. Does a fresh `LeapC` client *connect* at all?
//!   2. Does the *data path* stream frames? A healthy service streams frames
//!      continuously even with no hand (each frame just carries zero hands), so
//!      a nonzero frame count is an unambiguous liveness signal.
//!   3. Does `set_pause(false)` from a fresh client *revive* a dead data path?
//!      This is recovery-ladder **rung 1** ("client reconnect / clear pause"),
//!      which needs no OS privilege. If it works, the privileged service restart
//!      (rung 3) is rarely needed; if it doesn't, a service-level wedge requires
//!      escalation.
//!
//! Run (no hand required):
//!
//! ```text
//! cargo run -p wc-core --example leap_recovery_probe --features hand-tracking-gestures
//! ```
#![cfg(feature = "hand-tracking-gestures")]
#![allow(clippy::print_stdout, clippy::expect_used)]

use std::thread::sleep;
use std::time::{Duration, Instant};

use bevy::prelude::Messages;
use wc_core::input::provider::HandTrackingProvider;
use wc_core::input::providers::leap_native::LeaprsProvider;
use wc_core::input::state::HandTrackingFrame;

/// Poll once; return the number of frames produced since the last poll. Empty
/// (no-hand) frames are counted — they are the liveness signal here.
fn drain(provider: &mut LeaprsProvider, msgs: &mut Messages<HandTrackingFrame>) -> usize {
    provider.poll(Duration::ZERO, msgs);
    msgs.drain().count()
}

/// Poll tightly for `dur`, printing a frames-per-second line each second so a
/// slow trickle is distinguishable from a flat-dead stream. Returns the total
/// frame count over the window.
fn watch(
    provider: &mut LeaprsProvider,
    msgs: &mut Messages<HandTrackingFrame>,
    dur: Duration,
    label: &str,
) -> usize {
    let start = Instant::now();
    let mut total = 0usize;
    let mut sec_mark = Instant::now();
    let mut sec_frames = 0usize;
    while start.elapsed() < dur {
        let n = drain(provider, msgs);
        total += n;
        sec_frames += n;
        if sec_mark.elapsed() >= Duration::from_secs(1) {
            println!("    [{label}] {sec_frames} frames/s (running total {total})");
            sec_frames = 0;
            sec_mark = Instant::now();
        }
        sleep(Duration::from_millis(5));
    }
    total
}

fn main() {
    println!("Leap recovery probe — hand-free liveness + rung-1 (client reconnect) test.\n");

    let mut provider = LeaprsProvider::default();
    // An unfocused CLI receives ZERO frames from even a perfectly healthy service
    // unless it requests the BackgroundFrames policy — which would look identical
    // to a wedge. Request it so the frame count is a true liveness signal.
    provider.request_background = true;

    match provider.start() {
        Ok(()) => println!("client: start() OK — connection object created."),
        Err(err) => {
            println!("client: start() FAILED: {err:?}");
            println!(
                "\nVERDICT: a fresh client cannot even connect — the wedge is at or below the \
                 connection layer. Escalate to rung 3 (service restart)."
            );
            return;
        }
    }
    let mut msgs = Messages::<HandTrackingFrame>::default();

    // Phase A — observe the data path exactly as found.
    println!("\n[A] Observing the data path as-found for 3s (no hand needed)…");
    let before = watch(&mut provider, &mut msgs, Duration::from_secs(3), "as-found");
    if before > 0 {
        println!(
            "\nVERDICT: data path ALIVE ({before} frames in 3s) — NOT wedged right now. \
             The service is healthy (or already recovered)."
        );
        provider.stop();
        return;
    }

    // Phase B — recovery rung 1: a fresh client clears pause and re-observes.
    println!("\n[A] 0 frames in 3s → data path appears DEAD (frozen-but-alive wedge).");
    println!("[B] Rung 1 — fresh client clears pause: set_paused(false), then observe 4s…");
    provider.set_paused(false);
    let after = watch(
        &mut provider,
        &mut msgs,
        Duration::from_secs(4),
        "post-resume",
    );

    if after > 0 {
        println!(
            "\nVERDICT: ✅ REVIVED BY RUNG 1 — a fresh client + set_pause(false) restored the \
             stream ({after} frames). A service-level wedge clears with NO OS privilege."
        );
    } else {
        println!(
            "\nVERDICT: ❌ STILL WEDGED after rung 1 — the control path ack'd but the data path \
             stayed dead (0 frames in 4s). A fresh client does NOT clear this wedge; escalate to \
             rung 3 (privileged service restart) or a USB re-enumeration."
        );
    }
    provider.stop();
}
