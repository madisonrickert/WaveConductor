/*
 * obsbot_shim.h — flat extern "C" facade over the vendored OBSBOT libdev C++
 * SDK (vendor/libdev/include/dev/{dev,devs}.hpp, SDK v1.3.0).
 *
 * WHY THIS EXISTS
 *   libdev's API is C++11 (classes, std::string, std::function,
 *   std::shared_ptr) and cannot be consumed by bindgen or a plain Rust
 *   `extern "C"` block. This shim exposes the minimal surface WaveConductor
 *   needs — device discovery plus "take control" (disable on-device AI
 *   tracking + gesture control, recenter the gimbal, widest FOV, re-assert
 *   auto exposure) and manual gimbal/zoom/FOV control — as a C ABI.
 *
 * CONTRACTS
 *   - No C++ exception ever crosses this boundary: every function body is
 *     wrapped in try/catch and failures surface as error codes.
 *   - Devices are handed out as opaque `obsbot_device` pointers owning a
 *     `std::shared_ptr<Device>`; release with obsbot_device_release().
 *   - The SDK invokes its callbacks on SDK-owned threads. The shim confines
 *     that to a single atomic hotplug epoch counter; callers poll
 *     obsbot_hotplug_epoch() from their own thread instead of receiving
 *     cross-language callbacks.
 *   - Thread-safety: call init/shutdown once per process. The per-device
 *     functions are safe to call from any single worker thread (the Rust
 *     side funnels all device IO through one dedicated thread).
 *
 * The step bit constants below are mirrored in Rust by
 * `wc_core::input::obsbot::ControlSteps` — keep the two in sync.
 */
#ifndef OBSBOT_SHIM_H
#define OBSBOT_SHIM_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ---- return codes ------------------------------------------------------ */

/** Success. */
#define OBSBOT_OK 0
/** The SDK call returned an error (RM_RET_ERR). */
#define OBSBOT_ERR (-1)
/** A C++ exception was caught at the shim boundary. */
#define OBSBOT_ERR_EXCEPTION (-2)
/** A null handle or otherwise invalid argument was passed. */
#define OBSBOT_ERR_INVALID (-3)
/** obsbot_init() has not been called (or failed). */
#define OBSBOT_ERR_NOT_INITIALIZED (-4)

/* ---- take/release-control step bits ------------------------------------ */
/*
 * obsbot_take_control() returns a bitmask of the steps that SUCCEEDED.
 * obsbot_release_control() returns the same bit positions for the restore
 * direction; only AI_OFF (= "AI restored on") and GESTURE_OFF (= "gestures
 * restored on") are meaningful there — the other steps have nothing to
 * restore (FOV 86° and auto exposure ARE the factory defaults, and the
 * centered gimbal is left where the on-device AI can take over again).
 */

/** On-device AI tracking disabled (cameraSetAiModeU(AiWorkModeNone) /
 *  aiSetTargetSelectR(false) per product, plus aiSetEnabledR(false)). */
#define OBSBOT_STEP_AI_OFF (1u << 0)
/** On-device gesture control disabled (master aiSetGestureParaR and/or all
 *  individual aiSetGestureCtrlIndividualR gestures 0..3). */
#define OBSBOT_STEP_GESTURE_OFF (1u << 1)
/** Gimbal recentered to the zero position (gimbalRstPosR). */
#define OBSBOT_STEP_GIMBAL_CENTER (1u << 2)
/** Widest field of view selected (cameraSetFovU(FovType86)); digital zoom is
 *  also reset to 1.0 best-effort as part of this step. */
#define OBSBOT_STEP_FOV_WIDE (1u << 3)
/** Auto exposure re-asserted (cameraSetExposureModeR(DevExposureAllAuto)
 *  where supported, else AE-unlock + face-AE — see obsbot_take_control). */
#define OBSBOT_STEP_AUTO_EXPOSURE (1u << 4)

/* ---- FOV presets (mirror Device::FovType) ------------------------------ */

#define OBSBOT_FOV_86 0 /* wide (factory default) */
#define OBSBOT_FOV_78 1 /* medium */
#define OBSBOT_FOV_65 2 /* narrow */

/* ---- lifecycle --------------------------------------------------------- */

/** Opaque device handle owning a std::shared_ptr<Device>. */
typedef struct obsbot_device obsbot_device;

/**
 * Initialize the SDK singleton (Devices::get()), register the internal
 * hotplug callback that feeds obsbot_hotplug_epoch(), and disable mDNS
 * network scanning (WaveConductor only talks to USB cameras).
 *
 * Idempotent; call once per process before any other shim function.
 * Device enumeration is asynchronous — allow ~3 s (or poll
 * obsbot_hotplug_epoch()) before expecting obsbot_first_device() to
 * return a device.
 *
 * @return OBSBOT_OK or OBSBOT_ERR_EXCEPTION.
 */
int32_t obsbot_init(void);

/**
 * Stop the SDK's detection task and release the Devices singleton
 * (Devices::get().close()). After this, the SDK cannot be re-initialized
 * in the same process — call only at final shutdown.
 */
void obsbot_shutdown(void);

