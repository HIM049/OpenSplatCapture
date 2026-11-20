use std::{
    collections::BTreeMap,
    ffi::c_void,
    ptr,
    time::{Duration, Instant},
};

use bevy::prelude::*;
use bevy_mod_openxr::{
    openxr_session_running,
    resources::{OxrInstance, OxrSystemId},
    session::OxrSession,
};
use openxr::{
    raw::SpatialSensingBD,
    sys::{self, Handle as _},
};

use crate::{
    android::{SPATIAL_PERMISSION, has_permission},
    controls::ProbeCommand,
    device::StartedAt,
    report::{Report, Section, Status},
    xr::{check, poll},
};

/// 等待空间数据权限的最长时间，超时后照样尝试。
const PERMISSION_WAIT: Duration = Duration::from_secs(10);
const FUTURE_TIMEOUT: Duration = Duration::from_secs(10);
/// 房间设定由用户在系统界面里完成，给足时间。
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(600);
/// 数据提供方运行后每隔多久重新查询一次。
const QUERY_INTERVAL: Duration = Duration::from_secs(5);

pub struct SpatialPlugin;

impl Plugin for SpatialPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Spatial>()
            .add_systems(Update, run.run_if(openxr_session_running));
    }
}

/// 三种空间数据提供方。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Scene,
    Mesh,
    Plane,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Scene => "房间设定",
            Kind::Mesh => "空间网格",
            Kind::Plane => "平面",
        }
    }
}

enum Step {
    Starting(sys::FutureEXT, Instant),
    Running(Instant),
    Querying(sys::FutureEXT, Instant),
    Failed,
}

struct Provider {
    kind: Kind,
    handle: sys::SenseDataProviderBD,
    step: Step,
}

#[derive(Resource, Default)]
struct Spatial {
    started: bool,
    providers: Vec<Provider>,
    /// 进行中的房间设定。
    capture: Option<(sys::FutureEXT, Instant)>,
}

fn run(
    mut spatial: ResMut<Spatial>,
    instance: Res<OxrInstance>,
    system: Res<OxrSystemId>,
    session: Res<OxrSession>,
    started: Res<StartedAt>,
    mut commands: MessageReader<ProbeCommand>,
    mut report: ResMut<Report>,
) {
    let s = Section::Spatial;
    let capture_requested = commands.read().any(|c| *c == ProbeCommand::CaptureScene);
    let Some(fp) = instance.exts().bd_spatial_sensing else {
        report.set(s, "状态", Status::Fail, "XR_BD_spatial_sensing 不可用");
        return;
    };
    if !spatial.started {
        if !has_permission(SPATIAL_PERMISSION) && started.0.elapsed() < PERMISSION_WAIT {
            report.set(s, "状态", Status::Pending, "等待空间数据权限");
            return;
        }
        spatial.started = true;
        unsafe { start(&mut spatial, &instance, &system, &session, fp, &mut report) };
        return;
    }

    let spatial = &mut *spatial;
    for provider in &mut spatial.providers {
        if let Err(err) = unsafe { advance(provider, &instance, &session, fp, &mut report) } {
            report.set(s, provider.kind.name(), Status::Fail, err);
            provider.step = Step::Failed;
        }
    }
    if let Err(err) = unsafe { scene_capture(spatial, &instance, capture_requested, &mut report) } {
        report.set(s, "房间设定界面", Status::Fail, err);
        spatial.capture = None;
    }
}

