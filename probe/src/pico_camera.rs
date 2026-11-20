//! 按 PICO Native SDK 3.0.0 文档里的 XR_PICO_camera_image 定义手写的 FFI 和检测流程。

use std::{
    ffi::{CStr, c_char, c_void},
    fmt::Write as _,
    ptr,
    time::{Duration, Instant},
};

use bevy::{
    asset::RenderAssetUsages,
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
};
use bevy_mod_openxr::{
    exts::OxrEnabledExtensions, openxr_session_running, resources::OxrInstance, session::OxrSession,
};
use openxr::sys;

use crate::{
    android_camera::AndroidCameraState,
    controls::ProbeCommand,
    device::StartedAt,
    report::{Report, Section, Status, xr_result},
    xr::{check, poll},
};

const EXTENSION: &[u8] = b"XR_PICO_camera_image\0";
/// Android 相机检测一直没结束时，最多等这么久再开始。
const START_DEADLINE: Duration = Duration::from_secs(30);
/// 异步调用的超时。
const FUTURE_TIMEOUT: Duration = Duration::from_secs(10);
/// 连续取帧的时长，用来统计帧率。
const CAPTURE_DURATION: Duration = Duration::from_secs(3);
const THUMB_WIDTH: u32 = 320;

const TYPE_AVAILABLE_CAMERAS_ENUMERATE_INFO: i32 = 1010033000;
const TYPE_CAMERA_PROPERTIES_GET_INFO: i32 = 1010033001;
const TYPE_CAMERA_PROPERTIES: i32 = 1010033002;
const TYPE_CAMERA_PROPERTY_FACING: i32 = 1010033003;
const TYPE_CAMERA_PROPERTY_POSITION: i32 = 1010033004;
const TYPE_CAMERA_PROPERTY_CAMERA_TYPE: i32 = 1010033005;
const TYPE_SUPPORTED_CAPABILITIES_GET_INFO: i32 = 1010033006;
const TYPE_SUPPORTED_CAPABILITIES: i32 = 1010033007;
const TYPE_SUPPORTED_RESOLUTION: i32 = 1010033008;
const TYPE_CAPABILITY_RESOLUTION: i32 = 1010033009;
const TYPE_SUPPORTED_TRANSFER: i32 = 1010033010;
const TYPE_CAPABILITY_TRANSFER: i32 = 1010033011;
const TYPE_SUPPORTED_FORMAT: i32 = 1010033012;
const TYPE_CAPABILITY_FORMAT: i32 = 1010033013;
const TYPE_SUPPORTED_MODEL: i32 = 1010033014;
const TYPE_CAPABILITY_MODEL: i32 = 1010033015;
const TYPE_SUPPORTED_FPS: i32 = 1010033016;
const TYPE_CAPABILITY_FPS: i32 = 1010033017;
const TYPE_DEVICE_CREATE_INFO: i32 = 1010033018;
const TYPE_DEVICE_COMPLETION: i32 = 1010033019;
const TYPE_SESSION_CREATE_INFO: i32 = 1010033020;
const TYPE_SESSION_COMPLETION: i32 = 1010033021;
const TYPE_INTRINSICS: i32 = 1010033022;
const TYPE_EXTRINSICS: i32 = 1010033023;
const TYPE_BEGIN_INFO: i32 = 1010033024;
const TYPE_ACQUIRE_INFO: i32 = 1010033025;
const TYPE_IMAGE: i32 = 1010033026;
const TYPE_RAW_BUFFER: i32 = 1010033027;

const PROPERTY_FACING: i32 = 1;
const PROPERTY_POSITION: i32 = 2;
const PROPERTY_CAMERA_TYPE: i32 = 3;
const CAPABILITY_RESOLUTION: i32 = 1;
const CAPABILITY_FORMAT: i32 = 2;
const CAPABILITY_TRANSFER: i32 = 3;
const CAPABILITY_MODEL: i32 = 4;
const CAPABILITY_FPS: i32 = 5;
const POSITION_LEFT: i32 = 2;
const CAMERA_TYPE_PASSTHROUGH_COLOR: i32 = 1;
const FPS_30: i32 = 1;
const IMAGE_NO_UPDATE: i32 = 1010033000;

fn ty(raw: i32) -> sys::StructureType {
    sys::StructureType::from_raw(raw)
}

