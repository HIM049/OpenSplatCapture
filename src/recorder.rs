use std::{
    collections::HashMap,
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, SyncSender, TrySendError, sync_channel},
    },
    thread,
    time::{Duration, Instant},
};

use bevy::prelude::*;
use bevy_mod_openxr::{openxr_session_running, resources::OxrInstance, session::OxrSession};
use bevy_mod_xr::{session::XrState, spaces::XrPrimaryReferenceSpace};
use capture_format::{
    Side, Storage, TimeBase,
    convert::compress,
    gate::ShotGate,
    geometry::DepthView,
    layout::{HMD_POSES, LEFT_CONTROLLER_POSES, RIGHT_CONTROLLER_POSES},
    records::{DEPTH_DESCRIPTOR_HEADER, POSE_HEADER, Pose},
};

use crate::{
    input::{Haptic, HoldButton, InputSystems, ManualShot, RecordKey, ToggleRecording},
    pose::{ViewSpace, head_transform},
    room::RoomSetup,
    time::{new_time_base, session_name},
};

/// 写盘队列的容量，满了之后新任务被丢弃并计数。
const WRITE_QUEUE_CAPACITY: usize = 512;
use crate::tuning::{
    ADVANCE_HOLD, DEPTH_INTERVAL, HOLD_CAPTURE_FPS, HOLD_MAX_ANGULAR_SPEED, HOLD_MAX_LINEAR_SPEED,
    INDICATOR_DISTANCE, INDICATOR_PITCH, INDICATOR_RADIUS, INDICATOR_YAW, LONG_PRESS,
    MIN_SHOT_INTERVAL, SCAN_STAGE_DEPTH_INTERVAL,
};
/// 录制开始后先只记位姿，过这么久再发第一个采集节拍，保证每帧图像前后都有位姿。
const FIRST_TICK_DELAY: Duration = Duration::from_millis(200);
/// 剩余空间低于这个值时不开始录制，录制中则自动停止。
const MIN_FREE_BYTES: u64 = 1 << 30;
const FREE_SPACE_CHECK_INTERVAL: Duration = Duration::from_secs(1);
pub struct RecorderPlugin;

impl Plugin for RecorderPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Recorder>()
            .add_message::<RecordingStarted>()
            .add_message::<RecordingStopped>()
            .add_message::<CaptureTick>()
            .add_message::<ScanTick>()
            .add_message::<AdvanceStage>()
            .add_message::<CancelRecording>()
            .add_message::<CaptureCameraAt>()
            .add_message::<DepthFrameCaptured>()
            .add_message::<SessionPaused>()
            .add_systems(PostStartup, spawn_indicator)
            .add_systems(
                Update,
                (
                    detect_session_pause,
                    handle_record_key,
                    toggle_recording,
                    stop_when_storage_low,
                    control_stages,
                    emit_capture_ticks,
                    place_indicator,
                    update_indicator,
                )
                    .chain()
                    .in_set(RecorderSystems)
                    .after(InputSystems),
            );
    }
}

/// 录制状态切换和采集节拍所在的系统集，采集源排在它之后。
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct RecorderSystems;

#[derive(Message, Clone)]
pub struct RecordingStarted {
    pub dir: PathBuf,
    pub time: TimeBase,
    pub storage: Storage,
}

#[derive(Message)]
pub struct RecordingStopped;

/// 该拍一张照片了：采一帧深度和相机图像并存盘。
#[derive(Message)]
pub struct CaptureTick;

/// 该取一帧只用于发现表面的深度了，不存盘。
#[derive(Message)]
pub struct ScanTick;

/// 用户要求进入下一个阶段，最后一个阶段之后结束录制。
#[derive(Message)]
pub struct AdvanceStage;

/// 用户要求放弃这次录制：立即结束并删掉已写的文件。
#[derive(Message)]
pub struct CancelRecording;

/// 录制的三个阶段：扫描房间定出表面点，按小球拍摄，隐藏小球扫细节。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
    Scan,
    Detail,
    Free,
}

/// 要求相机保存传感器时间戳最接近该单调时钟时刻（纳秒）的一帧。
#[derive(Message)]
pub struct CaptureCameraAt(pub i64);

