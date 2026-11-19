//! 多帧深度融合出的表面点，以及每个点的外侧半球上已被照片涂到哪些方向的记录。

use std::collections::HashMap;

use crate::{
    convert::depth_normalized_to_linear,
    geometry::{DepthView, cross, rotate},
    room::RoomPoint,
};

/// 半球上的采样方向数：顶点一个，三圈各八个。
pub const DOME_VERTICES: usize = 25;
/// 每圈的采样方向数。
pub const RING_SIZE: usize = 8;
/// 三圈采样方向与法线的夹角（度）。
const RING_ANGLES: [f32; 3] = [30.0, 54.0, 78.0];
/// 顶点和三圈里每个方向各自代表的半球面积比例。
const DOME_WEIGHTS: [f32; 4] = [0.0341, 0.0278, 0.0421, 0.0508];

/// 半球上的采样方向，坐标依次沿表面的右方向、上方向和法线。
/// 第 0 个是法线方向，之后每八个是一圈，由内到外，相邻两圈错开半格。
pub fn dome_directions() -> [[f32; 3]; DOME_VERTICES] {
    let mut directions = [[0.0, 0.0, 1.0]; DOME_VERTICES];
    for (ring, polar) in RING_ANGLES.into_iter().enumerate() {
        let (sin, cos) = polar.to_radians().sin_cos();
        for step in 0..RING_SIZE {
            let azimuth = (step as f32 * 45.0 + ring as f32 * 22.5).to_radians();
            directions[1 + ring * RING_SIZE + step] =
                [sin * azimuth.cos(), sin * azimuth.sin(), cos];
        }
    }
    directions
}

/// 表面覆盖的参数，角度为弧度，长度为米。
#[derive(Clone, Copy, Debug)]
pub struct SurfaceSettings {
    /// 深度图上每隔多少像素取一个采样。
    pub pixel_step: u32,
    /// 表面点的间距。
    pub spacing: f32,
    /// 超过这个距离的表面不生成点，照片也不算拍到。
    pub max_distance: f32,
    /// 比 `min_distance` 近、比 `max_fuse_distance` 远的深度采样不可靠，不用来生成点。
    pub min_distance: f32,
    pub max_fuse_distance: f32,
    /// 视线与表面法线的夹角超过它的深度采样不用来生成点。
    pub max_fuse_angle: f32,
    /// 照片里的点比深度测到的表面远出 `它 + 距离 * depth_noise_ratio` 时算被遮挡。
    pub occlusion_margin: f32,
    /// 深度比点远出 `它 + 距离 * depth_noise_ratio` 时算看穿了这个点，相差在这以内算看到了它。
    pub carve_margin: f32,
    pub depth_noise_ratio: f32,
    /// 超过这个距离拍到的点不涂色。
    pub max_paint_distance: f32,
    /// 两个点的最小间距与 `spacing` 的比例。
    pub min_spacing_ratio: f32,
    /// 一帧里比 `min_distance` 近的有效深度超过这个比例时算离表面太近。
    pub too_close_ratio: f32,
    /// 相邻采样的深度差超过 `深度 * 该比例` 时视为不连续，不生成点。
    pub discontinuity_ratio: f32,
    /// 视线与法线的夹角超过它时不算拍到。
    pub max_view_angle: f32,
    /// 一张照片把半球上与视线夹角在它以内的方向涂上。
    pub paint_angle: f32,
    /// 在这个距离以内拍到时记为近距离。
    pub close_distance: f32,
    /// 涂到的半球面积比例达到它算拍够，达到 `detailed_area` 且有近距离照片算细节充分。
    pub enough_area: f32,
    pub detailed_area: f32,
    pub max_points: usize,
    /// 候选位置被这么多帧深度命中、且首末两次命中相隔 `confirm_span` 秒以上，才成为表面点。
    pub confirm_hits: u32,
    pub confirm_span: f64,
    /// 候选位置这么多秒没有再被命中就丢弃。
    pub candidate_timeout: f64,
    /// 沿法线方向与已有点或候选相距在它以内的采样算作同一层表面。
    pub merge_depth: f32,
    /// 离手柄这么近的深度采样不参与融合。
    pub mask_radius: f32,
    /// 表面点的可信度上限；被深度看穿一次减一，再次看到加一，减到零时删除。
    pub max_score: u8,
    /// 定形时相距在 `cluster_link` 以内的点连成一簇，点数少于 `min_cluster` 的簇被删除。
    pub cluster_link: f32,
    pub min_cluster: usize,
    /// 定形时沿法线前后相距在这个范围（最小、最大）内的两个点视为重叠的两层，只留命中多的。
    pub layer_depth: [f32; 2],
    /// 定形时至少有这么多点支持的平面才算数，离平面 `plane_distance` 以内的点对齐到平面上。
    pub plane_min_points: usize,
    pub plane_distance: f32,
    pub max_planes: usize,
    /// 找平面时沿法线方向统计点数的格宽。
    pub plane_bin: f32,
    /// 点的法线与平面法线的夹角在 `plane_seed_angle` 以内才参与找平面和拟合，
    /// 在 `plane_inlier_angle` 以内才对齐到平面上。
    pub plane_seed_angle: f32,
    pub plane_inlier_angle: f32,
    /// 平面网格上的空格，八个邻格里至少这么多个有点时补一个点。
    pub plane_fill_neighbours: u32,
    /// 法线夹角在它以内的两个点才可能被当成重叠的两层。
    pub layer_angle: f32,
    /// 隐藏的点被这么多帧深度在原处看到后显示出来。
    pub reveal_hits: u32,
}