#[repr(C)]
struct Header {
    ty: sys::StructureType,
    next: *const c_void,
}

#[repr(C)]
struct EnumerateInfo {
    ty: sys::StructureType,
    next: *const c_void,
    properties: *const c_void,
    capabilities: *const c_void,
}

#[repr(C)]
struct IdGetInfo {
    ty: sys::StructureType,
    next: *const c_void,
    camera_id: u64,
}

/// XrCameraPropertiesPICO 和 XrCameraSupportedCapabilitiesPICO 共用的布局。
#[repr(C)]
struct PointerList {
    ty: sys::StructureType,
    next: *const c_void,
    count: u32,
    items: *mut *mut Header,
}

/// 只有一个值的属性或能力结构。
#[repr(C)]
struct Value<T> {
    ty: sys::StructureType,
    next: *const c_void,
    value: T,
}

/// 支持的能力列表：容量、实际数量和数组。
#[repr(C)]
struct SupportedList<T> {
    ty: sys::StructureType,
    next: *const c_void,
    capacity: u32,
    count: u32,
    items: *mut T,
}

#[repr(C)]
struct HandleCompletion {
    ty: sys::StructureType,
    next: *mut c_void,
    future_result: sys::Result,
    handle: u64,
}

#[repr(C)]
struct SessionCreateInfo {
    ty: sys::StructureType,
    next: *const c_void,
    camera: u64,
    config_count: u32,
    configs: *const *const Header,
}

#[repr(C)]
struct Intrinsics {
    ty: sys::StructureType,
    next: *const c_void,
    focal_length: sys::Vector2f,
    principal_point: sys::Vector2f,
    fov: sys::Vector2f,
}

#[repr(C)]
struct Extrinsics {
    ty: sys::StructureType,
    next: *const c_void,
    pose: sys::Posef,
}

#[repr(C)]
struct AcquireInfo {
    ty: sys::StructureType,
    next: *const c_void,
    last_capture_time: sys::Time,
}

#[repr(C)]
struct CameraImage {
    ty: sys::StructureType,
    next: *const c_void,
    capture_time: sys::Time,
    image_id: u64,
}

#[repr(C)]
struct RawBuffer {
    ty: sys::StructureType,
    next: *const c_void,
    width: u32,
    height: u32,
    stride: u32,
    bytes_per_pixel: u32,
    pixel_stride: u32,
    buffer_size: u32,
    buffer: *mut u8,
}

type Instance = sys::Instance;
type R = sys::Result;

struct Fns {
    enumerate_cameras:
        unsafe extern "system" fn(Instance, *const EnumerateInfo, u32, *mut u32, *mut u64) -> R,
    enumerate_property_types:
        unsafe extern "system" fn(Instance, u64, u32, *mut u32, *mut i32) -> R,
    get_properties: unsafe extern "system" fn(Instance, *const IdGetInfo, *mut PointerList) -> R,
    enumerate_capability_types:
        unsafe extern "system" fn(Instance, u64, u32, *mut u32, *mut i32) -> R,
    get_supported_capabilities:
        unsafe extern "system" fn(Instance, *const IdGetInfo, *mut PointerList) -> R,
    create_device_async:
        unsafe extern "system" fn(Instance, *const IdGetInfo, *mut sys::FutureEXT) -> R,
    create_device_complete:
        unsafe extern "system" fn(Instance, sys::FutureEXT, *mut HandleCompletion) -> R,
    create_session_async:
        unsafe extern "system" fn(sys::Session, *const SessionCreateInfo, *mut sys::FutureEXT) -> R,
    create_session_complete:
        unsafe extern "system" fn(sys::Session, sys::FutureEXT, *mut HandleCompletion) -> R,
    get_intrinsics: unsafe extern "system" fn(u64, *mut Intrinsics) -> R,
    get_extrinsics: unsafe extern "system" fn(u64, *mut Extrinsics) -> R,
    begin_capture: unsafe extern "system" fn(u64, *mut Header) -> R,
    end_capture: unsafe extern "system" fn(u64) -> R,
    acquire_image: unsafe extern "system" fn(u64, *const AcquireInfo, *mut CameraImage) -> R,
    get_image_data: unsafe extern "system" fn(u64, u64, *mut RawBuffer) -> R,
    release_image: unsafe extern "system" fn(u64, u64) -> R,
}

