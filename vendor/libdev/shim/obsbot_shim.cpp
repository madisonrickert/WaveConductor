/*
 * obsbot_shim.cpp — implementation of the extern "C" facade declared in
 * obsbot_shim.h. See that header for the API contracts (no exceptions across
 * the boundary, opaque handle ownership, threading model).
 *
 * Compiled by wc-core's build.rs via the `cc` crate (Windows +
 * `obsbot-camera-control` feature only) and linked against
 * vendor/libdev/windows/win64-release/libdev.lib. Built with the release
 * dynamic CRT (/MD) to match both libdev.dll and Rust's MSVC target — do not
 * introduce debug-CRT (/MDd) flags here: MSVC STL container layouts differ
 * under _ITERATOR_DEBUG_LEVEL != 0 and libdev's API passes std::string /
 * std::function / std::shared_ptr across the DLL boundary.
 */

#include "obsbot_shim.h"

#include <atomic>
#include <cstring>
#include <memory>
#include <string>

#include "dev/devs.hpp"

/* Opaque handle: owns a shared_ptr keeping the SDK Device alive. */
struct obsbot_device {
	std::shared_ptr<Device> dev;
};

namespace {

/* Set once by obsbot_init(); checked by entry points that need the SDK. */
std::atomic<bool> g_inited{false};

/* Bumped by the SDK's hotplug callback (SDK-owned thread). The Rust worker
 * polls this instead of receiving cross-language callbacks. */
std::atomic<uint32_t> g_epoch{0};

/* Devices::setDevChangedCallback handler. Runs on an SDK thread: touch
 * nothing but the atomic (no allocation, no locks, no SDK re-entry). */
void on_dev_changed(std::string /*sn*/, bool /*plugged_in*/, void * /*param*/)
{
	g_epoch.fetch_add(1, std::memory_order_relaxed);
}

/* True for the first-generation tiny products whose AI/gesture surface uses
 * aiSetTargetSelectR instead of cameraSetAiModeU (per the SDK sample). */
bool is_gen1_tiny(ObsbotProductType pt)
{
	return pt == ObsbotProdTiny || pt == ObsbotProdTiny4k;
}

} // namespace