/// 读取系统能力，创建并启动各类数据提供方。
unsafe fn start(
    spatial: &mut Spatial,
    instance: &OxrInstance,
    system: &OxrSystemId,
    session: &OxrSession,
    fp: SpatialSensingBD,
    report: &mut Report,
) {
    let s = Section::Spatial;
    let exts = instance.exts();
    let mut sensing = sys::SystemSpatialSensingPropertiesBD {
        ty: sys::SystemSpatialSensingPropertiesBD::TYPE,
        next: ptr::null_mut(),
        supports_spatial_sensing: sys::FALSE,
    };
    let supported = unsafe { system_flag(instance, system, &mut sensing as *mut _ as *mut c_void) }
        .map(|_| bool::from(sensing.supports_spatial_sensing));
    report.set(
        s,
        "系统支持空间感知",
        flag_status(&supported),
        flag_text(&supported),
    );

    let mut kinds = Vec::new();
    if exts.bd_spatial_scene.is_some() {
        let mut props = sys::SystemSpatialScenePropertiesBD {
            ty: sys::SystemSpatialScenePropertiesBD::TYPE,
            next: ptr::null_mut(),
            supports_spatial_scene: sys::FALSE,
        };
        let supported =
            unsafe { system_flag(instance, system, &mut props as *mut _ as *mut c_void) }
                .map(|_| bool::from(props.supports_spatial_scene));
        report.set(
            s,
            "系统支持房间设定",
            flag_status(&supported),
            flag_text(&supported),
        );
        kinds.push(Kind::Scene);
    }
    if exts.bd_spatial_mesh.is_some() {
        let mut props = sys::SystemSpatialMeshPropertiesBD {
            ty: sys::SystemSpatialMeshPropertiesBD::TYPE,
            next: ptr::null_mut(),
            supports_spatial_mesh: sys::FALSE,
        };
        let supported =
            unsafe { system_flag(instance, system, &mut props as *mut _ as *mut c_void) }
                .map(|_| bool::from(props.supports_spatial_mesh));
        report.set(
            s,
            "系统支持空间网格",
            flag_status(&supported),
            flag_text(&supported),
        );
        kinds.push(Kind::Mesh);
    }
    if exts.bd_spatial_plane.is_some() {
        let mut props = sys::SystemSpatialPlanePropertiesBD {
            ty: sys::SystemSpatialPlanePropertiesBD::TYPE,
            next: ptr::null_mut(),
            supports_spatial_plane: sys::FALSE,
        };
        let supported =
            unsafe { system_flag(instance, system, &mut props as *mut _ as *mut c_void) }
                .map(|_| bool::from(props.supports_spatial_plane));
        report.set(
            s,
            "系统支持平面",
            flag_status(&supported),
            flag_text(&supported),
        );
        kinds.push(Kind::Plane);
    }
    if kinds.is_empty() {
        report.set(s, "状态", Status::Fail, "房间设定、网格、平面扩展都不可用");
        return;
    }
    report.set(s, "状态", Status::Info, "已启动，房间设定可按左 X 打开");

    for kind in kinds {
        match unsafe { create(kind, session, fp) } {
            Ok((handle, future)) => spatial.providers.push(Provider {
                kind,
                handle,
                step: Step::Starting(future, Instant::now()),
            }),
            Err(err) => report.set(s, kind.name(), Status::Fail, err),
        }
    }
}

/// 把一个系统能力结构接到 XrSystemProperties 后面查询。
unsafe fn system_flag(
    instance: &OxrInstance,
    system: &OxrSystemId,
    next: *mut c_void,
) -> Result<(), String> {
    let mut props: sys::SystemProperties = unsafe { std::mem::zeroed() };
    props.ty = sys::SystemProperties::TYPE;
    props.next = next;
    check(
        unsafe { (instance.fp().get_system_properties)(instance.as_raw(), system.0, &mut props) },
        "xrGetSystemProperties",
    )
}

fn flag_status(value: &Result<bool, String>) -> Status {
    match value {
        Ok(true) => Status::Ok,
        Ok(false) => Status::Warn,
        Err(_) => Status::Fail,
    }
}

fn flag_text(value: &Result<bool, String>) -> String {
    match value {
        Ok(true) => "是".into(),
        Ok(false) => "否".into(),
        Err(err) => err.clone(),
    }
}

