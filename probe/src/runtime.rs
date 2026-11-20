use std::{
    ffi::CStr,
    ptr,
    time::{Duration, Instant},
};

use bevy::prelude::*;
use bevy_mod_openxr::{
    environment_blend_mode::OxrEnvironmentBlendModes,
    exts::OxrEnabledExtensions,
    openxr_session_running,
    resources::{OxrInstance, OxrSystemId},
    session::OxrSession,
};
use bevy_mod_xr::{
    session::{XrPreDestroySession, XrSessionCreated},
    spaces::{XrPrimaryReferenceSpace, XrReferenceSpace},
};
use openxr::sys::{self, Handle as _};

use crate::report::{Report, Section, Status, xr_result};

/// 重点检查的扩展和它们的用途。
const KEY_EXTENSIONS: [(&str, &str); 20] = [
    ("XR_PICO_camera_image", "PICO 彩色相机"),
    ("XR_EXT_future", "异步调用，PICO 相机和空间数据需要"),
    ("XR_BD_spatial_sensing", "空间感知基础"),
    ("XR_BD_spatial_scene", "房间设定（墙、地面、家具）"),
    ("XR_BD_spatial_mesh", "空间网格"),
    ("XR_BD_spatial_plane", "平面检测"),
    ("XR_BD_spatial_anchor", "空间锚点"),
    ("XR_PICO_spatial_sensing", "PICO 命名的空间感知"),
    ("XR_PICO_scene_capture", "PICO 命名的房间设定"),
    ("XR_PICO_spatial_mesh", "PICO 命名的空间网格"),
    (
        "XR_PICO_secure_mixed_reality",
        "SecureMR（另一条读相机的路径）",
    ),
    ("XR_BD_controller_interaction", "PICO 手柄配置"),
    ("XR_BD_ultra_controller_interaction", "PICO Ultra 手柄配置"),
    ("XR_KHR_convert_timespec_time", "XrTime 与系统时钟换算"),
    ("XR_KHR_vulkan_enable2", "Vulkan 渲染"),
    ("XR_FB_passthrough", "Meta 透视"),
    ("XR_META_environment_depth", "Meta 深度"),
    ("XR_FB_scene", "Meta 房间设定"),
    ("XR_FB_spatial_entity", "Meta 空间实体"),
    ("XR_EXT_local_floor", "LOCAL_FLOOR 参考空间"),
];

pub struct RuntimePlugin;

impl Plugin for RuntimePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(XrSessionCreated, create_view_space)
            .add_systems(XrPreDestroySession, remove_view_space)
            .add_systems(
                Update,
                (probe_runtime, track_head, measure_frames).run_if(openxr_session_running),
            );
    }
}

/// 头显位姿所用的 VIEW 参考空间。
#[derive(Resource)]
pub struct ViewSpace(pub XrReferenceSpace);

fn create_view_space(session: Res<OxrSession>, mut commands: Commands) {
    match session.create_reference_space(openxr::ReferenceSpaceType::VIEW, Isometry3d::IDENTITY) {
        Ok(space) => commands.insert_resource(ViewSpace(space)),
        Err(err) => error!("probe: creating view space failed: {err}"),
    }
}

fn remove_view_space(mut commands: Commands) {
    commands.remove_resource::<ViewSpace>();
}

