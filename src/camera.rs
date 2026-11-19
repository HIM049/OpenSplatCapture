use std::fs;

use bevy::prelude::*;
use bevy_mod_openxr::openxr_session_running;
use capture_format::{
    Side, Storage,
    records::{CameraCharacteristics, camera_characteristics_json},
};
use jni::{
    JNIEnv,
    errors::Result as JniResult,
    objects::{GlobalRef, JObject, JValue},
};

use crate::{
    android::{
        CAMERA_PERMISSION, HEADSET_CAMERA_PERMISSION, call_string, has_permission, helper_class,
        with_activity,
    },
    depth::DepthSystems,
    recorder::{CaptureCameraAt, RecordingStarted, RecordingStopped, SessionPaused},
};

const CAPTURE_CLASS: &str = "com.example.opencapture.CameraCapture";
/// 压缩存储时相机帧的 JPEG 质量。
const JPEG_QUALITY: i32 = 95;

pub struct CameraPlugin;

impl Plugin for CameraPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Cameras>().add_systems(
            Update,
            (
                close_on_pause,
                open_cameras.run_if(openxr_session_running),
                start_recording,
                capture_frames,
                stop_recording,
            )
                .chain()
                .after(DepthSystems),
        );
    }
}

/// 左右透视相机的状态。
#[derive(Resource, Default)]
pub struct Cameras {
    failed: bool,
    found: Vec<Camera>,
    open: bool,
}

impl Cameras {
    /// 相机是否已打开并可以取帧。
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// 一侧相机水平和垂直半视场角的正切。
    pub fn half_fov_tangents(&self, side: Side) -> Option<[f32; 2]> {
        let camera = self.found.iter().find(|camera| camera.side == side)?;
        let c = &camera.characteristics;
        let (fx, fy) = (c.intrinsics[0], c.intrinsics[1]);
        (fx > 0.0 && fy > 0.0).then(|| {
            [
                (c.pixel_array[0] as f64 / 2.0 / fx) as f32,
                (c.pixel_array[1] as f64 / 2.0 / fy) as f32,
            ]
        })
    }
}

struct Camera {
    side: Side,
    capture: GlobalRef,
    characteristics: CameraCharacteristics,
}

/// 相机权限就绪后查找左右透视相机并打开；暂停后恢复运行时重新打开。
fn open_cameras(mut cameras: ResMut<Cameras>) {
    if cameras.failed || cameras.open {
        return;
    }
    if !has_permission(CAMERA_PERMISSION) || !has_permission(HEADSET_CAMERA_PERMISSION) {
        return;
    }
    let result = with_activity(|env, activity| {
        if cameras.found.is_empty() {
            cameras.found = find_all(env, activity)?;
        }
        for camera in &cameras.found {
            let id = env.new_string(&camera.characteristics.camera_id)?;
            env.call_method(
                camera.capture.as_obj(),
                "open",
                "(Ljava/lang/String;)V",
                &[JValue::Object(id.into())],
            )?;
        }
        Ok(())
    });
    match result {
        Ok(()) if !cameras.found.is_empty() => {
            info!("camera: opened {} cameras", cameras.found.len());
            cameras.open = true;
        }
        Ok(()) => {
            error!("camera: no passthrough camera found");
            cameras.failed = true;
        }
        Err(err) => {
            error!("camera: open failed: {err}");
            cameras.failed = true;
        }
    }
}

fn find_all(env: &JNIEnv, activity: JObject) -> JniResult<Vec<Camera>> {
    let class = helper_class(env, activity, CAPTURE_CLASS)?;
    let mut cameras = Vec::new();
    for side in Side::BOTH {
        let capture = env.new_object(
            class,
            "(Landroid/content/Context;)V",
            &[JValue::Object(activity)],
        )?;
        let id = env
            .call_method(
                capture,
                "findCamera",
                "(I)Ljava/lang/String;",
                &[JValue::Int(side.index() as i32)],
            )?
            .l()?;
        let camera_id: String = env.get_string(id.into())?.into();
        if camera_id.is_empty() {
            warn!("camera: no {} passthrough camera", side.prefix());
            continue;
        }
        let values = env
            .call_method(
                capture,
                "characteristics",
                "(Ljava/lang/String;)[D",
                &[JValue::Object(id)],
            )?
            .l()?;
        let mut v = [0f64; 18];
        env.get_double_array_region(values.into_raw(), 0, &mut v)?;
        let characteristics = CameraCharacteristics {
            camera_id: camera_id.clone(),
            intrinsics: [v[0], v[1], v[2], v[3], v[4]],
            pose_translation: [v[5], v[6], v[7]],
            pose_rotation: [v[8], v[9], v[10], v[11]],
            active_array: [v[12] as i32, v[13] as i32, v[14] as i32, v[15] as i32],
            pixel_array: [v[16] as i32, v[17] as i32],
        };
        info!(
            "camera: found {} camera id {camera_id}, {}x{}",
            side.prefix(),
            characteristics.pixel_array[0],
            characteristics.pixel_array[1]
        );
        cameras.push(Camera {
            side,
            capture: env.new_global_ref(capture)?,
            characteristics,
        });
    }
    Ok(cameras)
}