impl Default for SurfaceSettings {
    fn default() -> Self {
        Self {
            pixel_step: 4,
            spacing: 0.1,
            max_distance: 4.0,
            min_distance: 1.0,
            max_fuse_distance: 3.5,
            max_fuse_angle: 60f32.to_radians(),
            occlusion_margin: 0.2,
            carve_margin: 0.1,
            depth_noise_ratio: 0.05,
            max_paint_distance: 2.5,
            min_spacing_ratio: 0.7,
            too_close_ratio: 0.5,
            discontinuity_ratio: 0.1,
            max_view_angle: 70f32.to_radians(),
            paint_angle: 28f32.to_radians(),
            close_distance: 1.0,
            enough_area: 0.5,
            detailed_area: 0.75,
            max_points: 30_000,
            confirm_hits: 4,
            confirm_span: 0.3,
            candidate_timeout: 2.0,
            merge_depth: 0.08,
            mask_radius: 0.15,
            max_score: 6,
            cluster_link: 0.2,
            min_cluster: 8,
            layer_depth: [0.05, 0.25],
            plane_min_points: 60,
            plane_distance: 0.15,
            max_planes: 32,
            plane_bin: 0.05,
            plane_seed_angle: 25f32.to_radians(),
            plane_inlier_angle: 35f32.to_radians(),
            plane_fill_neighbours: 5,
            layer_angle: 45f32.to_radians(),
            reveal_hits: 2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// 还没拍够。
    Pending,
    Enough,
    /// 涂满了大部分半球，并且有近距离照片。
    Detailed,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfacePoint {
    pub position: [f32; 3],
    /// 指向表面外侧的单位法线。
    pub normal: [f32; 3],
    /// 半球上已涂到的采样方向，第 n 位对应 `dome_directions` 的第 n 个。
    pub painted: u32,
    pub close: bool,
    /// 可信度，为零表示这个点已被删除。
    pub score: u8,
    /// 被深度帧命中的次数。
    pub hits: u32,
    /// 判断为拍不到，暂不显示、不计数，在原处被深度看到后才显示。
    pub hidden: bool,
}

impl SurfacePoint {
    pub fn removed(&self) -> bool {
        self.score == 0
    }

    /// 没被删除也没被隐藏，要显示和计数。
    pub fn shown(&self) -> bool {
        !self.removed() && !self.hidden
    }

    /// 表面内的右方向和上方向，从表面外侧正对它看过去时的左右上下。
    pub fn tangents(&self) -> ([f32; 3], [f32; 3]) {
        let reference = if self.normal[1].abs() > 0.9 {
            [0.0, 0.0, -1.0]
        } else {
            [0.0, 1.0, 0.0]
        };
        let right = normalize(cross(reference, self.normal)).unwrap_or([1.0, 0.0, 0.0]);
        (right, cross(self.normal, right))
    }

    /// 已涂到的半球面积比例。
    pub fn painted_area(&self) -> f32 {
        (0..DOME_VERTICES)
            .filter(|vertex| self.painted & (1 << vertex) != 0)
            .map(|vertex| DOME_WEIGHTS[(vertex + RING_SIZE - 1) / RING_SIZE])
            .sum()
    }

    pub fn level(&self, settings: &SurfaceSettings) -> Level {
        let area = self.painted_area();
        if area < settings.enough_area {
            Level::Pending
        } else if area >= settings.detailed_area && self.close {
            Level::Detailed
        } else {
            Level::Enough
        }
    }

    /// 从 `to_camera`（指向相机的单位向量）方向拍这个点时涂到的采样方向；视线太斜时返回 None。
    fn paint_mask(
        &self,
        to_camera: [f32; 3],
        dome: &[[f32; 3]; DOME_VERTICES],
        settings: &SurfaceSettings,
    ) -> Option<u32> {
        let facing = dot(to_camera, self.normal);
        if facing < settings.max_view_angle.cos() {
            return None;
        }
        let (right, up) = self.tangents();
        let local = [dot(to_camera, right), dot(to_camera, up), facing];
        let limit = settings.paint_angle.cos();
        let mut mask = 0;
        for (vertex, direction) in dome.iter().enumerate() {
            if dot(*direction, local) >= limit {
                mask |= 1 << vertex;
            }
        }
        Some(mask)
    }
}

/// 还没确认的表面位置：落在这里的采样的位置和法线之和，以及命中情况。
struct Candidate {
    position: [f32; 3],
    normal: [f32; 3],
    samples: u32,
    /// 命中过的深度帧数，同一时刻的左右眼只算一次。
    hits: u32,
    first_hit: f64,
    last_hit: f64,
}

impl Candidate {
    fn mean_position(&self) -> [f32; 3] {
        self.position.map(|v| v / self.samples as f32)
    }
}

/// 定形时找到的平面：满足 `dot(p, normal) == offset` 的点。
struct Plane {
    normal: [f32; 3],
    offset: f32,
}

/// 一个深度采样与已有表面的关系。
enum Match {
    /// 属于某个已确认的点所在的表面。
    Point,
    /// 属于这个体素里的候选。
    Candidate([i32; 3]),
    /// 所在体素已被另一层表面占用。
    Blocked,
    None,
}

/// 多帧深度融合出的表面点。点的下标保持不变，被删除的点仍留在列表里。
#[derive(Default)]
pub struct SurfaceMap {
    points: Vec<SurfacePoint>,
    /// 每个边长为点间距的体素里最多一个未删除的点。
    index: HashMap<[i32; 3], u32>,
    candidates: HashMap<[i32; 3], Candidate>,
    /// 显示的点数，不含删除和隐藏的。
    live: usize,
    enough: usize,
    hidden: usize,
    /// 定形之后点不再增减。
    frozen: bool,
    /// 最近一帧深度里大部分读数都比 `min_distance` 近。
    too_close: bool,
}

impl SurfaceMap {
    pub fn clear(&mut self) {
        self.points.clear();
        self.index.clear();
        self.candidates.clear();
        self.live = 0;
        self.enough = 0;
        self.hidden = 0;
        self.frozen = false;
        self.too_close = false;
    }

    /// 用现成的表面点替换全部内容并直接定形，每个体素只留一个点。隐藏的点从零开始累计命中。
    pub fn load(&mut self, points: &[RoomPoint], settings: &SurfaceSettings) {
        self.clear();
        for point in points.iter().take(settings.max_points) {
            let voxel = voxel_of(point.position, settings);
            if self.index.contains_key(&voxel) {
                continue;
            }
            self.index.insert(voxel, self.points.len() as u32);
            self.points.push(SurfacePoint {
                position: point.position,
                normal: point.normal,
                painted: 0,
                close: false,
                score: settings.max_score.max(1),
                hits: if point.hidden {
                    0
                } else {
                    settings.confirm_hits
                },
                hidden: point.hidden,
            });
            if point.hidden {
                self.hidden += 1;
            } else {
                self.live += 1;
            }
        }
        self.frozen = true;
    }

    /// 隐藏的点数。
    pub fn hidden_count(&self) -> usize {
        self.hidden
    }

    /// 用同一时刻各眼的深度检查隐藏的点：点在某只眼的视野内、深度读数和点的距离吻合、
    /// 法线朝向相机时记一次命中，命中满 `reveal_hits` 次就显示出来。返回这一次显示出来的点的下标。
    pub fn reveal(&mut self, eyes: &[(&DepthView, &[u8])], settings: &SurfaceSettings) -> Vec<u32> {
        let mut shown = Vec::new();
        if self.hidden == 0 {
            return shown;
        }
        let cos = settings.max_view_angle.cos();
        for (i, point) in self.points.iter_mut().enumerate() {
            if !point.hidden || point.removed() {
                continue;
            }
            let seen = eyes.iter().any(|(view, depth)| {
                let Some((z, seen_z)) = depth_at(view, depth, point.position) else {
                    return false;
                };
                let offset = sub(view.pose.position, point.position);
                let distance = dot(offset, offset).sqrt();
                z <= settings.max_paint_distance
                    && (seen_z - z).abs()
                        <= settings.occlusion_margin + settings.depth_noise_ratio * z
                    && dot(offset, point.normal) >= cos * distance
            });
            if !seen {
                continue;
            }
            point.hits += 1;
            if point.hits >= settings.reveal_hits {
                point.hidden = false;
                self.hidden -= 1;
                self.live += 1;
                shown.push(i as u32);
            }
        }
        shown
    }

    /// 最近融合的一帧离表面太近，大部分深度都用不上。
    pub fn too_close(&self) -> bool {
        self.too_close
    }

    pub fn frozen(&self) -> bool {
        self.frozen
    }

    /// 所有表面点，包括已删除的。
    pub fn points(&self) -> &[SurfacePoint] {
        &self.points
    }

    /// 显示的点数，不含删除和隐藏的。
    pub fn live_count(&self) -> usize {
        self.live
    }

    /// 已拍够的点数。
    pub fn enough_count(&self) -> usize {
        self.enough
    }

    /// 融合一帧深度（小端 float32、自上而下的归一化值），`time` 是这一帧的时刻（秒）。
    ///
    /// 被多帧一致看到的位置成为新的表面点，追加在点列表末尾；被这一帧看穿的点降低可信度，
    /// 降到零时删除。`masks` 是手柄的位置，它们附近的采样被忽略。返回这一帧删除的点的下标。
    /// 定形之后什么也不做。
    pub fn fuse(
        &mut self,
        view: &DepthView,
        depth: &[u8],
        time: f64,
        masks: &[[f32; 3]],
        settings: &SurfaceSettings,
    ) -> Vec<u32> {
        let (width, height) = (view.width as usize, view.height as usize);
        let step = settings.pixel_step.max(1) as usize;
        if self.frozen || depth.len() < width * height * 4 {
            return Vec::new();
        }
        let removed = self.carve(view, depth, settings);
        self.candidates
            .retain(|_, candidate| time - candidate.last_hit < settings.candidate_timeout);
        let (cols, rows) = (width / step, height / step);

        // 每个采样的参考空间坐标和线性深度。
        let mask_radius = settings.mask_radius * settings.mask_radius;
        let mut grid: Vec<Option<([f32; 3], f32)>> = Vec::with_capacity(cols * rows);
        let (mut valid, mut near) = (0, 0);
        for row in 0..rows {
            for col in 0..cols {
                let (u, v) = (col * step + step / 2, row * step + step / 2);
                let d = read_depth(depth, v * width + u);
                let z = depth_normalized_to_linear(d, view.near_z, view.far_z);
                if d > 0.0 && d < 1.0 {
                    valid += 1;
                    near += (z < settings.min_distance) as u32;
                }
                let point = (z >= settings.min_distance && z <= settings.max_fuse_distance)
                    .then(|| view.unproject(u as f32 + 0.5, v as f32 + 0.5, d))
                    .flatten()
                    .filter(|point| {
                        masks.iter().all(|mask| {
                            let offset = sub(*point, *mask);
                            dot(offset, offset) > mask_radius
                        })
                    });
                grid.push(point.map(|p| (p, z)));
            }
        }

        self.too_close = near as f32 > valid as f32 * settings.too_close_ratio;

        // 法线取相隔两个采样的邻居，基线更长，受深度噪声的影响更小。
        const BASELINE: usize = 2;
        let min_facing = settings.max_fuse_angle.cos();
        for row in 0..rows.saturating_sub(BASELINE) {
            for col in 0..cols.saturating_sub(BASELINE) {
                let Some((point, z)) = grid[row * cols + col] else {
                    continue;
                };
                let (Some((right, right_z)), Some((down, down_z))) = (
                    grid[row * cols + col + BASELINE],
                    grid[(row + BASELINE) * cols + col],
                ) else {
                    continue;
                };
                let limit = z * settings.discontinuity_ratio;
                if (right_z - z).abs() > limit || (down_z - z).abs() > limit {
                    continue;
                }
                let Some(mut normal) = normalize(cross(sub(down, point), sub(right, point))) else {
                    continue;
                };
                let to_camera = sub(view.pose.position, point);
                if dot(normal, to_camera) < 0.0 {
                    normal = normal.map(|v| -v);
                }
                if dot(normal, to_camera) < min_facing * dot(to_camera, to_camera).sqrt() {
                    continue;
                }
                let voxel = voxel_of(point, settings);
                match self.find_surface(point, voxel, settings) {
                    Match::Point | Match::Blocked => {}
                    Match::Candidate(key) => {
                        let candidate = self.candidates.get_mut(&key).unwrap();
                        for axis in 0..3 {
                            candidate.position[axis] += point[axis];
                            candidate.normal[axis] += normal[axis];
                        }
                        candidate.samples += 1;
                        if candidate.last_hit != time {
                            candidate.hits += 1;
                            candidate.last_hit = time;
                        }
                    }
                    Match::None => {
                        self.candidates.insert(
                            voxel,
                            Candidate {
                                position: point,
                                normal,
                                samples: 1,
                                hits: 1,
                                first_hit: time,
                                last_hit: time,
                            },
                        );
                    }
                }
            }
        }
        self.confirm_candidates(settings);
        removed
    }

    /// 找出采样所属的表面：同一体素里沿法线相距不远的，或相邻体素里位置也足够近的。
    fn find_surface(&self, point: [f32; 3], voxel: [i32; 3], settings: &SurfaceSettings) -> Match {
        let lateral_limit = settings.spacing * settings.min_spacing_ratio;
        let same_surface = |position: [f32; 3], normal: [f32; 3], same_voxel: bool| {
            let offset = sub(point, position);
            let along = dot(offset, normal);
            let lateral = (dot(offset, offset) - along * along).max(0.0).sqrt();
            along.abs() <= settings.merge_depth && (same_voxel || lateral < lateral_limit)
        };
        let mut found = Match::None;
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    let key = [voxel[0] + dx, voxel[1] + dy, voxel[2] + dz];
                    let same_voxel = key == voxel;
                    if let Some(&index) = self.index.get(&key) {
                        let other = &self.points[index as usize];
                        if same_surface(other.position, other.normal, same_voxel) {
                            return Match::Point;
                        }
                        if same_voxel {
                            found = Match::Blocked;
                        }
                    }
                    if let Some(candidate) = self.candidates.get(&key) {
                        let normal = normalize(candidate.normal).unwrap_or([0.0, 1.0, 0.0]);
                        if same_surface(candidate.mean_position(), normal, same_voxel) {
                            if !matches!(found, Match::Blocked) {
                                found = Match::Candidate(key);
                            }
                        } else if same_voxel {
                            found = Match::Blocked;
                        }
                    }
                }
            }
        }
        found
    }

