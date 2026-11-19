//! 把系统保存的房间（平面和立方体）变成均匀分布的表面点。

use std::{collections::HashSet, fmt::Write};

use crate::{geometry::rotate, records::Pose};

/// 房间里的一块矩形平面：在 `pose` 的局部坐标里位于 z = 0，局部 +Z 或 -Z 朝向房间。
#[derive(Clone, Copy, Debug)]
pub struct RoomPlane {
    pub pose: Pose,
    /// 矩形在局部 XY 平面里的最小角和大小（米）。
    pub offset: [f32; 2],
    pub extent: [f32; 2],
}

/// 房间里的一个立方体（家具等），在 `pose` 的局部坐标里轴对齐。
#[derive(Clone, Copy, Debug)]
pub struct RoomBox {
    pub pose: Pose,
    /// 立方体在局部坐标里的最小角和大小（米）。
    pub offset: [f32; 3],
    pub extent: [f32; 3],
    /// 桌子一类下面是空的：只有顶面是实的，也不挡住它下面的东西。
    pub open_below: bool,
}

/// 生成表面点的参数。
#[derive(Clone, Copy, Debug)]
pub struct RoomSettings {
    /// 点的间距（米）。
    pub spacing: f32,
    /// 两个面相对、相距不超过它（米）时，夹缝两侧的点都隐藏。
    pub facing_gap: f32,
    /// 两个面的法线夹角（弧度）不小于它才算相对。
    pub facing_angle: f32,
}

/// 房间里的一个表面点。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoomPoint {
    pub position: [f32; 3],
    /// 朝向房间一侧的单位法线。
    pub normal: [f32; 3],
    /// 判断为拍不到，先不显示。
    pub hidden: bool,
}

/// 平面或立方体的一个矩形面：从 `corner` 出发沿 `u`、`v` 两条边张成，`normal` 朝向房间。
struct Face {
    corner: [f32; 3],
    u: [f32; 3],
    v: [f32; 3],
    normal: [f32; 3],
    /// 所属的平面或立方体，同一个物体的面之间不互相判断。
    owner: usize,
    /// 是否在这个面上布点；朝下的面不布点，但仍参与判断。
    sampled: bool,
    /// 属于平面，而不是立方体。
    plane: bool,
}

/// 在平面和立方体的面上每隔 `spacing` 米取一个点。立方体挡住的平面点和贴着平面的立方体面不取；
/// 和另一个面相对、中间的缝不超过 `facing_gap` 的点标为隐藏。`viewer` 是房间里的一个位置，用来判断平面朝向。
pub fn room_points(
    planes: &[RoomPlane],
    boxes: &[RoomBox],
    viewer: [f32; 3],
    settings: &RoomSettings,
) -> Vec<RoomPoint> {
    let spacing = settings.spacing;
    let faces = faces(planes, boxes, viewer);
    let mut points = Vec::new();
    for face in faces.iter().filter(|face| face.sampled) {
        let (u_length, v_length) = (dot(face.u, face.u).sqrt(), dot(face.v, face.v).sqrt());
        for a in steps(0.0, u_length, spacing) {
            for b in steps(0.0, v_length, spacing) {
                let position = add(
                    face.corner,
                    add(scale(face.u, a / u_length), scale(face.v, b / v_length)),
                );
                let covered = if face.plane {
                    boxes
                        .iter()
                        .any(|b| !b.open_below && inside_box(b, position, spacing * 0.5))
                } else {
                    planes.iter().any(|p| on_plane(p, position, spacing * 0.7))
                };
                if covered {
                    continue;
                }
                points.push(RoomPoint {
                    position,
                    normal: face.normal,
                    hidden: faces_another(position, face, &faces, settings),
                });
            }
        }
    }
    points.sort_by_key(|point| point.hidden);
    let mut taken = HashSet::new();
    points.retain(|point| taken.insert(point.position.map(|v| (v / spacing).floor() as i32)));
    points
}

