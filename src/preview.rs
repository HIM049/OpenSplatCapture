use std::collections::{HashMap, HashSet};

use bevy::{
    asset::RenderAssetUsages,
    camera::visibility::NoFrustumCulling,
    mesh::{Indices, PrimitiveTopology},
    prelude::*,
};
use capture_format::{
    Side,
    convert::depth_normalized_to_linear,
    coverage::{
        DOME_VERTICES, Level, RING_SIZE, SurfaceMap, SurfacePoint, SurfaceSettings, dome_directions,
    },
};

use crate::{
    camera::Cameras,
    depth::DepthSystems,
    input::{LeftHandAnchor, RightAimAnchor},
    recorder::{
        AdvanceStage, CaptureMode, DepthFrameCaptured, Recorder, RecordingStarted,
        RecordingStopped, Stage,
    },
    room::RoomLoaded,
    tuning::{
        CHUNK_SIZE, CLOUD_CELL, CLOUD_CHUNK_SIZE, CLOUD_COLOR, CLOUD_MAX_DISTANCE,
        CLOUD_MAX_POINTS, CLOUD_MIN_DISTANCE, CLOUD_PIXEL_STEP, CONTROLLER_MASK_RADIUS,
        DETAILED_COLORS, DOME_LIFT, DOME_RADIUS, ENOUGH_COLORS, FALLBACK_HALF_TAN, FOV_MARGIN,
        MAX_REBUILDS_PER_FRAME, PENDING_COLORS, surface_settings,
    },
};

pub struct PreviewPlugin;

impl Plugin for PreviewPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup).add_systems(
            Update,
            (
                clear_hints,
                load_room,
                finalize_after_scan,
                accumulate,
                rebuild_chunks,
                hide_balls_in_detail_scan,
                scan_details,
                rebuild_cloud,
            )
                .chain()
                .after(DepthSystems),
        );
    }
}

/// 表面点的覆盖状态和显示它们的网格块。
#[derive(Resource)]
pub struct Coverage {
    map: SurfaceMap,
    settings: SurfaceSettings,
    chunks: HashMap<IVec3, Chunk>,
    dirty: HashSet<IVec3>,
    /// 已分到块里的点数。
    assigned: usize,
    material: Handle<StandardMaterial>,
}

impl Coverage {
    /// 已确认的表面点数。
    pub fn total(&self) -> usize {
        self.map.live_count()
    }

    /// 扫描时离表面太近，深度用不上。
    pub fn too_close(&self) -> bool {
        self.map.too_close()
    }

    /// 已拍够的表面点数。
    pub fn enough(&self) -> usize {
        self.map.enough_count()
    }
}

#[derive(Default)]
struct Chunk {
    points: Vec<u32>,
    entity: Option<Entity>,
}

#[derive(Component)]
struct CoverageChunk;

/// 细节扫描阶段拍到的表面，显示成点云。
#[derive(Resource)]
pub struct DetailCloud {
    cells: HashSet<IVec3>,
    chunks: HashMap<IVec3, CloudChunk>,
    dirty: HashSet<IVec3>,
    material: Handle<StandardMaterial>,
}

impl DetailCloud {
    /// 点云里的点数。
    pub fn points(&self) -> usize {
        self.cells.len()
    }
}

#[derive(Default)]
struct CloudChunk {
    positions: Vec<[f32; 3]>,
    entity: Option<Entity>,
}

#[derive(Component)]
struct CloudChunkMesh;

fn setup(mut commands: Commands, mut materials: ResMut<Assets<StandardMaterial>>) {
    let material = materials.add(StandardMaterial {
        unlit: true,
        ..default()
    });
    commands.insert_resource(Coverage {
        map: SurfaceMap::default(),
        settings: surface_settings(),
        chunks: HashMap::new(),
        dirty: HashSet::new(),
        assigned: 0,
        material,
    });
    let material = materials.add(StandardMaterial {
        base_color: CLOUD_COLOR,
        unlit: true,
        ..default()
    });
    commands.insert_resource(DetailCloud {
        cells: HashSet::new(),
        chunks: HashMap::new(),
        dirty: HashSet::new(),
        material,
    });
}