extern "C" {

int32_t obsbot_init(void)
{
	try {
		if (g_inited.exchange(true)) {
			return OBSBOT_OK; /* idempotent */
		}
		Devices &devs = Devices::get();
		devs.setDevChangedCallback(&on_dev_changed, nullptr);
		/* USB cameras only — skip the network (mDNS) scan the SDK
		 * otherwise runs continuously. */
		devs.setEnableMdnsScan(false);
		return OBSBOT_OK;
	} catch (...) {
		g_inited.store(false);
		return OBSBOT_ERR_EXCEPTION;
	}
}

void obsbot_shutdown(void)
{
	try {
		if (g_inited.exchange(false)) {
			Devices::get().close();
		}
	} catch (...) {
		/* Swallow: shutdown must never throw across the boundary. */
	}
}

uint32_t obsbot_hotplug_epoch(void)
{
	return g_epoch.load(std::memory_order_relaxed);
}

obsbot_device *obsbot_first_device(void)
{
	try {
		if (!g_inited.load()) {
			return nullptr;
		}
		auto list = Devices::get().getDevList();
		for (const auto &dev : list) {
			if (dev) {
				return new obsbot_device{dev};
			}
		}
		return nullptr;
	} catch (...) {
		return nullptr;
	}
}

void obsbot_device_release(obsbot_device *dev)
{
	/* delete of nullptr is a no-op; ~shared_ptr does not call into the
	 * device, so this cannot throw. */
	delete dev;
}

int32_t obsbot_device_info(obsbot_device *dev, int32_t *product_type,
			   char *sn_buf, size_t sn_cap, char *fw_buf,
			   size_t fw_cap)
{
	if (dev == nullptr || !dev->dev) {
		return OBSBOT_ERR_INVALID;
	}
	try {
		if (product_type != nullptr) {
			*product_type =
				static_cast<int32_t>(dev->dev->productType());
		}
		if (sn_buf != nullptr && sn_cap > 0) {
			const std::string sn = dev->dev->devSn();
			const size_t n = sn.size() < sn_cap - 1 ? sn.size()
								: sn_cap - 1;
			std::memcpy(sn_buf, sn.data(), n);
			sn_buf[n] = '\0';
		}
		if (fw_buf != nullptr && fw_cap > 0) {
			const std::string fw = dev->dev->devVersion();
			const size_t n = fw.size() < fw_cap - 1 ? fw.size()
								: fw_cap - 1;
			std::memcpy(fw_buf, fw.data(), n);
			fw_buf[n] = '\0';
		}
		return OBSBOT_OK;
	} catch (...) {
		return OBSBOT_ERR_EXCEPTION;
	}
}

uint32_t obsbot_take_control(obsbot_device *dev)
{
	if (dev == nullptr || !dev->dev) {
		return 0;
	}
	uint32_t mask = 0;
	Device &d = *dev->dev;
	try {
		const ObsbotProductType pt = d.productType();

		/* 1. AI tracking OFF. Must precede manual gimbal control
		 * (dev.hpp 1916/1936/1943). */
		bool ai_off;
		if (is_gen1_tiny(pt)) {
			ai_off = d.aiSetTargetSelectR(false) == RM_RET_OK;
		} else {
			ai_off = d.cameraSetAiModeU(Device::AiWorkModeNone) ==
				 RM_RET_OK;
		}
		/* Belt-and-suspenders: also drop the AI master switch so
		 * manual gimbal speed control holds (dev.hpp 1916). */
		(void)d.aiSetEnabledR(false);
		if (ai_off) {
			mask |= OBSBOT_STEP_AI_OFF;
		}

		/* 2. Gesture control OFF: the master switch (tail2-and-later
		 * category) and each individual gesture (tiny series). Count
		 * the step done if either surface accepted. Gestures 0..3 =
		 * target / zoom / dynamic zoom / dynamic zoom direction. */
		const bool master =
			d.aiSetGestureParaR(Device::DevGestureParaTypeGesture,
					    false) == RM_RET_OK;
		bool individual = true;
		for (int32_t g = 0; g <= 3; ++g) {
			individual =
				(d.aiSetGestureCtrlIndividualR(g, false) ==
				 RM_RET_OK) &&
				individual;
		}
		if (master || individual) {
			mask |= OBSBOT_STEP_GESTURE_OFF;
		}

		/* 3. Recenter the gimbal (valid now that AI is off). */
		if (d.gimbalRstPosR() == RM_RET_OK) {
			mask |= OBSBOT_STEP_GIMBAL_CENTER;
		}

		/* 4. Widest FOV; also reset digital zoom to 1.0 best-effort
		 * (zoom is a crop on top of the FOV choice). */
		if (d.cameraSetFovU(Device::FovType86) == RM_RET_OK) {
			mask |= OBSBOT_STEP_FOV_WIDE;
		}
		(void)d.cameraSetZoomAbsoluteR(1.0f);

		/* 5. Auto exposure ON — explicitly re-asserted, never
		 * disabled. cameraSetExposureModeR/cameraSetAELockR are
		 * tail-air-category calls that tiny-series firmware may
		 * reject; cameraSetFaceAER is category "all" and serves as
		 * the tiny-series assertion. Any acceptance counts. */
		const bool mode_auto = d.cameraSetExposureModeR(
					       Device::DevExposureAllAuto) ==
				       RM_RET_OK;
		const bool ae_unlocked =
			d.cameraSetAELockR(false) == RM_RET_OK;
		const bool face_ae = d.cameraSetFaceAER(1) == RM_RET_OK;
		if (mode_auto || ae_unlocked || face_ae) {
			mask |= OBSBOT_STEP_AUTO_EXPOSURE;
		}
		return mask;
	} catch (...) {
		/* Return whatever was achieved before the exception. */
		return mask;
	}
}

uint32_t obsbot_release_control(obsbot_device *dev)
{
	if (dev == nullptr || !dev->dev) {
		return 0;
	}
	uint32_t mask = 0;
	Device &d = *dev->dev;
	try {
		const ObsbotProductType pt = d.productType();

		/* Re-enable AI: master switch back on, then the product's
		 * out-of-the-box tracking behavior (single-person upper-body
		 * for the tiny2 series, target auto-select for gen-1 tiny) —
		 * mirrors the SDK sample's "set ai mode" branch. */
		(void)d.aiSetEnabledR(true);
		bool ai_on;
		if (is_gen1_tiny(pt)) {
			ai_on = d.aiSetTargetSelectR(true) == RM_RET_OK;
		} else {
			ai_on = d.cameraSetAiModeU(
					Device::AiWorkModeHuman,
					Device::AiSubModeUpperBody) ==
				RM_RET_OK;
		}
		if (ai_on) {
			mask |= OBSBOT_STEP_AI_OFF;
		}

		/* Re-enable gesture control on both surfaces. */
		const bool master =
			d.aiSetGestureParaR(Device::DevGestureParaTypeGesture,
					    true) == RM_RET_OK;
		bool individual = true;
		for (int32_t g = 0; g <= 3; ++g) {
			individual = (d.aiSetGestureCtrlIndividualR(g, true) ==
				      RM_RET_OK) &&
				     individual;
		}
		if (master || individual) {
			mask |= OBSBOT_STEP_GESTURE_OFF;
		}
		return mask;
	} catch (...) {
		return mask;
	}
}

int32_t obsbot_set_gimbal_angle(obsbot_device *dev, float pitch, float yaw)
{
	if (dev == nullptr || !dev->dev) {
		return OBSBOT_ERR_INVALID;
	}
	try {
		return dev->dev->aiSetGimbalMotorAngleR(pitch, yaw) ==
				       RM_RET_OK
			       ? OBSBOT_OK
			       : OBSBOT_ERR;
	} catch (...) {
		return OBSBOT_ERR_EXCEPTION;
	}
}

int32_t obsbot_set_gimbal_speed(obsbot_device *dev, double pitch, double pan)
{
	if (dev == nullptr || !dev->dev) {
		return OBSBOT_ERR_INVALID;
	}
	try {
		return dev->dev->aiSetGimbalSpeedCtrlR(pitch, pan) == RM_RET_OK
			       ? OBSBOT_OK
			       : OBSBOT_ERR;
	} catch (...) {
		return OBSBOT_ERR_EXCEPTION;
	}
}

int32_t obsbot_gimbal_stop(obsbot_device *dev)
{
	if (dev == nullptr || !dev->dev) {
		return OBSBOT_ERR_INVALID;
	}
	try {
		return dev->dev->aiSetGimbalStop() == RM_RET_OK
			       ? OBSBOT_OK
			       : OBSBOT_ERR;
	} catch (...) {
		return OBSBOT_ERR_EXCEPTION;
	}
}

int32_t obsbot_set_zoom(obsbot_device *dev, float ratio)
{
	if (dev == nullptr || !dev->dev) {
		return OBSBOT_ERR_INVALID;
	}
	try {
		return dev->dev->cameraSetZoomAbsoluteR(ratio) == RM_RET_OK
			       ? OBSBOT_OK
			       : OBSBOT_ERR;
	} catch (...) {
		return OBSBOT_ERR_EXCEPTION;
	}
}

int32_t obsbot_set_fov(obsbot_device *dev, int32_t fov_type)
{
	if (dev == nullptr || !dev->dev) {
		return OBSBOT_ERR_INVALID;
	}
	if (fov_type < OBSBOT_FOV_86 || fov_type > OBSBOT_FOV_65) {
		return OBSBOT_ERR_INVALID;
	}
	try {
		return dev->dev->cameraSetFovU(static_cast<Device::FovType>(
				       fov_type)) == RM_RET_OK
			       ? OBSBOT_OK
			       : OBSBOT_ERR;
	} catch (...) {
		return OBSBOT_ERR_EXCEPTION;
	}
}

} /* extern "C" */
