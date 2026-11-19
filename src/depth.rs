use std::{
    ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
    },
};

use ash::vk::{self, Handle as _};
use bevy::{
    prelude::*,
    render::{
        Render, RenderApp,
        renderer::{RenderDevice, RenderQueue},
    },
};
use bevy_mod_openxr::{
    init::should_run_frame_loop,
    openxr_session_running,
    render::begin_frame,
    resources::{OxrFrameState, OxrInstance},
    session::OxrSession,
};
use bevy_mod_xr::{
    session::{XrPreDestroySession, XrRenderSystems},
    spaces::XrPrimaryReferenceSpace,
};
use capture_format::{
    Side, Storage,
    convert::{depth_layer_to_file_bytes, depth_layer_to_u16_bytes},
    geometry::DepthView,
    records::{DepthDescriptor, Pose, depth_descriptor_row},
};
use openxr::{
    raw::EnvironmentDepthMETA,
    sys::{self, Handle as _},
};
use wgpu::{Extent3d, TextureDimension, TextureFormat};

use crate::{
    android::{SCENE_PERMISSION, has_permission},
    recorder::{
        CaptureCameraAt, CaptureTick, CsvFile, DepthFrameCaptured, Recorder, RecorderSystems,
        ScanTick, SessionPaused,
    },
    time::{mono_now_ns, xr_to_mono_ns},
};

const MAP_PENDING: u8 = 0;
const MAP_OK: u8 = 1;
const MAP_FAILED: u8 = 2;

pub struct DepthPlugin;

impl Plugin for DepthPlugin {
    fn build(&self, app: &mut App) {
        let shared = DepthShared::default();
        app.insert_resource(shared.clone())
            .add_systems(
                Update,
                (
                    destroy_on_pause,
                    (ensure_provider, request_capture, collect_readback)
                        .chain()
                        .run_if(openxr_session_running),
                )
                    .chain()
                    .in_set(DepthSystems)
                    .after(RecorderSystems),
            )
            .add_systems(XrPreDestroySession, destroy_provider);
        app.sub_app_mut(RenderApp)
            .insert_resource(shared)
            .add_systems(
                Render,
                acquire_and_copy
                    .in_set(XrRenderSystems::PreRender)
                    .after(begin_frame)
                    .run_if(should_run_frame_loop),
            );
    }
}

/// 深度采集所在的系统集，相机和预览排在它之后。
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct DepthSystems;

/// 主世界与渲染世界共享的状态。
#[derive(Resource, Clone, Default)]
pub struct DepthShared(Arc<Mutex<Shared>>);

impl DepthShared {
    /// 深度 provider 是否已创建并在运行。
    pub fn is_ready(&self) -> bool {
        self.0.lock().is_ok_and(|shared| shared.provider.is_some())
    }
}

#[derive(Default)]
struct Shared {
    failed: bool,
    provider: Option<Provider>,
    /// 待处理的深度采集请求，值表示这一帧是否要存盘。
    requested: Option<bool>,
    pending: Option<Pending>,
}

struct Provider {
    fp: EnvironmentDepthMETA,
    provider: sys::EnvironmentDepthProviderMETA,
    swapchain: sys::EnvironmentDepthSwapchainMETA,
    size: UVec2,
    textures: Vec<wgpu::Texture>,
}

/// 一次 acquire 返回的元数据。
struct FrameMeta {
    /// 深度图的采集时刻；运行时不提供时退回到显示时刻。
    time: openxr::Time,
    near_z: f32,
    far_z: f32,
    fovs: [sys::Fovf; 2],
    poses: [sys::Posef; 2],
}

struct Pending {
    buffer: wgpu::Buffer,
    status: Arc<AtomicU8>,
    bytes_per_row: u32,
    size: UVec2,
    meta: FrameMeta,
    save: bool,
}

fn check(result: sys::Result, what: &str) -> Result<(), String> {
    if result.into_raw() >= 0 {
        Ok(())
    } else {
        Err(format!("{what} failed: {result:?}"))
    }
}

