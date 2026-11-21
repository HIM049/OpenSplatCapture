//! 读取录制目录里导出需要的文件。

use std::{
    fs,
    path::{Path, PathBuf},
};

use capture_format::{
    Side, Storage,
    convert::{
        decompress, depth_u16_to_normalized, position_to_unity, rotation_to_unity,
    },
    geometry::DepthView,
    layout::HMD_POSES,
    records::Pose as XrPose,
};

use crate::{
    json::{self, Value},
    pose::{Mount, Pose, PoseTrack},
};

pub type Result<T> = std::result::Result<T, String>;

/// 一侧相机的标定，内参已按实际图像尺寸缩放。
#[derive(Clone, Debug)]
pub struct Camera {
    pub width: u32,
    pub height: u32,
    /// fx, fy, cx, cy
    pub intrinsics: [f64; 4],
    pub mount: Mount,
}

/// 一张相机照片。
#[derive(Clone, Debug)]
pub struct Frame {
    pub side: Side,
    pub unix_ms: i64,
    pub path: PathBuf,
}

/// 一帧深度图的投影参数和文件位置；投影参数是 OpenXR 参考空间的。
#[derive(Clone, Debug)]
pub struct DepthFrame {
    pub view: DepthView,
    pub path: PathBuf,
    pub storage: Storage,
}

fn read_text(path: &Path) -> Result<String> {
    fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))
}

fn read_json(path: &Path) -> Result<Value> {
    json::parse(&read_text(path)?).map_err(|e| format!("parse {}: {e}", path.display()))
}