/// 创建一个数据提供方并异步启动它。
unsafe fn create(
    kind: Kind,
    session: &OxrSession,
    fp: SpatialSensingBD,
) -> Result<(sys::SenseDataProviderBD, sys::FutureEXT), String> {
    let mesh = sys::SenseDataProviderCreateInfoSpatialMeshBD {
        ty: sys::SenseDataProviderCreateInfoSpatialMeshBD::TYPE,
        next: ptr::null(),
        config_flags: sys::SpatialMeshConfigFlagsBD::SEMANTIC,
        lod: sys::SpatialMeshLodBD::COARSE,
    };
    let info = sys::SenseDataProviderCreateInfoBD {
        ty: sys::SenseDataProviderCreateInfoBD::TYPE,
        next: if kind == Kind::Mesh {
            &mesh as *const _ as *const c_void
        } else {
            ptr::null()
        },
        provider_type: match kind {
            Kind::Scene => sys::SenseDataProviderTypeBD::SCENE,
            Kind::Mesh => sys::SenseDataProviderTypeBD::MESH,
            Kind::Plane => sys::SenseDataProviderTypeBD::PLANE,
        },
    };
    let mut handle = sys::SenseDataProviderBD::NULL;
    check(
        unsafe { (fp.create_sense_data_provider)(session.as_raw(), &info, &mut handle) },
        "xrCreateSenseDataProviderBD",
    )?;
    let start = sys::SenseDataProviderStartInfoBD {
        ty: sys::SenseDataProviderStartInfoBD::TYPE,
        next: ptr::null(),
    };
    let mut future = sys::FutureEXT::default();
    check(
        unsafe { (fp.start_sense_data_provider_async)(handle, &start, &mut future) },
        "xrStartSenseDataProviderAsyncBD",
    )?;
    Ok((handle, future))
}

/// 推进一个数据提供方：等待启动完成，然后定期查询并汇总数据。
unsafe fn advance(
    provider: &mut Provider,
    instance: &OxrInstance,
    session: &OxrSession,
    fp: SpatialSensingBD,
    report: &mut Report,
) -> Result<(), String> {
    let s = Section::Spatial;
    match provider.step {
        Step::Starting(future, since) => {
            if !poll(instance, future, since, FUTURE_TIMEOUT)? {
                report.set(s, provider.kind.name(), Status::Pending, "启动中");
                return Ok(());
            }
            let mut completion = sys::FutureCompletionEXT {
                ty: sys::FutureCompletionEXT::TYPE,
                next: ptr::null_mut(),
                future_result: sys::Result::SUCCESS,
            };
            check(
                unsafe {
                    (fp.start_sense_data_provider_complete)(
                        session.as_raw(),
                        future,
                        &mut completion,
                    )
                },
                "xrStartSenseDataProviderCompleteBD",
            )?;
            check(completion.future_result, "启动数据提供方")?;
            let mut state = sys::SenseDataProviderStateBD::INITIALIZED;
            check(
                unsafe { (fp.get_sense_data_provider_state)(provider.handle, &mut state) },
                "xrGetSenseDataProviderStateBD",
            )?;
            report.set(
                s,
                provider.kind.name(),
                Status::Pending,
                format!("已启动（{state:?}），等待查询"),
            );
            provider.step = Step::Running(Instant::now());
        }
        Step::Running(since) => {
            if since.elapsed() < QUERY_INTERVAL
                && report.status(s, provider.kind.name()) != Some(Status::Pending)
            {
                return Ok(());
            }
            let info = sys::SenseDataQueryInfoBD {
                ty: sys::SenseDataQueryInfoBD::TYPE,
                next: ptr::null(),
            };
            let mut future = sys::FutureEXT::default();
            check(
                unsafe { (fp.query_sense_data_async)(provider.handle, &info, &mut future) },
                "xrQuerySenseDataAsyncBD",
            )?;
            provider.step = Step::Querying(future, Instant::now());
        }
        Step::Querying(future, since) => {
            if !poll(instance, future, since, FUTURE_TIMEOUT)? {
                return Ok(());
            }
            let mut completion = sys::SenseDataQueryCompletionBD {
                ty: sys::SenseDataQueryCompletionBD::TYPE,
                next: ptr::null_mut(),
                future_result: sys::Result::SUCCESS,
                snapshot: sys::SenseDataSnapshotBD::NULL,
            };
            check(
                unsafe { (fp.query_sense_data_complete)(provider.handle, future, &mut completion) },
                "xrQuerySenseDataCompleteBD",
            )?;
            check(completion.future_result, "查询数据")?;
            let summary = unsafe { summarize(provider.kind, completion.snapshot, fp) };
            unsafe { (fp.destroy_sense_data_snapshot)(completion.snapshot) };
            let (status, text) = summary?;
            report.set(s, provider.kind.name(), status, text);
            provider.step = Step::Running(Instant::now());
        }
        Step::Failed => {}
    }
    Ok(())
}

