//! 采集和覆盖显示的可调参数。应用里用到的数值都以这里为准。

use std::time::Duration;

use bevy::color::Color;
use capture_format::{coverage::SurfaceSettings, room::RoomSettings};

// ---- 按键 ----

/// 待机时录制键按住这么久开始录制。
pub const LONG_PRESS: Duration = Duration::from_millis(800);
/// 录制中录制键按住这么久进入下一阶段。
pub const ADVANCE_HOLD: Duration = Duration::from_secs(3);
/// 射线点击界面时震动的时长和强度。
pub const CLICK_PULSE: Duration = Duration::from_millis(20);
pub const CLICK_AMPLITUDE: f32 = 0.3;
/// 取消录制的按钮点过一次后，这么久内再点一次才生效。
pub const CANCEL_CONFIRM: Duration = Duration::from_secs(3);
/// 连续几次震动之间的间隔。
pub const PULSE_GAP: Duration = Duration::from_millis(90);

// ---- 拍摄 ----

/// 两次手动拍摄的最小间隔（秒）。
pub const MIN_SHOT_INTERVAL: f64 = 0.1;

// ---- 握住左侧键连续拍摄 ----

/// 握住期间每秒拍多少张。
pub const HOLD_CAPTURE_FPS: f32 = 3.0;
/// 连续拍摄允许的头部移动速度（米每秒）和转动速度（度每秒）上限，超过时跳过这一张。
pub const HOLD_MAX_LINEAR_SPEED: f32 = 0.5;
pub const HOLD_MAX_ANGULAR_SPEED: f32 = 45.0;

// ---- 深度融合的节奏 ----

/// 没有拍照时，隔这么久取一帧只用于融合表面的深度；扫描阶段取得更勤。
pub const DEPTH_INTERVAL: Duration = Duration::from_millis(500);
pub const SCAN_STAGE_DEPTH_INTERVAL: Duration = Duration::from_millis(100);

// ---- 表面点的生成（扫描阶段） ----

/// 深度图上每隔多少像素取一个采样。
pub const PIXEL_STEP: u32 = 4;
/// 表面点的间距（米），两个点最近不小于它的 `MIN_SPACING_RATIO` 倍。
pub const SPACING: f32 = 0.1;
pub const MIN_SPACING_RATIO: f32 = 0.7;
/// 只用这个距离范围（米）内的深度生成点。
pub const MIN_FUSE_DISTANCE: f32 = 1.0;
pub const MAX_FUSE_DISTANCE: f32 = 3.5;
/// 视线与表面法线的夹角（度）超过它的深度不用来生成点。
pub const MAX_FUSE_ANGLE: f32 = 60.0;
/// 相邻采样的深度差超过 `深度 * 该比例` 时视为不连续。
pub const DISCONTINUITY_RATIO: f32 = 0.1;
/// 一帧里太近的深度超过这个比例时提示后退。
pub const TOO_CLOSE_RATIO: f32 = 0.5;
/// 一个位置被这么多帧命中、首末相隔这么多秒以上才成为点；这么多秒没再命中就丢弃。
pub const CONFIRM_HITS: u32 = 4;
pub const CONFIRM_SPAN: f64 = 0.3;
pub const CANDIDATE_TIMEOUT: f64 = 2.0;
/// 沿法线相距在它（米）以内的采样算同一层表面。
pub const MERGE_DEPTH: f32 = 0.08;
/// 离手柄这么近（米）的深度不参与融合。
pub const CONTROLLER_MASK_RADIUS: f32 = 0.15;
/// 点的可信度上限；被看穿一次减一，减到零删除。
pub const MAX_SCORE: u8 = 6;
/// 判断看穿和看到的容差（米），以及深度误差随距离增长的比例。
pub const CARVE_MARGIN: f32 = 0.1;
pub const DEPTH_NOISE_RATIO: f32 = 0.05;
/// 核对已有点时考虑的最远距离（米）。
pub const MAX_CARVE_DISTANCE: f32 = 4.0;
pub const MAX_POINTS: usize = 30_000;

// ---- 定形（进入拍摄阶段时） ----