/// 从运行时取出所有函数指针，缺任何一个都返回它的名字。
unsafe fn load(instance: &OxrInstance) -> Result<Fns, String> {
    let get = instance.entry().fp().get_instance_proc_addr;
    let raw = instance.as_raw();
    let find = |name: &CStr| -> Result<unsafe extern "system" fn(), String> {
        let mut f = None;
        let result = unsafe { get(raw, name.as_ptr() as *const c_char, &mut f) };
        match f {
            Some(f) if result.into_raw() >= 0 => Ok(f),
            _ => Err(format!(
                "{} → {}",
                name.to_string_lossy(),
                xr_result(result)
            )),
        }
    };
    macro_rules! f {
        ($name:literal) => {
            unsafe { std::mem::transmute::<unsafe extern "system" fn(), _>(find($name)?) }
        };
    }
    Ok(Fns {
        enumerate_cameras: f!(c"xrEnumerateAvailableCamerasPICO"),
        enumerate_property_types: f!(c"xrEnumerateCameraPropertyTypesPICO"),
        get_properties: f!(c"xrGetCameraPropertiesPICO"),
        enumerate_capability_types: f!(c"xrEnumerateCameraCapabilityTypesPICO"),
        get_supported_capabilities: f!(c"xrGetCameraSupportedCapabilitiesPICO"),
        create_device_async: f!(c"xrCreateCameraDeviceAsyncPICO"),
        create_device_complete: f!(c"xrCreateCameraDeviceCompletePICO"),
        create_session_async: f!(c"xrCreateCameraCaptureSessionAsyncPICO"),
        create_session_complete: f!(c"xrCreateCameraCaptureSessionCompletePICO"),
        get_intrinsics: f!(c"xrGetCameraIntrinsicsPICO"),
        get_extrinsics: f!(c"xrGetCameraExtrinsicsPICO"),
        begin_capture: f!(c"xrBeginCameraCapturePICO"),
        end_capture: f!(c"xrEndCameraCapturePICO"),
        acquire_image: f!(c"xrAcquireCameraImagePICO"),
        get_image_data: f!(c"xrGetCameraImageDataPICO"),
        release_image: f!(c"xrReleaseCameraImagePICO"),
    })
}

/// 一个相机支持的能力，以及为采集会话选定的配置。
#[derive(Default)]
struct Capabilities {
    types: Vec<i32>,
    resolutions: Vec<sys::Extent2Di>,
    formats: Vec<i32>,
    transfers: Vec<i32>,
    models: Vec<i32>,
    fps: Vec<i32>,
}

enum Step {
    Waiting,
    DeviceFuture(sys::FutureEXT, Instant),
    SessionFuture(sys::FutureEXT, Instant),
    Capturing {
        since: Instant,
        last_time: sys::Time,
        frames: u32,
        thumbnail_done: bool,
    },
    Idle,
    Stopped,
}

#[derive(Resource)]
struct PicoCamera {
    fns: Option<Fns>,
    step: Step,
    camera: u64,
    capabilities: Capabilities,
    device: u64,
    session: u64,
}

impl Default for PicoCamera {
    fn default() -> Self {
        Self {
            fns: None,
            step: Step::Waiting,
            camera: 0,
            capabilities: Capabilities::default(),
            device: 0,
            session: 0,
        }
    }
}

pub struct PicoCameraPlugin;

impl Plugin for PicoCameraPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PicoCamera>()
            .add_systems(Update, run.run_if(openxr_session_running));
    }
}

