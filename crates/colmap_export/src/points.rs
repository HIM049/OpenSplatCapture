//! 由深度图离线融合出 points3D：反投影到体素里取平均，只保留被多帧看到的体素，再从照片取颜色。

use std::{
    collections::HashMap,
    sync::atomic::{AtomicU32, AtomicUsize, Ordering},
    thread,
};

use capture_format::convert::depth_normalized_to_linear;
use zune_jpeg::{JpegDecoder, zune_core::bytestream::ZCursor};

use crate::{
    colmap::Point3D,
    pose::{Extrinsics, Vec3},
    session::{DepthFrame, Result},
};

#[derive(Clone, Copy, Debug)]
pub struct FusionSettings {
    /// 体素边长，米。
    pub voxel: f32,
    /// 沿光轴超过这个距离的深度不用，米。
    pub max_depth: f32,
    /// 体素至少被这么多帧深度看到才保留。
    pub min_frames: u32,
    /// 与相邻像素的深度差超过自身深度的这个比例时视为边缘，丢弃。
    pub edge_ratio: f32,
}

impl Default for FusionSettings {
    fn default() -> Self {
        Self {
            voxel: 0.02,
            max_depth: 3.0,
            min_frames: 3,
            edge_ratio: 0.05,
        }
    }
}

/// 给点取色用的一张照片。
pub struct ColorView {
    pub extrinsics: Extrinsics,
    /// fx, fy, cx, cy
    pub intrinsics: [f64; 4],
    pub width: u32,
    pub height: u32,
    pub path: std::path::PathBuf,
}

#[derive(Default)]
struct Voxel {
    sum: [f64; 3],
    points: u32,
    frames: u32,
    last_frame: u32,
}

type VoxelMap = HashMap<[i32; 3], Voxel>;

fn workers() -> usize {
    thread::available_parallelism().map_or(4, usize::from)
}