/// 会话开始后读取一次运行时、系统、视图、混合模式、参考空间和扩展信息。
fn probe_runtime(
    mut done: Local<bool>,
    instance: Res<OxrInstance>,
    system: Res<OxrSystemId>,
    session: Res<OxrSession>,
    enabled: Res<OxrEnabledExtensions>,
    blend: Option<Res<OxrEnvironmentBlendModes>>,
    mut report: ResMut<Report>,
) {
    if *done {
        return;
    }
    *done = true;
    let s = Section::Runtime;
    match instance.properties() {
        Ok(props) => report.set(
            s,
            "运行时",
            Status::Info,
            format!("{} {}", props.runtime_name, props.runtime_version),
        ),
        Err(err) => report.set(s, "运行时", Status::Fail, err.to_string()),
    }
    match instance.system_properties(system.0) {
        Ok(props) => {
            report.set(
                s,
                "系统",
                Status::Info,
                format!("{} · vendor 0x{:x}", props.system_name, props.vendor_id),
            );
            report.set(
                s,
                "最大图层 / 交换链",
                Status::Info,
                format!(
                    "{} 层 · {}x{}",
                    props.graphics_properties.max_layer_count,
                    props.graphics_properties.max_swapchain_image_width,
                    props.graphics_properties.max_swapchain_image_height
                ),
            );
            let tracking = props.tracking_properties;
            report.set(
                Section::Tracking,
                "系统跟踪能力",
                if tracking.orientation_tracking && tracking.position_tracking {
                    Status::Ok
                } else {
                    Status::Fail
                },
                format!(
                    "朝向 {} · 位置 {}",
                    yes(tracking.orientation_tracking),
                    yes(tracking.position_tracking)
                ),
            );
        }
        Err(err) => report.set(s, "系统", Status::Fail, err.to_string()),
    }
    match instance
        .enumerate_view_configuration_views(system.0, openxr::ViewConfigurationType::PRIMARY_STEREO)
    {
        Ok(views) => {
            let text = views
                .iter()
                .map(|v| {
                    format!(
                        "推荐 {}x{} · 最大 {}x{}",
                        v.recommended_image_rect_width,
                        v.recommended_image_rect_height,
                        v.max_image_rect_width,
                        v.max_image_rect_height
                    )
                })
                .next()
                .unwrap_or_default();
            report.set(s, "每眼分辨率", Status::Info, text);
        }
        Err(err) => report.set(s, "每眼分辨率", Status::Fail, err.to_string()),
    }
    match instance
        .enumerate_environment_blend_modes(system.0, openxr::ViewConfigurationType::PRIMARY_STEREO)
    {
        Ok(modes) => {
            let passthrough = modes.contains(&openxr::EnvironmentBlendMode::ALPHA_BLEND);
            let current = blend
                .map(|b| format!("{:?}", b.blend_mode()))
                .unwrap_or_else(|| "?".into());
            report.set(
                s,
                "混合模式",
                if passthrough {
                    Status::Ok
                } else {
                    Status::Warn
                },
                format!("可用 {modes:?} · 当前 {current}"),
            );
        }
        Err(err) => report.set(s, "混合模式", Status::Fail, err.to_string()),
    }
    match session.enumerate_reference_spaces() {
        Ok(spaces) => report.set(s, "参考空间", Status::Info, format!("{spaces:?}")),
        Err(err) => report.set(s, "参考空间", Status::Fail, err.to_string()),
    }

    let available = available_extensions(&instance);
    let enabled_names: Vec<String> = enabled
        .names()
        .into_iter()
        .filter_map(|name| CStr::from_bytes_with_nul(name).ok())
        .map(|name| name.to_string_lossy().into_owned())
        .collect();
    report.set(
        s,
        "扩展数量",
        Status::Info,
        format!("可用 {} · 已启用 {}", available.len(), enabled_names.len()),
    );
    for (name, purpose) in KEY_EXTENSIONS {
        let version = available.iter().find(|(n, _)| n == name).map(|(_, v)| *v);
        let on = enabled_names.iter().any(|n| n == name);
        let (status, text) = match version {
            Some(v) if on => (Status::Ok, format!("v{v} 已启用 · {purpose}")),
            Some(v) => (Status::Ok, format!("v{v} · {purpose}")),
            None => (Status::Warn, format!("不可用 · {purpose}")),
        };
        report.set(Section::Extensions, name, status, text);
    }
    let list = available
        .iter()
        .map(|(name, version)| format!("{name} v{version}"))
        .collect::<Vec<_>>()
        .join("\n");
    report.block("全部 OpenXR 扩展", list);
}

/// 运行时支持的全部扩展名和版本。
fn available_extensions(instance: &OxrInstance) -> Vec<(String, u32)> {
    let enumerate = instance
        .entry()
        .fp()
        .enumerate_instance_extension_properties;
    unsafe {
        let mut count = 0;
        if enumerate(ptr::null(), 0, &mut count, ptr::null_mut()).into_raw() < 0 {
            return Vec::new();
        }
        let mut props = vec![
            sys::ExtensionProperties {
                ty: sys::ExtensionProperties::TYPE,
                next: ptr::null_mut(),
                extension_name: [0; sys::MAX_EXTENSION_NAME_SIZE],
                extension_version: 0,
            };
            count as usize
        ];
        if enumerate(ptr::null(), count, &mut count, props.as_mut_ptr()).into_raw() < 0 {
            return Vec::new();
        }
        let mut out: Vec<(String, u32)> = props
            .iter()
            .take(count as usize)
            .map(|p| {
                let name = CStr::from_ptr(p.extension_name.as_ptr());
                (name.to_string_lossy().into_owned(), p.extension_version)
            })
            .collect();
        out.sort();
        out
    }
}

