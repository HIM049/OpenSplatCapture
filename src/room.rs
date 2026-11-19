use std::{
    ptr,
    time::{Duration, Instant},
};

use bevy::prelude::*;
use bevy_mod_openxr::{
    poll_events::{OxrEventHandlerExt, OxrEventIn},
    resources::OxrInstance,
    session::OxrSession,
};
use bevy_mod_xr::{session::XrState, spaces::XrPrimaryReferenceSpace};
use capture_format::{
    layout::{ROOM_LAYOUT, ROOM_POINTS},
    room::{RoomBox, RoomPlane, RoomPoint, layout_ply, points_ply, room_points},
};
use openxr::{
    Event, raw,
    sys::{self, Handle},
};

use crate::{
    android::{SCENE_PERMISSION, has_permission},
    pose::{ViewSpace, head_transform, locate},
    recorder::{CaptureMode, Recorder, RecorderSystems, RecordingStarted, Stage},
    tuning::room_settings,
};

/// 查询或开启组件的请求发出后这么久还没有结果就放弃。
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
/// 等空间数据权限时，隔这么久看一次是否已授予。
const PERMISSION_POLL: Duration = Duration::from_secs(1);
/// 带这些标签的实体不布点：没有实体墙的开口，以及覆盖整个房间的扫描网格。
const SKIPPED_LABELS: [&str; 2] = ["INVISIBLE_WALL_FACE", "GLOBAL_MESH"];
/// 带这些标签的立方体下面是空的。
const OPEN_BELOW: [&str; 2] = ["TABLE", "DESK"];

pub struct RoomPlugin;

impl Plugin for RoomPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RoomSetup>()
            .add_message::<RoomLoaded>()
            .add_message::<RequestRoomSetup>()
            .add_oxr_event_handler(handle_events)
            .add_systems(
                Update,
                (start_check, finish_check, request_setup, load_for_recording)
                    .chain()
                    .after(RecorderSystems),
            );
    }
}

/// 从系统保存的房间生成好了表面点：位置和朝向房间一侧的法线。
#[derive(Message)]
pub struct RoomLoaded(pub Vec<RoomPoint>);

/// 用户要求打开系统的空间设置。
#[derive(Message)]
pub struct RequestRoomSetup;

#[derive(Clone, PartialEq, Debug, Default)]
pub enum RoomStatus {
    /// 还没查过，或者在等空间数据权限。
    #[default]
    Unchecked,
    Checking,
    /// 找到了这么多个房间实体。
    Found(usize),
    /// 没有可用的房间数据，附原因。
    Missing(MissingReason),
}

/// 没有可用房间数据的原因。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MissingReason {
    Unsupported,
    QueryFailed,
    TimedOut,
    Refused,
    NoSetup,
    NoSurfaces,
}

/// 打开系统空间设置的结果。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SetupNote {
    Opening,
    Failed,
}

/// 系统保存的房间：启动时查一次，录制开始时用它生成表面点。
#[derive(Resource, Default)]
pub struct RoomSetup {
    pub status: RoomStatus,
    /// 打开系统空间设置的结果，供界面显示。
    pub setup_note: Option<SetupNote>,
    /// 查询或开启组件的请求发出的时刻。
    asked: Option<Instant>,
    /// 还没结束的查询。
    pending: Vec<sys::AsyncRequestIdFB>,
    /// 还没完成的定位组件开启请求；还没发出时为 None。
    enabling: Option<Vec<sys::AsyncRequestIdFB>>,
    /// 以错误结束的查询里最后一个的结果码。
    refused: Option<sys::Result>,
    /// 查到的空间实体和它们的标识，同一个实体只留一份。
    spaces: Vec<([u8; 16], sys::Space)>,
    /// 当前录制在等着用房间生成表面点。
    wanted: bool,
}

impl RoomSetup {
    fn missing(&mut self, reason: MissingReason) {
        warn!("room: no usable room setup: {reason:?}");
        self.status = RoomStatus::Missing(reason);
        self.asked = None;
        self.pending.clear();
        self.enabling = None;
    }

    /// 房间数据已经找到或还在读取，可以用它开始录制。
    pub fn usable(&self) -> bool {
        matches!(self.status, RoomStatus::Found(_) | RoomStatus::Checking)
    }
}

