use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use bevy::{
    asset::RenderAssetUsages,
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
};
use bevy_mod_openxr::openxr_session_running;
use jni::objects::JValue;

use crate::{
    android::{CAMERA_PERMISSION, call_helper, has_permission, helper_class, with_activity},
    controls::ProbeCommand,
    device::StartedAt,
    report::{Report, Section, Status},
};

const CAMERA_CLASS: &str = "com.example.opencapture.probe.ProbeCamera";
/// 等待相机权限的最长时间，超时后照样尝试。
const PERMISSION_WAIT: Duration = Duration::from_secs(10);
/// 每个相机等第一帧的最长时间（毫秒）。
const GRAB_TIMEOUT_MS: i64 = 5000;
/// Java 端生成的缩略图宽度。
const THUMB_WIDTH: u32 = 240;

pub struct AndroidCameraPlugin;

impl Plugin for AndroidCameraPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AndroidCameraState>().add_systems(
            Update,
            (start, collect).chain().run_if(openxr_session_running),
        );
    }
}

/// 一个相机的检测结果。
struct CameraResult {
    id: String,
    description: String,
    thumbnail: Option<Thumbnail>,
    info: String,
    error: String,
}

#[derive(Default)]
enum Progress {
    #[default]
    Idle,
    Running,
    Done(Result<Vec<CameraResult>, String>),
}

/// Android 相机检测的进度；检测完成前 PICO 相机检测不会开始，以免争用相机。
#[derive(Resource, Default)]
pub struct AndroidCameraState {
    progress: Arc<Mutex<Progress>>,
    pub finished: bool,
}

/// 相机权限到位（或等待超时）后在后台线程里开始检测；按左 Y 时重新检测。
fn start(
    mut state: ResMut<AndroidCameraState>,
    started: Res<StartedAt>,
    mut commands: MessageReader<ProbeCommand>,
    mut report: ResMut<Report>,
    mut launched: Local<bool>,
) {
    let retry = commands.read().any(|c| *c == ProbeCommand::RetryCameras);
    if *launched && !retry {
        return;
    }
    if !*launched && !has_permission(CAMERA_PERMISSION) && started.0.elapsed() < PERMISSION_WAIT {
        report.set(
            Section::AndroidCamera,
            "状态",
            Status::Pending,
            "等待相机权限",
        );
        return;
    }
    {
        let mut progress = state.progress.lock().unwrap();
        if matches!(*progress, Progress::Running) {
            return;
        }
        *progress = Progress::Running;
    }
    *launched = true;
    state.finished = false;
    report.set(Section::AndroidCamera, "状态", Status::Pending, "检测中");
    let progress = state.progress.clone();
    std::thread::spawn(move || {
        let result = probe_all();
        *progress.lock().unwrap() = Progress::Done(result);
    });
}

/// 检测完成后把结果写进报告，并生成缩略图。
fn collect(
    mut state: ResMut<AndroidCameraState>,
    mut report: ResMut<Report>,
    mut images: ResMut<Assets<Image>>,
) {
    let result = {
        let mut progress = state.progress.lock().unwrap();
        if !matches!(*progress, Progress::Done(_)) {
            return;
        }
        match std::mem::take(&mut *progress) {
            Progress::Done(result) => result,
            _ => unreachable!(),
        }
    };
    state.finished = true;
    let s = Section::AndroidCamera;
    let cameras = match result {
        Ok(cameras) => cameras,
        Err(err) => {
            report.set(s, "状态", Status::Fail, err);
            return;
        }
    };
    let ok = cameras.iter().filter(|c| c.thumbnail.is_some()).count();
    report.set(
        s,
        "状态",
        if ok > 0 { Status::Ok } else { Status::Fail },
        format!("{} 个相机，{ok} 个取到画面", cameras.len()),
    );
    for camera in cameras {
        let facing = camera
            .description
            .lines()
            .find_map(|l| l.strip_prefix("facing="))
            .unwrap_or("?")
            .to_string();
        let key = format!("相机 {}", camera.id);
        match camera.thumbnail {
            Some((pixels, width, height)) => {
                report.set(s, &key, Status::Ok, format!("{facing} · {}", camera.info));
                let image = Image::new(
                    Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    TextureDimension::D2,
                    pixels,
                    TextureFormat::Rgba8UnormSrgb,
                    RenderAssetUsages::default(),
                );
                report.thumbnail(format!("Android 相机 {}", camera.id), images.add(image));
            }
            None => report.set(
                s,
                &key,
                Status::Fail,
                format!("{facing} · {}", camera.error),
            ),
        }
        report.block(
            format!("Android 相机 {} 特性", camera.id),
            camera.description,
        );
    }
}

/// 列出所有相机，逐个读取特性并尝试取一帧。
fn probe_all() -> Result<Vec<CameraResult>, String> {
    let ids = call_helper(CAMERA_CLASS, "ids", &[]).map_err(|e| e.to_string())?;
    if let Some(err) = ids.strip_prefix("error:") {
        return Err(err.trim().to_string());
    }
    let mut out = Vec::new();
    for id in ids.split(',').filter(|id| !id.is_empty()) {
        let description =
            call_helper(CAMERA_CLASS, "describe", &[id]).unwrap_or_else(|e| e.to_string());
        let (thumbnail, info, error) = match grab(id) {
            Ok(result) => result,
            Err(err) => (None, String::new(), err.to_string()),
        };
        out.push(CameraResult {
            id: id.to_string(),
            description,
            thumbnail,
            info,
            error,
        });
    }
    Ok(out)
}

/// 亮度缩略图的 RGBA 像素和宽高。
type Thumbnail = (Vec<u8>, u32, u32);

/// 调用 ProbeCamera.grab，返回缩略图、画面信息和错误原因。
fn grab(id: &str) -> jni::errors::Result<(Option<Thumbnail>, String, String)> {
    with_activity(|env, activity| {
        let class = helper_class(env, activity, CAMERA_CLASS)?;
        let pixels = env
            .call_static_method(
                class,
                "grab",
                "(Landroid/content/Context;Ljava/lang/String;J)[B",
                &[
                    JValue::Object(activity),
                    JValue::Object(env.new_string(id)?.into()),
                    JValue::Long(GRAB_TIMEOUT_MS),
                ],
            )?
            .l()?;
        let text = |method: &str| -> jni::errors::Result<String> {
            let value = env
                .call_static_method(class, method, "()Ljava/lang/String;", &[])?
                .l()?;
            Ok(env.get_string(value.into())?.into())
        };
        let info = text("lastInfo")?;
        let error = text("lastError")?;
        if pixels.is_null() {
            return Ok((None, info, error));
        }
        let bytes = env.convert_byte_array(pixels.into_raw())?;
        let height = (bytes.len() / 4 / THUMB_WIDTH as usize) as u32;
        Ok((Some((bytes, THUMB_WIDTH, height)), info, error))
    })
}
