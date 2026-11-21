//! 位姿插值，以及从 Unity 坐标系的头显位姿换算出 COLMAP 的相机外参，换算方式与 OpenQuestCapture 的 Python 管线一致。

/// 四元数 (x, y, z, w)。
pub type Quat = [f64; 4];
pub type Vec3 = [f64; 3];
type Mat3 = [[f64; 3]; 3];

pub const IDENTITY: Quat = [0.0, 0.0, 0.0, 1.0];

pub fn normalize(q: Quat) -> Quat {
    let n = q.iter().map(|c| c * c).sum::<f64>().sqrt();
    if n > 0.0 { q.map(|c| c / n) } else { IDENTITY }
}

pub fn conjugate(q: Quat) -> Quat {
    [-q[0], -q[1], -q[2], q[3]]
}

pub fn multiply(a: Quat, b: Quat) -> Quat {
    [
        a[3] * b[0] + a[0] * b[3] + a[1] * b[2] - a[2] * b[1],
        a[3] * b[1] - a[0] * b[2] + a[1] * b[3] + a[2] * b[0],
        a[3] * b[2] + a[0] * b[1] - a[1] * b[0] + a[2] * b[3],
        a[3] * b[3] - a[0] * b[0] - a[1] * b[1] - a[2] * b[2],
    ]
}

pub fn rotate(q: Quat, v: Vec3) -> Vec3 {
    let m = to_matrix(q);
    mat_vec(&m, v)
}

/// 球面线性插值，走较短的一侧。
pub fn slerp(a: Quat, b: Quat, t: f64) -> Quat {
    let mut dot: f64 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let b = if dot < 0.0 {
        dot = -dot;
        b.map(|c| -c)
    } else {
        b
    };
    if dot > 0.9995 {
        return normalize(std::array::from_fn(|i| a[i] + t * (b[i] - a[i])));
    }
    let theta = dot.clamp(-1.0, 1.0).acos();
    let (wa, wb) = (
        ((1.0 - t) * theta).sin() / theta.sin(),
        (t * theta).sin() / theta.sin(),
    );
    normalize(std::array::from_fn(|i| wa * a[i] + wb * b[i]))
}

fn to_matrix(q: Quat) -> Mat3 {
    let [x, y, z, w] = normalize(q);
    [
        [1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y - z * w), 2.0 * (x * z + y * w)],
        [2.0 * (x * y + z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z - x * w)],
        [2.0 * (x * z - y * w), 2.0 * (y * z + x * w), 1.0 - 2.0 * (x * x + y * y)],
    ]
}

fn from_matrix(m: &Mat3) -> Quat {
    let trace = m[0][0] + m[1][1] + m[2][2];
    let q = if trace > 0.0 {
        let s = (trace + 1.0).sqrt() * 2.0;
        [
            (m[2][1] - m[1][2]) / s,
            (m[0][2] - m[2][0]) / s,
            (m[1][0] - m[0][1]) / s,
            0.25 * s,
        ]
    } else if m[0][0] > m[1][1] && m[0][0] > m[2][2] {
        let s = (1.0 + m[0][0] - m[1][1] - m[2][2]).sqrt() * 2.0;
        [
            0.25 * s,
            (m[0][1] + m[1][0]) / s,
            (m[0][2] + m[2][0]) / s,
            (m[2][1] - m[1][2]) / s,
        ]
    } else if m[1][1] > m[2][2] {
        let s = (1.0 + m[1][1] - m[0][0] - m[2][2]).sqrt() * 2.0;
        [
            (m[0][1] + m[1][0]) / s,
            0.25 * s,
            (m[1][2] + m[2][1]) / s,
            (m[0][2] - m[2][0]) / s,
        ]
    } else {
        let s = (1.0 + m[2][2] - m[0][0] - m[1][1]).sqrt() * 2.0;
        [
            (m[0][2] + m[2][0]) / s,
            (m[1][2] + m[2][1]) / s,
            0.25 * s,
            (m[1][0] - m[0][1]) / s,
        ]
    };
    normalize(q)
}

fn mat_vec(m: &Mat3, v: Vec3) -> Vec3 {
    std::array::from_fn(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
}

/// Unity 世界（Y 上、左手）到 COLMAP 世界（Y 下、右手）只需翻转 Y 轴。
pub fn unity_to_colmap_position(p: Vec3) -> Vec3 {
    [p[0], -p[1], p[2]]
}

/// 相机到世界的旋转从 Unity 换到 COLMAP：两边相机轴约定相同，只做 Y 轴翻转的共轭。
fn unity_to_colmap_rotation(q: Quat) -> Quat {
    let mut m = to_matrix(q);
    for (i, row) in m.iter_mut().enumerate() {
        for (j, value) in row.iter_mut().enumerate() {
            if (i == 1) != (j == 1) {
                *value = -*value;
            }
        }
    }
    from_matrix(&m)
}

/// 一个时刻的位姿，Unity 坐标系，旋转为相机（或头显）到世界。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub position: Vec3,
    pub rotation: Quat,
}

/// 按 Unix 毫秒排好序的头显位姿。
pub struct PoseTrack {
    samples: Vec<(f64, Pose)>,
}

impl PoseTrack {
    /// 前后两个样本都在 `window_ms` 之内时才插值。
    const WINDOW_MS: f64 = 30.0;