/// 在多个线程上按下标逐个处理任务，返回每个线程自己的结果。
fn parallel<T: Send>(count: usize, init: impl Fn() -> T + Sync, job: impl Fn(&mut T, usize) + Sync) -> Vec<T> {
    let next = AtomicUsize::new(0);
    thread::scope(|scope| {
        let handles: Vec<_> = (0..workers().min(count.max(1)))
            .map(|_| {
                scope.spawn(|| {
                    let mut state = init();
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        if index >= count {
                            break state;
                        }
                        job(&mut state, index);
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("worker thread panicked"))
            .collect()
    })
}

/// OpenXR 参考空间（Y 上、-Z 向前）到 COLMAP 世界（Y 下）。
fn xr_to_colmap(p: [f32; 3]) -> Vec3 {
    [f64::from(p[0]), -f64::from(p[1]), -f64::from(p[2])]
}

fn fuse_frame(map: &mut VoxelMap, frame_id: u32, frame: &DepthFrame, settings: &FusionSettings) -> Result<()> {
    let depth = frame.load()?;
    let view = &frame.view;
    let (w, h) = (view.width as usize, view.height as usize);
    let linear: Vec<f32> = depth
        .iter()
        .map(|&d| {
            if d > 0.0 && d < 1.0 {
                depth_normalized_to_linear(d, view.near_z, view.far_z)
            } else {
                0.0
            }
        })
        .collect();
    let voxel = f64::from(settings.voxel);
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let z = linear[i];
            if !(z > 0.0 && z <= settings.max_depth) {
                continue;
            }
            let edge = [(x > 0, i.wrapping_sub(1)), (x + 1 < w, i + 1), (y > 0, i.wrapping_sub(w)), (y + 1 < h, i + w)]
                .into_iter()
                .any(|(inside, n)| inside && (linear[n] <= 0.0 || (linear[n] - z).abs() > settings.edge_ratio * z));
            if edge {
                continue;
            }
            let Some(p) = view.unproject(x as f32 + 0.5, y as f32 + 0.5, depth[i]) else {
                continue;
            };
            let p = xr_to_colmap(p);
            let key = p.map(|c| (c / voxel).floor() as i32);
            let cell = map.entry(key).or_default();
            for (s, c) in cell.sum.iter_mut().zip(p) {
                *s += c;
            }
            cell.points += 1;
            if cell.frames == 0 || cell.last_frame != frame_id {
                cell.frames += 1;
                cell.last_frame = frame_id;
            }
        }
    }
    Ok(())
}

/// 融合全部深度帧，返回 COLMAP 世界坐标下各个保留体素的平均位置。
pub fn fuse(frames: &[DepthFrame], settings: &FusionSettings) -> Vec<Vec3> {
    let failures = AtomicUsize::new(0);
    let maps = parallel(frames.len(), VoxelMap::new, |map, index| {
        if let Err(e) = fuse_frame(map, index as u32, &frames[index], settings)
            && failures.fetch_add(1, Ordering::Relaxed) < 5
        {
            eprintln!("skipping depth frame: {e}");
        }
    });
    let failed = failures.into_inner();
    if failed > 0 {
        eprintln!("{failed} depth frames could not be read");
    }
    let mut maps = maps.into_iter();
    let mut merged = maps.next().unwrap_or_default();
    for map in maps {
        for (key, voxel) in map {
            let cell = merged.entry(key).or_default();
            for (s, c) in cell.sum.iter_mut().zip(voxel.sum) {
                *s += c;
            }
            cell.points += voxel.points;
            // 各线程处理的帧互不重叠，帧数可以直接相加。
            cell.frames += voxel.frames;
        }
    }
    let mut points: Vec<(Vec3, [i32; 3])> = merged
        .into_iter()
        .filter(|(_, v)| v.frames >= settings.min_frames)
        .map(|(key, v)| (v.sum.map(|s| s / f64::from(v.points)), key))
        .collect();
    // 按体素排序，输出与线程调度无关。
    points.sort_unstable_by_key(|(_, key)| *key);
    points.into_iter().map(|(p, _)| p).collect()
}

/// z 缓冲的格子边长，像素。
const ZBUFFER_CELL: usize = 4;

fn decode_rgb(view: &ColorView) -> Result<Vec<u8>> {
    let data = std::fs::read(&view.path).map_err(|e| format!("read {}: {e}", view.path.display()))?;
    let mut decoder = JpegDecoder::new(ZCursor::new(data));
    let pixels = decoder
        .decode()
        .map_err(|e| format!("decode {}: {e:?}", view.path.display()))?;
    let (w, h) = decoder.dimensions().unwrap_or_default();
    if (w, h) != (view.width as usize, view.height as usize) || pixels.len() != w * h * 3 {
        return Err(format!(
            "{}: decoded {w}x{h}, expected {}x{} RGB",
            view.path.display(),
            view.width,
            view.height
        ));
    }
    Ok(pixels)
}

/// 点在照片里的像素位置和沿光轴的距离。
fn project(view: &ColorView, p: Vec3) -> Option<(f64, f64, f64)> {
    let c = view.extrinsics.camera_point(p);
    if c[2] < 0.05 {
        return None;
    }
    let [fx, fy, cx, cy] = view.intrinsics;
    let (u, v) = (fx * c[0] / c[2] + cx, fy * c[1] / c[2] + cy);
    (u >= 0.0 && v >= 0.0 && u < f64::from(view.width) && v < f64::from(view.height)).then_some((u, v, c[2]))
}

/// 把每张照片里没被遮挡的点的像素颜色累加起来求平均；任何照片都看不到的点丢弃。
pub fn colorize(positions: &[Vec3], views: &[ColorView], voxel: f64) -> Vec<Point3D> {
    let sums: Vec<AtomicU32> = (0..positions.len() * 4).map(|_| AtomicU32::new(0)).collect();
    let failures = AtomicUsize::new(0);
    parallel(views.len(), || (), |_, index| {
        let view = &views[index];
        let rgb = match decode_rgb(view) {
            Ok(rgb) => rgb,
            Err(e) => {
                if failures.fetch_add(1, Ordering::Relaxed) < 5 {
                    eprintln!("skipping photo for colours: {e}");
                }
                return;
            }
        };
        let (w, h) = (view.width as usize, view.height as usize);
        let (zw, zh) = (w.div_ceil(ZBUFFER_CELL), h.div_ceil(ZBUFFER_CELL));
        let mut zbuffer = vec![f64::INFINITY; zw * zh];
        let fx = view.intrinsics[0];
        for &p in positions {
            let Some((u, v, z)) = project(view, p) else {
                continue;
            };
            // 按体素在图上的大小铺开，避免远处点之间漏缝让背后的点透过来。
            let radius = (0.5 * voxel * fx / (z * ZBUFFER_CELL as f64)).round().clamp(0.0, 3.0) as isize;
            let (gx, gy) = ((u as usize / ZBUFFER_CELL) as isize, (v as usize / ZBUFFER_CELL) as isize);
            for y in (gy - radius).max(0)..=(gy + radius).min(zh as isize - 1) {
                for x in (gx - radius).max(0)..=(gx + radius).min(zw as isize - 1) {
                    let cell = &mut zbuffer[y as usize * zw + x as usize];
                    *cell = cell.min(z);
                }
            }
        }
        for (i, &p) in positions.iter().enumerate() {
            let Some((u, v, z)) = project(view, p) else {
                continue;
            };
            let front = zbuffer[(v as usize / ZBUFFER_CELL) * zw + u as usize / ZBUFFER_CELL];
            if z > front + (2.0 * voxel).max(0.02 * z) {
                continue;
            }
            let px = ((v as usize) * w + u as usize) * 3;
            for c in 0..3 {
                sums[i * 4 + c].fetch_add(u32::from(rgb[px + c]), Ordering::Relaxed);
            }
            sums[i * 4 + 3].fetch_add(1, Ordering::Relaxed);
        }
    });
    let failed = failures.into_inner();
    if failed > 0 {
        eprintln!("{failed} photos could not be decoded");
    }
    positions
        .iter()
        .enumerate()
        .filter_map(|(i, &xyz)| {
            let count = sums[i * 4 + 3].load(Ordering::Relaxed);
            (count > 0).then(|| Point3D {
                xyz,
                rgb: std::array::from_fn(|c| {
                    ((sums[i * 4 + c].load(Ordering::Relaxed) + count / 2) / count) as u8
                }),
            })
        })
        .collect()
}