/// 至少这么多点支持的平面才算数，最多找这么多个平面。
pub const PLANE_MIN_POINTS: usize = 60;
pub const MAX_PLANES: usize = 32;
/// 离平面这么近（米）的点对齐到平面上。
pub const PLANE_DISTANCE: f32 = 0.15;
/// 找平面时沿法线统计点数的格宽（米）。
pub const PLANE_BIN: f32 = 0.05;
/// 参与拟合、参与对齐的点的法线与平面法线的最大夹角（度）。
pub const PLANE_SEED_ANGLE: f32 = 25.0;
pub const PLANE_INLIER_ANGLE: f32 = 35.0;
/// 网格上的空格，八个邻格里至少这么多个有点时补上。
pub const PLANE_FILL_NEIGHBOURS: u32 = 5;
/// 相距这么近（米）的点连成一簇，少于这么多点的簇删除。
pub const CLUSTER_LINK: f32 = 0.2;
pub const MIN_CLUSTER: usize = 8;
/// 沿法线前后相距在这个范围（米）内、法线夹角（度）在 `LAYER_ANGLE` 以内的两个点只留一个。
pub const LAYER_DEPTH: [f32; 2] = [0.05, 0.25];
pub const LAYER_ANGLE: f32 = 45.0;

// ---- 着色和等级 ----

/// 超过这个距离（米）拍到的点不涂色。
pub const MAX_PAINT_DISTANCE: f32 = 2.5;
/// 视线与法线的夹角（度）超过它时不涂色。
pub const MAX_VIEW_ANGLE: f32 = 70.0;
/// 一张照片把半球上与视线夹角（度）在它以内的方向涂上。
pub const PAINT_ANGLE: f32 = 28.0;
/// 点比深度测到的表面远出这么多（米，另加距离的 `DEPTH_NOISE_RATIO`）时算被遮挡。
pub const OCCLUSION_MARGIN: f32 = 0.2;
/// 判断照片拍到哪些点时，相机视场按这个比例缩小；读不到内参时用后面的半视场角正切。
pub const FOV_MARGIN: f32 = 0.9;
pub const FALLBACK_HALF_TAN: [f32; 2] = [0.7, 0.5];
/// 涂到的半球面积比例达到它算拍够。
pub const ENOUGH_AREA: f32 = 0.5;
/// 达到它，并且有 `CLOSE_DISTANCE`（米）以内的照片，算细节充分。
pub const DETAILED_AREA: f32 = 0.75;
pub const CLOSE_DISTANCE: f32 = 1.0;

// ---- 显示 ----

/// 每个表面点画成的半球的半径和底面离开表面的距离（米）。
pub const DOME_RADIUS: f32 = 0.015;
pub const DOME_LIFT: f32 = 0.003;
/// 表面点按这个边长（米）分块，每帧最多重建这么多块的网格。
pub const CHUNK_SIZE: f32 = 1.0;
pub const MAX_REBUILDS_PER_FRAME: usize = 6;
/// 各等级下半球上已涂到和未涂到的方向的颜色。
pub const PENDING_COLORS: [Color; 2] =
    [Color::srgb(1.0, 0.55, 0.05), Color::srgb(0.06, 0.06, 0.07)];
pub const ENOUGH_COLORS: [Color; 2] = [Color::srgb(0.2, 1.0, 0.35), Color::srgb(0.03, 0.22, 0.08)];
pub const DETAILED_COLORS: [Color; 2] = [Color::srgb(0.1, 0.85, 1.0), Color::srgb(0.02, 0.2, 0.3)];

// ---- 细节扫描的点云 ----

/// 深度图上每隔多少像素取一个点，只取这个距离范围（米）内的。
pub const CLOUD_PIXEL_STEP: usize = 2;
pub const CLOUD_MIN_DISTANCE: f32 = 0.3;
pub const CLOUD_MAX_DISTANCE: f32 = 1.5;
/// 每个这么大（米）的格子里只留一个点，总数不超过上限。
pub const CLOUD_CELL: f32 = 0.02;
pub const CLOUD_MAX_POINTS: usize = 400_000;
/// 点云按这个边长（米）分块。
pub const CLOUD_CHUNK_SIZE: f32 = 0.5;
pub const CLOUD_COLOR: Color = Color::srgb(0.3, 0.95, 1.0);