/// 每帧推进一步检测流程。
fn run(
    mut camera: ResMut<PicoCamera>,
    instance: Res<OxrInstance>,
    session: Res<OxrSession>,
    enabled: Res<OxrEnabledExtensions>,
    android: Res<AndroidCameraState>,
    started: Res<StartedAt>,
    mut commands: MessageReader<ProbeCommand>,
    mut report: ResMut<Report>,
    mut images: ResMut<Assets<Image>>,
) {
    let s = Section::PicoCamera;
    let retry = commands.read().any(|c| *c == ProbeCommand::RetryCameras);
    let camera = &mut *camera;
    let result = match &mut camera.step {
        Step::Waiting => {
            if !android.finished && started.0.elapsed() < START_DEADLINE {
                report.set(s, "状态", Status::Pending, "等 Android 相机检测结束");
                return;
            }
            if !enabled.other.iter().any(|e| e.as_slice() == EXTENSION) {
                report.set(s, "状态", Status::Fail, "扩展不可用，无法检测");
                camera.step = Step::Stopped;
                return;
            }
            unsafe { start(camera, &instance, &mut report) }
        }
        Step::DeviceFuture(future, since) => {
            let (future, since) = (*future, *since);
            match poll(&instance, future, since, FUTURE_TIMEOUT) {
                Ok(false) => return,
                Ok(true) => unsafe {
                    finish_device(camera, &instance, &session, future, &mut report)
                },
                Err(err) => Err(format!("创建相机设备：{err}")),
            }
        }
        Step::SessionFuture(future, since) => {
            let (future, since) = (*future, *since);
            match poll(&instance, future, since, FUTURE_TIMEOUT) {
                Ok(false) => return,
                Ok(true) => unsafe { finish_session(camera, &session, future, &mut report) },
                Err(err) => Err(format!("创建采集会话：{err}")),
            }
        }
        Step::Capturing { .. } => unsafe { capture(camera, &instance, &mut report, &mut images) },
        Step::Idle => {
            if retry {
                unsafe { begin(camera, &mut report) }
            } else {
                Ok(())
            }
        }
        Step::Stopped => {
            if retry {
                camera.step = Step::Waiting;
            }
            Ok(())
        }
    };
    if let Err(err) = result {
        report.set(s, "状态", Status::Fail, err);
        camera.step = Step::Stopped;
    }
}

/// 加载函数、列出相机并读取属性和能力，然后开始创建相机设备。
unsafe fn start(
    camera: &mut PicoCamera,
    instance: &OxrInstance,
    report: &mut Report,
) -> Result<(), String> {
    let s = Section::PicoCamera;
    let fns = unsafe { load(instance) }.map_err(|e| format!("取函数指针失败：{e}"))?;
    report.set(s, "函数指针", Status::Ok, "16 个函数都已取到");
    let raw = instance.as_raw();
    let info = EnumerateInfo {
        ty: ty(TYPE_AVAILABLE_CAMERAS_ENUMERATE_INFO),
        next: ptr::null(),
        properties: ptr::null(),
        capabilities: ptr::null(),
    };
    let mut count = 0;
    check(
        unsafe { (fns.enumerate_cameras)(raw, &info, 0, &mut count, ptr::null_mut()) },
        "xrEnumerateAvailableCamerasPICO",
    )?;
    let mut ids = vec![0u64; count as usize];
    check(
        unsafe { (fns.enumerate_cameras)(raw, &info, count, &mut count, ids.as_mut_ptr()) },
        "xrEnumerateAvailableCamerasPICO",
    )?;
    ids.truncate(count as usize);
    report.set(
        s,
        "相机列表",
        if ids.is_empty() {
            Status::Fail
        } else {
            Status::Ok
        },
        format!("{} 个：{ids:?}", ids.len()),
    );
    if ids.is_empty() {
        return Err("运行时没有列出任何相机".into());
    }

    let mut chosen = None;
    let mut details = String::new();
    for &id in &ids {
        let props = unsafe { properties(&fns, raw, id) };
        let caps = unsafe { capabilities(&fns, raw, id) };
        let position = props
            .iter()
            .find(|(t, _)| *t == PROPERTY_POSITION)
            .map(|(_, v)| *v);
        let kind = props
            .iter()
            .find(|(t, _)| *t == PROPERTY_CAMERA_TYPE)
            .map(|(_, v)| *v);
        let summary = format!(
            "{} · {} · {}",
            match position {
                Some(1) => "位置未指定",
                Some(2) => "左",
                Some(3) => "右",
                _ => "位置?",
            },
            match kind {
                Some(1) => "彩色透视",
                _ => "类型?",
            },
            caps.resolutions
                .iter()
                .map(|r| format!("{}x{}", r.width, r.height))
                .collect::<Vec<_>>()
                .join("/")
        );
        report.set(s, format!("相机 {id}"), Status::Info, summary);
        let _ = writeln!(
            details,
            "camera {id}: properties {props:?}\n  capability types {:?}\n  resolutions {:?}\n  formats {:?} transfers {:?} models {:?} fps {:?}",
            caps.types,
            caps.resolutions
                .iter()
                .map(|r| (r.width, r.height))
                .collect::<Vec<_>>(),
            caps.formats,
            caps.transfers,
            caps.models,
            caps.fps
        );
        let preferred =
            position == Some(POSITION_LEFT) && kind == Some(CAMERA_TYPE_PASSTHROUGH_COLOR);
        if chosen.is_none() || preferred {
            chosen = Some((id, caps));
            if preferred {
                break;
            }
        }
    }
    report.block("PICO 相机属性和能力", details);
    let (id, caps) = chosen.expect("ids is not empty");
    camera.camera = id;
    camera.capabilities = caps;

    let create = IdGetInfo {
        ty: ty(TYPE_DEVICE_CREATE_INFO),
        next: ptr::null(),
        camera_id: id,
    };
    let mut future = sys::FutureEXT::default();
    check(
        unsafe { (fns.create_device_async)(raw, &create, &mut future) },
        "xrCreateCameraDeviceAsyncPICO",
    )?;
    report.set(s, "状态", Status::Pending, format!("正在打开相机 {id}"));
    camera.fns = Some(fns);
    camera.step = Step::DeviceFuture(future, Instant::now());
    Ok(())
}