fn check(result: sys::Result, what: &str) -> Result<(), String> {
    if result.into_raw() >= 0 {
        Ok(())
    } else {
        Err(format!("{what} failed: {result:?}"))
    }
}

/// 会话运行起来、有了空间数据权限后，向系统查询保存的平面和立方体；会话停下时作废，恢复后重查。
fn start_check(
    state: Option<Res<XrState>>,
    instance: Option<Res<OxrInstance>>,
    session: Option<Res<OxrSession>>,
    mut room: ResMut<RoomSetup>,
    mut was_running: Local<bool>,
    mut last_poll: Local<Option<Instant>>,
) {
    let running = matches!(state.as_deref(), Some(XrState::Running)) && session.is_some();
    if *was_running && !running {
        *room = RoomSetup::default();
    }
    *was_running = running;
    let (Some(instance), Some(session)) = (instance, session) else {
        return;
    };
    if !running || room.status != RoomStatus::Unchecked {
        return;
    }
    if last_poll.is_some_and(|last| last.elapsed() < PERMISSION_POLL) {
        return;
    }
    *last_poll = Some(Instant::now());
    if !has_permission(SCENE_PERMISSION) {
        return;
    }
    let Some(fp) = instance.exts().fb_spatial_entity_query else {
        room.missing(MissingReason::Unsupported);
        return;
    };
    // 不带条件的查询不返回系统的房间实体，按组件类型各查一次。
    for component in [
        sys::SpaceComponentTypeFB::BOUNDED_2D,
        sys::SpaceComponentTypeFB::BOUNDED_3D,
    ] {
        let location = sys::SpaceStorageLocationFilterInfoFB {
            ty: sys::SpaceStorageLocationFilterInfoFB::TYPE,
            next: ptr::null(),
            location: sys::SpaceStorageLocationFB::LOCAL,
        };
        let filter = sys::SpaceComponentFilterInfoFB {
            ty: sys::SpaceComponentFilterInfoFB::TYPE,
            next: (&raw const location).cast(),
            component_type: component,
        };
        let info = sys::SpaceQueryInfoFB {
            ty: sys::SpaceQueryInfoFB::TYPE,
            next: ptr::null(),
            query_action: sys::SpaceQueryActionFB::LOAD,
            max_result_count: 128,
            timeout: sys::Duration::from_nanos(0),
            filter: (&raw const filter).cast(),
            exclude_filter: ptr::null(),
        };
        let mut request = sys::AsyncRequestIdFB::from_raw(0);
        let result =
            unsafe { (fp.query_spaces)(session.as_raw(), (&raw const info).cast(), &mut request) };
        match check(result, "xrQuerySpacesFB") {
            Ok(()) => room.pending.push(request),
            Err(err) => warn!("room: {component:?}: {err}"),
        }
    }
    if room.pending.is_empty() {
        room.missing(MissingReason::QueryFailed);
        return;
    }
    room.status = RoomStatus::Checking;
    room.asked = Some(Instant::now());
}

/// 收集查询结果和组件开启的完成通知；系统的空间设置结束后重新检查。
fn handle_events(
    event: OxrEventIn,
    instance: Res<OxrInstance>,
    session: Option<Res<OxrSession>>,
    mut room: ResMut<RoomSetup>,
) {
    match *event {
        Event::SpaceQueryResultsAvailableFB(available)
            if room.pending.contains(&available.request_id()) =>
        {
            let (Some(fp), Some(session)) = (instance.exts().fb_spatial_entity_query, session)
            else {
                return;
            };
            match unsafe { retrieve_results(&fp, session.as_raw(), available.request_id()) } {
                Ok(found) => {
                    for (uuid, space) in found {
                        if !room.spaces.iter().any(|(known, _)| *known == uuid) {
                            room.spaces.push((uuid, space));
                        }
                    }
                }
                Err(err) => warn!("room: {err}"),
            }
        }
        Event::SpaceQueryCompleteFB(complete) => {
            let request = complete.request_id();
            if complete.result().into_raw() < 0 {
                room.refused = Some(complete.result());
            }
            room.pending.retain(|pending| *pending != request);
        }
        Event::SpaceSetStatusCompleteFB(done) => {
            let request = done.request_id();
            if let Some(enabling) = &mut room.enabling {
                enabling.retain(|pending| *pending != request);
            }
        }
        Event::SceneCaptureCompleteFB(done) => {
            info!("room: room setup finished with {:?}", done.result());
            let wanted = room.wanted;
            *room = RoomSetup {
                wanted,
                ..default()
            };
        }
        _ => {}
    }
}