// ---- 视野里的状态圆点 ----

/// 圆点离眼睛的距离（米）、偏向右边和上边的角度（度）以及它的半径（米）。
pub const INDICATOR_DISTANCE: f32 = 0.8;
pub const INDICATOR_YAW: f32 = 18.0;
pub const INDICATOR_PITCH: f32 = 12.0;
pub const INDICATOR_RADIUS: f32 = 0.012;

// ---- 提示窗的跟随 ----

/// 头部转开（度）或走开（米）超过这些值后，提示窗开始跟到视线下方。
pub const HUD_FOLLOW_ANGLE: f32 = 30.0;
pub const HUD_FOLLOW_DISTANCE: f32 = 0.3;
/// 提示窗追上目标位置大约要用的时间（秒），越大越缓。
pub const HUD_SMOOTH_TIME: f32 = 0.35;
/// 离目标位置这么近（度、米）后停止跟随。
pub const HUD_SETTLE_ANGLE: f32 = 2.0;
pub const HUD_SETTLE_DISTANCE: f32 = 0.02;

// ---- 已设定的房间 ----

/// 两个面相对（法线夹角不小于这么多度）、中间的缝不超过这么宽（米）时，缝两侧的点先隐藏。
pub const ROOM_FACING_GAP: f32 = 0.4;
pub const ROOM_FACING_ANGLE: f32 = 150.0;
/// 隐藏的点被这么多帧深度在原处看到后显示出来。
pub const ROOM_REVEAL_HITS: u32 = 2;

/// 用上面的常量组成从已设定的房间生成表面点的参数。
pub fn room_settings() -> RoomSettings {
    RoomSettings {
        spacing: SPACING,
        facing_gap: ROOM_FACING_GAP,
        facing_angle: ROOM_FACING_ANGLE.to_radians(),
    }
}

/// 用上面的常量组成表面覆盖的参数。
pub fn surface_settings() -> SurfaceSettings {
    SurfaceSettings {
        pixel_step: PIXEL_STEP,
        spacing: SPACING,
        max_distance: MAX_CARVE_DISTANCE,
        min_distance: MIN_FUSE_DISTANCE,
        max_fuse_distance: MAX_FUSE_DISTANCE,
        max_fuse_angle: MAX_FUSE_ANGLE.to_radians(),
        occlusion_margin: OCCLUSION_MARGIN,
        carve_margin: CARVE_MARGIN,
        depth_noise_ratio: DEPTH_NOISE_RATIO,
        max_paint_distance: MAX_PAINT_DISTANCE,
        min_spacing_ratio: MIN_SPACING_RATIO,
        too_close_ratio: TOO_CLOSE_RATIO,
        discontinuity_ratio: DISCONTINUITY_RATIO,
        max_view_angle: MAX_VIEW_ANGLE.to_radians(),
        paint_angle: PAINT_ANGLE.to_radians(),
        close_distance: CLOSE_DISTANCE,
        enough_area: ENOUGH_AREA,
        detailed_area: DETAILED_AREA,
        max_points: MAX_POINTS,
        confirm_hits: CONFIRM_HITS,
        confirm_span: CONFIRM_SPAN,
        candidate_timeout: CANDIDATE_TIMEOUT,
        merge_depth: MERGE_DEPTH,
        mask_radius: CONTROLLER_MASK_RADIUS,
        max_score: MAX_SCORE,
        cluster_link: CLUSTER_LINK,
        min_cluster: MIN_CLUSTER,
        layer_depth: LAYER_DEPTH,
        plane_min_points: PLANE_MIN_POINTS,
        plane_distance: PLANE_DISTANCE,
        max_planes: MAX_PLANES,
        plane_bin: PLANE_BIN,
        plane_seed_angle: PLANE_SEED_ANGLE.to_radians(),
        plane_inlier_angle: PLANE_INLIER_ANGLE.to_radians(),
        plane_fill_neighbours: PLANE_FILL_NEIGHBOURS,
        layer_angle: LAYER_ANGLE.to_radians(),
        reveal_hits: ROOM_REVEAL_HITS,
    }
}