/// 录制开始和结束时清掉表面点和点云。
fn clear_hints(
    mut started: MessageReader<RecordingStarted>,
    mut stopped: MessageReader<RecordingStopped>,
    mut coverage: ResMut<Coverage>,
    mut cloud: ResMut<DetailCloud>,
    chunks: Query<Entity, Or<(With<CoverageChunk>, With<CloudChunkMesh>)>>,
    mut commands: Commands,
) {
    if started.read().count() + stopped.read().count() == 0 {
        return;
    }
    cloud.cells.clear();
    cloud.chunks.clear();
    cloud.dirty.clear();
    coverage.map.clear();
    coverage.chunks.clear();
    coverage.dirty.clear();
    coverage.assigned = 0;
    for chunk in &chunks {
        commands.entity(chunk).despawn();
    }
}

/// 扫描阶段收到从已设定的房间生成的表面点时，用它们直接定形并进入拍摄阶段。
fn load_room(
    mut loaded: MessageReader<RoomLoaded>,
    recorder: Res<Recorder>,
    mut coverage: ResMut<Coverage>,
    mut advance: MessageWriter<AdvanceStage>,
) {
    let Some(room) = loaded.read().last() else {
        return;
    };
    let scanning = recorder
        .active
        .as_ref()
        .is_some_and(|active| active.mode == CaptureMode::ByPose && active.stage == Stage::Scan);
    if !scanning || coverage.map.frozen() {
        return;
    }
    let coverage = &mut *coverage;
    coverage.map.load(&room.0, &coverage.settings);
    info!(
        "preview: loaded {} points from the saved room, {} hidden",
        coverage.map.live_count(),
        coverage.map.hidden_count()
    );
    reassign_chunks(coverage);
    advance.write(AdvanceStage);
}

/// 扫描阶段结束时给表面点定形，之后只涂色。
fn finalize_after_scan(recorder: Res<Recorder>, mut coverage: ResMut<Coverage>) {
    let capturing = recorder
        .active
        .as_ref()
        .is_some_and(|active| active.mode == CaptureMode::ByPose && active.stage != Stage::Scan);
    if !capturing || coverage.map.frozen() {
        return;
    }
    let coverage = &mut *coverage;
    let removed = coverage.map.finalize(&coverage.settings);
    info!(
        "preview: finalized {} points, removed {removed}",
        coverage.map.live_count()
    );
    reassign_chunks(coverage);
}

/// 点被整体替换或移动之后，把所有的点重新分到块里。
fn reassign_chunks(coverage: &mut Coverage) {
    for (key, chunk) in &mut coverage.chunks {
        chunk.points.clear();
        coverage.dirty.insert(*key);
    }
    for (index, point) in coverage.map.points().iter().enumerate() {
        if point.removed() {
            continue;
        }
        let key = chunk_of(point);
        coverage
            .chunks
            .entry(key)
            .or_default()
            .points
            .push(index as u32);
        coverage.dirty.insert(key);
    }
    coverage.assigned = coverage.map.points().len();
}

fn chunk_of(point: &SurfacePoint) -> IVec3 {
    (Vec3::from_array(point.position) / CHUNK_SIZE)
        .floor()
        .as_ivec3()
}

fn in_detail_scan(recorder: &Recorder) -> bool {
    recorder
        .active
        .as_ref()
        .is_some_and(|active| active.mode == CaptureMode::ByPose && active.stage == Stage::Free)
}

/// 手柄的位置，它们附近的深度不算表面。
fn controller_masks<'a>(controllers: impl Iterator<Item = &'a GlobalTransform>) -> Vec<[f32; 3]> {
    controllers
        .map(|transform| transform.translation())
        .filter(|position| *position != Vec3::ZERO)
        .map(|position| position.to_array())
        .collect()
}

