//! Windows OBSBOT backend: raw bindings to the vendored libdev SDK's
//! extern "C" shim (`vendor/libdev/shim/obsbot_shim.h`) plus the dedicated
//! worker thread that owns every device call.
//!
//! ## Threading model
//!
//! All SDK IO happens on one `obsbot-control` `std::thread`: libdev's
//! Block-mode setters wait for a device round-trip (up to seconds on a
//! wedged USB link), which must never stall the Bevy schedule. The SDK's own
//! hotplug callback fires on an SDK-owned thread; the shim confines it to an
//! atomic epoch counter that this worker polls — no cross-language callback
//! ever reaches Rust. Bevy talks to the worker over two `std::sync::mpsc`
//! channels (commands in, [`ObsbotStatus`] out); the drain system in
//! `obsbot::mod` mirrors the latter into `Res<ObsbotControl>`.
//!
//! ## Hot-path discipline
//!
//! The worker loop wakes at `POLL_INTERVAL` and, in steady state (device
//! held, no hotplug event, empty command queue), performs one atomic load and
//! two `try_recv`-class checks — no allocation, no SDK call. Device scans
//! (which do allocate, in the SDK) run only on a hotplug epoch change or on
//! the `RESCAN_INTERVAL` backoff while no device is present.
//!
//! ## Shutdown
//!
//! Dropping `WorkerHandle` (with the `ObsbotControl` resource, when the
//! Bevy `App` drops) sends `Shutdown` and joins: the worker releases control
//! — re-enabling the camera's AI + gestures — then closes the SDK. A hard
//! process kill skips this; `docs/runbooks/obsbot.md` covers manual recovery
//! via OBSBOT Center.

// The workspace lint `unsafe_code = "deny"` is lifted for this FFI module
// (the same pattern as `lifecycle/thermal/platform/windows.rs` and the
// capture backends): every `unsafe` block below is a call into the audited
// obsbot_shim C ABI and carries a SAFETY comment.
#![allow(unsafe_code)]