/// 读取相机的朝向、位置和类型属性，每次只查一项，某项失败不影响其他项。
unsafe fn properties(fns: &Fns, raw: Instance, id: u64) -> Vec<(i32, i32)> {
    let mut count = 0;
    let mut types = [0i32; 8];
    let result = unsafe {
        (fns.enumerate_property_types)(raw, id, types.len() as u32, &mut count, types.as_mut_ptr())
    };
    if result.into_raw() < 0 {
        return vec![(-1, result.into_raw())];
    }
    let mut out = Vec::new();
    for &kind in types.iter().take(count as usize) {
        let struct_type = match kind {
            PROPERTY_FACING => TYPE_CAMERA_PROPERTY_FACING,
            PROPERTY_POSITION => TYPE_CAMERA_PROPERTY_POSITION,
            PROPERTY_CAMERA_TYPE => TYPE_CAMERA_PROPERTY_CAMERA_TYPE,
            _ => continue,
        };
        let mut value = Value::<i32> {
            ty: ty(struct_type),
            next: ptr::null(),
            value: 0,
        };
        let mut pointer = &mut value as *mut Value<i32> as *mut Header;
        let mut list = PointerList {
            ty: ty(TYPE_CAMERA_PROPERTIES),
            next: ptr::null(),
            count: 1,
            items: &mut pointer,
        };
        let info = IdGetInfo {
            ty: ty(TYPE_CAMERA_PROPERTIES_GET_INFO),
            next: ptr::null(),
            camera_id: id,
        };
        let result = unsafe { (fns.get_properties)(raw, &info, &mut list) };
        out.push((
            kind,
            if result.into_raw() < 0 {
                result.into_raw()
            } else {
                value.value
            },
        ));
    }
    out
}

/// 读取相机支持的分辨率、格式、传输方式、相机模型和帧率，每次只查一项。
unsafe fn capabilities(fns: &Fns, raw: Instance, id: u64) -> Capabilities {
    let mut caps = Capabilities::default();
    let mut count = 0;
    let mut types = [0i32; 8];
    let result = unsafe {
        (fns.enumerate_capability_types)(
            raw,
            id,
            types.len() as u32,
            &mut count,
            types.as_mut_ptr(),
        )
    };
    if result.into_raw() < 0 {
        caps.types.push(result.into_raw());
        return caps;
    }
    caps.types = types[..count as usize].to_vec();
    let info = IdGetInfo {
        ty: ty(TYPE_SUPPORTED_CAPABILITIES_GET_INFO),
        next: ptr::null(),
        camera_id: id,
    };
    let query = |header: *mut Header| -> bool {
        let mut pointer = header;
        let mut list = PointerList {
            ty: ty(TYPE_SUPPORTED_CAPABILITIES),
            next: ptr::null(),
            count: 1,
            items: &mut pointer,
        };
        unsafe { (fns.get_supported_capabilities)(raw, &info, &mut list) }.into_raw() >= 0
    };
    for &kind in &caps.types.clone() {
        if kind == CAPABILITY_RESOLUTION {
            let mut items = [sys::Extent2Di {
                width: 0,
                height: 0,
            }; 16];
            let mut list = SupportedList {
                ty: ty(TYPE_SUPPORTED_RESOLUTION),
                next: ptr::null(),
                capacity: items.len() as u32,
                count: 0,
                items: items.as_mut_ptr(),
            };
            if query(&mut list as *mut _ as *mut Header) {
                caps.resolutions = items[..(list.count as usize).min(items.len())].to_vec();
            }
            continue;
        }
        let (struct_type, target) = match kind {
            CAPABILITY_FORMAT => (TYPE_SUPPORTED_FORMAT, &mut caps.formats),
            CAPABILITY_TRANSFER => (TYPE_SUPPORTED_TRANSFER, &mut caps.transfers),
            CAPABILITY_MODEL => (TYPE_SUPPORTED_MODEL, &mut caps.models),
            CAPABILITY_FPS => (TYPE_SUPPORTED_FPS, &mut caps.fps),
            _ => continue,
        };
        let mut items = [0i32; 16];
        let mut list = SupportedList {
            ty: ty(struct_type),
            next: ptr::null(),
            capacity: items.len() as u32,
            count: 0,
            items: items.as_mut_ptr(),
        };
        if query(&mut list as *mut _ as *mut Header) {
            *target = items[..(list.count as usize).min(items.len())].to_vec();
        }
    }
    caps
}