/// 平面和立方体的各个面。下面是空的立方体只有顶面。
fn faces(planes: &[RoomPlane], boxes: &[RoomBox], viewer: [f32; 3]) -> Vec<Face> {
    let mut faces = Vec::new();
    for (owner, plane) in planes.iter().enumerate() {
        let corner = to_world(&plane.pose, [plane.offset[0], plane.offset[1], 0.0]);
        let u = rotate(plane.pose.rotation, [plane.extent[0], 0.0, 0.0]);
        let v = rotate(plane.pose.rotation, [0.0, plane.extent[1], 0.0]);
        let centre = add(corner, scale(add(u, v), 0.5));
        let mut normal = rotate(plane.pose.rotation, [0.0, 0.0, 1.0]);
        if dot(normal, sub(viewer, centre)) < 0.0 {
            normal = normal.map(|v| -v);
        }
        faces.push(Face {
            corner,
            u,
            v,
            normal,
            owner,
            sampled: true,
            plane: true,
        });
    }
    for (index, room_box) in boxes.iter().enumerate() {
        for axis in 0..3 {
            let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
            for side in [0.0, 1.0] {
                let mut local_normal = [0.0; 3];
                local_normal[axis] = side * 2.0 - 1.0;
                let normal = rotate(room_box.pose.rotation, local_normal);
                if room_box.open_below && normal[1] <= 0.7 {
                    continue;
                }
                let mut corner = room_box.offset;
                corner[axis] += room_box.extent[axis] * side;
                let (mut u_edge, mut v_edge) = ([0.0; 3], [0.0; 3]);
                u_edge[u] = room_box.extent[u];
                v_edge[v] = room_box.extent[v];
                faces.push(Face {
                    corner: to_world(&room_box.pose, corner),
                    u: rotate(room_box.pose.rotation, u_edge),
                    v: rotate(room_box.pose.rotation, v_edge),
                    normal,
                    owner: planes.len() + index,
                    sampled: normal[1] >= -0.7,
                    plane: false,
                });
            }
        }
    }
    faces
}

/// 从 `face` 上的 `position` 沿法线往外 `facing_gap` 以内，是否有别的物体的面和它相对、并且挡在正前方。
fn faces_another(position: [f32; 3], face: &Face, faces: &[Face], settings: &RoomSettings) -> bool {
    let cos = settings.facing_angle.cos();
    let margin = settings.spacing * 0.5;
    faces.iter().any(|other| {
        let along = dot(face.normal, other.normal);
        if other.owner == face.owner || along > cos {
            return false;
        }
        let distance = dot(sub(other.corner, position), other.normal) / along;
        if !(-margin..=settings.facing_gap).contains(&distance) {
            return false;
        }
        let hit = sub(add(position, scale(face.normal, distance)), other.corner);
        within(dot(hit, other.u), other.u, margin) && within(dot(hit, other.v), other.v, margin)
    })
}

/// 沿边 `edge` 的投影 `projection`（未除以边长）是否落在这条边上，两端各放宽 `margin` 米。
fn within(projection: f32, edge: [f32; 3], margin: f32) -> bool {
    let length = dot(edge, edge).sqrt();
    let along = projection / length;
    along >= -margin && along <= length + margin
}

/// 表面点写成 ASCII PLY：位置、法线和是否隐藏，坐标是参考空间的。
pub fn points_ply(points: &[RoomPoint]) -> String {
    let mut out = format!(
        "ply\nformat ascii 1.0\nelement vertex {}\n\
         property float x\nproperty float y\nproperty float z\n\
         property float nx\nproperty float ny\nproperty float nz\n\
         property uchar hidden\nend_header\n",
        points.len()
    );
    for point in points {
        let ([x, y, z], [nx, ny, nz]) = (point.position, point.normal);
        let _ = writeln!(out, "{x} {y} {z} {nx} {ny} {nz} {}", u8::from(point.hidden));
    }
    out
}

/// 平面和立方体的各个面写成 ASCII PLY，每个面是一个四边形，坐标是参考空间的。
pub fn layout_ply(planes: &[RoomPlane], boxes: &[RoomBox], viewer: [f32; 3]) -> String {
    let faces = faces(planes, boxes, viewer);
    let mut out = format!(
        "ply\nformat ascii 1.0\nelement vertex {}\n\
         property float x\nproperty float y\nproperty float z\n\
         element face {}\nproperty list uchar int vertex_indices\nend_header\n",
        faces.len() * 4,
        faces.len()
    );
    for face in &faces {
        let far = add(face.corner, add(face.u, face.v));
        for [x, y, z] in [
            face.corner,
            add(face.corner, face.u),
            far,
            add(face.corner, face.v),
        ] {
            let _ = writeln!(out, "{x} {y} {z}");
        }
    }
    for i in 0..faces.len() {
        let _ = writeln!(out, "4 {} {} {} {}", i * 4, i * 4 + 1, i * 4 + 2, i * 4 + 3);
    }
    out
}