/// 采到了一帧深度：左右眼的投影参数和文件格式的深度数据。
#[derive(Message)]
pub struct DepthFrameCaptured {
    pub views: [DepthView; 2],
    pub layers: [Vec<u8>; 2],
    /// 这一帧是否随照片一起存盘了。
    pub saved: bool,
}

/// XR 会话离开运行状态（休眠、摘下头显等）。
#[derive(Message)]
pub struct SessionPaused;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum CsvFile {
    Hmd,
    LeftController,
    RightController,
    DepthDescriptors(usize),
}

enum Job {
    Line(CsvFile, String),
    File(PathBuf, Vec<u8>),
    CompressedFile(PathBuf, Vec<u8>),
}

#[derive(Resource)]
pub struct Recorder {
    pub log_controllers: bool,
    /// 下一次录制使用的存储方式。
    pub storage: Storage,
    /// 用已设定的房间放置小球，否则先扫描房间。
    pub use_room: bool,
    /// 开始前的操作说明正在显示，再确认一次才真正开始。
    pub intro_open: bool,
    /// 选了使用已设定的房间但没有可用的房间数据，开始被拦下。
    pub start_blocked: bool,
    pub active: Option<ActiveRecording>,
    /// 正在长按录制键准备进入下一阶段时，还要按住多少秒。
    pub advance_countdown: Option<u32>,
    /// 最近一次录制结束或未能开始的情况，供界面显示。
    pub last_stop: Option<LastStop>,
}

/// 录制结束的原因。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StopReason {
    User,
    Finished,
    Cancelled,
    SessionPaused,
    StorageLow,
}

/// 最近一次录制的结果。
#[derive(Clone, Debug)]
pub enum LastStop {
    Stopped {
        name: String,
        reason: StopReason,
        seconds: f32,
        frames: u32,
        dropped: u32,
    },
    /// 没能开始，附原因；`None` 表示存储空间不足。
    NotStarted(Option<String>),
}

impl Default for Recorder {
    fn default() -> Self {
        Self {
            log_controllers: true,
            storage: Storage::default(),
            use_room: true,
            intro_open: false,
            start_blocked: false,
            active: None,
            advance_countdown: None,
            last_stop: None,
        }
    }
}

impl Recorder {
    /// 结束当前录制并记录原因；没有在录制时什么也不做。
    fn stop(&mut self, reason: StopReason, stopped: &mut MessageWriter<RecordingStopped>) {
        let Some(active) = self.active.take() else {
            return;
        };
        let seconds = active.elapsed().as_secs_f32();
        info!(
            "recorder: stopped {} ({reason:?}): {seconds:.1} s, {} frames, {} dropped writes",
            active.name(),
            active.ticks,
            active.dropped
        );
        self.last_stop = Some(LastStop::Stopped {
            name: active.name(),
            reason,
            seconds,
            frames: active.ticks,
            dropped: active.dropped,
        });
        stopped.write(RecordingStopped);
    }
}

pub struct ActiveRecording {
    pub dir: PathBuf,
    pub time: TimeBase,
    pub storage: Storage,
    pub stage: Stage,
    /// 头部动得太快，连续拍摄在等待。
    pub too_fast: bool,
    gate: ShotGate,
    jobs: SyncSender<Job>,
    /// 置位后写盘线程丢弃剩余任务并删掉会话目录。
    discard: Arc<AtomicBool>,
    started: Instant,
    last_shot: Option<Instant>,
    /// 最近一次请求深度的时刻。
    last_depth: Instant,
    last_space_check: Instant,
    pub ticks: u32,
    pub dropped: u32,
}