/// 权限就绪后创建深度 provider 和 swapchain。
fn ensure_provider(
    shared: Res<DepthShared>,
    instance: Res<OxrInstance>,
    session: Res<OxrSession>,
    device: Res<RenderDevice>,
) {
    let mut shared = shared.0.lock().unwrap();
    if shared.provider.is_some() || shared.failed {
        return;
    }
    let Some(fp) = instance.exts().meta_environment_depth else {
        error!("depth: XR_META_environment_depth is not enabled on this instance");
        shared.failed = true;
        return;
    };
    if !has_permission(SCENE_PERMISSION) {
        return;
    }
    match unsafe { create_provider(fp, &session, device.wgpu_device()) } {
        Ok(provider) => {
            info!(
                "depth: provider started, {}x{}, {} swapchain images",
                provider.size.x,
                provider.size.y,
                provider.textures.len()
            );
            shared.provider = Some(provider);
        }
        Err(err) => {
            error!("depth: {err}");
            shared.failed = true;
        }
    }
}

unsafe fn create_provider(
    fp: EnvironmentDepthMETA,
    session: &OxrSession,
    device: &wgpu::Device,
) -> Result<Provider, String> {
    unsafe {
        let mut provider = sys::EnvironmentDepthProviderMETA::NULL;
        let info = sys::EnvironmentDepthProviderCreateInfoMETA {
            ty: sys::EnvironmentDepthProviderCreateInfoMETA::TYPE,
            next: ptr::null(),
            create_flags: sys::EnvironmentDepthProviderCreateFlagsMETA::EMPTY,
        };
        check(
            (fp.create_environment_depth_provider)(session.as_raw(), &info, &mut provider),
            "xrCreateEnvironmentDepthProviderMETA",
        )?;

        let mut swapchain = sys::EnvironmentDepthSwapchainMETA::NULL;
        let info = sys::EnvironmentDepthSwapchainCreateInfoMETA {
            ty: sys::EnvironmentDepthSwapchainCreateInfoMETA::TYPE,
            next: ptr::null(),
            create_flags: sys::EnvironmentDepthSwapchainCreateFlagsMETA::EMPTY,
        };
        check(
            (fp.create_environment_depth_swapchain)(provider, &info, &mut swapchain),
            "xrCreateEnvironmentDepthSwapchainMETA",
        )?;

        let mut state = sys::EnvironmentDepthSwapchainStateMETA {
            ty: sys::EnvironmentDepthSwapchainStateMETA::TYPE,
            next: ptr::null_mut(),
            width: 0,
            height: 0,
        };
        check(
            (fp.get_environment_depth_swapchain_state)(swapchain, &mut state),
            "xrGetEnvironmentDepthSwapchainStateMETA",
        )?;
        let size = UVec2::new(state.width, state.height);

        let mut count = 0;
        check(
            (fp.enumerate_environment_depth_swapchain_images)(
                swapchain,
                0,
                &mut count,
                ptr::null_mut(),
            ),
            "xrEnumerateEnvironmentDepthSwapchainImagesMETA (count)",
        )?;
        let mut images = vec![
            sys::SwapchainImageVulkanKHR {
                ty: sys::SwapchainImageVulkanKHR::TYPE,
                next: ptr::null_mut(),
                image: 0,
            };
            count as usize
        ];
        check(
            (fp.enumerate_environment_depth_swapchain_images)(
                swapchain,
                count,
                &mut count,
                images.as_mut_ptr().cast(),
            ),
            "xrEnumerateEnvironmentDepthSwapchainImagesMETA",
        )?;

        check(
            (fp.start_environment_depth_provider)(provider),
            "xrStartEnvironmentDepthProviderMETA",
        )?;

        let textures = images
            .iter()
            .map(|image| import_depth_image(device, vk::Image::from_raw(image.image), size))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Provider {
            fp,
            provider,
            swapchain,
            size,
            textures,
        })
    }
}

/// 把运行时拥有的深度 VkImage 包成只读的 wgpu 纹理。
unsafe fn import_depth_image(
    device: &wgpu::Device,
    image: vk::Image,
    size: UVec2,
) -> Result<wgpu::Texture, String> {
    let extent = Extent3d {
        width: size.x,
        height: size.y,
        depth_or_array_layers: 2,
    };
    unsafe {
        let hal_texture = {
            let hal_device = device
                .as_hal::<wgpu_hal::vulkan::Api>()
                .ok_or("wgpu device is not a Vulkan device")?;
            hal_device.texture_from_raw(
                image,
                &wgpu_hal::TextureDescriptor {
                    label: Some("environment depth"),
                    size: extent,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: TextureDimension::D2,
                    format: TextureFormat::Depth16Unorm,
                    usage: wgpu::TextureUses::COPY_SRC,
                    memory_flags: wgpu_hal::MemoryFlags::empty(),
                    view_formats: vec![],
                },
                None,
                wgpu_hal::vulkan::TextureMemory::External,
            )
        };
        Ok(device.create_texture_from_hal::<wgpu_hal::vulkan::Api>(
            hal_texture,
            &wgpu::TextureDescriptor {
                label: Some("environment depth"),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: TextureDimension::D2,
                format: TextureFormat::Depth16Unorm,
                usage: wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            },
        ))
    }
}