/// 统计一份快照里的实体：语义标签、包围盒、边框和网格大小。
unsafe fn summarize(
    kind: Kind,
    snapshot: sys::SenseDataSnapshotBD,
    fp: SpatialSensingBD,
) -> Result<(Status, String), String> {
    let mut get_info = sys::QueriedSenseDataGetInfoBD {
        ty: sys::QueriedSenseDataGetInfoBD::TYPE,
        next: ptr::null(),
    };
    let mut data = sys::QueriedSenseDataBD {
        ty: sys::QueriedSenseDataBD::TYPE,
        next: ptr::null_mut(),
        state_capacity_input: 0,
        state_count_output: 0,
        states: ptr::null_mut(),
    };
    check(
        unsafe { (fp.get_queried_sense_data)(snapshot, &mut get_info, &mut data) },
        "xrGetQueriedSenseDataBD",
    )?;
    let mut states: Vec<sys::SpatialEntityStateBD> = (0..data.state_count_output)
        .map(|_| sys::SpatialEntityStateBD {
            ty: sys::SpatialEntityStateBD::TYPE,
            next: ptr::null_mut(),
            entity_id: sys::SpatialEntityIdBD::from_raw(0),
            last_update_time: sys::Time::from_nanos(0),
            uuid: sys::UuidEXT { data: [0; 16] },
        })
        .collect();
    data.state_capacity_input = states.len() as u32;
    data.states = states.as_mut_ptr();
    check(
        unsafe { (fp.get_queried_sense_data)(snapshot, &mut get_info, &mut data) },
        "xrGetQueriedSenseDataBD",
    )?;
    states.truncate(data.state_count_output as usize);

    let mut labels: BTreeMap<&'static str, u32> = BTreeMap::new();
    let (mut boxes, mut polygons, mut vertices, mut indices) = (0u32, 0u32, 0u64, 0u64);
    for state in &states {
        let id = state.entity_id;
        let mut count = 0;
        let mut types = [sys::SpatialEntityComponentTypeBD::LOCATION; 16];
        if unsafe {
            (fp.enumerate_spatial_entity_component_types)(
                snapshot,
                id,
                types.len() as u32,
                &mut count,
                types.as_mut_ptr(),
            )
        }
        .into_raw()
            < 0
        {
            continue;
        }
        let types = &types[..(count as usize).min(16)];
        let component = |kind: sys::SpatialEntityComponentTypeBD, data: *mut c_void| -> bool {
            let info = sys::SpatialEntityComponentGetInfoBD {
                ty: sys::SpatialEntityComponentGetInfoBD::TYPE,
                next: ptr::null(),
                entity_id: id,
                component_type: kind,
            };
            unsafe { (fp.get_spatial_entity_component_data)(snapshot, &info, data as *mut _) }
                .into_raw()
                >= 0
        };
        if types.contains(&sys::SpatialEntityComponentTypeBD::SEMANTIC) {
            let mut buffer = [sys::SemanticLabelBD::UNKNOWN; 8];
            let mut semantic = sys::SpatialEntityComponentDataSemanticBD {
                ty: sys::SpatialEntityComponentDataSemanticBD::TYPE,
                next: ptr::null_mut(),
                label_capacity_input: buffer.len() as u32,
                label_count_output: 0,
                labels: buffer.as_mut_ptr(),
            };
            if component(
                sys::SpatialEntityComponentTypeBD::SEMANTIC,
                &mut semantic as *mut _ as *mut c_void,
            ) {
                let n = (semantic.label_count_output as usize).min(buffer.len());
                if kind == Kind::Mesh {
                    *labels.entry("带语义").or_default() += (n > 0) as u32;
                } else if let Some(label) = buffer[..n].first() {
                    *labels.entry(label_name(*label)).or_default() += 1;
                }
            }
        }
        if types.contains(&sys::SpatialEntityComponentTypeBD::BOUNDING_BOX_3D) {
            boxes += 1;
        }
        if types.contains(&sys::SpatialEntityComponentTypeBD::POLYGON)
            || types.contains(&sys::SpatialEntityComponentTypeBD::BOUNDING_BOX_2D)
        {
            polygons += 1;
        }
        if types.contains(&sys::SpatialEntityComponentTypeBD::TRIANGLE_MESH) {
            let mut mesh = sys::SpatialEntityComponentDataTriangleMeshBD {
                ty: sys::SpatialEntityComponentDataTriangleMeshBD::TYPE,
                next: ptr::null_mut(),
                vertex_capacity_input: 0,
                vertex_count_output: 0,
                vertices: ptr::null_mut(),
                index_capacity_input: 0,
                index_count_output: 0,
                indices: ptr::null_mut(),
            };
            if component(
                sys::SpatialEntityComponentTypeBD::TRIANGLE_MESH,
                &mut mesh as *mut _ as *mut c_void,
            ) {
                vertices += mesh.vertex_count_output as u64;
                indices += mesh.index_count_output as u64;
            }
        }
    }
    let n = states.len();
    let status = if n > 0 { Status::Ok } else { Status::Warn };
    let label_text = labels
        .iter()
        .map(|(name, count)| format!("{name} {count}"))
        .collect::<Vec<_>>()
        .join("、");
    let text = match kind {
        Kind::Scene => {
            format!("{n} 个物体 · {label_text} · 3D 包围盒 {boxes} · 平面边框 {polygons}")
        }
        Kind::Mesh => format!(
            "{n} 块 · 顶点 {vertices} · 三角形 {} · {label_text}",
            indices / 3
        ),
        Kind::Plane => format!("{n} 个平面 · 带边框 {polygons}"),
    };
    Ok((
        status,
        if n == 0 {
            format!("{text}（没有数据）")
        } else {
            text
        },
    ))
}

