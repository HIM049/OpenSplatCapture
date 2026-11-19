use std::time::{SystemTime, UNIX_EPOCH};

use bevy::prelude::*;
use bevy_mod_openxr::{openxr_session_running, resources::OxrInstance};
use capture_format::TimeBase;

pub struct TimePlugin;

impl Plugin for TimePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, log_clock_offset.run_if(openxr_session_running));
    }
}

/// 单调时钟的当前读数（纳秒），与 Camera2 传感器时间戳同源。
pub fn mono_now_ns() -> i64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as i64 * 1_000_000_000 + ts.tv_nsec as i64
}

/// 把 OpenXR 时间换算成单调时钟纳秒。
pub fn xr_to_mono_ns(instance: &OxrInstance, time: openxr::Time) -> Option<i64> {
    let fp = instance.exts().khr_convert_timespec_time?;
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let result = unsafe { (fp.convert_time_to_timespec_time)(instance.as_raw(), time, &mut ts) };
    (result.into_raw() >= 0).then(|| ts.tv_sec as i64 * 1_000_000_000 + ts.tv_nsec as i64)
}

/// 同时采样单调时钟和 Unix 时间，作为一次录制的换算基准。
pub fn new_time_base() -> TimeBase {
    let unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default();
    TimeBase {
        mono_ns: mono_now_ns(),
        unix_ms,
    }
}

/// 本地时间的 `yyyyMMdd_HHmmss`，用作录制目录名。
pub fn session_name() -> String {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        libc::localtime_r(&now, &mut tm);
    }
    format!(
        "{:04}{:02}{:02}_{:02}{:02}{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// 会话开始后打印一次 OpenXR 时间与单调时钟的差值。
fn log_clock_offset(instance: Res<OxrInstance>, mut done: Local<bool>) {
    if *done {
        return;
    }
    *done = true;
    match instance.now() {
        Ok(now) => match xr_to_mono_ns(&instance, now) {
            Some(mono) => info!(
                "clock: OpenXR time {} ns, monotonic {} ns, difference {} ns",
                now.as_nanos(),
                mono,
                now.as_nanos() - mono
            ),
            None => error!("clock: XR_KHR_convert_timespec_time is unavailable"),
        },
        Err(err) => error!("clock: xrConvertTimespecTimeToTimeKHR failed: {err}"),
    }
}
