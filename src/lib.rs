#![cfg(target_os = "android")]

use bevy::{prelude::*, render::view::NoIndirectDrawing};
use bevy_mod_openxr::{
    add_xr_plugins, exts::OxrExtensions, init::OxrInitPlugin, openxr_session_running,
    resources::OxrSessionConfig,
};
use bevy_mod_xr::{
    camera::XrCamera,
    session::{
        XrBeginSessionMessage, XrCreateSessionMessage, XrDestroySessionMessage,
        XrEndSessionMessage, XrFirst, XrHandleEvents, XrSessionPlugin, XrState, XrStateChanged,
    },
};
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
            // 每只眼睛约为推荐分辨率（1680x1760）的 1.1 和 1.2 倍，取最后一个不超过运行时上限的。
            resolutions: Some(vec![UVec2::new(1848, 1936), UVec2::new(2016, 2112)]),
            ..default()
        })
        .insert_resource(ClearColor(Color::NONE))
        .add_plugins(
            add_xr_plugins(DefaultPlugins)
                .set(OxrInitPlugin { exts, ..default() })
                .set(XrSessionPlugin { auto_handle: false }),
        )
        // XR 相机逐个网格直接绘制，不使用间接绘制。
        .register_required_components::<XrCamera, NoIndirectDrawing>()
        // XR 相机不做多重采样抗锯齿。
        .register_required_components_with::<XrCamera, Msaa>(|| Msaa::Off)
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
        .add_systems(
            XrFirst,
            handle_session_state.in_set(XrHandleEvents::ExitEvents),
        )
        .run();
}

/// 按运行时的状态变化创建、开始、结束和销毁会话；在轮询之后、会话状态处理之前运行，
/// 状态变化的当帧就生效。
fn handle_session_state(
    mut state_changed: MessageReader<XrStateChanged>,
    mut create_session: MessageWriter<XrCreateSessionMessage>,
    mut begin_session: MessageWriter<XrBeginSessionMessage>,
    mut end_session: MessageWriter<XrEndSessionMessage>,
    mut destroy_session: MessageWriter<XrDestroySessionMessage>,
    mut no_auto_restart: Local<bool>,
) {
    for XrStateChanged(state) in state_changed.read() {
        match state {
            XrState::Available if !*no_auto_restart => {
                create_session.write_default();
            }
            XrState::Ready => {
                begin_session.write_default();
            }
            XrState::Stopping => {
                end_session.write_default();
            }
            XrState::Exiting { should_restart } => {
                *no_auto_restart = !should_restart;
                destroy_session.write_default();
            }
            _ => {}
        }
    }
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