unsafe fn retrieve_results(
    fp: &raw::SpatialEntityQueryFB,
    session: sys::Session,
    request: sys::AsyncRequestIdFB,
) -> Result<Vec<([u8; 16], sys::Space)>, String> {
    unsafe {
        let mut results = sys::SpaceQueryResultsFB {
            ty: sys::SpaceQueryResultsFB::TYPE,
            next: ptr::null_mut(),
            result_capacity_input: 0,
            result_count_output: 0,
            results: ptr::null_mut(),
        };
        check(
            (fp.retrieve_space_query_results)(session, request, &mut results),
            "xrRetrieveSpaceQueryResultsFB (count)",
        )?;
        let mut buffer: Vec<sys::SpaceQueryResultFB> =
            vec![std::mem::zeroed(); results.result_count_output as usize];
        results.result_capacity_input = buffer.len() as u32;
        results.results = buffer.as_mut_ptr();
        check(
            (fp.retrieve_space_query_results)(session, request, &mut results),
            "xrRetrieveSpaceQueryResultsFB",
        )?;
        buffer.truncate(results.result_count_output as usize);
        Ok(buffer
            .into_iter()
            .map(|result| (result.uuid.data, result.space))
            .collect())
    }
}

/// 查询结束后让查到的实体可以定位，完成后确定有没有可用的房间。
fn finish_check(mut room: ResMut<RoomSetup>, instance: Option<Res<OxrInstance>>) {
    if room.status != RoomStatus::Checking {
        return;
    }
    let Some(asked) = room.asked else {
        return;
    };
    let timed_out = asked.elapsed() > QUERY_TIMEOUT;
    if !room.pending.is_empty() {
        if timed_out {
            room.missing(MissingReason::TimedOut);
        }
        return;
    }
    let Some(instance) = instance else {
        return;
    };
    let Some(entity) = instance.exts().fb_spatial_entity else {
        room.missing(MissingReason::Unsupported);
        return;
    };
    match &room.enabling {
        None => {
            // 查到的实体默认不能定位，先请求打开定位；形状和标签由系统管理，本来就是开着的。
            let mut requests = Vec::new();
            for &(_, space) in &room.spaces {
                if !unsafe { supports(&entity, space, sys::SpaceComponentTypeFB::LOCATABLE) }
                    || unsafe {
                        component_enabled(&entity, space, sys::SpaceComponentTypeFB::LOCATABLE)
                    }
                {
                    continue;
                }
                let info = sys::SpaceComponentStatusSetInfoFB {
                    ty: sys::SpaceComponentStatusSetInfoFB::TYPE,
                    next: ptr::null(),
                    component_type: sys::SpaceComponentTypeFB::LOCATABLE,
                    enabled: sys::TRUE,
                    timeout: sys::Duration::from_nanos(0),
                };
                let mut request = sys::AsyncRequestIdFB::from_raw(0);
                let result =
                    unsafe { (entity.set_space_component_status)(space, &info, &mut request) };
                match check(result, "xrSetSpaceComponentStatusFB") {
                    Ok(()) => requests.push(request),
                    Err(err) => warn!("room: {err}"),
                }
            }
            room.enabling = Some(requests);
            room.asked = Some(Instant::now());
            return;
        }
        Some(waiting) if !waiting.is_empty() && !timed_out => return,
        Some(_) => {}
    }
    room.asked = None;
    room.enabling = None;
    if room.spaces.is_empty() {
        if let Some(result) = room.refused {
            warn!("room: query refused with {result:?}");
            room.missing(MissingReason::Refused);
        } else {
            room.missing(MissingReason::NoSetup);
        }
        return;
    }
    info!("room: found {} saved room entities", room.spaces.len());
    room.status = RoomStatus::Found(room.spaces.len());
}