/// 把每帧深度融合进表面点，并显示在原处被看到的隐藏点；随照片存盘的帧还记下它拍到了哪些点的哪个方向。
fn accumulate(
    mut frames: MessageReader<DepthFrameCaptured>,
    cameras: Res<Cameras>,
    controllers: Query<&GlobalTransform, Or<(With<LeftHandAnchor>, With<RightAimAnchor>)>>,
    time: Res<Time>,
    mut coverage: ResMut<Coverage>,
) {
    let masks = controller_masks(controllers.iter());
    let now = time.elapsed_secs_f64();
    for frame in frames.read() {
        let coverage = &mut *coverage;
        let eyes = [0, 1].map(|eye| (&frame.views[eye], frame.layers[eye].as_slice()));
        for index in coverage.map.reveal(&eyes, &coverage.settings) {
            coverage
                .dirty
                .insert(chunk_of(&coverage.map.points()[index as usize]));
        }
        for side in Side::BOTH {
            let eye = side.index();
            let (view, layer) = (&frame.views[eye], &frame.layers[eye]);
            for index in coverage
                .map
                .fuse(view, layer, now, &masks, &coverage.settings)
            {
                coverage
                    .dirty
                    .insert(chunk_of(&coverage.map.points()[index as usize]));
            }
            if !frame.saved {
                continue;
            }
            let half_tan = cameras
                .half_fov_tangents(side)
                .unwrap_or(FALLBACK_HALF_TAN)
                .map(|tan| tan * FOV_MARGIN);
            for index in coverage
                .map
                .mark_photo(view, layer, half_tan, &coverage.settings)
            {
                coverage
                    .dirty
                    .insert(chunk_of(&coverage.map.points()[index as usize]));
            }
        }
        let points = coverage.map.points();
        for index in coverage.assigned..points.len() {
            let key = chunk_of(&points[index]);
            coverage
                .chunks
                .entry(key)
                .or_default()
                .points
                .push(index as u32);
            coverage.dirty.insert(key);
        }
        coverage.assigned = points.len();
    }
}

/// 重建状态有变化的块的网格，每帧只处理几块。
fn rebuild_chunks(
    mut coverage: ResMut<Coverage>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut commands: Commands,
) {
    if coverage.dirty.is_empty() {
        return;
    }
    let coverage = &mut *coverage;
    let keys: Vec<IVec3> = coverage
        .dirty
        .iter()
        .take(MAX_REBUILDS_PER_FRAME)
        .copied()
        .collect();
    for key in keys {
        coverage.dirty.remove(&key);
        let Some(chunk) = coverage.chunks.get_mut(&key) else {
            continue;
        };
        let points = coverage.map.points();
        chunk
            .points
            .retain(|index| !points[*index as usize].removed());
        if !chunk
            .points
            .iter()
            .any(|index| points[*index as usize].shown())
        {
            if let Some(entity) = chunk.entity.take() {
                commands.entity(entity).despawn();
            }
            continue;
        }
        let mesh = meshes.add(chunk_mesh(&chunk.points, points, &coverage.settings));
        match chunk.entity {
            Some(entity) => {
                commands.entity(entity).insert(Mesh3d(mesh));
            }
            None => {
                let entity = commands.spawn((
                    Mesh3d(mesh),
                    MeshMaterial3d(coverage.material.clone()),
                    NoFrustumCulling,
                    CoverageChunk,
                ));
                chunk.entity = Some(entity.id());
            }
        }
    }
}

/// 细节扫描阶段不显示表面点的小球。
fn hide_balls_in_detail_scan(
    recorder: Res<Recorder>,
    mut chunks: Query<&mut Visibility, With<CoverageChunk>>,
) {
    let wanted = if in_detail_scan(&recorder) {
        Visibility::Hidden
    } else {
        Visibility::Inherited
    };
    for mut visibility in &mut chunks {
        if *visibility != wanted {
            *visibility = wanted;
        }
    }
}

/// 细节扫描阶段把每张照片的近处深度加进点云，每个小格子只留一个点。
fn scan_details(
    mut frames: MessageReader<DepthFrameCaptured>,
    recorder: Res<Recorder>,
    controllers: Query<&GlobalTransform, Or<(With<LeftHandAnchor>, With<RightAimAnchor>)>>,
    mut cloud: ResMut<DetailCloud>,
) {
    let scanning = in_detail_scan(&recorder);
    let masks = controller_masks(controllers.iter());
    let mask_radius = CONTROLLER_MASK_RADIUS * CONTROLLER_MASK_RADIUS;
    for frame in frames.read() {
        if !scanning || !frame.saved {
            continue;
        }
        let cloud = &mut *cloud;
        for (view, layer) in frame.views.iter().zip(&frame.layers) {
            let (width, height) = (view.width as usize, view.height as usize);
            if layer.len() < width * height * 4 {
                continue;
            }
            for v in (0..height).step_by(CLOUD_PIXEL_STEP) {
                for u in (0..width).step_by(CLOUD_PIXEL_STEP) {
                    if cloud.cells.len() >= CLOUD_MAX_POINTS {
                        return;
                    }
                    let offset = (v * width + u) * 4;
                    let depth = f32::from_le_bytes([
                        layer[offset],
                        layer[offset + 1],
                        layer[offset + 2],
                        layer[offset + 3],
                    ]);
                    let z = depth_normalized_to_linear(depth, view.near_z, view.far_z);
                    if !(CLOUD_MIN_DISTANCE..=CLOUD_MAX_DISTANCE).contains(&z) {
                        continue;
                    }
                    let Some(point) = view.unproject(u as f32 + 0.5, v as f32 + 0.5, depth) else {
                        continue;
                    };
                    let position = Vec3::from_array(point);
                    let masked = masks.iter().any(|mask| {
                        Vec3::from_array(*mask).distance_squared(position) < mask_radius
                    });
                    if masked
                        || !cloud
                            .cells
                            .insert((position / CLOUD_CELL).floor().as_ivec3())
                    {
                        continue;
                    }
                    let key = (position / CLOUD_CHUNK_SIZE).floor().as_ivec3();
                    cloud.chunks.entry(key).or_default().positions.push(point);
                    cloud.dirty.insert(key);
                }
            }
        }
    }
}

