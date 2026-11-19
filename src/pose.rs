use std::ptr;

use bevy::prelude::*;
use bevy_mod_openxr::{openxr_session_running, resources::OxrInstance, session::OxrSession};
use bevy_mod_xr::{
    session::{XrPreDestroySession, XrSessionCreated},
    spaces::{XrPrimaryReferenceSpace, XrReferenceSpace},
};
use capture_format::records::{Pose, pose_row};
use openxr::sys::{self, Handle as _};

use crate::{
    input::ControllerSpaces,
    recorder::{CsvFile, Recorder, RecorderSystems},
    time::xr_to_mono_ns,
};

pub struct PosePlugin;

impl Plugin for PosePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(XrSessionCreated, create_view_space)
            .add_systems(XrPreDestroySession, remove_view_space)
            .add_systems(
                Update,
                log_poses
                    .after(RecorderSystems)
                    .run_if(openxr_session_running),
            );
    }
}

/// 头显位姿所用的 VIEW 参考空间。
#[derive(Resource)]
pub struct ViewSpace(XrReferenceSpace);

fn create_view_space(session: Res<OxrSession>, mut commands: Commands) {
    match session.create_reference_space(openxr::ReferenceSpaceType::VIEW, Isometry3d::IDENTITY) {
        Ok(space) => commands.insert_resource(ViewSpace(space)),
        Err(err) => error!("pose: creating view space failed: {err}"),
    }
}

fn remove_view_space(mut commands: Commands) {
    commands.remove_resource::<ViewSpace>();
}

/// 头显当前在参考空间里的位置和朝向。
pub fn head_transform(
    instance: &OxrInstance,
    base: &XrPrimaryReferenceSpace,
    view: &ViewSpace,
) -> Option<Transform> {
    let pose = locate(
        instance,
        view.0.as_raw(),
        base.as_raw(),
        instance.now().ok()?,
    )?;
    Some(Transform {
        translation: Vec3::from_array(pose.position),
        rotation: Quat::from_array(pose.rotation),
        scale: Vec3::ONE,
    })
}

/// 在指定时刻定位一个空间相对基准空间的位姿，位置或朝向无效时返回 None。
pub fn locate(instance: &OxrInstance, space: u64, base: u64, time: openxr::Time) -> Option<Pose> {
    let mut location: sys::SpaceLocation = unsafe { std::mem::zeroed() };
    location.ty = sys::SpaceLocation::TYPE;
    location.next = ptr::null_mut();
    let result = unsafe {
        (instance.fp().locate_space)(
            sys::Space::from_raw(space),
            sys::Space::from_raw(base),
            time,
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
    Some(Pose {
        position: [p.x, p.y, p.z],
        rotation: [q.x, q.y, q.z, q.w],
    })
}

/// 录制期间每帧记录头显和左右手柄在当前时刻的位姿。
fn log_poses(
    mut recorder: ResMut<Recorder>,
    instance: Res<OxrInstance>,
    base: Option<Res<XrPrimaryReferenceSpace>>,
    view: Option<Res<ViewSpace>>,
    controllers: Option<Res<ControllerSpaces>>,
) {
    let controllers = controllers.filter(|_| recorder.log_controllers);
    let (Some(active), Some(base)) = (&mut recorder.active, base) else {
        return;
    };
    let Ok(now) = instance.now() else {
        return;
    };
    let Some(mono_ns) = xr_to_mono_ns(&instance, now) else {
        return;
    };
    let unix_ms = active.time.unix_ms_f64(mono_ns);
    let seconds = mono_ns as f64 / 1e9;

    let spaces = [
        (CsvFile::Hmd, view.map(|v| v.0.as_raw())),
        (
            CsvFile::LeftController,
            controllers.as_ref().map(|c| c.left.as_raw()),
        ),
        (
            CsvFile::RightController,
            controllers.as_ref().map(|c| c.right.as_raw()),
        ),
    ];
    for (file, space) in spaces {
        if let Some(pose) = space.and_then(|s| locate(&instance, s, base.as_raw(), now)) {
            active.write_line(file, pose_row(unix_ms, seconds, &pose));
        }
    }
}