    /// 把命中次数够了的候选变成表面点。
    fn confirm_candidates(&mut self, settings: &SurfaceSettings) {
        let mut ready: Vec<[i32; 3]> = self
            .candidates
            .iter()
            .filter(|(_, candidate)| {
                candidate.hits >= settings.confirm_hits
                    && candidate.last_hit - candidate.first_hit >= settings.confirm_span
            })
            .map(|(key, _)| *key)
            .collect();
        ready.sort_unstable();
        for key in ready {
            let Some(candidate) = self.candidates.remove(&key) else {
                continue;
            };
            if self.points.len() >= settings.max_points {
                continue;
            }
            let position = candidate.mean_position();
            let voxel = voxel_of(position, settings);
            let Some(normal) = normalize(candidate.normal) else {
                continue;
            };
            if self.index.contains_key(&voxel)
                || self.has_point_near(
                    position,
                    voxel,
                    settings.spacing * settings.min_spacing_ratio,
                )
            {
                continue;
            }
            self.index.insert(voxel, self.points.len() as u32);
            self.points.push(SurfacePoint {
                position,
                normal,
                painted: 0,
                close: false,
                score: settings.max_score.min(settings.confirm_hits as u8).max(1),
                hits: candidate.hits,
                hidden: false,
            });
            self.live += 1;
        }
    }

    /// 用一帧深度核对视野里的点：仍在原处的加可信度，被看穿的减可信度并在减到零时删除。
    fn carve(&mut self, view: &DepthView, depth: &[u8], settings: &SurfaceSettings) -> Vec<u32> {
        // 看穿的判断要求这个像素距离内的邻居也都看到了更远处，避开物体边缘。
        const NEIGHBOUR: usize = 2;
        let (width, height) = (view.width as usize, view.height as usize);
        let q = view.pose.rotation;
        let inverse = [-q[0], -q[1], -q[2], q[3]];
        let (left, right) = (view.fov_left.tan(), view.fov_right.tan());
        let (up, down) = (view.fov_up.tan(), view.fov_down.tan());
        let min_facing = settings.max_view_angle.cos();

        let Self {
            points,
            index,
            live,
            enough,
            ..
        } = self;
        let mut removed = Vec::new();
        for (i, point) in points.iter_mut().enumerate() {
            if point.removed() {
                continue;
            }
            let offset = sub(point.position, view.pose.position);
            let local = rotate(inverse, offset);
            let z = -local[2];
            if !(z > view.near_z && z <= settings.max_distance) {
                continue;
            }
            let u = (local[0] / z - left) / (right - left) * width as f32;
            let v = (local[1] / z - up) / (down - up) * height as f32;
            if !(u >= NEIGHBOUR as f32 && v >= NEIGHBOUR as f32) {
                continue;
            }
            let (u, v) = (u as usize, v as usize);
            if u + NEIGHBOUR >= width || v + NEIGHBOUR >= height {
                continue;
            }
            if -dot(offset, point.normal) < min_facing * dot(offset, offset).sqrt() {
                continue;
            }
            let margin = settings.carve_margin + settings.depth_noise_ratio * z;
            let pixels = [
                (u, v),
                (u - NEIGHBOUR, v),
                (u + NEIGHBOUR, v),
                (u, v - NEIGHBOUR),
                (u, v + NEIGHBOUR),
            ];
            let mut seen = [0.0; 5];
            let mut valid = true;
            for (slot, (x, y)) in seen.iter_mut().zip(pixels) {
                let d = read_depth(depth, y * width + x);
                valid &= d > 0.0 && d < 1.0;
                *slot = depth_normalized_to_linear(d, view.near_z, view.far_z);
            }
            if !valid {
                continue;
            }
            if (seen[0] - z).abs() <= margin {
                point.score = (point.score + 1).min(settings.max_score);
                point.hits += 1;
            } else if seen.iter().all(|seen_z| *seen_z > z + margin) {
                point.score -= 1;
                if point.removed() {
                    if point.level(settings) != Level::Pending {
                        *enough -= 1;
                    }
                    *live -= 1;
                    let voxel = voxel_of(point.position, settings);
                    if index.get(&voxel) == Some(&(i as u32)) {
                        index.remove(&voxel);
                    }
                    removed.push(i as u32);
                }
            }
        }
        removed
    }