/// 重建有新点的点云块。
fn rebuild_cloud(
    mut cloud: ResMut<DetailCloud>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut commands: Commands,
) {
    if cloud.dirty.is_empty() {
        return;
    }
    let cloud = &mut *cloud;
    for key in cloud.dirty.drain() {
        let Some(chunk) = cloud.chunks.get_mut(&key) else {
            continue;
        };
        let mesh = meshes.add(
            Mesh::new(
                PrimitiveTopology::PointList,
                RenderAssetUsages::RENDER_WORLD,
            )
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, chunk.positions.clone()),
        );
        match chunk.entity {
            Some(entity) => {
                commands.entity(entity).insert(Mesh3d(mesh));
            }
            None => {
                let entity = commands.spawn((
                    Mesh3d(mesh),
                    MeshMaterial3d(cloud.material.clone()),
                    NoFrustumCulling,
                    CloudChunkMesh,
                ));
                chunk.entity = Some(entity.id());
            }
        }
    }
}

/// 把一块里显示的每个点画成贴着表面、朝外的半球，顶点颜色表示该方向是否已被照片涂到。
fn chunk_mesh(indices: &[u32], points: &[SurfacePoint], settings: &SurfaceSettings) -> Mesh {
    let dome = dome_directions();
    let mut positions = Vec::with_capacity(indices.len() * DOME_VERTICES);
    let mut colors = Vec::with_capacity(indices.len() * DOME_VERTICES);
    let mut triangles = Vec::with_capacity(indices.len() * 120);
    for &index in indices {
        let point = &points[index as usize];
        if !point.shown() {
            continue;
        }
        let [painted, unpainted] = match point.level(settings) {
            Level::Pending => PENDING_COLORS,
            Level::Enough => ENOUGH_COLORS,
            Level::Detailed => DETAILED_COLORS,
        }
        .map(|color| color.to_linear().to_f32_array());
        let normal = Vec3::from_array(point.normal);
        let (right, up) = point.tangents();
        let (right, up) = (Vec3::from_array(right), Vec3::from_array(up));
        let centre = Vec3::from_array(point.position) + normal * DOME_LIFT;
        let first = positions.len() as u32;
        for (vertex, [x, y, z]) in dome.into_iter().enumerate() {
            let offset = (right * x + up * y + normal * z) * DOME_RADIUS;
            positions.push((centre + offset).to_array());
            colors.push(if point.painted & (1 << vertex) != 0 {
                painted
            } else {
                unpainted
            });
        }
        // 顶点 0 是顶，之后每八个是一圈；外面一圈的第 k 个在里面一圈的第 k 和 k+1 个之间。
        let ring = RING_SIZE as u32;
        for step in 0..ring {
            let next = (step + 1) % ring;
            triangles.extend([first, first + 1 + step, first + 1 + next]);
            for inner_ring in 0..2 {
                let inner_first = first + 1 + inner_ring * ring;
                let outer_first = inner_first + ring;
                let (inner, inner_next) = (inner_first + step, inner_first + next);
                let (outer, outer_next) = (outer_first + step, outer_first + next);
                triangles.extend([inner, outer, inner_next]);
                triangles.extend([inner_next, outer, outer_next]);
            }
        }
    }
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colors)
    .with_inserted_indices(Indices::U32(triangles))
}