/// 把 `[start, start + length]` 按 `spacing` 等分后各段的中点。
fn steps(start: f32, length: f32, spacing: f32) -> impl Iterator<Item = f32> {
    let count = (length / spacing).round().max(1.0) as u32;
    let step = length / count as f32;
    (0..count).map(move |i| start + (i as f32 + 0.5) * step)
}

fn to_world(pose: &Pose, local: [f32; 3]) -> [f32; 3] {
    add(rotate(pose.rotation, local), pose.position)
}

fn to_local(pose: &Pose, world: [f32; 3]) -> [f32; 3] {
    let [x, y, z, w] = pose.rotation;
    rotate([-x, -y, -z, w], sub(world, pose.position))
}

/// 点是否落在平面的矩形范围内、且离平面不超过 `margin`。
fn on_plane(plane: &RoomPlane, position: [f32; 3], margin: f32) -> bool {
    let local = to_local(&plane.pose, position);
    local[2].abs() <= margin
        && (0..2).all(|k| {
            local[k] >= plane.offset[k] - margin
                && local[k] <= plane.offset[k] + plane.extent[k] + margin
        })
}

/// 点是否在立方体内部，离每个面都超过 `margin`。
fn inside_box(room_box: &RoomBox, position: [f32; 3], margin: f32) -> bool {
    let local = to_local(&room_box.pose, position);
    (0..3).all(|k| {
        local[k] >= room_box.offset[k] + margin
            && local[k] <= room_box.offset[k] + room_box.extent[k] - margin
    })
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale(a: [f32; 3], s: f32) -> [f32; 3] {
    a.map(|v| v * s)
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDENTITY: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
    const VIEWER: [f32; 3] = [0.0, 1.5, 0.0];
    const SETTINGS: RoomSettings = RoomSettings {
        spacing: 0.1,
        facing_gap: 0.4,
        facing_angle: 150.0 * std::f32::consts::PI / 180.0,
    };

    /// 一面在 z = -2 处、朝 +Z 的墙，宽 2 米高 1 米。
    fn wall() -> RoomPlane {
        RoomPlane {
            pose: Pose {
                position: [0.0, 1.0, -2.0],
                rotation: IDENTITY,
            },
            offset: [-1.0, -0.5],
            extent: [2.0, 1.0],
        }
    }

    /// 边长为 `2 * half` 米、朝上的地板，局部 +Z 转到世界 +Y。
    fn floor(half: f32) -> RoomPlane {
        let q = std::f32::consts::FRAC_1_SQRT_2;
        RoomPlane {
            pose: Pose {
                position: [0.0; 3],
                rotation: [-q, 0.0, 0.0, q],
            },
            offset: [-half, -half],
            extent: [half * 2.0, half * 2.0],
        }
    }

    /// 宽 1 米、高 1 米、深 0.5 米的柜子，背面离墙 `gap` 米，绕竖直轴转 `yaw` 弧度。
    fn cabinet(gap: f32, yaw: f32) -> RoomBox {
        RoomBox {
            pose: Pose {
                position: [0.0, 1.0, -2.0 + gap],
                rotation: [0.0, (yaw / 2.0).sin(), 0.0, (yaw / 2.0).cos()],
            },
            offset: [-0.5, -0.5, 0.0],
            extent: [1.0, 1.0, 0.5],
            open_below: false,
        }
    }

    fn on_wall(point: &RoomPoint) -> bool {
        (point.position[2] + 2.0).abs() < 1e-4 && point.normal[2] > 0.99
    }

    /// 墙上正对柜子背面的那一块。
    fn behind_cabinet(point: &RoomPoint) -> bool {
        on_wall(point) && point.position[0].abs() < 0.4 && (0.6..1.4).contains(&point.position[1])
    }

    #[test]
    fn plane_is_covered_by_a_grid_facing_the_viewer() {
        let points = room_points(&[wall()], &[], VIEWER, &SETTINGS);
        assert_eq!(points.len(), 200);
        assert!(points.iter().all(|p| on_wall(p) && !p.hidden));
    }

    #[test]
    fn plane_normal_flips_towards_the_viewer() {
        let behind = [0.0, 1.5, -4.0];
        let points = room_points(&[wall()], &[], behind, &SETTINGS);
        assert!(points.iter().all(|p| p.normal[2] < -0.99));
    }

    #[test]
    fn wall_behind_a_cabinet_against_it_is_hidden() {
        let points = room_points(&[wall()], &[cabinet(0.0, 0.0)], VIEWER, &SETTINGS);
        // 柜子没有底面的点，贴墙的背面也不取。
        assert!(points.iter().all(|p| p.normal[1] > -0.7));
        assert!(!points.iter().any(|p| p.normal[2] < -0.99));
        assert!(points.iter().any(behind_cabinet));
        assert!(
            points
                .iter()
                .filter(|p| behind_cabinet(p))
                .all(|p| p.hidden)
        );
        // 墙上柜子两边的部分和柜子正面照常显示。
        assert!(
            points
                .iter()
                .filter(|p| on_wall(p) && p.position[0].abs() > 0.6)
                .all(|p| !p.hidden)
        );
        let front: Vec<_> = points
            .iter()
            .filter(|p| (p.position[2] + 1.5).abs() < 1e-4)
            .collect();
        assert!(!front.is_empty() && front.iter().all(|p| !p.hidden));
    }

    #[test]
    fn both_sides_of_a_narrow_gap_are_hidden() {
        let points = room_points(&[wall()], &[cabinet(0.2, 0.0)], VIEWER, &SETTINGS);
        assert!(
            points
                .iter()
                .filter(|p| behind_cabinet(p))
                .all(|p| p.hidden)
        );
        let back: Vec<_> = points.iter().filter(|p| p.normal[2] < -0.99).collect();
        assert!(!back.is_empty() && back.iter().all(|p| p.hidden));
        // 缝够宽时两侧都显示。
        let points = room_points(&[wall()], &[cabinet(0.6, 0.0)], VIEWER, &SETTINGS);
        assert!(points.iter().all(|p| !p.hidden));
    }

    #[test]
    fn faces_at_a_steep_angle_are_not_facing() {
        // 转了 45 度的柜子，离墙最近的面和墙夹 135 度。
        let points = room_points(
            &[wall()],
            &[cabinet(0.2, 45f32.to_radians())],
            VIEWER,
            &SETTINGS,
        );
        assert!(points.iter().all(|p| !p.hidden));
    }

    #[test]
    fn table_keeps_only_its_top_and_the_floor_below() {
        let table = RoomBox {
            pose: Pose {
                position: [0.5, 0.7, 0.0],
                rotation: IDENTITY,
            },
            offset: [-0.5, -0.7, -0.5],
            extent: [1.0, 0.7, 1.0],
            open_below: true,
        };
        let points = room_points(&[floor(1.0)], &[table], VIEWER, &SETTINGS);
        let on_floor = points.iter().filter(|p| p.position[1].abs() < 1e-4).count();
        let on_top = points
            .iter()
            .filter(|p| (p.position[1] - 0.7).abs() < 1e-4)
            .count();
        assert_eq!((on_floor, on_top, points.len()), (400, 100, 500));
        assert!(points.iter().all(|p| !p.hidden));
    }

    #[test]
    fn floor_under_a_low_raised_box_is_hidden() {
        // 离地 0.15 米悬空的床：床底朝下的面不布点，但挡住了它下面的地板。
        let bed = RoomBox {
            pose: Pose {
                position: [-1.0, 0.15, 0.0],
                rotation: IDENTITY,
            },
            offset: [-0.5, 0.0, -1.0],
            extent: [1.0, 0.3, 2.0],
            open_below: false,
        };
        let points = room_points(&[floor(2.0)], &[bed], VIEWER, &SETTINGS);
        let on_floor = |x: (f32, f32), hidden: bool| {
            points
                .iter()
                .filter(|p| {
                    let [px, py, pz] = p.position;
                    py.abs() < 1e-4 && px > x.0 && px < x.1 && pz.abs() < 0.9 && p.hidden == hidden
                })
                .count()
        };
        assert_eq!(on_floor((-1.4, -0.6), false), 0);
        assert!(on_floor((-1.4, -0.6), true) > 0);
        assert_eq!(on_floor((0.0, 2.0), true), 0);
        assert!(points.iter().all(|p| p.normal[1] > -0.7));
    }

    #[test]
    fn ply_headers_match_their_contents() {
        let point = RoomPoint {
            position: [0.0, 1.0, 2.0],
            normal: [0.0, 0.0, 1.0],
            hidden: true,
        };
        let text = points_ply(&[point; 3]);
        assert!(text.contains("element vertex 3\n") && text.contains("property uchar hidden\n"));
        assert_eq!(text.lines().count(), 11 + 3);
        assert!(text.ends_with(" 1\n"));
        let text = layout_ply(&[wall()], &[cabinet(0.0, 0.0)], VIEWER);
        assert!(text.contains("element vertex 28\n") && text.contains("element face 7\n"));
        assert_eq!(text.lines().count(), 9 + 28 + 7);
    }
}