/**
 * Monotonic counter incremented on every device plug-in or unplug event the
 * SDK reports. Poll it: a changed value means the device list changed and
 * any held obsbot_device should be re-resolved. Callable from any thread.
 */
uint32_t obsbot_hotplug_epoch(void);

/**
 * Get the first device in the SDK's current device list.
 *
 * @return A newly allocated opaque handle (release with
 *         obsbot_device_release), or NULL if no device is present, init was
 *         not called, or an exception occurred.
 */
obsbot_device *obsbot_first_device(void);

/**
 * Release a handle returned by obsbot_first_device(). Safe on NULL.
 * Does not touch the physical device.
 */
void obsbot_device_release(obsbot_device *dev);

/* ---- identity ---------------------------------------------------------- */

/**
 * Read the device's product type, serial number and firmware version.
 *
 * @param dev            Device handle.
 * @param product_type   Out: raw ObsbotProductType value (e.g. 3 =
 *                       ObsbotProdTiny2Lite). May be NULL.
 * @param sn_buf/sn_cap  Out: NUL-terminated 14-char serial (truncated to
 *                       sn_cap - 1 bytes). May be NULL/0 to skip.
 * @param fw_buf/fw_cap  Out: NUL-terminated firmware version string
 *                       (e.g. "1.2.3.4"), truncated likewise.
 * @return OBSBOT_OK, OBSBOT_ERR_INVALID or OBSBOT_ERR_EXCEPTION.
 */
int32_t obsbot_device_info(obsbot_device *dev, int32_t *product_type,
			   char *sn_buf, size_t sn_cap, char *fw_buf,
			   size_t fw_cap);

/* ---- take / release control -------------------------------------------- */

/**
 * Take control of the camera for app-driven operation, in this order (the
 * SDK docs require AI off BEFORE manual gimbal control holds — dev.hpp
 * 1916/1936/1943):
 *
 *   1. AI tracking OFF     — cameraSetAiModeU(AiWorkModeNone) for tiny2
 *                            series / tail air (aiSetTargetSelectR(false)
 *                            for tiny/tiny4k), plus aiSetEnabledR(false).
 *   2. Gesture control OFF — master aiSetGestureParaR(Gesture, false)
 *                            (tail2+) and individual gestures 0..3 via
 *                            aiSetGestureCtrlIndividualR (tiny series).
 *   3. Gimbal recenter     — gimbalRstPosR().
 *   4. Widest FOV          — cameraSetFovU(FovType86) + zoom 1.0.
 *   5. Auto exposure ON    — cameraSetExposureModeR(DevExposureAllAuto)
 *                            (tail-air category), falling back to
 *                            cameraSetAELockR(false) + cameraSetFaceAER(1)
 *                            for the tiny series.
 *
 * Each step is attempted even if an earlier one failed.
 *
 * @return Bitmask of OBSBOT_STEP_* bits that SUCCEEDED (0 on invalid
 *         handle). An exception aborts remaining steps; bits already earned
 *         are returned.
 */
uint32_t obsbot_take_control(obsbot_device *dev);

/**
 * Restore the camera to its out-of-the-box behavior so other software (OBSBOT
 * Center, the device's own gesture UX) is not surprised after WaveConductor
 * exits: re-enable AI (aiSetEnabledR(true) + human tracking mode per
 * product) and re-enable gesture control (master + individual gestures).
 * FOV/exposure are left at the factory defaults take-control already set.
 *
 * @return Bitmask: OBSBOT_STEP_AI_OFF = AI restored, OBSBOT_STEP_GESTURE_OFF
 *         = gestures restored. Other bits are never set.
 */
uint32_t obsbot_release_control(obsbot_device *dev);

/* ---- manual control (valid after obsbot_take_control) ------------------ */

/**
 * Move the gimbal to an absolute motor angle (aiSetGimbalMotorAngleR).
 * @param pitch  Degrees, valid range -90..90.
 * @param yaw    Degrees, valid range -180..180 (Tiny-series mechanical range
 *               is narrower; invalid values are ignored by the device).
 */
int32_t obsbot_set_gimbal_angle(obsbot_device *dev, float pitch, float yaw);

/**
 * Rotate the gimbal at a constant speed (aiSetGimbalSpeedCtrlR); 0/0 stops.
 * @param pitch  Pitch speed, valid range -90..90.
 * @param pan    Pan speed, valid range -180..180.
 */
int32_t obsbot_set_gimbal_speed(obsbot_device *dev, double pitch, double pan);

/** Stop any gimbal motion (aiSetGimbalStop). */
int32_t obsbot_gimbal_stop(obsbot_device *dev);

/**
 * Set absolute digital zoom (cameraSetZoomAbsoluteR).
 * @param ratio  Normalized zoom, valid range 1.0..2.0.
 */
int32_t obsbot_set_zoom(obsbot_device *dev, float ratio);

/**
 * Set the camera field of view (cameraSetFovU).
 * @param fov_type  One of OBSBOT_FOV_86 / OBSBOT_FOV_78 / OBSBOT_FOV_65.
 */
int32_t obsbot_set_fov(obsbot_device *dev, int32_t fov_type);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* OBSBOT_SHIM_H */
