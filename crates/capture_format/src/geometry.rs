//! 深度图的反投影。

use crate::{convert::depth_normalized_to_linear, records::Pose};

/// 一只眼深度图的投影参数，视场角为 OpenXR 的带符号弧度。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DepthView {
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

impl DepthView {
    /// 把文件行序（自上而下）的像素坐标和归一化深度反投影到参考空间；深度无效时返回 None。
    pub fn unproject(&self, u: f32, v: f32, depth: f32) -> Option<[f32; 3]> {
        if !(depth > 0.0 && depth < 1.0) {
            return None;
        }
        let z = depth_normalized_to_linear(depth, self.near_z, self.far_z);
        if !(z > 0.0 && z.is_finite()) {
            return None;
        }
        let (left, right) = (self.fov_left.tan(), self.fov_right.tan());
        let (up, down) = (self.fov_up.tan(), self.fov_down.tan());
        let x = left + u / self.width as f32 * (right - left);
        let y = up + v / self.height as f32 * (down - up);
        let local = [x * z, y * z, -z];
        let rotated = rotate(self.pose.rotation, local);
        let p = self.pose.position;
        Some([rotated[0] + p[0], rotated[1] + p[1], rotated[2] + p[2]])
    }
}

/// 用四元数 (x, y, z, w) 旋转向量。
pub fn rotate(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let u = [q[0], q[1], q[2]];
    let uv = cross(u, v);
    let uuv = cross(u, uv);
    [
        v[0] + 2.0 * (q[3] * uv[0] + uuv[0]),
        v[1] + 2.0 * (q[3] * uv[1] + uuv[1]),
        v[2] + 2.0 * (q[3] * uv[2] + uuv[2]),
    ]
}

pub fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(width: u32, height: u32) -> DepthView {
        DepthView {
            pose: Pose {
                position: [0.0, 1.5, 0.0],
                rotation: [0.0, 0.0, 0.0, 1.0],
            },
            fov_left: -std::f32::consts::FRAC_PI_4,
            fov_right: std::f32::consts::FRAC_PI_4,
            fov_up: std::f32::consts::FRAC_PI_4,
            fov_down: -std::f32::consts::FRAC_PI_4,
            near_z: 0.1,
            far_z: f32::INFINITY,
            width,
            height,
        }
    }

    /// 线性深度 z 对应的归一化值（远平面无穷远）。
    fn normalized(z: f32) -> f32 {
        1.0 - 0.1 / z
    }

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-3)
    }

    #[test]
    fn unprojects_centre_and_corners() {
        let view = view(100, 100);
        let d = normalized(2.0);
        // 画面中心在正前方（-Z）2 米处。
        assert!(close(
            view.unproject(50.0, 50.0, d).unwrap(),
            [0.0, 1.5, -2.0]
        ));
        // 左上角：向左、向上各 45 度。
        assert!(close(
            view.unproject(0.0, 0.0, d).unwrap(),
            [-2.0, 3.5, -2.0]
        ));
        // 右下角。
        assert!(close(
            view.unproject(100.0, 100.0, d).unwrap(),
            [2.0, -0.5, -2.0]
        ));
        assert!(view.unproject(50.0, 50.0, 0.0).is_none());
        assert!(view.unproject(50.0, 50.0, 1.0).is_none());
    }

    #[test]
    fn unproject_applies_pose_rotation() {
        let mut view = view(100, 100);
        // 绕 Y 轴左转 90 度后，正前方变成 -X。
        let half = std::f32::consts::FRAC_PI_4;
        view.pose.rotation = [0.0, half.sin(), 0.0, half.cos()];
        assert!(close(
            view.unproject(50.0, 50.0, normalized(2.0)).unwrap(),
            [-2.0, 1.5, 0.0]
        ));
    }
}
