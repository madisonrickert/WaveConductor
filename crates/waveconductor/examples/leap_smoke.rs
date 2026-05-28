//! Smoke test: open a leaprs connection, poll for one tracking frame, print
//! a one-line summary, exit. Verifies the vendored `LeapC` + leap-sys +
//! .cargo/config.toml integration is wired correctly on the current host.
//!
//! Run with:
//! ```bash
//! cargo run --example leap_smoke -p waveconductor
//! ```
//!
//! Expected output (with Ultraleap service running + device attached):
//! ```text
//! leaprs::Connection opened
//! waiting for first tracking event...
//! frame: 1 hand(s), palm0 = (-12.4, 178.3, 41.2) mm, pinch=0.05 grab=0.02
//! ```
//!
//! Expected output (no service): a startup error from `Connection::create()`.

#[cfg(feature = "hand-tracking-gestures")]
fn main() {
    use leaprs::{Connection, ConnectionConfig, EventRef};

    let mut conn = match Connection::create(ConnectionConfig::default()) {
        Ok(c) => c,
        Err(err) => {
            eprintln!("Failed to create connection: {err:?}");
            std::process::exit(1);
        }
    };

    if let Err(err) = conn.open() {
        eprintln!("Failed to open connection: {err:?}");
        std::process::exit(1);
    }

    println!("leaprs::Connection opened");

    // LeapC connection handshake: must receive Connection then Device events
    // before tracking frames start flowing. Timeout is normal during startup;
    // treat it as "keep waiting".
    println!("waiting for service connection event...");
    poll_until(&mut conn, 10, |e| matches!(e, EventRef::Connection(_)));
    println!("service connected; waiting for device event...");
    poll_until(&mut conn, 10, |e| matches!(e, EventRef::Device(_)));
    println!("device ready; waiting for first tracking frame (Ctrl-C to abort)...");

    let timeout = std::time::Duration::from_secs(30);
    let deadline = std::time::Instant::now() + timeout;

    while std::time::Instant::now() < deadline {
        let Ok(msg) = conn.poll(100) else {
            continue; // Timeout or other transient — keep polling.
        };

        if let EventRef::Tracking(tracking) = msg.event() {
            let hands = tracking.hands();
            print!("frame: {} hand(s)", hands.len());
            if let Some(h) = hands.first() {
                let palm = h.palm();
                // `.array()` copies the packed LEAP_VECTOR fields into a [f32; 3],
                // avoiding the unaligned-reference UB that dereferencing x/y/z
                // directly through the packed-struct Deref would cause.
                let [px, py, pz] = palm.position().array();
                // HandRef derefs to LEAP_HAND (packed FFI struct); copy the
                // f32 fields into locals before passing to format args.
                let pinch = h.pinch_strength;
                let grab = h.grab_strength;
                print!(
                    ", palm0 = ({px:.1}, {py:.1}, {pz:.1}) mm, pinch={pinch:.2} grab={grab:.2}"
                );
            }
            println!();
            return;
        }
    }

    eprintln!("no tracking event within {}s", timeout.as_secs());
    std::process::exit(2);
}

/// Poll the connection until `predicate` returns true for an event, or until
/// `timeout_secs` seconds elapse (in which case we exit with an error).
#[cfg(feature = "hand-tracking-gestures")]
fn poll_until(conn: &mut leaprs::Connection, timeout_secs: u64, predicate: impl Fn(&leaprs::EventRef<'_>) -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    while std::time::Instant::now() < deadline {
        if let Ok(msg) = conn.poll(250) {
            if predicate(&msg.event()) {
                return;
            }
        }
    }
    eprintln!("timed out waiting for expected connection event");
    std::process::exit(1);
}

#[cfg(not(feature = "hand-tracking-gestures"))]
fn main() {
    eprintln!(
        "Built without the hand-tracking-gestures feature. \
         Re-run with: cargo run --example leap_smoke -p waveconductor"
    );
    std::process::exit(1);
}