    /// 定形：估出房间里的平面，把平面上的点对齐并排成网格，清掉重叠的次表面和零散的小簇。
    /// 之后点不再增减。点的位置会变，下标不变；返回被删除的点数。
    pub fn finalize(&mut self, settings: &SurfaceSettings) -> usize {
        self.candidates.clear();
        self.frozen = true;
        let before = self.live;
        let scanned = self.points.len();
        let mut assigned: Vec<bool> = self.points.iter().map(SurfacePoint::removed).collect();
        let mut added = 0;
        for _ in 0..settings.max_planes {
            let Some((plane, inliers)) = self.find_plane(&assigned[..scanned], settings) else {
                break;
            };
            for index in &inliers {
                assigned[*index as usize] = true;
            }
            if inliers.len() >= settings.plane_min_points {
                added += self.align_to_plane(&plane, &inliers, settings);
            }
        }

        let mut grid: HashMap<[i32; 3], Vec<u32>> = HashMap::new();
        for (i, point) in self.points.iter().enumerate() {
            if !point.removed() {
                grid.entry(voxel_of(point.position, settings))
                    .or_default()
                    .push(i as u32);
            }
        }
        for index in self.overlapping_layers(&grid, settings) {
            self.remove(index, settings);
        }
        for index in self.small_clusters(&grid, settings) {
            self.remove(index, settings);
        }
        self.index.clear();
        before + added - self.live
    }

    /// 在还没归属平面的点里找支持点最多的平面：水平面，或法线水平的竖直面。
    fn find_plane(
        &self,
        assigned: &[bool],
        settings: &SurfaceSettings,
    ) -> Option<(Plane, Vec<u32>)> {
        let bin_size = settings.plane_bin;
        let seed_facing = settings.plane_seed_angle.cos();
        let inlier_facing = settings.plane_inlier_angle.cos();
        let free = |i: usize| !assigned[i];
        let mut directions = vec![[0.0, 1.0, 0.0], [0.0, -1.0, 0.0]];
        for step in 0..36 {
            let angle = (step as f32 * 10.0).to_radians();
            directions.push([angle.cos(), 0.0, angle.sin()]);
        }

        let mut best: Option<(u32, [f32; 3], i32)> = None;
        for direction in directions {
            let mut bins: HashMap<i32, u32> = HashMap::new();
            for (i, point) in self.points[..assigned.len()].iter().enumerate() {
                if free(i) && dot(point.normal, direction) > seed_facing {
                    *bins
                        .entry((dot(point.position, direction) / bin_size).floor() as i32)
                        .or_default() += 1;
                }
            }
            for (&bin, &count) in &bins {
                let neighbours = [bin - 1, bin + 1].map(|b| bins.get(&b).copied().unwrap_or(0));
                let score = count + neighbours[0] + neighbours[1];
                let better = match best {
                    None => true,
                    Some((best_score, _, best_bin)) => (score, -bin) > (best_score, -best_bin),
                };
                if better {
                    best = Some((score, direction, bin));
                }
            }
        }
        let (score, direction, bin) = best?;
        if (score as usize) < settings.plane_min_points {
            return None;
        }

        // 用峰值附近的点重新拟合：水平面取平均高度，竖直面在水平面内拟合一条直线。
        let centre = (bin as f32 + 0.5) * bin_size;
        let seeds: Vec<[f32; 3]> = self.points[..assigned.len()]
            .iter()
            .enumerate()
            .filter(|(i, point)| {
                free(*i)
                    && dot(point.normal, direction) > seed_facing
                    && (dot(point.position, direction) - centre).abs() <= bin_size * 1.5
            })
            .map(|(_, point)| point.position)
            .collect();
        let count = seeds.len() as f32;
        let mut mean = [0.0; 3];
        for seed in &seeds {
            for axis in 0..3 {
                mean[axis] += seed[axis] / count;
            }
        }
        let mut normal = direction;
        if direction[1] == 0.0 {
            let (mut sxx, mut sxz, mut szz) = (0.0, 0.0, 0.0);
            for seed in &seeds {
                let (x, z) = (seed[0] - mean[0], seed[2] - mean[2]);
                sxx += x * x;
                sxz += x * z;
                szz += z * z;
            }
            let smallest = (sxx + szz) / 2.0 - (((sxx - szz) / 2.0).powi(2) + sxz * sxz).sqrt();
            let fitted = if sxz.abs() > 1e-9 {
                normalize([sxz, 0.0, smallest - sxx])
            } else if sxx < szz {
                Some([1.0, 0.0, 0.0])
            } else {
                Some([0.0, 0.0, 1.0])
            };
            if let Some(fitted) = fitted {
                let sign = if dot(fitted, direction) < 0.0 {
                    -1.0
                } else {
                    1.0
                };
                // 拟合出的方向偏离太多说明点不成一条线，保留原方向。
                if dot(fitted, direction).abs() > seed_facing {
                    normal = fitted.map(|v| v * sign);
                }
            }
        }
        let plane = Plane {
            normal,
            offset: dot(mean, normal),
        };
        let inliers = self.points[..assigned.len()]
            .iter()
            .enumerate()
            .filter(|(i, point)| {
                free(*i)
                    && dot(point.normal, plane.normal) > inlier_facing
                    && (dot(point.position, plane.normal) - plane.offset).abs()
                        <= settings.plane_distance
            })
            .map(|(i, _)| i as u32)
            .collect();
        Some((plane, inliers))
    }

    /// 把属于平面的点移到平面上，按点间距排成网格，每格只留一个点，并补上被包围的空格。
    /// 返回新补的点数。
    fn align_to_plane(
        &mut self,
        plane: &Plane,
        inliers: &[u32],
        settings: &SurfaceSettings,
    ) -> usize {
        let normal = plane.normal;
        let (right, up) = if normal[1].abs() > 0.9 {
            ([1.0, 0.0, 0.0], [0.0, 0.0, 1.0])
        } else {
            (
                normalize(cross([0.0, 1.0, 0.0], normal)).unwrap_or([1.0, 0.0, 0.0]),
                [0.0, 1.0, 0.0],
            )
        };
        let mut cells: HashMap<[i32; 2], u32> = HashMap::new();
        for &index in inliers {
            let position = self.points[index as usize].position;
            let cell =
                [right, up].map(|axis| (dot(position, axis) / settings.spacing).floor() as i32);
            match cells.get(&cell).copied() {
                None => {
                    cells.insert(cell, index);
                }
                Some(other) => {
                    let (keep, drop) =
                        if self.points[index as usize].hits > self.points[other as usize].hits {
                            (index, other)
                        } else {
                            (other, index)
                        };
                    let dropped = self.points[drop as usize];
                    let kept = &mut self.points[keep as usize];
                    kept.hits += dropped.hits;
                    kept.painted |= dropped.painted;
                    kept.close |= dropped.close;
                    self.remove(drop, settings);
                    cells.insert(cell, keep);
                }
            }
        }
        let centre = |cell: [i32; 2]| {
            let (a, b) = (
                (cell[0] as f32 + 0.5) * settings.spacing,
                (cell[1] as f32 + 0.5) * settings.spacing,
            );
            [0, 1, 2].map(|axis| normal[axis] * plane.offset + right[axis] * a + up[axis] * b)
        };
        for (&cell, &index) in &cells {
            let point = &mut self.points[index as usize];
            point.position = centre(cell);
            point.normal = normal;
        }

        // 八个邻格里至少五个有点的空格补一个点。
        let mut empty: HashMap<[i32; 2], u32> = HashMap::new();
        for cell in cells.keys() {
            for dx in -1..=1 {
                for dy in -1..=1 {
                    let neighbour = [cell[0] + dx, cell[1] + dy];
                    if !cells.contains_key(&neighbour) {
                        *empty.entry(neighbour).or_default() += 1;
                    }
                }
            }
        }
        let mut holes: Vec<[i32; 2]> = empty
            .into_iter()
            .filter(|(_, occupied)| *occupied >= settings.plane_fill_neighbours)
            .map(|(cell, _)| cell)
            .collect();
        holes.sort_unstable();
        let mut added = 0;
        for cell in holes {
            if self.points.len() >= settings.max_points {
                break;
            }
            self.points.push(SurfacePoint {
                position: centre(cell),
                normal,
                painted: 0,
                close: false,
                score: settings.max_score.min(settings.confirm_hits as u8).max(1),
                hits: settings.confirm_hits,
                hidden: false,
            });
            self.live += 1;
            added += 1;
        }
        added
    }

