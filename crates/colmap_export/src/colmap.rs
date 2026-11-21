//! COLMAP 稀疏模型的二进制文件：cameras.bin、images.bin、points3D.bin。

use std::{
    fs::File,
    io::{BufWriter, Result, Write},
    path::Path,
};

use crate::pose::Extrinsics;

/// COLMAP 的 PINHOLE 相机模型编号。
const PINHOLE: i32 = 1;

pub struct CameraModel {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    /// fx, fy, cx, cy
    pub params: [f64; 4],
}

pub struct ImageEntry {
    pub id: u32,
    pub camera_id: u32,
    pub name: String,
    pub extrinsics: Extrinsics,
}

/// 一个 3D 点；观测轨迹为空，只用作初始化。
#[derive(Clone, Copy, Debug)]
pub struct Point3D {
    pub xyz: [f64; 3],
    pub rgb: [u8; 3],
}

fn create(path: &Path) -> Result<BufWriter<File>> {
    Ok(BufWriter::with_capacity(1 << 20, File::create(path)?))
}

fn f64s(out: &mut impl Write, values: &[f64]) -> Result<()> {
    values.iter().try_for_each(|v| out.write_all(&v.to_le_bytes()))
}

pub fn write_cameras(path: &Path, cameras: &[CameraModel]) -> Result<()> {
    let mut out = create(path)?;
    out.write_all(&(cameras.len() as u64).to_le_bytes())?;
    for camera in cameras {
        out.write_all(&(camera.id as i32).to_le_bytes())?;
        out.write_all(&PINHOLE.to_le_bytes())?;
        out.write_all(&u64::from(camera.width).to_le_bytes())?;
        out.write_all(&u64::from(camera.height).to_le_bytes())?;
        f64s(&mut out, &camera.params)?;
    }
    out.flush()
}

pub fn write_images(path: &Path, images: &[ImageEntry]) -> Result<()> {
    let mut out = create(path)?;
    out.write_all(&(images.len() as u64).to_le_bytes())?;
    for image in images {
        out.write_all(&(image.id as i32).to_le_bytes())?;
        f64s(&mut out, &image.extrinsics.qvec)?;
        f64s(&mut out, &image.extrinsics.tvec)?;
        out.write_all(&(image.camera_id as i32).to_le_bytes())?;
        out.write_all(image.name.as_bytes())?;
        out.write_all(&[0])?;
        out.write_all(&0u64.to_le_bytes())?;
    }
    out.flush()
}

/// 点的编号从 1 开始。
pub fn write_points(path: &Path, points: &[Point3D]) -> Result<()> {
    let mut out = create(path)?;
    out.write_all(&(points.len() as u64).to_le_bytes())?;
    for (index, point) in points.iter().enumerate() {
        out.write_all(&(index as u64 + 1).to_le_bytes())?;
        f64s(&mut out, &point.xyz)?;
        out.write_all(&point.rgb)?;
        f64s(&mut out, &[0.0])?;
        out.write_all(&0u64.to_le_bytes())?;
    }
    out.flush()
}