/// 每帧定位头显，显示跟踪状态和位置；每秒检查一次 XrTime 与单调时钟的换算。
fn track_head(
    instance: Res<OxrInstance>,
    base: Option<Res<XrPrimaryReferenceSpace>>,
    view: Option<Res<ViewSpace>>,
    mut report: ResMut<Report>,
    mut last_time_check: Local<Option<Instant>>,
) {
    let s = Section::Tracking;
    let (Some(base), Some(view)) = (base, view) else {
        report.set(s, "头显位姿", Status::Pending, "等待参考空间");
        return;
    };
    let Ok(now) = instance.now() else {
        return;
    };
    let mut location: sys::SpaceLocation = unsafe { std::mem::zeroed() };
    location.ty = sys::SpaceLocation::TYPE;
    let result = unsafe {
        (instance.fp().locate_space)(
            sys::Space::from_raw(view.0.as_raw()),
            sys::Space::from_raw(base.as_raw()),
            now,
            &mut location,
        )
    };
    if result.into_raw() < 0 {
        report.set(s, "头显位姿", Status::Fail, xr_result(result));
    } else {
        let flags = location.location_flags;
        let tracked = flags.contains(
            sys::SpaceLocationFlags::POSITION_TRACKED
                | sys::SpaceLocationFlags::ORIENTATION_TRACKED,
        );
        let p = location.pose.position;
        report.set(
            s,
            "头显位姿",
            if tracked { Status::Ok } else { Status::Warn },
            format!(
                "位置 {} 朝向 {} · ({:+.2}, {:+.2}, {:+.2}) m",
                flag_text(
                    flags.contains(sys::SpaceLocationFlags::POSITION_VALID),
                    flags.contains(sys::SpaceLocationFlags::POSITION_TRACKED)
                ),
                flag_text(
                    flags.contains(sys::SpaceLocationFlags::ORIENTATION_VALID),
                    flags.contains(sys::SpaceLocationFlags::ORIENTATION_TRACKED)
                ),
                p.x,
                p.y,
                p.z
            ),
        );
    }

    if last_time_check.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
        return;
    }
    *last_time_check = Some(Instant::now());
    let Some(fp) = instance.exts().khr_convert_timespec_time else {
        report.set(
            s,
            "时间换算",
            Status::Fail,
            "XR_KHR_convert_timespec_time 未启用",
        );
        return;
    };
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let result = unsafe { (fp.convert_time_to_timespec_time)(instance.as_raw(), now, &mut ts) };
    if result.into_raw() < 0 {
        report.set(s, "时间换算", Status::Fail, xr_result(result));
        return;
    }
    let converted = ts.tv_sec as i64 * 1_000_000_000 + ts.tv_nsec as i64;
    let offset_ms = (converted - mono_now_ns()) as f64 / 1e6;
    report.set(
        s,
        "时间换算",
        if offset_ms.abs() < 50.0 {
            Status::Ok
        } else {
            Status::Warn
        },
        format!("XrTime 换算后比 CLOCK_MONOTONIC 快 {offset_ms:+.1} ms"),
    );
}

/// 头显当前在参考空间里的位置和朝向，位姿无效时返回 None。
pub fn head_transform(
    instance: &OxrInstance,
    base: &XrPrimaryReferenceSpace,
    view: &ViewSpace,
) -> Option<Transform> {
    let mut location: sys::SpaceLocation = unsafe { std::mem::zeroed() };
    location.ty = sys::SpaceLocation::TYPE;
    let result = unsafe {
        (instance.fp().locate_space)(
            sys::Space::from_raw(view.0.as_raw()),
            sys::Space::from_raw(base.as_raw()),
            instance.now().ok()?,
            &mut location,
        )
    };
    let valid =
        sys::SpaceLocationFlags::POSITION_VALID | sys::SpaceLocationFlags::ORIENTATION_VALID;
    if result.into_raw() < 0 || !location.location_flags.contains(valid) {
        return None;
    }
    let p = location.pose.position;
    let q = location.pose.orientation;
    Some(Transform {
        translation: Vec3::new(p.x, p.y, p.z),
        rotation: Quat::from_xyzw(q.x, q.y, q.z, q.w),
        scale: Vec3::ONE,
    })
}

/// 单调时钟的当前读数（纳秒）。
pub fn mono_now_ns() -> i64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as i64 * 1_000_000_000 + ts.tv_nsec as i64
}

/// 每秒统计一次实际帧率。
fn measure_frames(mut window: Local<Option<(Instant, u32)>>, mut report: ResMut<Report>) {
    let (start, frames) = window.get_or_insert_with(|| (Instant::now(), 0));
    *frames += 1;
    let elapsed = start.elapsed();
    if elapsed >= Duration::from_secs(1) {
        let fps = *frames as f64 / elapsed.as_secs_f64();
        report.set(
            Section::Runtime,
            "帧率",
            Status::Info,
            format!("{fps:.1} fps"),
        );
        *window = None;
    }
}

fn yes(value: bool) -> &'static str {
    if value { "有" } else { "无" }
}

fn flag_text(valid: bool, tracked: bool) -> &'static str {
    match (valid, tracked) {
        (true, true) => "跟踪中",
        (true, false) => "有效未跟踪",
        _ => "无效",
    }
}