/// 把拍照和扫描节拍转成给渲染世界的深度采集请求；拍照时深度不可用就直接让相机按当前时刻取帧。
fn request_capture(
    shared: Res<DepthShared>,
    mut ticks: MessageReader<CaptureTick>,
    mut scans: MessageReader<ScanTick>,
    mut camera: MessageWriter<CaptureCameraAt>,
) {
    let save = ticks.read().count() > 0;
    let scan = scans.read().count() > 0;
    if !save && !scan {
        return;
    }
    let mut shared = shared.0.lock().unwrap();
    if shared.provider.is_some() {
        shared.requested = Some(save || shared.requested == Some(true));
    } else if save {
        camera.write(CaptureCameraAt(mono_now_ns()));
    }
}

/// 在渲染世界的 xrBeginFrame 之后响应采集请求：acquire 一帧深度并发起到 CPU 的异步拷贝。
fn acquire_and_copy(
    shared: Res<DepthShared>,
    frame_state: Res<OxrFrameState>,
    space: Option<Res<XrPrimaryReferenceSpace>>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
) {
    let mut shared = shared.0.lock().unwrap();
    let shared = &mut *shared;
    let (Some(provider), Some(space)) = (&shared.provider, space) else {
        return;
    };
    let Some(save) = shared.requested else {
        return;
    };
    if shared.pending.is_some() {
        return;
    }

    let info = sys::EnvironmentDepthImageAcquireInfoMETA {
        ty: sys::EnvironmentDepthImageAcquireInfoMETA::TYPE,
        next: ptr::null(),
        space: sys::Space::from_raw(space.as_raw()),
        display_time: frame_state.predicted_display_time,
    };
    let mut timestamp = sys::EnvironmentDepthImageTimestampMETA {
        ty: sys::EnvironmentDepthImageTimestampMETA::TYPE,
        next: ptr::null(),
        capture_time: openxr::Time::from_nanos(0),
    };
    let mut image: sys::EnvironmentDepthImageMETA = unsafe { std::mem::zeroed() };
    image.ty = sys::EnvironmentDepthImageMETA::TYPE;
    image.next = (&raw mut timestamp).cast();
    for view in &mut image.views {
        view.ty = sys::EnvironmentDepthImageViewMETA::TYPE;
    }
    let result = unsafe {
        (provider.fp.acquire_environment_depth_image)(provider.provider, &info, &mut image)
    };
    if result != sys::Result::SUCCESS {
        return;
    }
    let Some(texture) = provider.textures.get(image.swapchain_index as usize) else {
        error!(
            "depth: swapchain index {} out of range",
            image.swapchain_index
        );
        return;
    };
    shared.requested = None;

    let size = provider.size;
    let bytes_per_row = (size.x * 2).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let device = device.wgpu_device();
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("environment depth readback"),
        size: (bytes_per_row * size.y * 2) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::DepthOnly,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(size.y),
            },
        },
        Extent3d {
            width: size.x,
            height: size.y,
            depth_or_array_layers: 2,
        },
    );
    queue.submit([encoder.finish()]);

    let status = Arc::new(AtomicU8::new(MAP_PENDING));
    let status_in_callback = status.clone();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let value = if result.is_ok() { MAP_OK } else { MAP_FAILED };
            status_in_callback.store(value, Ordering::Release);
        });

    let time = if timestamp.capture_time.as_nanos() > 0 {
        timestamp.capture_time
    } else {
        frame_state.predicted_display_time
    };
    shared.pending = Some(Pending {
        buffer,
        status,
        bytes_per_row,
        size,
        meta: FrameMeta {
            time,
            near_z: image.near_z,
            far_z: image.far_z,
            fovs: [image.views[0].fov, image.views[1].fov],
            poses: [image.views[0].pose, image.views[1].pose],
        },
        save,
    });
}

