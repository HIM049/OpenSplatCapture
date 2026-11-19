//! CSV 行与 JSON 文档的文本格式。

use crate::convert::{position_to_unity, rotation_to_unity};

pub const POSE_HEADER: &str = "unix_time,ovr_timestamp,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,rot_w";

pub const DEPTH_DESCRIPTOR_HEADER: &str = "timestamp_ms,ovr_timestamp,create_pose_location_x,create_pose_location_y,create_pose_location_z,create_pose_rotation_x,create_pose_rotation_y,create_pose_rotation_z,create_pose_rotation_w,fov_left_angle_tangent,fov_right_angle_tangent,fov_top_angle_tangent,fov_down_angle_tangent,near_z,far_z,width,height";

/// OpenXR 约定下的位姿：位置 (x, y, z) 和四元数 (x, y, z, w)。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub position: [f32; 3],
    pub rotation: [f32; 4],
}

impl Pose {
    fn unity_columns(&self) -> String {
        let p = position_to_unity(self.position);
        let r = rotation_to_unity(self.rotation);
        format!(
            "{},{},{},{},{},{},{}",
            p[0], p[1], p[2], r[0], r[1], r[2], r[3]
        )
    }
}

/// 位姿 CSV 的一行，位姿换算到 Unity 坐标系；`runtime_seconds` 是单调时钟的秒数。
pub fn pose_row(unix_ms: f64, runtime_seconds: f64, pose: &Pose) -> String {
    format!("{unix_ms:.3},{runtime_seconds:.6},{}", pose.unity_columns())
}

/// 一只眼的深度帧描述，视场角为 OpenXR 的带符号弧度。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DepthDescriptor {
    pub unix_ms: i64,
    pub runtime_seconds: f64,
    pub pose: Pose,
    pub fov_left: f32,
    pub fov_right: f32,
    pub fov_up: f32,
    pub fov_down: f32,
    pub near_z: f32,
    pub far_z: f32,
    pub width: u32,
    pub height: u32,
}

/// 深度描述 CSV 的一行，视场角写成正的正切值。
pub fn depth_descriptor_row(d: &DepthDescriptor) -> String {
    let tangent = |angle: f32| angle.abs().tan();
    format!(
        "{},{:.6},{},{},{},{},{},{},{},{},{}",
        d.unix_ms,
        d.runtime_seconds,
        d.pose.unity_columns(),
        tangent(d.fov_left),
        tangent(d.fov_right),
        tangent(d.fov_up),
        tangent(d.fov_down),
        d.near_z,
        csv_float(d.far_z),
        d.width,
        d.height,
    )
}

/// 无穷大写成 pandas 能解析的 `inf`。
fn csv_float(value: f32) -> String {
    if value.is_infinite() {
        if value > 0.0 {
            "inf".into()
        } else {
            "-inf".into()
        }
    } else {
        value.to_string()
    }
}

/// Camera2 给出的相机标定参数。
#[derive(Clone, Debug, PartialEq)]
pub struct CameraCharacteristics {
    pub camera_id: String,
    /// fx, fy, cx, cy, s
    pub intrinsics: [f64; 5],
    pub pose_translation: [f64; 3],
    /// x, y, z, w
    pub pose_rotation: [f64; 4],
    /// left, top, right, bottom
    pub active_array: [i32; 4],
    /// width, height
    pub pixel_array: [i32; 2],
}

/// 相机标定参数的 JSON 文档。
pub fn camera_characteristics_json(c: &CameraCharacteristics) -> String {
    let [fx, fy, cx, cy, s] = c.intrinsics;
    let [tx, ty, tz] = c.pose_translation;
    let [qx, qy, qz, qw] = c.pose_rotation;
    let [left, top, right, bottom] = c.active_array;
    let [width, height] = c.pixel_array;
    format!(
        r#"{{
  "cameraId": "{id}",
  "pose": {{
    "translation": [{tx}, {ty}, {tz}],
    "rotation": [{qx}, {qy}, {qz}, {qw}]
  }},
  "intrinsics": {{ "fx": {fx}, "fy": {fy}, "cx": {cx}, "cy": {cy}, "s": {s} }},
  "sensor": {{
    "pixelArraySize": {{ "width": {width}, "height": {height} }},
    "activeArraySize": {{ "left": {left}, "top": {top}, "right": {right}, "bottom": {bottom} }}
  }}
}}
"#,
        id = c.camera_id,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pose_row_uses_unity_axes() {
        let pose = Pose {
            position: [1.0, 2.0, 3.0],
            rotation: [0.1, 0.2, 0.3, 0.9],
        };
        assert_eq!(
            pose_row(1700000000000.5, 12.25, &pose),
            "1700000000000.500,12.250000,1,2,-3,-0.1,-0.2,0.3,0.9"
        );
        assert_eq!(POSE_HEADER.split(',').count(), 9);
    }

    #[test]
    fn depth_row_has_positive_tangents_and_all_columns() {
        let row = depth_descriptor_row(&DepthDescriptor {
            unix_ms: 1700000000000,
            runtime_seconds: 1.5,
            pose: Pose {
                position: [0.0, 1.0, 0.0],
                rotation: [0.0, 0.0, 0.0, 1.0],
            },
            fov_left: -std::f32::consts::FRAC_PI_4,
            fov_right: std::f32::consts::FRAC_PI_4,
            fov_up: std::f32::consts::FRAC_PI_4,
            fov_down: -std::f32::consts::FRAC_PI_4,
            near_z: 0.1,
            far_z: f32::INFINITY,
            width: 320,
            height: 320,
        });
        let columns: Vec<&str> = row.split(',').collect();
        assert_eq!(columns.len(), DEPTH_DESCRIPTOR_HEADER.split(',').count());
        for tangent in &columns[9..13] {
            assert!((tangent.parse::<f32>().unwrap() - 1.0).abs() < 1e-5);
        }
        assert_eq!(columns[14], "inf");
        assert_eq!(&columns[15..], ["320", "320"]);
    }

    #[test]
    fn characteristics_json_has_expected_fields() {
        let json = camera_characteristics_json(&CameraCharacteristics {
            camera_id: "50".into(),
            intrinsics: [867.5, 867.5, 640.0, 636.0, 0.0],
            pose_translation: [-0.03, -0.02, -0.06],
            pose_rotation: [-0.99, 0.0, 0.0, 0.1],
            active_array: [0, 0, 1280, 1280],
            pixel_array: [1280, 1280],
        });
        for needle in [
            r#""translation": [-0.03, -0.02, -0.06]"#,
            r#""rotation": [-0.99, 0, 0, 0.1]"#,
            r#""fx": 867.5"#,
            r#""activeArraySize": { "left": 0, "top": 0, "right": 1280, "bottom": 1280 }"#,
        ] {
            assert!(json.contains(needle), "missing {needle} in {json}");
        }
    }
}