/// 拉起系统的空间设置，失败时记下原因。
fn request_setup(
    mut requests: MessageReader<RequestRoomSetup>,
    instance: Option<Res<OxrInstance>>,
    session: Option<Res<OxrSession>>,
    mut room: ResMut<RoomSetup>,
) {
    if requests.read().count() == 0 {
        return;
    }
    let (Some(instance), Some(session)) = (instance, session) else {
        return;
    };
    let result = match instance.exts().fb_scene_capture {
        Some(fp) => {
            let info = sys::SceneCaptureRequestInfoFB {
                ty: sys::SceneCaptureRequestInfoFB::TYPE,
                next: ptr::null(),
                request_byte_count: 0,
                request: ptr::null(),
            };
            let mut request = sys::AsyncRequestIdFB::from_raw(0);
            check(
                unsafe { (fp.request_scene_capture)(session.as_raw(), &info, &mut request) },
                "xrRequestSceneCaptureFB",
            )
        }
        None => Err("room setup cannot be opened from this app".to_owned()),
    };
    room.setup_note = Some(match result {
        Ok(()) => SetupNote::Opening,
        Err(err) => {
            warn!("room: {err}");
            SetupNote::Failed
        }
    });
}

/// 按位姿录制选了已设定的房间时，房间找到后用它生成表面点，调试版还把布局和点存进录制；还在查时等查完再生成。
fn load_for_recording(
    mut started: MessageReader<RecordingStarted>,
    mut recorder: ResMut<Recorder>,
    mut room: ResMut<RoomSetup>,
    instance: Option<Res<OxrInstance>>,
    session: Option<Res<OxrSession>>,
    base: Option<Res<XrPrimaryReferenceSpace>>,
    view: Option<Res<ViewSpace>>,
    mut loaded: MessageWriter<RoomLoaded>,
) {
    if started.read().count() > 0 {
        room.wanted = recorder.mode == CaptureMode::ByPose && recorder.use_room;
    }
    let scanning = recorder
        .active
        .as_ref()
        .is_some_and(|active| active.mode == CaptureMode::ByPose && active.stage == Stage::Scan);
    if !room.wanted {
        return;
    }
    if !scanning {
        room.wanted = false;
        return;
    }
    match room.status {
        RoomStatus::Found(_) => {}
        RoomStatus::Missing(_) => {
            room.wanted = false;
            return;
        }
        RoomStatus::Unchecked | RoomStatus::Checking => return,
    }
    let (Some(instance), Some(session), Some(base), Some(view)) = (instance, session, base, view)
    else {
        return;
    };
    let Some(head) = head_transform(&instance, &base, &view) else {
        return;
    };
    let Ok(now) = instance.now() else {
        return;
    };
    room.wanted = false;
    let exts = instance.exts();
    let (Some(entity), Some(scene)) = (exts.fb_spatial_entity, exts.fb_scene) else {
        return;
    };

    let (mut planes, mut boxes) = (Vec::new(), Vec::new());
    for &(_, space) in &room.spaces {
        let has = |component| unsafe { component_enabled(&entity, space, component) };
        if !has(sys::SpaceComponentTypeFB::LOCATABLE) {
            continue;
        }
        let labels = if has(sys::SpaceComponentTypeFB::SEMANTIC_LABELS) {
            unsafe { semantic_labels(&scene, session.as_raw(), space) }.unwrap_or_default()
        } else {
            String::new()
        };
        if SKIPPED_LABELS.iter().any(|label| labels.contains(label)) {
            continue;
        }
        let Some(pose) = locate(&instance, space.into_raw(), base.as_raw(), now) else {
            continue;
        };
        if has(sys::SpaceComponentTypeFB::BOUNDED_3D) {
            let mut rect: sys::Rect3DfFB = unsafe { std::mem::zeroed() };
            let result =
                unsafe { (scene.get_space_bounding_box3_d)(session.as_raw(), space, &mut rect) };
            if result.into_raw() >= 0 {
                let room_box = RoomBox {
                    pose,
                    offset: [rect.offset.x, rect.offset.y, rect.offset.z],
                    extent: [rect.extent.width, rect.extent.height, rect.extent.depth],
                    open_below: OPEN_BELOW.iter().any(|label| labels.contains(label)),
                };
                info!(
                    "room: box '{labels}' {:.2} x {:.2} x {:.2} m, open below: {}",
                    room_box.extent[0], room_box.extent[1], room_box.extent[2], room_box.open_below
                );
                boxes.push(room_box);
            }
        } else if has(sys::SpaceComponentTypeFB::BOUNDED_2D) {
            let mut rect: sys::Rect2Df = unsafe { std::mem::zeroed() };
            let result =
                unsafe { (scene.get_space_bounding_box2_d)(session.as_raw(), space, &mut rect) };
            if result.into_raw() >= 0 {
                planes.push(RoomPlane {
                    pose,
                    offset: [rect.offset.x, rect.offset.y],
                    extent: [rect.extent.width, rect.extent.height],
                });
            }
        }
    }
    let started_at = Instant::now();
    let viewer = head.translation.to_array();
    let points = room_points(&planes, &boxes, viewer, &room_settings());
    info!(
        "room: {} planes, {} boxes -> {} points ({} hidden) in {:.0} ms",
        planes.len(),
        boxes.len(),
        points.len(),
        points.iter().filter(|point| point.hidden).count(),
        started_at.elapsed().as_secs_f32() * 1000.0
    );
    if cfg!(debug_assertions)
        && let Some(active) = &mut recorder.active
    {
        let (layout_path, points_path) =
            (active.dir.join(ROOM_LAYOUT), active.dir.join(ROOM_POINTS));
        active.write_file(
            layout_path,
            layout_ply(&planes, &boxes, viewer).into_bytes(),
        );
        active.write_file(points_path, points_ply(&points).into_bytes());
    }
    if points.is_empty() {
        room.missing(MissingReason::NoSurfaces);
        return;
    }
    loaded.write(RoomLoaded(points));
}