use std::ptr;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{mpsc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use bevy::log::{error, info, warn};

use super::super::{product_name, take_control_outcome, ControlSteps, ObsbotStatus, WorkerCommand};

/// Raw extern "C" bindings to `vendor/libdev/shim/obsbot_shim.h`. Contracts
/// (error codes, ownership, threading) are documented on the C declarations;
/// wc-core's build.rs compiles the shim and links `libdev.lib`.
pub(super) mod ffi {
    use std::os::raw::c_char;

    /// Opaque device handle allocated by the shim
    /// (owns a `std::shared_ptr<Device>`); release via
    /// [`obsbot_device_release`].
    #[repr(C)]
    pub struct ObsbotDevice {
        _opaque: [u8; 0],
    }

    /// `OBSBOT_OK` — success return code.
    pub const OBSBOT_OK: i32 = 0;

    extern "C" {
        pub fn obsbot_init() -> i32;
        pub fn obsbot_shutdown();
        pub fn obsbot_hotplug_epoch() -> u32;
        pub fn obsbot_first_device() -> *mut ObsbotDevice;
        pub fn obsbot_device_release(dev: *mut ObsbotDevice);
        pub fn obsbot_device_info(
            dev: *mut ObsbotDevice,
            product_type: *mut i32,
            sn_buf: *mut c_char,
            sn_cap: usize,
            fw_buf: *mut c_char,
            fw_cap: usize,
        ) -> i32;
        pub fn obsbot_take_control(dev: *mut ObsbotDevice) -> u32;
        pub fn obsbot_release_control(dev: *mut ObsbotDevice) -> u32;
        pub fn obsbot_set_gimbal_angle(dev: *mut ObsbotDevice, pitch: f32, yaw: f32) -> i32;
        pub fn obsbot_set_gimbal_speed(dev: *mut ObsbotDevice, pitch: f64, pan: f64) -> i32;
        pub fn obsbot_gimbal_stop(dev: *mut ObsbotDevice) -> i32;
        pub fn obsbot_set_zoom(dev: *mut ObsbotDevice, ratio: f32) -> i32;
        pub fn obsbot_set_fov(dev: *mut ObsbotDevice, fov_type: i32) -> i32;
    }
}

/// Worker wake interval: the command-channel wait that doubles as the
/// hotplug-epoch poll tick. Steady state does no SDK work (see module docs).
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Backoff between device-list scans while no device is present. Scans
/// allocate inside the SDK, so they are rate-limited rather than per-tick;
/// a hotplug event (epoch change) always triggers an immediate scan.
const RESCAN_INTERVAL: Duration = Duration::from_secs(5);

/// Per-step log labels for the take-control sequence, in execution order.
const STEP_LABELS: [(ControlSteps, &str); 5] = [
    (ControlSteps::AI_OFF, "disable on-device AI tracking"),
    (ControlSteps::GESTURE_OFF, "disable gesture control"),
    (ControlSteps::GIMBAL_CENTER, "recenter gimbal"),
    (ControlSteps::FOV_WIDE, "widest FOV (86\u{b0}) + zoom reset"),
    (ControlSteps::AUTO_EXPOSURE, "re-assert auto exposure"),
];

/// Handle to the `obsbot-control` worker thread. Owned by
/// `ObsbotControl.worker`; dropping it performs the clean release +
/// shutdown described in the module docs.
pub struct WorkerHandle {
    cmd_tx: Sender<WorkerCommand>,
    /// `mpsc::Receiver` is `!Sync`; the `Mutex` makes the handle a valid
    /// Bevy `Resource` field. Uncontended — the drain system is the only
    /// caller.
    status_rx: Mutex<Receiver<ObsbotStatus>>,
    join: Option<JoinHandle<()>>,
}

impl WorkerHandle {
    /// Send a command to the worker. Returns `false` if the worker exited.
    pub fn send(&self, cmd: WorkerCommand) -> bool {
        self.cmd_tx.send(cmd).is_ok()
    }

    /// Non-blocking: next status update from the worker, if any.
    pub fn try_recv_status(&self) -> Option<ObsbotStatus> {
        self.status_rx.lock().ok()?.try_recv().ok()
    }
}

impl Drop for WorkerHandle {
    /// Clean shutdown: ask the worker to release control (bounded by the
    /// SDK's Block-mode round-trip timeouts) and join it, so the camera is
    /// restored before the process ends.
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(WorkerCommand::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Spawn the `obsbot-control` worker thread. `take_control` seeds whether a
/// discovered device is taken over immediately (the persisted setting).
/// Returns `None` only if the OS refuses the thread.
pub fn spawn_worker(take_control: bool) -> Option<WorkerHandle> {
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (status_tx, status_rx) = mpsc::channel();
    let join = std::thread::Builder::new()
        .name("obsbot-control".into())
        .spawn(move || worker_main(&cmd_rx, &status_tx, take_control))
        .map_err(|err| error!("OBSBOT control worker failed to spawn: {err}"))
        .ok()?;
    Some(WorkerHandle {
        cmd_tx,
        status_rx: Mutex::new(status_rx),
        join: Some(join),
    })
}

/// Worker entry point: initialize the SDK singleton, then run the
/// command/poll loop until shutdown.
fn worker_main(
    cmd_rx: &Receiver<WorkerCommand>,
    status_tx: &Sender<ObsbotStatus>,
    want_control: bool,
) {
    // SAFETY: first SDK call on this thread; obsbot_init is idempotent,
    // catches all C++ exceptions, and merely registers the epoch callback.
    let ret = unsafe { ffi::obsbot_init() };
    if ret != ffi::OBSBOT_OK {
        error!("OBSBOT SDK init failed ({ret}); camera control unavailable");
        let _ = status_tx.send(ObsbotStatus::NoDevice);
        return;
    }
    info!("OBSBOT SDK initialized (libdev); watching for devices");
    Worker {
        status_tx: status_tx.clone(),
        dev: ptr::null_mut(),
        identity: None,
        in_control: false,
        want_control,
        last_epoch: None,
        last_scan: Instant::now(),
    }
    .run(cmd_rx);
}

/// Identity strings read once per device acquisition.
#[derive(Clone)]
struct DeviceIdentity {
    sn: String,
    firmware: String,
    product_raw: i32,
}

/// State owned by the worker thread. Holds the raw device pointer, so the
/// type is deliberately `!Send` — it is constructed and dropped inside the
/// thread and never crosses it.
struct Worker {
    status_tx: Sender<ObsbotStatus>,
    dev: *mut ffi::ObsbotDevice,
    identity: Option<DeviceIdentity>,
    in_control: bool,
    want_control: bool,
    last_epoch: Option<u32>,
    last_scan: Instant,
}

impl Worker {
    /// Command/poll loop; returns after `Shutdown` (or a dropped command
    /// channel) with the device released and the SDK closed.
    fn run(mut self, cmd_rx: &Receiver<WorkerCommand>) {
        loop {
            match cmd_rx.recv_timeout(POLL_INTERVAL) {
                Ok(WorkerCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(cmd) => self.handle_command(cmd),
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.tick();
        }
        self.shutdown();
    }

    /// One poll tick: rescan on a hotplug epoch change, or on the backoff
    /// interval while no device is held. Steady state does no SDK work.
    fn tick(&mut self) {
        // SAFETY: atomic load in the shim; callable from any thread.
        let epoch = unsafe { ffi::obsbot_hotplug_epoch() };
        let epoch_changed = self.last_epoch != Some(epoch);
        let rescan_due = self.dev.is_null() && self.last_scan.elapsed() >= RESCAN_INTERVAL;
        if epoch_changed || rescan_due {
            self.last_epoch = Some(epoch);
            self.rescan();
        }
    }

    /// Re-resolve the first device after a hotplug event (or backoff retry)
    /// and, if the operator wants it, run the take-control sequence.
    fn rescan(&mut self) {
        self.last_scan = Instant::now();
        // A held handle may refer to an unplugged device; drop it and
        // re-resolve. Control (if any) is re-taken immediately below, so no
        // release/restore churn happens on the device itself.
        self.drop_device();
        // SAFETY: SDK initialized in worker_main; the returned handle (if
        // non-null) is owned by us until obsbot_device_release.
        self.dev = unsafe { ffi::obsbot_first_device() };
        if self.dev.is_null() {
            self.publish(ObsbotStatus::NoDevice);
            return;
        }
        self.identity = self.read_identity();
        let Some(identity) = self.identity.clone() else {
            // Unreadable identity usually means the device vanished between
            // list and query; drop it and let the next tick retry.
            self.drop_device();
            self.publish(ObsbotStatus::NoDevice);
            return;
        };
        info!(
            "OBSBOT device detected: {} sn={} fw={}",
            product_name(identity.product_raw),
            identity.sn,
            identity.firmware
        );
        if self.want_control {
            self.take_control();
        } else {
            self.publish(ObsbotStatus::ControlDisabled { sn: identity.sn });
        }
    }

    /// Run the take-control sequence on the held device and publish the
    /// outcome. Logs one INFO line per step — the operator's confirmation at
    /// a gig that the camera's AI is actually out of the loop.
    fn take_control(&mut self) {
        let Some(identity) = self.identity.clone() else {
            return;
        };
        self.publish(ObsbotStatus::TakingControl);
        // SAFETY: non-null handle owned by this thread; the shim catches all
        // exceptions and returns the achieved-step bitmask.
        let bits = unsafe { ffi::obsbot_take_control(self.dev) };
        let achieved = ControlSteps::from_bits_truncate(bits);
        for (step, label) in STEP_LABELS {
            if achieved.contains(step) {
                info!("OBSBOT take control: {label}: ok");
            } else {
                warn!("OBSBOT take control: {label}: FAILED");
            }
        }
        // Any achieved step is worth undoing at shutdown, even on a Failed
        // run — release_control is safe to call regardless.
        self.in_control = !achieved.is_empty();
        let status = take_control_outcome(
            achieved,
            &identity.sn,
            &identity.firmware,
            product_name(identity.product_raw),
        );
        if let ObsbotStatus::Failed { .. } = &status {
            warn!(
                "OBSBOT take control FAILED (achieved {achieved:?}); the camera's on-device AI \
                 may still fight app tracking — disable AI/gestures manually in OBSBOT Center \
                 (see docs/runbooks/obsbot.md)"
            );
        }
        self.publish(status);
    }

    /// Restore the camera's own behavior (AI + gestures back on). No-op when
    /// control was never taken.
    fn release_control(&mut self) {
        if self.dev.is_null() || !self.in_control {
            return;
        }
        // SAFETY: non-null handle owned by this thread; exception-safe shim.
        let bits = unsafe { ffi::obsbot_release_control(self.dev) };
        let restored = ControlSteps::from_bits_truncate(bits);
        info!(
            "OBSBOT release control: AI restore: {}; gesture restore: {}",
            if restored.contains(ControlSteps::AI_OFF) {
                "ok"
            } else {
                "FAILED"
            },
            if restored.contains(ControlSteps::GESTURE_OFF) {
                "ok"
            } else {
                "FAILED"
            },
        );
        self.in_control = false;
    }

    /// Apply a command from the Bevy side (`Shutdown` is intercepted by
    /// `Worker::run` and never reaches here).
    fn handle_command(&mut self, cmd: WorkerCommand) {
        match cmd {
            WorkerCommand::SetTakeControl(true) => {
                self.want_control = true;
                if !self.dev.is_null() && !self.in_control {
                    self.take_control();
                }
            }
            WorkerCommand::SetTakeControl(false) => {
                self.want_control = false;
                if self.in_control {
                    self.release_control();
                    if let Some(identity) = &self.identity {
                        let sn = identity.sn.clone();
                        self.publish(ObsbotStatus::ControlDisabled { sn });
                    }
                }
            }
            WorkerCommand::SetGimbalAngle { pitch, yaw } => {
                // SAFETY (each arm): `manual` guards for a non-null,
                // in-control handle before invoking the exception-safe shim.
                self.manual("set gimbal angle", |dev| unsafe {
                    ffi::obsbot_set_gimbal_angle(dev, pitch, yaw)
                });
            }
            WorkerCommand::SetGimbalSpeed { pitch, pan } => {
                self.manual("set gimbal speed", |dev| unsafe {
                    ffi::obsbot_set_gimbal_speed(dev, pitch, pan)
                });
            }
            WorkerCommand::GimbalStop => {
                self.manual("gimbal stop", |dev| unsafe { ffi::obsbot_gimbal_stop(dev) });
            }
            WorkerCommand::SetZoom(ratio) => {
                self.manual("set zoom", |dev| unsafe {
                    ffi::obsbot_set_zoom(dev, ratio)
                });
            }
            WorkerCommand::SetFov(fov) => {
                self.manual("set fov", |dev| unsafe {
                    ffi::obsbot_set_fov(dev, fov.raw())
                });
            }
            WorkerCommand::Shutdown => {}
        }
    }

    /// Run a manual-control call if (and only if) the app currently holds
    /// control — the SDK requires AI off before manual gimbal commands hold.
    fn manual(&self, what: &'static str, call: impl FnOnce(*mut ffi::ObsbotDevice) -> i32) {
        if self.dev.is_null() || !self.in_control {
            warn!("OBSBOT manual command '{what}' ignored: not in control of a device");
            return;
        }
        let ret = call(self.dev);
        if ret == ffi::OBSBOT_OK {
            info!("OBSBOT manual command '{what}': ok");
        } else {
            warn!("OBSBOT manual command '{what}' failed ({ret})");
        }
    }

    /// Read product/serial/firmware from the held device.
    fn read_identity(&self) -> Option<DeviceIdentity> {
        let mut product_raw: i32 = -1;
        let mut sn = [0u8; 64];
        let mut fw = [0u8; 64];
        // SAFETY: non-null handle; buffers outlive the call and capacities
        // are passed, so the shim NUL-terminates within bounds.
        let ret = unsafe {
            ffi::obsbot_device_info(
                self.dev,
                &raw mut product_raw,
                sn.as_mut_ptr().cast(),
                sn.len(),
                fw.as_mut_ptr().cast(),
                fw.len(),
            )
        };
        if ret != ffi::OBSBOT_OK {
            warn!("OBSBOT device info read failed ({ret})");
            return None;
        }
        Some(DeviceIdentity {
            sn: buf_to_string(&sn),
            firmware: buf_to_string(&fw),
            product_raw,
        })
    }

    /// Release the device handle (not the SDK) and reset per-device state.
    fn drop_device(&mut self) {
        if !self.dev.is_null() {
            // SAFETY: handle owned by this thread; release is null-safe and
            // only drops the shared_ptr (no device IO).
            unsafe { ffi::obsbot_device_release(self.dev) };
            self.dev = ptr::null_mut();
        }
        self.identity = None;
        self.in_control = false;
    }

    /// Final teardown: restore the camera, drop the handle, close the SDK.
    fn shutdown(&mut self) {
        self.release_control();
        self.drop_device();
        // SAFETY: last SDK call in the process (the singleton cannot be
        // re-initialized after close; see obsbot_shim.h).
        unsafe { ffi::obsbot_shutdown() };
        info!("OBSBOT control worker stopped");
    }

    /// Publish a status update; the drain system may already be gone during
    /// app teardown, which is fine.
    fn publish(&self, status: ObsbotStatus) {
        let _ = self.status_tx.send(status);
    }
}

/// NUL-terminated shim string buffer → owned `String` (lossy UTF-8).
fn buf_to_string(buf: &[u8]) -> String {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use super::*;

    /// NUL handling for the identity buffers.
    #[test]
    fn buf_to_string_stops_at_nul() {
        assert_eq!(buf_to_string(b"1.2.3.4\0garbage"), "1.2.3.4");
        assert_eq!(buf_to_string(b"\0"), "");
        assert_eq!(buf_to_string(b"no-nul"), "no-nul");
    }

    /// Hardware smoke test — ignored by default because it needs a real
    /// OBSBOT camera on USB (and `libdev.dll` + `w32-pthreads.dll` beside
    /// the test binary, which build.rs stages). Run with:
    ///
    /// ```text
    /// cargo test -p wc-core --features obsbot-camera-control \
    ///     obsbot_hardware_smoke -- --ignored --nocapture
    /// ```
    ///
    /// Inits the SDK, waits out the async enumeration, takes control (AI off,
    /// gestures off, gimbal recenter — the camera should physically move —
    /// widest FOV, auto exposure), holds for 2 s, then releases and shuts
    /// down. Prints every return code for the operator to eyeball.
    #[test]
    #[ignore = "requires a plugged-in OBSBOT camera; run with -- --ignored --nocapture"]
    fn obsbot_hardware_smoke() {
        use crate::input::obsbot::REQUIRED_STEPS;

        // SAFETY: single-threaded use of the shim exactly per its header
        // contract — init once, one device handle, release before shutdown.
        unsafe {
            assert_eq!(ffi::obsbot_init(), ffi::OBSBOT_OK, "SDK init failed");
            // Enumeration + per-device init are asynchronous (the SDK sample
            // waits a flat 3 s; the Tiny 2 Lite has been observed to finish a
            // hair after that), so poll for up to 10 s.
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut dev = ffi::obsbot_first_device();
            while dev.is_null() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(500));
                dev = ffi::obsbot_first_device();
            }
            assert!(
                !dev.is_null(),
                "no OBSBOT device found — is one plugged in?"
            );

            let mut product_raw: i32 = -1;
            let mut sn = [0u8; 64];
            let mut fw = [0u8; 64];
            let info_ret = ffi::obsbot_device_info(
                dev,
                &raw mut product_raw,
                sn.as_mut_ptr().cast(),
                sn.len(),
                fw.as_mut_ptr().cast(),
                fw.len(),
            );
            println!(
                "device_info ret={info_ret} product={product_raw} ({}) sn={} fw={}",
                product_name(product_raw),
                buf_to_string(&sn),
                buf_to_string(&fw),
            );

            let taken = ControlSteps::from_bits_truncate(ffi::obsbot_take_control(dev));
            println!("take_control achieved: {taken:?}");
            for (step, label) in STEP_LABELS {
                println!(
                    "  {label}: {}",
                    if taken.contains(step) { "ok" } else { "FAILED" }
                );
            }

            std::thread::sleep(Duration::from_secs(2));

            let restored = ControlSteps::from_bits_truncate(ffi::obsbot_release_control(dev));
            println!("release_control restored: {restored:?}");

            ffi::obsbot_device_release(dev);
            ffi::obsbot_shutdown();

            assert!(
                taken.contains(REQUIRED_STEPS),
                "required steps (AI off + gestures off) did not all succeed: {taken:?}"
            );
        }
    }
}
