#![cfg(target_os = "android")]
//! 检查头显能否支持拍摄程序：设备、OpenXR 运行时、6DoF、手柄、相机和空间数据，结果显示在眼前的面板上。

use bevy::{prelude::*, render::view::NoIndirectDrawing};
use bevy_mod_openxr::{
    add_xr_plugins, exts::OxrExtensions, init::OxrInitPlugin, resources::OxrSessionConfig,
};
use bevy_mod_xr::camera::XrCamera;
use openxr::EnvironmentBlendMode;

mod android;
mod android_camera;
mod controls;
mod device;
mod panel;
mod pico_camera;
mod report;
mod runtime;
mod spatial;
mod xr;

#[bevy_main]
fn main() {
    let mut exts = OxrExtensions::default();
    let raw = exts.raw_mut();
    raw.khr_convert_timespec_time = true;
    raw.ext_future = true;
    raw.bd_controller_interaction = true;
    raw.bd_ultra_controller_interaction = true;
    raw.bd_spatial_sensing = true;
    raw.bd_spatial_scene = true;
    raw.bd_spatial_mesh = true;
    raw.bd_spatial_plane = true;
    raw.other.push(b"XR_PICO_camera_image\0".to_vec());

    App::new()
        .insert_resource(OxrSessionConfig {
            blend_mode_preference: vec![
                EnvironmentBlendMode::ALPHA_BLEND,
                EnvironmentBlendMode::OPAQUE,
            ],
            ..default()
        })
        .insert_resource(ClearColor(Color::NONE))
        .init_resource::<report::Report>()
        .add_plugins(add_xr_plugins(DefaultPlugins).set(OxrInitPlugin { exts, ..default() }))
        // XR 相机逐个网格直接绘制，不做多重采样抗锯齿。
        .register_required_components::<XrCamera, NoIndirectDrawing>()
        .register_required_components_with::<XrCamera, Msaa>(|| Msaa::Off)
        .add_plugins((
            controls::ControlsPlugin,
            device::DevicePlugin,
            runtime::RuntimePlugin,
            android_camera::AndroidCameraPlugin,
            pico_camera::PicoCameraPlugin,
            spatial::SpatialPlugin,
            panel::PanelPlugin,
        ))
        .run();
}
