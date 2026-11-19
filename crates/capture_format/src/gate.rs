//! 拍照时的头部速度估计和拍摄间隔。

use crate::records::Pose;

/// 两次位姿采样间隔超过这个时间（秒）时不做速度平滑。
const SPEED_GAP: f64 = 0.5;

/// 记录头部速度和上一次拍摄的时刻。
#[derive(Default)]
pub struct ShotGate {
    last_sample: Option<(Pose, f64)>,
    linear_speed: f32,
    angular_speed: f32,
    last_shot: Option<f64>,
}

impl ShotGate {
    /// 用一次新的头部位姿采样更新速度估计，`time` 为秒。
    pub fn observe(&mut self, pose: &Pose, time: f64) {
        if let Some((last, last_time)) = &self.last_sample {
            let dt = time - last_time;
            if dt > 1e-4 {
                let linear = distance(pose.position, last.position) / dt as f32;
                let angular = angle_between(pose.rotation, last.rotation) / dt as f32;
                if dt > SPEED_GAP {
                    self.linear_speed = linear;
                    self.angular_speed = angular;
                } else {
                    self.linear_speed = 0.5 * (self.linear_speed + linear);
                    self.angular_speed = 0.5 * (self.angular_speed + angular);
                }
            }
        }
        self.last_sample = Some((*pose, time));
    }

    /// 当前估计的头部移动速度（米每秒）和转动速度（弧度每秒）。
    pub fn speeds(&self) -> (f32, f32) {
        (self.linear_speed, self.angular_speed)
    }

    /// 距上一次拍摄是否已过 `min_interval` 秒。
    pub fn interval_elapsed(&self, time: f64, min_interval: f64) -> bool {
        self.last_shot
            .is_none_or(|last| time - last >= min_interval)
    }

    /// 记下这一刻拍了一张照片。
    pub fn record(&mut self, time: f64) {
        self.last_shot = Some(time);
    }
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}

/// 两个单位四元数 (x, y, z, w) 之间的夹角（弧度）。
fn angle_between(a: [f32; 4], b: [f32; 4]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    2.0 * dot.abs().min(1.0).acos()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pose(x: f32, yaw: f32) -> Pose {
        Pose {
            position: [x, 1.5, 0.0],
            rotation: [0.0, (yaw / 2.0).sin(), 0.0, (yaw / 2.0).cos()],
        }
    }

    #[test]
    fn waits_for_the_minimum_interval() {
        let mut gate = ShotGate::default();
        assert!(gate.interval_elapsed(0.0, 0.1));
        gate.record(1.0);
        assert!(!gate.interval_elapsed(1.05, 0.1));
        assert!(gate.interval_elapsed(1.2, 0.1));
    }

    #[test]
    fn estimates_head_speed() {
        let mut gate = ShotGate::default();
        // 每 1/72 秒移动 2 厘米，约 1.4 米每秒。
        for step in 0..10 {
            gate.observe(&pose(step as f32 * 0.02, 0.0), step as f64 / 72.0);
        }
        let (linear, angular) = gate.speeds();
        assert!((linear - 1.44).abs() < 0.05);
        assert!(angular < 1e-3);
        // 停下来之后速度估计回落。
        for step in 10..40 {
            gate.observe(&pose(0.18, 0.0), step as f64 / 72.0);
        }
        assert!(gate.speeds().0 < 0.01);

        // 原地转头只有转动速度。
        let mut gate = ShotGate::default();
        for step in 0..10 {
            gate.observe(&pose(0.0, step as f32 * 0.03), step as f64 / 72.0);
        }
        let (linear, angular) = gate.speeds();
        assert!(linear < 1e-3);
        assert!((angular - 2.16).abs() < 0.05);
    }

    #[test]
    fn measures_quaternion_angles() {
        let a = pose(0.0, 0.0).rotation;
        let b = pose(0.0, 0.5).rotation;
        assert!((angle_between(a, b) - 0.5).abs() < 1e-4);
        // q 和 -q 是同一个朝向。
        assert!(angle_between(a, a.map(|v| -v)) < 1e-4);
    }
}