    pub fn new(mut samples: Vec<(f64, Pose)>) -> Self {
        samples.sort_by(|a, b| a.0.total_cmp(&b.0));
        Self { samples }
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn at(&self, unix_ms: f64) -> Option<Pose> {
        let after = self.samples.partition_point(|(t, _)| *t < unix_ms);
        let next = self.samples.get(after)?;
        let prev = if next.0 == unix_ms {
            next
        } else {
            self.samples.get(after.checked_sub(1)?)?
        };
        if unix_ms - prev.0 > Self::WINDOW_MS || next.0 - unix_ms > Self::WINDOW_MS {
            return None;
        }
        let alpha = if next.0 > prev.0 {
            (unix_ms - prev.0) / (next.0 - prev.0)
        } else {
            0.0
        };
        let (a, b) = (prev.1, next.1);
        Some(Pose {
            position: std::array::from_fn(|i| (1.0 - alpha) * a.position[i] + alpha * b.position[i]),
            rotation: slerp(a.rotation, b.rotation, alpha),
        })
    }
}

/// 相机相对头显的安装位姿，已换到 Unity 坐标系。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mount {
    pub translation: Vec3,
    pub rotation: Quat,
}

impl Mount {
    /// 由 Camera2 的 lens pose 得到：平移翻转 Z，旋转镜像后取逆，再绕 X 轴转 180 度对齐 Android 相机轴。
    pub fn from_lens_pose(translation: &[f64], rotation: &[f64]) -> Self {
        let translation = match translation {
            [x, y, z, ..] => [*x, *y, -*z],
            _ => [0.0; 3],
        };
        let rotation = match rotation {
            [x, y, z, w, ..] => multiply(
                conjugate(normalize([-*x, -*y, *z, *w])),
                [1.0, 0.0, 0.0, 0.0],
            ),
            _ => IDENTITY,
        };
        Self {
            translation,
            rotation,
        }
    }

    /// 头显位姿叠加安装位姿后的相机位姿，Unity 坐标系。
    pub fn camera_pose(&self, hmd: &Pose) -> Pose {
        let offset = rotate(hmd.rotation, self.translation);
        Pose {
            position: std::array::from_fn(|i| hmd.position[i] + offset[i]),
            rotation: normalize(multiply(hmd.rotation, self.rotation)),
        }
    }
}

/// COLMAP 的世界到相机外参。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Extrinsics {
    /// 四元数 (w, x, y, z)，与 images.bin 的顺序相同。
    pub qvec: [f64; 4],
    pub tvec: Vec3,
}

impl Extrinsics {
    pub fn from_unity_camera(camera: &Pose) -> Self {
        let position = unity_to_colmap_position(camera.position);
        let world_to_camera = conjugate(unity_to_colmap_rotation(camera.rotation));
        let t = rotate(world_to_camera, position);
        let [x, y, z, w] = world_to_camera;
        Self {
            qvec: [w, x, y, z],
            tvec: t.map(|c| -c),
        }
    }

    /// 把 COLMAP 世界坐标的点换到相机坐标（X 右、Y 下、Z 前）。
    pub fn camera_point(self, p: Vec3) -> Vec3 {
        let [w, x, y, z] = self.qvec;
        let r = rotate([x, y, z, w], p);
        std::array::from_fn(|i| r[i] + self.tvec[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: &[f64], b: &[f64]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9)
    }

    #[test]
    fn matrix_round_trip() {
        for q in [
            [0.1, 0.2, 0.3, 0.9],
            [0.9, -0.1, 0.2, 0.05],
            [0.0, 0.0, 1.0, 0.0],
            [-0.3, 0.8, 0.1, -0.2],
        ] {
            let q = normalize(q);
            let back = from_matrix(&to_matrix(q));
            let sign = if back[3] * q[3] < 0.0 { -1.0 } else { 1.0 };
            assert!(close(&back.map(|c| c * sign), &q), "{q:?} -> {back:?}");
        }
    }

    #[test]
    fn interpolates_inside_window_only() {
        let half = std::f64::consts::FRAC_PI_4;
        let track = PoseTrack::new(vec![
            (
                20.0,
                Pose {
                    position: [2.0, 0.0, 0.0],
                    rotation: [0.0, half.sin(), 0.0, half.cos()],
                },
            ),
            (
                0.0,
                Pose {
                    position: [0.0; 3],
                    rotation: IDENTITY,
                },
            ),
        ]);
        let mid = track.at(10.0).unwrap();
        assert!(close(&mid.position, &[1.0, 0.0, 0.0]));
        let quarter = (std::f64::consts::PI / 8.0).sin();
        assert!(close(&mid.rotation, &[0.0, quarter, 0.0, (std::f64::consts::PI / 8.0).cos()]));
        assert!(track.at(20.0).is_some());
        assert!(track.at(-1.0).is_none());
        assert!(track.at(51.0).is_none());
    }

    #[test]
    fn identity_camera_looks_down_colmap_z() {
        // Unity 相机在 (1, 2, 3) 朝 +Z 看，Y 向上；COLMAP 里同一台相机朝 +Z 看，Y 向下。
        let ext = Extrinsics::from_unity_camera(&Pose {
            position: [1.0, 2.0, 3.0],
            rotation: IDENTITY,
        });
        // 相机前方一米、上方一米的点（Unity 坐标 (1, 3, 4)）。
        let p = ext.camera_point(unity_to_colmap_position([1.0, 3.0, 4.0]));
        assert!(close(&p, &[0.0, -1.0, 1.0]), "{p:?}");
    }

    #[test]
    fn mount_offsets_camera_from_head() {
        let mount = Mount {
            translation: [0.1, 0.0, 0.0],
            rotation: IDENTITY,
        };
        let half = std::f64::consts::FRAC_PI_4;
        // 头绕 Y 轴转 90 度后，右侧偏移变成沿 -Z（Unity 左手系中 +Y 转动让 +X 转向 -Z）。
        let cam = mount.camera_pose(&Pose {
            position: [0.0; 3],
            rotation: [0.0, half.sin(), 0.0, half.cos()],
        });
        assert!(close(&cam.position, &[0.0, 0.0, -0.1]), "{:?}", cam.position);
    }
}