/// 按左 X 时拉起系统的房间设定，完成后让房间设定提供方立即重新查询。
unsafe fn scene_capture(
    spatial: &mut Spatial,
    instance: &OxrInstance,
    requested: bool,
    report: &mut Report,
) -> Result<(), String> {
    let s = Section::Spatial;
    let Some(fp) = instance.exts().bd_spatial_scene else {
        if requested {
            report.set(
                s,
                "房间设定界面",
                Status::Fail,
                "XR_BD_spatial_scene 不可用",
            );
        }
        return Ok(());
    };
    let Some(provider) = spatial.providers.iter_mut().find(|p| p.kind == Kind::Scene) else {
        return Ok(());
    };
    match spatial.capture {
        None if requested => {
            let info = sys::SceneCaptureInfoBD {
                ty: sys::SceneCaptureInfoBD::TYPE,
                next: ptr::null(),
            };
            let mut future = sys::FutureEXT::default();
            check(
                unsafe { (fp.capture_scene_async)(provider.handle, &info, &mut future) },
                "xrCaptureSceneAsyncBD",
            )?;
            report.set(s, "房间设定界面", Status::Pending, "已打开，等待完成");
            spatial.capture = Some((future, Instant::now()));
        }
        Some((future, since)) => {
            if !poll(instance, future, since, CAPTURE_TIMEOUT)? {
                return Ok(());
            }
            let mut completion = sys::FutureCompletionEXT {
                ty: sys::FutureCompletionEXT::TYPE,
                next: ptr::null_mut(),
                future_result: sys::Result::SUCCESS,
            };
            check(
                unsafe { (fp.capture_scene_complete)(provider.handle, future, &mut completion) },
                "xrCaptureSceneCompleteBD",
            )?;
            check(completion.future_result, "房间设定")?;
            report.set(s, "房间设定界面", Status::Ok, "已完成");
            if matches!(provider.step, Step::Running(_)) {
                let due = Instant::now()
                    .checked_sub(QUERY_INTERVAL)
                    .unwrap_or_else(Instant::now);
                provider.step = Step::Running(due);
            }
            spatial.capture = None;
        }
        None => {}
    }
    Ok(())
}

fn label_name(label: sys::SemanticLabelBD) -> &'static str {
    match label.into_raw() {
        1 => "地面",
        2 => "天花板",
        3 => "墙",
        4 => "门",
        5 => "窗",
        6 => "开口",
        7 => "桌子",
        8 => "沙发",
        9 => "椅子",
        10 => "人",
        11 => "横梁",
        12 => "柱子",
        13 => "窗帘",
        14 => "柜子",
        15 => "床",
        16 => "植物",
        17 => "屏幕",
        18 => "虚拟墙",
        19 => "冰箱",
        _ => "其他",
    }
}
