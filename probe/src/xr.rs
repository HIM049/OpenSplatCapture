use std::{
    ptr,
    time::{Duration, Instant},
};

use bevy_mod_openxr::resources::OxrInstance;
use openxr::sys;

use crate::report::xr_result;

/// 结果码为错误时返回带调用名的说明。
pub fn check(result: sys::Result, what: &str) -> Result<(), String> {
    if result.into_raw() < 0 {
        Err(format!("{what} → {}", xr_result(result)))
    } else {
        Ok(())
    }
}

/// 查询异步调用是否完成；超过 `timeout` 或出错时返回错误。
pub fn poll(
    instance: &OxrInstance,
    future: sys::FutureEXT,
    since: Instant,
    timeout: Duration,
) -> Result<bool, String> {
    let fp = instance.exts().ext_future.ok_or("XR_EXT_future 未启用")?;
    let info = sys::FuturePollInfoEXT {
        ty: sys::FuturePollInfoEXT::TYPE,
        next: ptr::null(),
        future,
    };
    let mut result = sys::FuturePollResultEXT {
        ty: sys::FuturePollResultEXT::TYPE,
        next: ptr::null_mut(),
        state: sys::FutureStateEXT::PENDING,
    };
    check(
        unsafe { (fp.poll_future)(instance.as_raw(), &info, &mut result) },
        "xrPollFutureEXT",
    )?;
    if result.state == sys::FutureStateEXT::READY {
        return Ok(true);
    }
    if since.elapsed() > timeout {
        return Err(format!("{} 秒内没有完成", timeout.as_secs()));
    }
    Ok(false)
}