/// 空间实体是否支持某个组件。
unsafe fn supports(
    fp: &raw::SpatialEntityFB,
    space: sys::Space,
    component: sys::SpaceComponentTypeFB,
) -> bool {
    unsafe {
        let mut count = 0;
        if (fp.enumerate_space_supported_components)(space, 0, &mut count, ptr::null_mut())
            .into_raw()
            < 0
        {
            return false;
        }
        let mut components = vec![sys::SpaceComponentTypeFB::LOCATABLE; count as usize];
        let result = (fp.enumerate_space_supported_components)(
            space,
            count,
            &mut count,
            components.as_mut_ptr(),
        );
        result.into_raw() >= 0 && components[..count as usize].contains(&component)
    }
}

unsafe fn component_enabled(
    fp: &raw::SpatialEntityFB,
    space: sys::Space,
    component: sys::SpaceComponentTypeFB,
) -> bool {
    let mut status = sys::SpaceComponentStatusFB {
        ty: sys::SpaceComponentStatusFB::TYPE,
        next: ptr::null_mut(),
        enabled: sys::FALSE,
        change_pending: sys::FALSE,
    };
    let result = unsafe { (fp.get_space_component_status)(space, component, &mut status) };
    result.into_raw() >= 0 && status.enabled != sys::FALSE
}

/// 空间实体的语义标签，多个标签用逗号隔开。
unsafe fn semantic_labels(
    fp: &raw::SceneFB,
    session: sys::Session,
    space: sys::Space,
) -> Result<String, String> {
    unsafe {
        let mut labels = sys::SemanticLabelsFB {
            ty: sys::SemanticLabelsFB::TYPE,
            next: ptr::null(),
            buffer_capacity_input: 0,
            buffer_count_output: 0,
            buffer: ptr::null_mut(),
        };
        check(
            (fp.get_space_semantic_labels)(session, space, &mut labels),
            "xrGetSpaceSemanticLabelsFB (count)",
        )?;
        let mut buffer = vec![0u8; labels.buffer_count_output as usize];
        labels.buffer_capacity_input = buffer.len() as u32;
        labels.buffer = buffer.as_mut_ptr().cast();
        check(
            (fp.get_space_semantic_labels)(session, space, &mut labels),
            "xrGetSpaceSemanticLabelsFB",
        )?;
        buffer.truncate(labels.buffer_count_output as usize);
        Ok(String::from_utf8_lossy(&buffer)
            .trim_end_matches('\0')
            .to_owned())
    }
}