impl ActiveRecording {
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn name(&self) -> String {
        self.dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// 向指定 CSV 追加一行。
    pub fn write_line(&mut self, file: CsvFile, line: String) {
        self.send(Job::Line(file, line));
    }

    /// 把一段数据写成独立文件。
    pub fn write_file(&mut self, path: PathBuf, data: Vec<u8>) {
        self.send(Job::File(path, data));
    }

    /// 把一段数据无损压缩后写成独立文件，压缩在写盘线程上进行。
    pub fn write_compressed_file(&mut self, path: PathBuf, data: Vec<u8>) {
        self.send(Job::CompressedFile(path, data));
    }

    fn send(&mut self, job: Job) {
        match self.jobs.try_send(job) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => self.dropped += 1,
            Err(TrySendError::Disconnected(_)) => {
                if self.dropped == 0 {
                    error!("recorder: writer thread has exited");
                }
                self.dropped += 1;
            }
        }
    }
}

fn csv_files(dir: &Path) -> Vec<(CsvFile, PathBuf, &'static str)> {
    let mut files = vec![
        (CsvFile::Hmd, dir.join(HMD_POSES), POSE_HEADER),
        (
            CsvFile::LeftController,
            dir.join(LEFT_CONTROLLER_POSES),
            POSE_HEADER,
        ),
        (
            CsvFile::RightController,
            dir.join(RIGHT_CONTROLLER_POSES),
            POSE_HEADER,
        ),
    ];
    for side in Side::BOTH {
        files.push((
            CsvFile::DepthDescriptors(side.index()),
            side.depth_descriptors(dir),
            DEPTH_DESCRIPTOR_HEADER,
        ));
    }
    files
}

/// 建好会话目录和各 CSV 文件，返回写盘线程要用的句柄。
fn create_session_files(dir: &Path) -> std::io::Result<HashMap<CsvFile, BufWriter<File>>> {
    fs::create_dir_all(dir)?;
    for side in Side::BOTH {
        fs::create_dir_all(side.camera_dir(dir))?;
        fs::create_dir_all(side.depth_dir(dir))?;
    }
    let mut writers = HashMap::new();
    for (id, path, header) in csv_files(dir) {
        let mut writer = BufWriter::new(File::create(path)?);
        writeln!(writer, "{header}")?;
        writers.insert(id, writer);
    }
    Ok(writers)
}

/// 写盘线程：按到达顺序处理任务，发送端关闭后落盘退出；录制被放弃时改为删掉会话目录。
fn run_writer(
    jobs: Receiver<Job>,
    mut writers: HashMap<CsvFile, BufWriter<File>>,
    dir: PathBuf,
    discard: Arc<AtomicBool>,
) {
    let mut failures = 0u32;
    for job in jobs {
        if discard.load(Ordering::Relaxed) {
            continue;
        }
        let result = match job {
            Job::Line(file, line) => match writers.get_mut(&file) {
                Some(writer) => writeln!(writer, "{line}"),
                None => Ok(()),
            },
            Job::File(path, data) => fs::write(path, data),
            Job::CompressedFile(path, data) => fs::write(path, compress(&data)),
        };
        if let Err(err) = result {
            if failures == 0 {
                error!("recorder: write failed: {err}");
            }
            failures += 1;
        }
    }
    if discard.load(Ordering::Relaxed) {
        drop(writers);
        if let Err(err) = fs::remove_dir_all(&dir) {
            error!("recorder: deleting {} failed: {err}", dir.display());
        }
        return;
    }
    for writer in writers.values_mut() {
        if let Err(err) = writer.flush() {
            error!("recorder: flush failed: {err}");
        }
    }
    if failures > 0 {
        error!("recorder: {failures} writes failed in total");
    }
}

/// 所有录制会话所在的目录。
pub fn recordings_root() -> PathBuf {
    bevy::android::ANDROID_APP
        .get()
        .and_then(|app| app.external_data_path())
        .unwrap_or_default()
}