    fn remove(&mut self, index: u32, settings: &SurfaceSettings) {
        let point = &mut self.points[index as usize];
        if point.removed() {
            return;
        }
        point.score = 0;
        if point.level(settings) != Level::Pending {
            self.enough -= 1;
        }
        self.live -= 1;
        let voxel = voxel_of(point.position, settings);
        if self.index.get(&voxel) == Some(&index) {
            self.index.remove(&voxel);
        }
    }

    /// `grid`（按体素分好的未删除的点）里，与 `index` 号点相距在 `range` 以内的其他未删除的点。
    fn neighbours(
        &self,
        grid: &HashMap<[i32; 3], Vec<u32>>,
        index: u32,
        range: f32,
        settings: &SurfaceSettings,
    ) -> Vec<u32> {
        let position = self.points[index as usize].position;
        let voxel = voxel_of(position, settings);
        let reach = (range / settings.spacing).ceil() as i32;
        let mut found = Vec::new();
        for dx in -reach..=reach {
            for dy in -reach..=reach {
                for dz in -reach..=reach {
                    let key = [voxel[0] + dx, voxel[1] + dy, voxel[2] + dz];
                    for &other in grid.get(&key).into_iter().flatten() {
                        let candidate = &self.points[other as usize];
                        let offset = sub(candidate.position, position);
                        if other != index
                            && !candidate.removed()
                            && dot(offset, offset) <= range * range
                        {
                            found.push(other);
                        }
                    }
                }
            }
        }
        found
    }

    /// 找出与另一个点前后重叠、且命中次数较少的点。
    fn overlapping_layers(
        &self,
        grid: &HashMap<[i32; 3], Vec<u32>>,
        settings: &SurfaceSettings,
    ) -> Vec<u32> {
        let [nearest, farthest] = settings.layer_depth;
        let lateral_limit = settings.spacing * settings.min_spacing_ratio;
        let range = (farthest * farthest + lateral_limit * lateral_limit).sqrt();
        let mut losers = Vec::new();
        for (i, point) in self.points.iter().enumerate() {
            if point.removed() {
                continue;
            }
            let weaker = self
                .neighbours(grid, i as u32, range, settings)
                .into_iter()
                .any(|other| {
                    let stronger = &self.points[other as usize];
                    let offset = sub(stronger.position, point.position);
                    let along = dot(offset, point.normal);
                    let lateral = (dot(offset, offset) - along * along).max(0.0).sqrt();
                    lateral < lateral_limit
                        && along.abs() >= nearest
                        && along.abs() <= farthest
                        && dot(point.normal, stronger.normal) > settings.layer_angle.cos()
                        && (stronger.hits, other) > (point.hits, i as u32)
                });
            if weaker {
                losers.push(i as u32);
            }
        }
        losers
    }

    /// 找出所在的簇太小的点。
    fn small_clusters(
        &self,
        grid: &HashMap<[i32; 3], Vec<u32>>,
        settings: &SurfaceSettings,
    ) -> Vec<u32> {
        let mut visited = vec![false; self.points.len()];
        let mut small = Vec::new();
        for start in 0..self.points.len() {
            if visited[start] || self.points[start].removed() {
                continue;
            }
            visited[start] = true;
            let mut cluster = vec![start as u32];
            let mut next = 0;
            while next < cluster.len() {
                for other in self.neighbours(grid, cluster[next], settings.cluster_link, settings) {
                    if !visited[other as usize] {
                        visited[other as usize] = true;
                        cluster.push(other);
                    }
                }
                next += 1;
            }
            if cluster.len() < settings.min_cluster {
                small.extend(cluster);
            }
        }
        small
    }