/// 相机设备创建完成后，按支持的能力配置并创建采集会话。
unsafe fn finish_device(
    camera: &mut PicoCamera,
    instance: &OxrInstance,
    session: &OxrSession,
    future: sys::FutureEXT,
    report: &mut Report,
) -> Result<(), String> {
    let s = Section::PicoCamera;
    let fns = camera.fns.as_ref().ok_or("函数未加载")?;
    let mut completion = HandleCompletion {
        ty: ty(TYPE_DEVICE_COMPLETION),
        next: ptr::null_mut(),
        future_result: sys::Result::SUCCESS,
        handle: 0,
    };
    check(
        unsafe { (fns.create_device_complete)(instance.as_raw(), future, &mut completion) },
        "xrCreateCameraDeviceCompletePICO",
    )?;
    check(completion.future_result, "打开相机设备")?;
    camera.device = completion.handle;
    report.set(
        s,
        "相机设备",
        Status::Ok,
        format!("相机 {} 已打开", camera.camera),
    );

    let caps = &camera.capabilities;
    let resolution = caps
        .resolutions
        .iter()
        .max_by_key(|r| r.width * r.height)
        .copied();
    let pick = |list: &[i32], preferred: i32| {
        list.iter()
            .copied()
            .find(|v| *v == preferred)
            .or(list.first().copied())
    };
    let mut resolution_config = resolution.map(|r| Value {
        ty: ty(TYPE_CAPABILITY_RESOLUTION),
        next: ptr::null(),
        value: r,
    });
    let mut values: Vec<Value<i32>> = [
        (TYPE_CAPABILITY_FORMAT, pick(&caps.formats, 1)),
        (TYPE_CAPABILITY_TRANSFER, pick(&caps.transfers, 1)),
        (TYPE_CAPABILITY_MODEL, pick(&caps.models, 1)),
        (TYPE_CAPABILITY_FPS, pick(&caps.fps, FPS_30)),
    ]
    .into_iter()
    .filter_map(|(t, v)| {
        v.map(|value| Value {
            ty: ty(t),
            next: ptr::null(),
            value,
        })
    })
    .collect();
    let mut configs: Vec<*const Header> = values
        .iter_mut()
        .map(|v| v as *mut Value<i32> as *const Header)
        .collect();
    if let Some(config) = resolution_config.as_mut() {
        configs.insert(0, config as *mut Value<sys::Extent2Di> as *const Header);
    }
    let info = SessionCreateInfo {
        ty: ty(TYPE_SESSION_CREATE_INFO),
        next: ptr::null(),
        camera: camera.device,
        config_count: configs.len() as u32,
        configs: configs.as_ptr(),
    };
    let mut future = sys::FutureEXT::default();
    check(
        unsafe { (fns.create_session_async)(session.as_raw(), &info, &mut future) },
        "xrCreateCameraCaptureSessionAsyncPICO",
    )?;
    report.set(
        s,
        "会话配置",
        Status::Info,
        format!(
            "{} 项 · 分辨率 {}",
            configs.len(),
            resolution
                .map(|r| format!("{}x{}", r.width, r.height))
                .unwrap_or("未指定".into())
        ),
    );
    camera.step = Step::SessionFuture(future, Instant::now());
    Ok(())
}