/// 指定路径所在文件系统的剩余空间（字节）。
pub fn free_bytes(path: &Path) -> Option<u64> {
    let path = std::ffi::CString::new(path.to_string_lossy().as_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    (unsafe { libc::statvfs(path.as_ptr(), &mut stat) } == 0)
        .then(|| stat.f_bavail as u64 * stat.f_frsize as u64)
}

/// 会话从运行变为非运行时停止录制并通知各采集源。
fn detect_session_pause(
    state: Option<Res<XrState>>,
    session: Option<Res<OxrSession>>,
    mut was_running: Local<bool>,
    mut recorder: ResMut<Recorder>,
    mut paused: MessageWriter<SessionPaused>,
    mut stopped: MessageWriter<RecordingStopped>,
) {
    let is_running = openxr_session_running(state, session);
    if *was_running && !is_running {
        info!("recorder: session paused");
        recorder.stop(StopReason::SessionPaused, &mut stopped);
        paused.write(SessionPaused);
    }
    *was_running = is_running;
}

/// 长按录制键：待机时开始录制；录制中按满倒计时进入下一阶段，每过一秒震一下，松开则取消。
fn handle_record_key(
    key: Res<RecordKey>,
    mut recorder: ResMut<Recorder>,
    mut state: Local<(bool, u32)>,
    mut toggles: MessageWriter<ToggleRecording>,
    mut advances: MessageWriter<AdvanceStage>,
    mut haptics: MessageWriter<Haptic>,
) {
    let (fired, announced) = &mut *state;
    let Some(held) = key.held_since.map(|since| since.elapsed()) else {
        *fired = false;
        *announced = 0;
        if recorder.advance_countdown.is_some() {
            recorder.advance_countdown = None;
        }
        return;
    };
    if *fired {
        return;
    }
    if recorder.active.is_none() {
        if held >= LONG_PRESS {
            *fired = true;
            toggles.write(ToggleRecording);
        }
        return;
    }
    if held >= ADVANCE_HOLD {
        *fired = true;
        recorder.advance_countdown = None;
        advances.write(AdvanceStage);
        return;
    }
    let seconds = held.as_secs() as u32 + 1;
    if seconds > *announced {
        *announced = seconds;
        haptics.write(Haptic::Tick);
    }
    let remaining = (ADVANCE_HOLD - held).as_secs_f32().ceil() as u32;
    if recorder.advance_countdown != Some(remaining) {
        recorder.advance_countdown = Some(remaining);
    }
}

/// 开始或结束录制。选了已设定的房间却没有可用数据就不开始；
/// 否则先显示操作说明，再确认一次才开始。
fn toggle_recording(
    mut toggles: MessageReader<ToggleRecording>,
    room: Res<RoomSetup>,
    mut recorder: ResMut<Recorder>,
    mut started: MessageWriter<RecordingStarted>,
    mut stopped: MessageWriter<RecordingStopped>,
    mut haptics: MessageWriter<Haptic>,
) {
    if toggles.read().count() == 0 {
        return;
    }
    if recorder.active.is_some() {
        recorder.stop(StopReason::User, &mut stopped);
        haptics.write(Haptic::Record);
        return;
    }
    if recorder.use_room && !room.usable() {
        recorder.start_blocked = true;
        haptics.write(Haptic::Stage);
        return;
    }
    recorder.start_blocked = false;
    if !recorder.intro_open {
        recorder.intro_open = true;
        haptics.write(Haptic::Stage);
        return;
    }
    recorder.intro_open = false;

    let root = recordings_root();
    if free_bytes(&root).is_some_and(|free| free < MIN_FREE_BYTES) {
        warn!("recorder: not starting, storage almost full");
        recorder.last_stop = Some(LastStop::NotStarted(None));
        return;
    }
    let dir = root.join(session_name());
    let writers = match create_session_files(&dir) {
        Ok(writers) => writers,
        Err(err) => {
            error!("recorder: cannot create {}: {err}", dir.display());
            recorder.last_stop = Some(LastStop::NotStarted(Some(err.to_string())));
            return;
        }
    };
    let (jobs, receiver) = sync_channel(WRITE_QUEUE_CAPACITY);
    let discard = Arc::new(AtomicBool::new(false));
    let (writer_dir, writer_discard) = (dir.clone(), discard.clone());
    if let Err(err) = thread::Builder::new()
        .name("capture-writer".into())
        .spawn(move || run_writer(receiver, writers, writer_dir, writer_discard))
    {
        error!("recorder: cannot start writer thread: {err}");
        return;
    }
    let time = new_time_base();
    let storage = recorder.storage;
    info!("recorder: started {} ({storage:?})", dir.display());
    haptics.write(Haptic::Record);
    started.write(RecordingStarted {
        dir: dir.clone(),
        time,
        storage,
    });
    recorder.active = Some(ActiveRecording {
        dir,
        time,
        storage,
        stage: Stage::Scan,
        too_fast: false,
        gate: ShotGate::default(),
        jobs,
        discard,
        started: Instant::now(),
        last_shot: None,
        last_depth: Instant::now(),
        last_space_check: Instant::now(),
        ticks: 0,
        dropped: 0,
    });
}

fn stop_when_storage_low(
    mut recorder: ResMut<Recorder>,
    mut stopped: MessageWriter<RecordingStopped>,
) {
    let low = match &mut recorder.active {
        Some(active) if active.last_space_check.elapsed() >= FREE_SPACE_CHECK_INTERVAL => {
            active.last_space_check = Instant::now();
            free_bytes(&active.dir).is_some_and(|free| free < MIN_FREE_BYTES)
        }
        _ => false,
    };
    if low {
        recorder.stop(StopReason::StorageLow, &mut stopped);
    }
}

/// 处理"下一阶段"和放弃录制：推进阶段，最后一个阶段之后结束录制。
fn control_stages(
    mut advances: MessageReader<AdvanceStage>,
    mut cancels: MessageReader<CancelRecording>,
    mut recorder: ResMut<Recorder>,
    mut stopped: MessageWriter<RecordingStopped>,
    mut haptics: MessageWriter<Haptic>,
) {
    let advance = advances.read().count() > 0;
    let cancel = cancels.read().count() > 0;
    let Some(active) = &mut recorder.active else {
        return;
    };
    if cancel {
        active.discard.store(true, Ordering::Relaxed);
        recorder.stop(StopReason::Cancelled, &mut stopped);
        haptics.write(Haptic::Record);
        return;
    }
    if !advance {
        return;
    }
    let next = match active.stage {
        Stage::Scan => Some(Stage::Detail),
        Stage::Detail => Some(Stage::Free),
        Stage::Free => None,
    };
    match next {
        Some(stage) => {
            info!("recorder: stage {stage:?} after {} frames", active.ticks);
            active.stage = stage;
            haptics.write(Haptic::Stage);
        }
        None => {
            recorder.stop(StopReason::Finished, &mut stopped);
            haptics.write(Haptic::Record);
        }
    }
}

/// 录制期间决定这一帧要不要拍照：扫描阶段不拍照，之后手动拍一张，
/// 或握住侧键时在头部不太快的情况下按固定帧率连续拍。
/// 不拍照时按较低的频率请求一帧只用于发现表面的深度。
fn emit_capture_ticks(
    mut recorder: ResMut<Recorder>,
    mut manual: MessageReader<ManualShot>,
    hold: Res<HoldButton>,
    instance: Option<Res<OxrInstance>>,
    base: Option<Res<XrPrimaryReferenceSpace>>,
    view: Option<Res<ViewSpace>>,
    mut ticks: MessageWriter<CaptureTick>,
    mut scans: MessageWriter<ScanTick>,
    mut haptics: MessageWriter<Haptic>,
) {
    let manual = manual.read().count() > 0;
    let Some(active) = &mut recorder.active else {
        return;
    };
    let now = Instant::now();
    if now - active.started < FIRST_TICK_DELAY {
        return;
    }
    let shoot = match active.stage {
        Stage::Scan => {
            active.too_fast = false;
            false
        }
        Stage::Detail | Stage::Free => {
            let time = active.started.elapsed().as_secs_f64();
            let head = match (&instance, &base, &view) {
                (Some(instance), Some(base), Some(view)) => head_transform(instance, base, view),
                _ => None,
            }
            .map(|head| Pose {
                position: head.translation.to_array(),
                rotation: head.rotation.to_array(),
            });
            let mut shoot = manual && active.gate.interval_elapsed(time, MIN_SHOT_INTERVAL);
            active.too_fast = false;
            if let Some(pose) = &head {
                active.gate.observe(pose, time);
                if !shoot && hold.pressed {
                    let hold_interval = Duration::from_secs_f32(1.0 / HOLD_CAPTURE_FPS);
                    if active
                        .last_shot
                        .is_none_or(|last| now - last >= hold_interval)
                    {
                        let (linear, angular) = active.gate.speeds();
                        if linear <= HOLD_MAX_LINEAR_SPEED
                            && angular <= HOLD_MAX_ANGULAR_SPEED.to_radians()
                        {
                            shoot = true;
                        } else {
                            active.too_fast = true;
                        }
                    }
                }
            }
            if shoot {
                active.gate.record(time);
            }
            shoot
        }
    };
    if shoot {
        active.last_shot = Some(now);
        active.last_depth = now;
        active.ticks += 1;
        ticks.write(CaptureTick);
        haptics.write(Haptic::Shot);
        return;
    }
    let scan_interval = if active.stage == Stage::Scan {
        SCAN_STAGE_DEPTH_INTERVAL
    } else {
        DEPTH_INTERVAL
    };
    if now - active.last_depth >= scan_interval {
        active.last_depth = now;
        scans.write(ScanTick);
    }
}

#[derive(Resource)]
struct Indicator {
    entity: Entity,
    material: Handle<StandardMaterial>,
}

const IDLE_COLOR: Color = Color::srgb(0.2, 0.8, 0.3);
const RECORDING_COLOR: Color = Color::srgb(0.9, 0.1, 0.1);
const TOO_FAST_COLOR: Color = Color::srgb(1.0, 0.8, 0.0);
const SHOT_COLOR: Color = Color::WHITE;
/// 每拍一张照片，小球变白这么久。
const SHOT_FLASH: Duration = Duration::from_millis(120);

/// 视野右上角的圆点：绿色待机，红色拍摄中，黄色表示动得太快，拍照时闪白。
fn spawn_indicator(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let material = materials.add(StandardMaterial {
        base_color: IDLE_COLOR,
        unlit: true,
        ..default()
    });
    let entity = commands
        .spawn((
            Mesh3d(meshes.add(Sphere::new(INDICATOR_RADIUS))),
            MeshMaterial3d(material.clone()),
            Visibility::Hidden,
        ))
        .id();
    commands.insert_resource(Indicator { entity, material });
}

/// 让圆点固定在视野的右上角；拿不到头部位姿时隐藏。
fn place_indicator(
    indicator: Res<Indicator>,
    instance: Option<Res<OxrInstance>>,
    base: Option<Res<XrPrimaryReferenceSpace>>,
    view: Option<Res<ViewSpace>>,
    mut transforms: Query<(&mut Transform, &mut Visibility)>,
) {
    let Ok((mut transform, mut visibility)) = transforms.get_mut(indicator.entity) else {
        return;
    };
    let head = match (&instance, &base, &view) {
        (Some(instance), Some(base), Some(view)) => head_transform(instance, base, view),
        _ => None,
    };
    let Some(head) = head else {
        if *visibility != Visibility::Hidden {
            *visibility = Visibility::Hidden;
        }
        return;
    };
    let (yaw, pitch) = (INDICATOR_YAW.to_radians(), INDICATOR_PITCH.to_radians());
    let direction = Vec3::new(
        yaw.sin() * pitch.cos(),
        pitch.sin(),
        -yaw.cos() * pitch.cos(),
    );
    transform.translation = head.translation + head.rotation * direction * INDICATOR_DISTANCE;
    if *visibility != Visibility::Visible {
        *visibility = Visibility::Visible;
    }
}

fn update_indicator(
    recorder: Res<Recorder>,
    indicator: Res<Indicator>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut shown: Local<Option<Color>>,
) {
    let color = match &recorder.active {
        None => IDLE_COLOR,
        Some(active)
            if active
                .last_shot
                .is_some_and(|shot| shot.elapsed() < SHOT_FLASH) =>
        {
            SHOT_COLOR
        }
        Some(active) if active.too_fast => TOO_FAST_COLOR,
        Some(_) => RECORDING_COLOR,
    };
    if *shown == Some(color) {
        return;
    }
    *shown = Some(color);
    if let Some(mut material) = materials.get_mut(&indicator.material) {
        material.base_color = color;
    }
}