/// 会话暂停时关闭相机。
fn close_on_pause(mut cameras: ResMut<Cameras>, mut paused: MessageReader<SessionPaused>) {
    if paused.read().count() == 0 || !cameras.open {
        return;
    }
    cameras.open = false;
    let result = with_activity(|env, _| {
        for camera in &cameras.found {
            env.call_method(camera.capture.as_obj(), "close", "()V", &[])?;
        }
        Ok(())
    });
    match result {
        Ok(()) => info!("camera: closed for pause"),
        Err(err) => error!("camera: close failed: {err}"),
    }
}

/// 录制开始时写出标定参数，并告诉 Java 侧图像的保存位置和时间基准。
fn start_recording(cameras: Res<Cameras>, mut started: MessageReader<RecordingStarted>) {
    let Some(recording) = started.read().last() else {
        return;
    };
    if !cameras.open {
        warn!("camera: recording started without any open camera");
        return;
    }
    let result = with_activity(|env, _| {
        for camera in &cameras.found {
            let side = camera.side;
            let json = camera_characteristics_json(&camera.characteristics);
            if let Err(err) = fs::write(side.camera_characteristics(&recording.dir), json) {
                error!("camera: writing characteristics failed: {err}");
            }
            let dir = env.new_string(side.camera_dir(&recording.dir).to_string_lossy())?;
            let format =
                env.new_string(side.camera_image_format(&recording.dir).to_string_lossy())?;
            env.call_method(
                camera.capture.as_obj(),
                "startRecording",
                "(Ljava/lang/String;Ljava/lang/String;JJI)V",
                &[
                    JValue::Object(dir.into()),
                    JValue::Object(format.into()),
                    JValue::Long(recording.time.mono_ns),
                    JValue::Long(recording.time.unix_ms),
                    JValue::Int(match recording.storage {
                        Storage::Compressed => JPEG_QUALITY,
                        Storage::Raw => 0,
                    }),
                ],
            )?;
        }
        Ok(())
    });
    if let Err(err) = result {
        error!("camera: startRecording failed: {err}");
    }
}

/// 让左右相机各保存时间戳最接近请求时刻的一帧，并上报 Java 侧的错误。
fn capture_frames(cameras: Res<Cameras>, mut requests: MessageReader<CaptureCameraAt>) {
    let targets: Vec<i64> = requests.read().map(|request| request.0).collect();
    if targets.is_empty() || !cameras.open {
        return;
    }
    let result = with_activity(|env, _| {
        for camera in &cameras.found {
            let capture = camera.capture.as_obj();
            for target in &targets {
                env.call_method(capture, "captureAt", "(J)V", &[JValue::Long(*target)])?;
            }
            let error = call_string(env, capture, "takeError")?;
            if !error.is_empty() {
                error!("camera {}: {error}", camera.side.prefix());
            }
        }
        Ok(())
    });
    if let Err(err) = result {
        error!("camera: captureAt failed: {err}");
    }
}

fn stop_recording(cameras: Res<Cameras>, mut stopped: MessageReader<RecordingStopped>) {
    if stopped.read().count() == 0 || cameras.found.is_empty() {
        return;
    }
    let result = with_activity(|env, _| {
        for camera in &cameras.found {
            let capture = camera.capture.as_obj();
            let saved = env.call_method(capture, "savedCount", "()I", &[])?.i()?;
            let exposure = env.call_method(capture, "exposureNs", "()J", &[])?.j()?;
            env.call_method(capture, "stopRecording", "()V", &[])?;
            info!(
                "camera {}: saved {saved} frames, last exposure {:.2} ms",
                camera.side.prefix(),
                exposure as f64 / 1e6
            );
        }
        Ok(())
    });
    if let Err(err) = result {
        error!("camera: stopRecording failed: {err}");
    }
}