    fn has_point_near(&self, point: [f32; 3], voxel: [i32; 3], radius: f32) -> bool {
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    let key = [voxel[0] + dx, voxel[1] + dy, voxel[2] + dz];
                    if let Some(&other) = self.index.get(&key) {
                        let offset = sub(self.points[other as usize].position, point);
                        if dot(offset, offset) < radius * radius {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    /// 记下一张照片涂到了哪些点的哪些方向，返回状态有变化的点的下标。
    ///
    /// 照片的视场是以 `view` 的位姿为中心、半角正切为 `half_tan`（水平、垂直）的矩形；
    /// `depth` 是同一时刻的深度图，用来排除被遮挡的点。
    pub fn mark_photo(
        &mut self,
        view: &DepthView,
        depth: &[u8],
        half_tan: [f32; 2],
        settings: &SurfaceSettings,
    ) -> Vec<u32> {
        let (width, height) = (view.width as usize, view.height as usize);
        if depth.len() < width * height * 4 {
            return Vec::new();
        }
        let q = view.pose.rotation;
        let inverse = [-q[0], -q[1], -q[2], q[3]];
        let (left, right) = (view.fov_left.tan(), view.fov_right.tan());
        let (up, down) = (view.fov_up.tan(), view.fov_down.tan());

        let dome = dome_directions();
        let mut changed = Vec::new();
        for (i, point) in self.points.iter_mut().enumerate() {
            if !point.shown() {
                continue;
            }
            let offset = sub(point.position, view.pose.position);
            let local = rotate(inverse, offset);
            let z = -local[2];
            if !(z > view.near_z && z <= settings.max_paint_distance) {
                continue;
            }
            let (x, y) = (local[0] / z, local[1] / z);
            if x.abs() > half_tan[0] || y.abs() > half_tan[1] {
                continue;
            }
            let u = (x - left) / (right - left) * width as f32;
            let v = (y - up) / (down - up) * height as f32;
            if !(u >= 0.0 && v >= 0.0 && (u as usize) < width && (v as usize) < height) {
                continue;
            }
            let d = read_depth(depth, v as usize * width + u as usize);
            if !(d > 0.0 && d < 1.0) {
                continue;
            }
            let seen_z = depth_normalized_to_linear(d, view.near_z, view.far_z);
            if seen_z < z - (settings.occlusion_margin + settings.depth_noise_ratio * z) {
                continue;
            }
            let distance = dot(offset, offset).sqrt();
            let to_camera = offset.map(|v| -v / distance);
            let Some(mask) = point.paint_mask(to_camera, &dome, settings) else {
                continue;
            };
            let before = *point;
            point.painted |= mask;
            point.close |= distance < settings.close_distance;
            if *point != before {
                if before.level(settings) == Level::Pending
                    && point.level(settings) != Level::Pending
                {
                    self.enough += 1;
                }
                changed.push(i as u32);
            }
        }
        changed
    }
}

/// 世界坐标里的点投到深度图上时，它沿视线的距离和该像素读到的距离；不在视野内或读数无效时为 None。
fn depth_at(view: &DepthView, depth: &[u8], position: [f32; 3]) -> Option<(f32, f32)> {
    let (width, height) = (view.width as usize, view.height as usize);
    if depth.len() < width * height * 4 {
        return None;
    }
    let q = view.pose.rotation;
    let local = rotate(
        [-q[0], -q[1], -q[2], q[3]],
        sub(position, view.pose.position),
    );
    let z = -local[2];
    if z <= view.near_z {
        return None;
    }
    let (left, right) = (view.fov_left.tan(), view.fov_right.tan());
    let (up, down) = (view.fov_up.tan(), view.fov_down.tan());
    let u = (local[0] / z - left) / (right - left) * width as f32;
    let v = (local[1] / z - up) / (down - up) * height as f32;
    if !(u >= 0.0 && v >= 0.0 && (u as usize) < width && (v as usize) < height) {
        return None;
    }
    let d = read_depth(depth, v as usize * width + u as usize);
    (d > 0.0 && d < 1.0).then(|| (z, depth_normalized_to_linear(d, view.near_z, view.far_z)))
}

/// 位置所在的体素，体素边长是点间距。
fn voxel_of(position: [f32; 3], settings: &SurfaceSettings) -> [i32; 3] {
    position.map(|v| (v / settings.spacing).floor() as i32)
}

fn read_depth(depth: &[u8], pixel: usize) -> f32 {
    let offset = pixel * 4;
    f32::from_le_bytes([
        depth[offset],
        depth[offset + 1],
        depth[offset + 2],
        depth[offset + 3],
    ])
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normalize(v: [f32; 3]) -> Option<[f32; 3]> {
    let length = dot(v, v).sqrt();
    (length > 1e-9).then(|| v.map(|c| c / length))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::records::Pose;

    const SIZE: u32 = 128;

    fn view_at(position: [f32; 3], yaw: f32) -> DepthView {
        DepthView {
            pose: Pose {
                position,
                rotation: [0.0, (yaw / 2.0).sin(), 0.0, (yaw / 2.0).cos()],
            },
            fov_left: -std::f32::consts::FRAC_PI_4,
            fov_right: std::f32::consts::FRAC_PI_4,
            fov_up: std::f32::consts::FRAC_PI_4,
            fov_down: -std::f32::consts::FRAC_PI_4,
            near_z: 0.1,
            far_z: f32::INFINITY,
            width: SIZE,
            height: SIZE,
        }
    }

    /// 整幅都是线性深度 z 的深度图。
    fn flat(z: f32) -> Vec<u8> {
        std::iter::repeat_n((1.0 - 0.1 / z).to_le_bytes(), (SIZE * SIZE) as usize)
            .flatten()
            .collect()
    }

    fn front() -> DepthView {
        view_at([0.0, 1.5, 0.0], 0.0)
    }

    /// 从原点正对 -Z 方向 2 米处的墙连扫几帧，直到表面点被确认。
    fn wall_map(settings: &SurfaceSettings) -> SurfaceMap {
        let mut map = SurfaceMap::default();
        for frame in 0..4 {
            map.fuse(&front(), &flat(2.0), frame as f64 * 0.2, &[], settings);
        }
        map
    }

    const HALF_TAN: [f32; 2] = [0.7, 0.5];

    #[test]
    fn confirms_points_after_several_frames() {
        let settings = SurfaceSettings::default();
        let mut map = SurfaceMap::default();
        for frame in 0..3 {
            map.fuse(&front(), &flat(2.0), frame as f64 * 0.2, &[], &settings);
            assert!(map.points().is_empty());
        }
        map.fuse(&front(), &flat(2.0), 0.6, &[], &settings);
        let count = map.points().len();
        // 4 米见方的墙，10 厘米间距，最多约 1600 个点。
        assert!(count > 400 && count <= 1700, "{count} points");
        assert_eq!(map.live_count(), count);
        for point in map.points() {
            assert!((point.position[2] + 2.0).abs() < 1e-3);
            assert!(point.normal[2] > 0.99, "normal {:?}", point.normal);
            assert_eq!(point.painted, 0);
        }
        let points = map.points();
        for (i, a) in points.iter().enumerate().take(200) {
            for b in &points[i + 1..] {
                let offset = sub(a.position, b.position);
                assert!(
                    dot(offset, offset).sqrt()
                        >= settings.spacing * settings.min_spacing_ratio - 1e-4
                );
            }
        }
        // 同一视角再扫几次不增加点。
        for frame in 4..10 {
            map.fuse(&front(), &flat(2.0), frame as f64 * 0.2, &[], &settings);
        }
        assert_eq!(map.points().len(), count);
        // 太远的表面不生成点。
        let mut far = SurfaceMap::default();
        for frame in 0..6 {
            far.fuse(&front(), &flat(6.0), frame as f64 * 0.2, &[], &settings);
        }
        assert!(far.points().is_empty());
    }

    #[test]
    fn ignores_surfaces_seen_too_briefly_or_too_rarely() {
        let settings = SurfaceSettings::default();
        // 同一时刻的左右眼只算一次命中。
        let mut map = SurfaceMap::default();
        for _ in 0..8 {
            map.fuse(&front(), &flat(2.0), 1.0, &[], &settings);
        }
        assert!(map.points().is_empty());
        // 两次命中之间隔得太久，候选被丢弃。
        let mut map = SurfaceMap::default();
        for frame in 0..8 {
            map.fuse(&front(), &flat(2.0), frame as f64 * 3.0, &[], &settings);
        }
        assert!(map.points().is_empty());
    }

    #[test]
    fn merges_noisy_layers_into_one() {
        let settings = SurfaceSettings::default();
        let single = wall_map(&settings).points().len();
        let mut map = SurfaceMap::default();
        for frame in 0..12 {
            let z = if frame % 2 == 0 { 2.0 } else { 2.05 };
            map.fuse(&front(), &flat(z), frame as f64 * 0.2, &[], &settings);
        }
        let count = map.points().len();
        assert!(
            count > single / 2 && count <= single * 6 / 5,
            "{count} vs {single}"
        );
        for point in map.points() {
            assert!(point.position[2] <= -1.999 && point.position[2] >= -2.051);
        }
    }

    #[test]
    fn removes_points_that_depth_sees_through() {
        let settings = SurfaceSettings::default();
        let mut map = wall_map(&settings);
        let wall = map.points().len();
        let central = |point: &SurfacePoint| {
            point.position[0].abs() < 1.5 && (point.position[1] - 1.5).abs() < 1.5
        };
        // 被前面的东西挡住时不删。
        for frame in 0..8 {
            assert!(
                map.fuse(
                    &front(),
                    &flat(1.0),
                    1.0 + frame as f64 * 0.2,
                    &[],
                    &settings
                )
                .is_empty()
            );
        }
        assert!(map.points()[..wall].iter().all(|point| !point.removed()));
        // 墙的位置被看穿几次后，原来的点被删掉。
        let mut removed = Vec::new();
        for frame in 0..8 {
            removed.extend(map.fuse(
                &front(),
                &flat(3.2),
                10.0 + frame as f64 * 0.2,
                &[],
                &settings,
            ));
        }
        assert!(!removed.is_empty());
        for (i, point) in map.points()[..wall].iter().enumerate() {
            assert_eq!(removed.contains(&(i as u32)), point.removed());
            if central(point) {
                assert!(point.removed());
            }
        }
        let live = map.points().iter().filter(|point| !point.removed()).count();
        assert_eq!(map.live_count(), live);
        // 后面 3.2 米处的表面成为新的点。
        assert!(
            map.points()[wall..]
                .iter()
                .any(|point| (point.position[2] + 3.2).abs() < 1e-3)
        );
    }

    #[test]
    fn skips_samples_near_the_controllers() {
        let settings = SurfaceSettings::default();
        let controller = [0.0, 1.5, -2.0];
        let mut map = SurfaceMap::default();
        for frame in 0..4 {
            map.fuse(
                &front(),
                &flat(2.0),
                frame as f64 * 0.2,
                &[controller],
                &settings,
            );
        }
        assert!(!map.points().is_empty());
        for point in map.points() {
            let offset = sub(point.position, controller);
            assert!(dot(offset, offset).sqrt() > settings.mask_radius);
        }
    }

    #[test]
    fn ignores_depth_that_is_too_close() {
        let settings = SurfaceSettings::default();
        let mut map = SurfaceMap::default();
        for frame in 0..6 {
            map.fuse(&front(), &flat(0.8), frame as f64 * 0.2, &[], &settings);
            assert!(map.too_close());
        }
        assert!(map.points().is_empty());
        map.fuse(&front(), &flat(2.0), 2.0, &[], &settings);
        assert!(!map.too_close());
    }

    /// 直接放进一个已确认的点。
    fn insert(map: &mut SurfaceMap, position: [f32; 3], hits: u32, settings: &SurfaceSettings) {
        map.index
            .insert(voxel_of(position, settings), map.points.len() as u32);
        map.points.push(SurfacePoint {
            position,
            normal: [0.0, 0.0, 1.0],
            painted: 0,
            close: false,
            score: 4,
            hits,
            hidden: false,
        });
        map.live += 1;
    }

    #[test]
    fn finalize_removes_stray_clusters_and_weaker_layers() {
        let settings = SurfaceSettings::default();
        let mut map = SurfaceMap::default();
        // 一面 1 米见方的墙。
        for row in 0..10 {
            for col in 0..10 {
                insert(
                    &mut map,
                    [col as f32 * 0.1 + 0.05, row as f32 * 0.1 + 0.05, -2.0],
                    20,
                    &settings,
                );
            }
        }
        // 墙前 15 厘米处命中较少的一小片次表面，足够大，不会被当成小簇。
        for row in 0..4 {
            for col in 0..4 {
                insert(
                    &mut map,
                    [col as f32 * 0.1 + 0.05, row as f32 * 0.1 + 0.05, -1.85],
                    5,
                    &settings,
                );
            }
        }
        // 远处三个零散的点。
        for step in 0..3 {
            insert(
                &mut map,
                [3.0 + step as f32 * 0.1, 0.05, -2.0],
                20,
                &settings,
            );
        }
        assert_eq!(map.finalize(&settings), 19);
        assert_eq!(map.live_count(), 100);
        for (i, point) in map.points().iter().enumerate() {
            assert_eq!(point.removed(), i >= 100);
        }
        // 定形之后不再增减。
        assert!(map.frozen());
        for frame in 0..8 {
            assert!(
                map.fuse(&front(), &flat(3.0), frame as f64 * 0.2, &[], &settings)
                    .is_empty()
            );
        }
        assert_eq!(map.points().len(), 119);
        assert_eq!(map.live_count(), 100);
        map.clear();
        assert!(!map.frozen());
    }

    #[test]
    fn finalize_aligns_noisy_walls_to_a_grid() {
        let settings = SurfaceSettings::default();
        let mut map = SurfaceMap::default();
        // 一面 1.5 米见方、绕竖直轴转了 20 度的墙，点的位置前后左右都有偏差，中间缺一个点。
        let yaw = 20f32.to_radians();
        let normal = [yaw.sin(), 0.0, yaw.cos()];
        let along = [yaw.cos(), 0.0, -yaw.sin()];
        let mut noise = 0u32;
        let mut jitter = |scale: f32| {
            noise = noise.wrapping_mul(1664525).wrapping_add(1013904223);
            ((noise >> 8) as f32 / (1 << 24) as f32 - 0.5) * 2.0 * scale
        };
        for row in 0..15 {
            for col in 0..15 {
                if (row, col) == (7, 7) {
                    continue;
                }
                let (a, b) = (
                    col as f32 * 0.1 + 0.05 + jitter(0.02),
                    row as f32 * 0.1 + 0.05 + jitter(0.02),
                );
                let depth = -2.0 + jitter(0.06);
                let position = [0, 1, 2].map(|axis| {
                    along[axis] * a + normal[axis] * depth + if axis == 1 { b } else { 0.0 }
                });
                let index = map.points.len() as u32;
                insert(&mut map, position, 10, &settings);
                map.points[index as usize].normal =
                    normalize([normal[0] + jitter(0.15), jitter(0.15), normal[2]]).unwrap();
            }
        }
        let wall = map.points().len();
        // 旁边一堆朝向各异的杂物。
        let clutter: Vec<[f32; 3]> = (0..12)
            .map(|step| {
                [
                    3.0 + step as f32 * 0.1,
                    0.55,
                    1.0 + (step % 3) as f32 * 0.03,
                ]
            })
            .collect();
        for (step, position) in clutter.iter().enumerate() {
            let index = map.points.len();
            insert(&mut map, *position, 10, &settings);
            map.points[index].normal =
                [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]][step % 3];
        }
        map.finalize(&settings);

        let on_wall: Vec<&SurfacePoint> = map
            .points()
            .iter()
            .enumerate()
            .filter(|(i, point)| !point.removed() && !(wall..wall + 12).contains(i))
            .map(|(_, point)| point)
            .collect();
        // 墙上的点都在同一个平面上，法线一致，位置互不重复，缺的那一格被补上。
        assert!(
            on_wall.len() >= 200 && on_wall.len() <= 260,
            "{} points",
            on_wall.len()
        );
        let offset = dot(on_wall[0].position, on_wall[0].normal);
        assert!((offset + 2.0).abs() < 0.03, "offset {offset}");
        assert!(dot(on_wall[0].normal, normal) > 0.999);
        for (i, point) in on_wall.iter().enumerate() {
            assert_eq!(point.normal, on_wall[0].normal);
            assert!((dot(point.position, point.normal) - offset).abs() < 1e-4);
            for other in &on_wall[i + 1..] {
                let gap = sub(point.position, other.position);
                assert!(dot(gap, gap).sqrt() > settings.spacing * 0.99);
            }
        }
        let hole = [0, 1, 2].map(|axis| along[axis] * 0.75 + if axis == 1 { 0.75 } else { 0.0 });
        assert!(on_wall.iter().any(|point| {
            let gap = sub(point.position, hole);
            let lateral = dot(gap, gap) - dot(gap, normal).powi(2);
            lateral.sqrt() < settings.spacing
        }));
        // 杂物保持原位。
        for (step, position) in clutter.iter().enumerate() {
            assert!(!map.points()[wall + step].removed());
            assert_eq!(map.points()[wall + step].position, *position);
        }
    }

    #[test]
    fn paints_only_points_inside_the_photo() {
        let settings = SurfaceSettings::default();
        let mut map = wall_map(&settings);
        let changed = map.mark_photo(&front(), &flat(2.0), HALF_TAN, &settings);
        assert!(!changed.is_empty());
        for (i, point) in map.points().iter().enumerate() {
            let inside = (point.position[0] / 2.0).abs() <= HALF_TAN[0]
                && ((point.position[1] - 1.5) / 2.0).abs() <= HALF_TAN[1];
            assert_eq!(changed.contains(&(i as u32)), inside);
            assert_eq!(point.painted != 0, inside);
            // 画面中心的点是正对着拍的，涂到法线方向。
            if point.position[0].abs() < 0.3 && (point.position[1] - 1.5).abs() < 0.3 {
                assert_eq!(point.painted & 1, 1);
            }
        }
        // 同一张照片再记一次没有变化。
        assert!(
            map.mark_photo(&front(), &flat(2.0), HALF_TAN, &settings)
                .is_empty()
        );
    }

    #[test]
    fn paints_the_side_facing_the_camera() {
        let settings = SurfaceSettings::default();
        let dome = dome_directions();
        for direction in dome {
            assert!((dot(direction, direction) - 1.0).abs() < 1e-5);
        }
        let point = SurfacePoint {
            position: [0.0, 1.5, -2.0],
            normal: [0.0, 0.0, 1.0],
            painted: 0,
            close: false,
            score: 1,
            hits: 1,
            hidden: false,
        };
        let mask = |camera: [f32; 3]| {
            let to_camera = normalize(sub(camera, point.position)).unwrap();
            point.paint_mask(to_camera, &dome, &settings)
        };
        // 涂到的方向都偏向相机所在的一侧：axis 0 是右，1 是上。
        let all_on_side = |mask: u32, axis: usize, sign: f32| {
            mask != 0
                && (0..DOME_VERTICES)
                    .filter(|vertex| mask & (1 << vertex) != 0)
                    .all(|vertex| dome[vertex][axis] * sign > 0.0)
        };
        assert_eq!(mask([0.0, 1.5, 0.0]), Some(1));
        assert!(all_on_side(mask([2.0, 1.5, 0.0]).unwrap(), 0, 1.0));
        assert!(all_on_side(mask([-2.0, 1.5, 0.0]).unwrap(), 0, -1.0));
        assert!(all_on_side(mask([0.0, 3.5, 0.0]).unwrap(), 1, 1.0));
        assert!(all_on_side(mask([0.0, -0.5, 0.0]).unwrap(), 1, -1.0));
        // 掠射角和背面都不算。
        assert_eq!(mask([8.0, 1.5, -1.0]), None);
        assert_eq!(mask([0.0, 1.5, -4.0]), None);
        // 70 度以内的任何视线都至少涂到一个方向。
        for polar in [10.0f32, 30.0, 50.0, 69.0] {
            for azimuth in 0..36 {
                let (polar, azimuth) = (polar.to_radians(), (azimuth as f32 * 10.0).to_radians());
                let to_camera = [
                    polar.sin() * azimuth.cos(),
                    polar.sin() * azimuth.sin(),
                    polar.cos(),
                ];
                assert_ne!(point.paint_mask(to_camera, &dome, &settings), Some(0));
            }
        }
    }

    #[test]
    fn does_not_paint_from_too_far() {
        let settings = SurfaceSettings::default();
        let mut map = SurfaceMap::default();
        for frame in 0..4 {
            map.fuse(&front(), &flat(3.0), frame as f64 * 0.2, &[], &settings);
        }
        assert!(!map.points().is_empty());
        assert!(
            map.mark_photo(&front(), &flat(3.0), HALF_TAN, &settings)
                .is_empty()
        );
    }

    #[test]
    fn skips_occluded_points() {
        let settings = SurfaceSettings::default();
        let mut map = wall_map(&settings);
        // 拍照时前方 1 米处有东西挡着。
        let changed = map.mark_photo(&front(), &flat(1.0), HALF_TAN, &settings);
        assert!(changed.is_empty());
    }

    /// 2 米远、正对前方相机的一小片墙上的点，`hidden` 决定是否隐藏。
    fn loaded_wall(hidden: bool, settings: &SurfaceSettings) -> SurfaceMap {
        let points: Vec<RoomPoint> = (0..5)
            .flat_map(|row| {
                (0..5).map(move |col| RoomPoint {
                    position: [col as f32 * 0.1 - 0.15, 1.35 + row as f32 * 0.1, -2.0],
                    normal: [0.0, 0.0, 1.0],
                    hidden,
                })
            })
            .collect();
        let mut map = SurfaceMap::default();
        map.load(&points, settings);
        map
    }

    #[test]
    fn hidden_points_are_not_counted_or_painted() {
        let settings = SurfaceSettings::default();
        let mut map = loaded_wall(true, &settings);
        assert_eq!((map.live_count(), map.hidden_count()), (0, 25));
        assert!(
            map.mark_photo(&front(), &flat(2.0), HALF_TAN, &settings)
                .is_empty()
        );
        assert!(map.points().iter().all(|point| !point.shown()));
    }

    #[test]
    fn hidden_points_appear_after_depth_sees_them_twice() {
        let settings = SurfaceSettings::default();
        let mut map = loaded_wall(true, &settings);
        let depth = flat(2.0);
        assert!(map.reveal(&[(&front(), &depth)], &settings).is_empty());
        assert_eq!(map.hidden_count(), 25);
        let shown = map.reveal(&[(&front(), &depth)], &settings);
        assert_eq!(shown.len(), 25);
        assert_eq!((map.live_count(), map.hidden_count()), (25, 0));
        // 显示出来之后照片照常涂色。
        assert_eq!(
            map.mark_photo(&front(), &depth, HALF_TAN, &settings).len(),
            25
        );
    }

    #[test]
    fn hidden_points_stay_hidden_behind_something() {
        let settings = SurfaceSettings::default();
        let mut map = loaded_wall(true, &settings);
        // 前方 1 米处有东西挡着，或者那里其实是空的、读数远得多，都不显示。
        for depth in [flat(1.0), flat(3.5)] {
            for _ in 0..4 {
                assert!(map.reveal(&[(&front(), &depth)], &settings).is_empty());
            }
        }
        // 从背面看过去也不算。
        let behind = view_at([0.0, 1.5, -4.0], std::f32::consts::PI);
        for _ in 0..4 {
            assert!(map.reveal(&[(&behind, &flat(2.0))], &settings).is_empty());
        }
        assert_eq!(map.hidden_count(), 25);
    }

    #[test]
    fn loaded_points_that_are_not_hidden_count_right_away() {
        let settings = SurfaceSettings::default();
        let mut map = loaded_wall(false, &settings);
        assert_eq!((map.live_count(), map.hidden_count()), (25, 0));
        assert!(map.reveal(&[(&front(), &flat(2.0))], &settings).is_empty());
    }

    #[test]
    fn levels_follow_painted_area_and_distance() {
        let settings = SurfaceSettings::default();
        let mut point = SurfacePoint {
            position: [0.0; 3],
            normal: [0.0, 0.0, 1.0],
            painted: 0b111,
            close: true,
            score: 1,
            hits: 1,
            hidden: false,
        };
        assert_eq!(point.level(&settings), Level::Pending);
        // 法线方向、整个内圈加中圈的六个方向约占半球的 51%。
        point.close = false;
        point.painted = 0b111_1111_1111_1111;
        assert!((point.painted_area() - 0.5091).abs() < 1e-3);
        assert_eq!(point.level(&settings), Level::Enough);
        point.close = true;
        assert_eq!(point.level(&settings), Level::Enough);
        point.close = false;
        point.painted = (1 << DOME_VERTICES) - 1;
        assert!((point.painted_area() - 1.0).abs() < 1e-3);
        assert_eq!(point.level(&settings), Level::Enough);
        point.close = true;
        assert_eq!(point.level(&settings), Level::Detailed);
    }

    #[test]
    fn counts_points_that_became_enough() {
        let settings = SurfaceSettings {
            enough_area: 0.15,
            max_paint_distance: 4.0,
            ..SurfaceSettings::default()
        };
        let mut map = wall_map(&settings);
        // 正面、左侧、右侧各拍一张，都对着墙的中心。
        map.mark_photo(&front(), &flat(2.0), [0.2, 0.2], &settings);
        assert_eq!(map.enough_count(), 0);
        let yaw = std::f32::consts::FRAC_PI_4;
        let slanted = flat(2.0 * std::f32::consts::SQRT_2);
        map.mark_photo(
            &view_at([2.0, 1.5, 0.0], yaw),
            &slanted,
            [0.2, 0.2],
            &settings,
        );
        map.mark_photo(
            &view_at([-2.0, 1.5, 0.0], -yaw),
            &slanted,
            [0.2, 0.2],
            &settings,
        );
        let enough = map
            .points()
            .iter()
            .filter(|p| p.level(&settings) != Level::Pending)
            .count();
        assert!(enough > 0);
        assert_eq!(map.enough_count(), enough);
        // 拍够的点被删除后不再计数。
        for frame in 0..8 {
            map.fuse(
                &front(),
                &flat(3.9),
                10.0 + frame as f64 * 0.2,
                &[],
                &settings,
            );
        }
        let enough = map
            .points()
            .iter()
            .filter(|p| !p.removed() && p.level(&settings) != Level::Pending)
            .count();
        assert_eq!(map.enough_count(), enough);
        map.clear();
        assert_eq!(map.enough_count(), 0);
        assert!(map.points().is_empty());
    }
}