/// 拷贝完成后通知预览；要存盘的帧还把左右两层深度和各自的描述行交给录制器，并通知相机。
fn collect_readback(
    shared: Res<DepthShared>,
    device: Res<RenderDevice>,
    instance: Res<OxrInstance>,
    mut recorder: ResMut<Recorder>,
    mut camera: MessageWriter<CaptureCameraAt>,
    mut captured: MessageWriter<DepthFrameCaptured>,
) {
    let _ = device.wgpu_device().poll(wgpu::PollType::Poll);
    let pending = {
        let mut shared = shared.0.lock().unwrap();
        match &shared.pending {
            Some(p) if p.status.load(Ordering::Acquire) != MAP_PENDING => shared.pending.take(),
            _ => None,
        }
    };
    let Some(pending) = pending else {
        return;
    };
    if pending.status.load(Ordering::Acquire) == MAP_FAILED {
        error!("depth: readback buffer mapping failed");
        return;
    }
    let Some(active) = &mut recorder.active else {
        return;
    };
    let Pending {
        buffer,
        bytes_per_row,
        size,
        meta,
        save,
        ..
    } = pending;
    let Some(mono_ns) = xr_to_mono_ns(&instance, meta.time) else {
        error!("depth: cannot convert frame time");
        return;
    };
    let unix_ms = active.time.unix_ms(mono_ns);

    let layer_bytes = (bytes_per_row * size.y) as usize;
    let (row, width) = (bytes_per_row as usize, size.x as usize);
    let (layers, packed): (Vec<Vec<u8>>, Vec<Vec<u8>>) = {
        let mapped = buffer.slice(..).get_mapped_range();
        let layers = mapped.chunks_exact(layer_bytes);
        (
            layers
                .clone()
                .map(|layer| depth_layer_to_file_bytes(layer, row, width))
                .collect(),
            match active.storage {
                Storage::Compressed if save => layers
                    .map(|layer| depth_layer_to_u16_bytes(layer, row, width))
                    .collect(),
                _ => Vec::new(),
            },
        )
    };
    buffer.unmap();
    let mut packed = packed.into_iter();
    let Ok(layers) = <[Vec<u8>; 2]>::try_from(layers) else {
        error!("depth: unexpected readback size");
        return;
    };
    if save {
        camera.write(CaptureCameraAt(mono_ns));
    }

    let views = Side::BOTH.map(|side| {
        let eye = side.index();
        let fov = meta.fovs[eye];
        let pose = meta.poses[eye];
        DepthView {
            pose: Pose {
                position: [pose.position.x, pose.position.y, pose.position.z],
                rotation: [
                    pose.orientation.x,
                    pose.orientation.y,
                    pose.orientation.z,
                    pose.orientation.w,
                ],
            },
            fov_left: fov.angle_left,
            fov_right: fov.angle_right,
            fov_up: fov.angle_up,
            fov_down: fov.angle_down,
            near_z: meta.near_z,
            far_z: meta.far_z,
            width: size.x,
            height: size.y,
        }
    });
    for side in Side::BOTH.into_iter().filter(|_| save) {
        let eye = side.index();
        let view = &views[eye];
        let descriptor = DepthDescriptor {
            unix_ms,
            runtime_seconds: mono_ns as f64 / 1e9,
            pose: view.pose,
            fov_left: view.fov_left,
            fov_right: view.fov_right,
            fov_up: view.fov_up,
            fov_down: view.fov_down,
            near_z: view.near_z,
            far_z: view.far_z,
            width: view.width,
            height: view.height,
        };
        let path = side.depth_file(&active.dir, unix_ms, active.storage);
        match packed.next() {
            Some(data) => active.write_compressed_file(path, data),
            None => active.write_file(path, layers[eye].clone()),
        }
        active.write_line(
            CsvFile::DepthDescriptors(eye),
            depth_descriptor_row(&descriptor),
        );
    }
    captured.write(DepthFrameCaptured {
        views,
        layers,
        saved: save,
    });
}

/// 会话暂停时销毁 provider，恢复运行后由 `ensure_provider` 重建。
fn destroy_on_pause(shared: Res<DepthShared>, mut paused: MessageReader<SessionPaused>) {
    if paused.read().count() > 0 {
        destroy_provider(shared);
    }
}

fn destroy_provider(shared: Res<DepthShared>) {
    let mut shared = shared.0.lock().unwrap();
    shared.pending = None;
    shared.requested = None;
    let Some(provider) = shared.provider.take() else {
        return;
    };
    let Provider {
        fp,
        provider,
        swapchain,
        textures,
        ..
    } = provider;
    drop(textures);
    unsafe {
        (fp.stop_environment_depth_provider)(provider);
        (fp.destroy_environment_depth_swapchain)(swapchain);
        (fp.destroy_environment_depth_provider)(provider);
    }
}