/// 带表头的 CSV，按列名取值；字段不全或解析失败的行跳过。
fn read_csv<T>(path: &Path, columns: &[&str], row: impl Fn(&[f64]) -> T) -> Result<Vec<T>> {
    let text = read_text(path)?;
    let mut lines = text.lines();
    let header: Vec<&str> = lines.next().unwrap_or_default().split(',').map(str::trim).collect();
    let indices = columns
        .iter()
        .map(|name| {
            header
                .iter()
                .position(|h| h == name)
                .ok_or_else(|| format!("{}: missing column {name}", path.display()))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut out = Vec::new();
    let mut values = Vec::with_capacity(indices.len());
    for line in lines {
        let fields: Vec<&str> = line.split(',').map(str::trim).collect();
        values.clear();
        for &i in &indices {
            match fields.get(i).and_then(|f| f.parse::<f64>().ok()) {
                Some(v) if !v.is_nan() => values.push(v),
                _ => break,
            }
        }
        if values.len() == indices.len() {
            out.push(row(&values));
        }
    }
    Ok(out)
}

const POSE_COLUMNS: [&str; 7] = ["pos_x", "pos_y", "pos_z", "rot_x", "rot_y", "rot_z", "rot_w"];

pub fn load_hmd_poses(session: &Path) -> Result<PoseTrack> {
    let mut columns = vec!["unix_time"];
    columns.extend(POSE_COLUMNS);
    let samples = read_csv(&session.join(HMD_POSES), &columns, |v| {
        (
            v[0],
            Pose {
                position: [v[1], v[2], v[3]],
                rotation: [v[4], v[5], v[6], v[7]],
            },
        )
    })?;
    Ok(PoseTrack::new(samples))
}

/// 读一侧相机的标定；没有这一侧的照片时返回 None。
pub fn load_camera(session: &Path, side: Side) -> Result<Option<Camera>> {
    let format_path = side.camera_image_format(session);
    if !format_path.exists() || !side.camera_dir(session).is_dir() {
        return Ok(None);
    }
    let format = read_json(&format_path)?;
    let characteristics = read_json(&side.camera_characteristics(session))?;
    let field = |doc: &Value, path: &[&str]| {
        doc.number_at(path)
            .ok_or_else(|| format!("{} camera: missing {}", side.prefix(), path.join(".")))
    };
    let (width, height) = (field(&format, &["width"])?, field(&format, &["height"])?);
    let mut intrinsics = [
        field(&characteristics, &["intrinsics", "fx"])?,
        field(&characteristics, &["intrinsics", "fy"])?,
        field(&characteristics, &["intrinsics", "cx"])?,
        field(&characteristics, &["intrinsics", "cy"])?,
    ];
    let active = ["left", "top", "right", "bottom"]
        .map(|key| characteristics.number_at(&["sensor", "activeArraySize", key]));
    if let [Some(left), Some(top), Some(right), Some(bottom)] = active {
        let (sx, sy) = (width / (right - left), height / (bottom - top));
        if (sx - 1.0).abs() > 1e-6 || (sy - 1.0).abs() > 1e-6 {
            eprintln!(
                "{}: image is {width}x{height} but the active array is {}x{}; scaling intrinsics",
                side.prefix(),
                right - left,
                bottom - top
            );
            intrinsics = [intrinsics[0] * sx, intrinsics[1] * sy, intrinsics[2] * sx, intrinsics[3] * sy];
        }
    }
    let pose = characteristics.get("pose");
    let numbers = |key| pose.and_then(|p| p.get(key)).and_then(Value::numbers).unwrap_or_default();
    Ok(Some(Camera {
        width: width as u32,
        height: height as u32,
        intrinsics,
        mount: Mount::from_lens_pose(&numbers("translation"), &numbers("rotation")),
    }))
}

/// 一侧的全部照片，按时间排序；遇到未压缩的 YUV 帧时报错。
pub fn list_frames(session: &Path, side: Side) -> Result<Vec<Frame>> {
    let dir = side.camera_dir(session);
    let entries = fs::read_dir(&dir).map_err(|e| format!("read {}: {e}", dir.display()))?;
    let mut frames = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(unix_ms) = path.file_stem().and_then(|s| s.to_str()).and_then(|s| s.parse().ok())
        else {
            continue;
        };
        match path.extension().and_then(|e| e.to_str()) {
            Some("jpg") => frames.push(Frame {
                side,
                unix_ms,
                path,
            }),
            Some("yuv") => {
                return Err(format!(
                    "{}: uncompressed YUV frames are not supported yet",
                    dir.display()
                ));
            }
            _ => {}
        }
    }
    frames.sort_by_key(|f| f.unix_ms);
    Ok(frames)
}

/// 一侧的全部深度帧；描述 CSV 里的位姿是 Unity 坐标系的，换回 OpenXR 参考空间。
pub fn list_depth(session: &Path, side: Side) -> Result<Vec<DepthFrame>> {
    let csv = side.depth_descriptors(session);
    if !csv.exists() {
        return Ok(Vec::new());
    }
    let columns = [
        "timestamp_ms",
        "create_pose_location_x",
        "create_pose_location_y",
        "create_pose_location_z",
        "create_pose_rotation_x",
        "create_pose_rotation_y",
        "create_pose_rotation_z",
        "create_pose_rotation_w",
        "fov_left_angle_tangent",
        "fov_right_angle_tangent",
        "fov_top_angle_tangent",
        "fov_down_angle_tangent",
        "near_z",
        "far_z",
        "width",
        "height",
    ];
    let rows = read_csv(&csv, &columns, |v| v.to_vec())?;
    let mut frames = Vec::new();
    for v in rows {
        let unix_ms = v[0] as i64;
        let Some((path, storage)) = [Storage::Compressed, Storage::Raw]
            .into_iter()
            .map(|storage| (side.depth_file(session, unix_ms, storage), storage))
            .find(|(path, _)| path.exists())
        else {
            continue;
        };
        let f = |i: usize| v[i] as f32;
        frames.push(DepthFrame {
            view: DepthView {
                pose: XrPose {
                    position: position_to_unity([f(1), f(2), f(3)]),
                    rotation: rotation_to_unity([f(4), f(5), f(6), f(7)]),
                },
                fov_left: -f(8).atan(),
                fov_right: f(9).atan(),
                fov_up: f(10).atan(),
                fov_down: -f(11).atan(),
                near_z: f(12),
                far_z: f(13),
                width: v[14] as u32,
                height: v[15] as u32,
            },
            path,
            storage,
        });
    }
    Ok(frames)
}

impl DepthFrame {
    /// 自上而下的归一化深度，每像素一个值。
    pub fn load(&self) -> Result<Vec<f32>> {
        let bytes = fs::read(&self.path).map_err(|e| format!("read {}: {e}", self.path.display()))?;
        let pixels = (self.view.width * self.view.height) as usize;
        let values: Vec<f32> = match self.storage {
            Storage::Compressed => decompress(&bytes)
                .map_err(|e| format!("decompress {}: {e}", self.path.display()))?
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&b| depth_u16_to_normalized(u16::from_le_bytes(b)))
                .collect(),
            Storage::Raw => bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|&b| f32::from_le_bytes(b))
                .collect(),
        };
        if values.len() != pixels {
            return Err(format!(
                "{}: expected {pixels} pixels, found {}",
                self.path.display(),
                values.len()
            ));
        }
        Ok(values)
    }
}