/// 采集会话创建完成后读取内参和外参，然后开始采集。
unsafe fn finish_session(
    camera: &mut PicoCamera,
    session: &OxrSession,
    future: sys::FutureEXT,
    report: &mut Report,
) -> Result<(), String> {
    let s = Section::PicoCamera;
    let fns = camera.fns.as_ref().ok_or("函数未加载")?;
    let mut completion = HandleCompletion {
        ty: ty(TYPE_SESSION_COMPLETION),
        next: ptr::null_mut(),
        future_result: sys::Result::SUCCESS,
        handle: 0,
    };
    check(
        unsafe { (fns.create_session_complete)(session.as_raw(), future, &mut completion) },
        "xrCreateCameraCaptureSessionCompletePICO",
    )?;
    check(completion.future_result, "创建采集会话")?;
    camera.session = completion.handle;

    let mut intrinsics = Intrinsics {
        ty: ty(TYPE_INTRINSICS),
        next: ptr::null(),
        focal_length: sys::Vector2f { x: 0.0, y: 0.0 },
        principal_point: sys::Vector2f { x: 0.0, y: 0.0 },
        fov: sys::Vector2f { x: 0.0, y: 0.0 },
    };
    let result = unsafe { (fns.get_intrinsics)(camera.session, &mut intrinsics) };
    if result.into_raw() < 0 {
        report.set(s, "内参", Status::Fail, xr_result(result));
    } else {
        report.set(
            s,
            "内参",
            Status::Ok,
            format!(
                "f ({:.1}, {:.1}) · c ({:.1}, {:.1}) · fov ({:.3}, {:.3})",
                intrinsics.focal_length.x,
                intrinsics.focal_length.y,
                intrinsics.principal_point.x,
                intrinsics.principal_point.y,
                intrinsics.fov.x,
                intrinsics.fov.y
            ),
        );
    }
    let mut extrinsics = Extrinsics {
        ty: ty(TYPE_EXTRINSICS),
        next: ptr::null(),
        pose: sys::Posef {
            orientation: sys::Quaternionf {
                x: 0.0,
                y: 0.0,
                z: 0.0,
                w: 1.0,
            },
            position: sys::Vector3f {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
        },
    };
    let result = unsafe { (fns.get_extrinsics)(camera.session, &mut extrinsics) };
    if result.into_raw() < 0 {
        report.set(s, "外参", Status::Fail, xr_result(result));
    } else {
        let p = extrinsics.pose.position;
        let q = extrinsics.pose.orientation;
        report.set(
            s,
            "外参",
            Status::Ok,
            format!(
                "t ({:+.3}, {:+.3}, {:+.3}) · q ({:+.3}, {:+.3}, {:+.3}, {:+.3})",
                p.x, p.y, p.z, q.x, q.y, q.z, q.w
            ),
        );
    }
    unsafe { begin(camera, report) }
}

/// 开始采集并进入取帧阶段。
unsafe fn begin(camera: &mut PicoCamera, report: &mut Report) -> Result<(), String> {
    let fns = camera.fns.as_ref().ok_or("函数未加载")?;
    let mut info = Header {
        ty: ty(TYPE_BEGIN_INFO),
        next: ptr::null(),
    };
    check(
        unsafe { (fns.begin_capture)(camera.session, &mut info) },
        "xrBeginCameraCapturePICO",
    )?;
    report.set(Section::PicoCamera, "状态", Status::Pending, "采集中");
    camera.step = Step::Capturing {
        since: Instant::now(),
        last_time: sys::Time::from_nanos(0),
        frames: 0,
        thumbnail_done: false,
    };
    Ok(())
}

/// 取最新的一帧：第一帧生成缩略图，持续一段时间后统计帧率并停止采集。
unsafe fn capture(
    camera: &mut PicoCamera,
    instance: &OxrInstance,
    report: &mut Report,
    images: &mut Assets<Image>,
) -> Result<(), String> {
    let s = Section::PicoCamera;
    let session = camera.session;
    let fns = camera.fns.as_ref().ok_or("函数未加载")?;
    let Step::Capturing {
        since,
        last_time,
        frames,
        thumbnail_done,
    } = &mut camera.step
    else {
        return Ok(());
    };
    let info = AcquireInfo {
        ty: ty(TYPE_ACQUIRE_INFO),
        next: ptr::null(),
        last_capture_time: *last_time,
    };
    let mut image = CameraImage {
        ty: ty(TYPE_IMAGE),
        next: ptr::null(),
        capture_time: sys::Time::from_nanos(0),
        image_id: 0,
    };
    let result = unsafe { (fns.acquire_image)(session, &info, &mut image) };
    if result.into_raw() < 0 {
        report.set(s, "取帧", Status::Fail, xr_result(result));
    } else if result.into_raw() != IMAGE_NO_UPDATE {
        *frames += 1;
        *last_time = image.capture_time;
        if !*thumbnail_done {
            *thumbnail_done = true;
            let latency = instance
                .now()
                .map(|now| (now.as_nanos() - image.capture_time.as_nanos()) as f64 / 1e6)
                .unwrap_or(f64::NAN);
            let mut data = RawBuffer {
                ty: ty(TYPE_RAW_BUFFER),
                next: ptr::null(),
                width: 0,
                height: 0,
                stride: 0,
                bytes_per_pixel: 0,
                pixel_stride: 0,
                buffer_size: 0,
                buffer: ptr::null_mut(),
            };
            let result = unsafe { (fns.get_image_data)(session, image.image_id, &mut data) };
            if result.into_raw() < 0 {
                report.set(s, "图像数据", Status::Fail, xr_result(result));
            } else {
                report.set(
                    s,
                    "图像数据",
                    Status::Ok,
                    format!(
                        "{}x{} · 行 {} 字节 · 每像素 {} / 间隔 {} · 共 {} 字节 · 延迟 {latency:.1} ms",
                        data.width,
                        data.height,
                        data.stride,
                        data.bytes_per_pixel,
                        data.pixel_stride,
                        data.buffer_size
                    ),
                );
                if let Some(image) = unsafe { thumbnail(&data) } {
                    report.thumbnail("PICO 相机", images.add(image));
                }
            }
        }
        let result = unsafe { (fns.release_image)(session, image.image_id) };
        if result.into_raw() < 0 {
            report.set(s, "释放图像", Status::Fail, xr_result(result));
        }
    }
    let elapsed = since.elapsed();
    if elapsed < CAPTURE_DURATION {
        return Ok(());
    }
    let fps = *frames as f64 / elapsed.as_secs_f64();
    let got = *frames > 0;
    report.set(
        s,
        "帧率",
        if got { Status::Ok } else { Status::Fail },
        format!(
            "{} 秒内 {} 帧 · {fps:.1} fps",
            CAPTURE_DURATION.as_secs(),
            frames
        ),
    );
    let result = unsafe { (fns.end_capture)(session) };
    if result.into_raw() < 0 {
        report.set(s, "结束采集", Status::Warn, xr_result(result));
    }
    report.set(
        s,
        "状态",
        if got { Status::Ok } else { Status::Fail },
        if got {
            "可以取到画面（左 Y 重测）"
        } else {
            "会话正常但没有取到帧"
        },
    );
    camera.step = Step::Idle;
    Ok(())
}

/// 把 RGBA 缓冲缩放成宽 THUMB_WIDTH 的缩略图；缓冲信息不合理时返回 None。
unsafe fn thumbnail(data: &RawBuffer) -> Option<Image> {
    let bpp = data.pixel_stride.max(data.bytes_per_pixel) as usize;
    if data.buffer.is_null() || data.width == 0 || data.height == 0 || bpp < 3 {
        return None;
    }
    let stride = data.stride.max(data.width * bpp as u32) as usize;
    let needed = stride * (data.height as usize - 1) + data.width as usize * bpp;
    if (data.buffer_size as usize) < needed {
        return None;
    }
    let source = unsafe { std::slice::from_raw_parts(data.buffer, needed) };
    let height = (data.height * THUMB_WIDTH / data.width).max(1);
    let mut pixels = Vec::with_capacity((THUMB_WIDTH * height * 4) as usize);
    for y in 0..height {
        let sy = (y * data.height / height) as usize;
        for x in 0..THUMB_WIDTH {
            let sx = (x * data.width / THUMB_WIDTH) as usize;
            let o = sy * stride + sx * bpp;
            pixels.extend_from_slice(&[source[o], source[o + 1], source[o + 2], 255]);
        }
    }
    Some(Image::new(
        Extent3d {
            width: THUMB_WIDTH,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        pixels,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    ))
}
