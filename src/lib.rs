#![cfg(target_os = "android")]

use bevy::{prelude::*, render::view::NoIndirectDrawing};
use bevy_mod_openxr::{
    add_xr_plugins, exts::OxrExtensions, init::OxrInitPlugin, openxr_session_running,
    resources::OxrSessionConfig,
};
use bevy_mod_xr::camera::XrCamera;
use openxr::EnvironmentBlendMode;

rust_i18n::i18n!("locales", fallback = "en");

mod android;
mod camera;
mod depth;
mod input;
mod pose;
mod preview;
mod recorder;
mod room;
mod sessions;
mod time;
mod tuning;
mod ui;

#[bevy_main]
fn main() {
    let mut exts = OxrExtensions::default();
    exts.raw_mut().meta_environment_depth = true;
    exts.raw_mut().khr_convert_timespec_time = true;
    exts.raw_mut().fb_spatial_entity = true;
    exts.raw_mut().fb_spatial_entity_query = true;
    exts.raw_mut().fb_spatial_entity_storage = true;
    exts.raw_mut().fb_spatial_entity_container = true;
    exts.raw_mut().fb_scene = true;
    exts.raw_mut().fb_scene_capture = true;

    App::new()
        .insert_resource(OxrSessionConfig {
            blend_mode_preference: vec![
                EnvironmentBlendMode::ALPHA_BLEND,
                EnvironmentBlendMode::OPAQUE,
            ],
            ..default()
        })
        .insert_resource(ClearColor(Color::NONE))
        .add_plugins(add_xr_plugins(DefaultPlugins).set(OxrInitPlugin { exts, ..default() }))
        // XR 相机逐个网格直接绘制，不使用间接绘制。
        .register_required_components::<XrCamera, NoIndirectDrawing>()
        .add_plugins((
            time::TimePlugin,
            input::InputPlugin,
            recorder::RecorderPlugin,
            pose::PosePlugin,
            depth::DepthPlugin,
            camera::CameraPlugin,
            preview::PreviewPlugin,
            room::RoomPlugin,
            sessions::SessionsPlugin,
            ui::UiPlugin,
        ))
        .add_systems(Update, request_permissions.run_if(openxr_session_running))
        .run();
}

/// 会话开始后一次性申请所有尚未授予的权限。
fn request_permissions(mut done: Local<bool>) {
    if *done {
        return;
    }
    *done = true;
    let missing: Vec<&str> = android::ALL_PERMISSIONS
        .into_iter()
        .filter(|p| !android::has_permission(p))
        .collect();
    if missing.is_empty() {
        return;
    }
    info!("requesting permissions: {missing:?}");
    if let Err(err) = android::request_permissions(&missing) {
        error!("permission request failed: {err}");
    }
}
