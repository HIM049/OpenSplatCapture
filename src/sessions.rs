use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        mpsc::{Receiver, channel},
    },
    thread,
    time::{Duration, Instant},
};

use bevy::prelude::*;
use capture_format::Side;
use jni::objects::{GlobalRef, JValue};

use crate::{
    android::{call_string, helper_class, with_activity},
    recorder::{Recorder, RecorderSystems, RecordingStopped, recordings_root},
};

const EXPORTER_CLASS: &str = "com.example.opencapture.Exporter";
const EXPORT_POLL_INTERVAL: Duration = Duration::from_millis(500);
/// 录制停止后等写盘线程收尾再重新扫描。
const RESCAN_DELAY: Duration = Duration::from_secs(1);

const EXPORT_RUNNING: i32 = 1;
const EXPORT_DONE: i32 = 2;
const EXPORT_FAILED: i32 = 3;

pub struct SessionsPlugin;

impl Plugin for SessionsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Sessions>()
            .add_message::<SessionCommand>()
            .add_systems(Startup, |mut sessions: ResMut<Sessions>| {
                sessions.scan(None)
            })
            .add_systems(
                Update,
                (
                    rescan_after_recording,
                    handle_commands,
                    poll_scan,
                    poll_export,
                )
                    .chain()
                    .after(RecorderSystems),
            );
    }
}

#[derive(Message, Clone)]
pub enum SessionCommand {
    Refresh,
    Delete(String),
    Export(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionInfo {
    pub name: String,
    pub bytes: u64,
    pub frames: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum ExportStatus {
    #[default]
    Idle,
    Running {
        name: String,
        percent: i32,
    },
    Done(String),
    Failed(String),
}

/// 已录制会话的列表和导出状态。
#[derive(Resource, Default)]
pub struct Sessions {
    /// 按时间从新到旧排列。
    pub list: Vec<SessionInfo>,
    /// 列表每次更新后加一，供界面判断是否需要重建。
    pub revision: u32,
    pub export: ExportStatus,
    scan_result: Option<Mutex<Receiver<Vec<SessionInfo>>>>,
    rescan_at: Option<Instant>,
    exporter: Option<GlobalRef>,
    last_export_poll: Option<Instant>,
}

impl Sessions {
    /// 在后台线程里（可选地先删除一个会话再）扫描录制目录。
    fn scan(&mut self, delete: Option<PathBuf>) {
        let (sender, receiver) = channel();
        self.scan_result = Some(Mutex::new(receiver));
        let root = recordings_root();
        thread::spawn(move || {
            if let Some(dir) = delete
                && let Err(err) = fs::remove_dir_all(&dir)
            {
                error!("sessions: deleting {} failed: {err}", dir.display());
            }
            let _ = sender.send(list_sessions(&root));
        });
    }
}

/// 会话目录名形如 `yyyyMMdd_HHmmss`。
fn is_session_name(name: &str) -> bool {
    name.len() == 15
        && name.bytes().enumerate().all(|(i, b)| {
            if i == 8 {
                b == b'_'
            } else {
                b.is_ascii_digit()
            }
        })
}

fn dir_size(dir: &Path) -> u64 {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| match entry.metadata() {
            Ok(meta) if meta.is_dir() => dir_size(&entry.path()),
            Ok(meta) => meta.len(),
            Err(_) => 0,
        })
        .sum()
}

fn list_sessions(root: &Path) -> Vec<SessionInfo> {
    let mut sessions: Vec<SessionInfo> = fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !is_session_name(&name) || !entry.path().is_dir() {
                return None;
            }
            let frames = fs::read_dir(Side::Left.camera_dir(&entry.path()))
                .map(|dir| dir.count())
                .unwrap_or(0);
            Some(SessionInfo {
                bytes: dir_size(&entry.path()),
                frames,
                name,
            })
        })
        .collect();
    sessions.sort_by(|a, b| b.name.cmp(&a.name));
    sessions
}

fn rescan_after_recording(
    mut stopped: MessageReader<RecordingStopped>,
    mut sessions: ResMut<Sessions>,
) {
    if stopped.read().count() > 0 {
        sessions.rescan_at = Some(Instant::now() + RESCAN_DELAY);
    }
    if sessions.rescan_at.is_some_and(|at| Instant::now() >= at) {
        sessions.rescan_at = None;
        sessions.scan(None);
    }
}

fn handle_commands(
    mut commands: MessageReader<SessionCommand>,
    mut sessions: ResMut<Sessions>,
    recorder: Res<Recorder>,
) {
    for command in commands.read() {
        let recording = recorder.active.as_ref().map(|active| active.name());
        match command {
            SessionCommand::Refresh => sessions.scan(None),
            SessionCommand::Delete(name) | SessionCommand::Export(name)
                if recording.as_deref() == Some(name) =>
            {
                warn!("sessions: {name} is still recording");
            }
            SessionCommand::Delete(name) => {
                if matches!(&sessions.export, ExportStatus::Running { name: exporting, .. } if exporting == name)
                {
                    warn!("sessions: {name} is being exported");
                    continue;
                }
                info!("sessions: deleting {name}");
                sessions.scan(Some(recordings_root().join(name)));
            }
            SessionCommand::Export(name) => start_export(&mut sessions, name),
        }
    }
}

fn start_export(sessions: &mut Sessions, name: &str) {
    if matches!(sessions.export, ExportStatus::Running { .. }) {
        warn!("sessions: an export is already running");
        return;
    }
    let dir = recordings_root().join(name);
    let result = with_activity(|env, activity| {
        if sessions.exporter.is_none() {
            let class = helper_class(env, activity, EXPORTER_CLASS)?;
            let exporter = env.new_object(
                class,
                "(Landroid/content/Context;)V",
                &[JValue::Object(activity)],
            )?;
            sessions.exporter = Some(env.new_global_ref(exporter)?);
        }
        let Some(exporter) = &sessions.exporter else {
            return Ok(false);
        };
        let dir = env.new_string(dir.to_string_lossy())?;
        let zip = env.new_string(format!("{name}.zip"))?;
        env.call_method(
            exporter.as_obj(),
            "start",
            "(Ljava/lang/String;Ljava/lang/String;)Z",
            &[JValue::Object(dir.into()), JValue::Object(zip.into())],
        )?
        .z()
    });
    match result {
        Ok(true) => {
            info!("sessions: exporting {name}");
            sessions.export = ExportStatus::Running {
                name: name.to_owned(),
                percent: 0,
            };
        }
        Ok(false) => warn!("sessions: exporter is busy"),
        Err(err) => {
            error!("sessions: starting export failed: {err}");
            sessions.export = ExportStatus::Failed(err.to_string());
        }
    }
}

fn poll_scan(mut sessions: ResMut<Sessions>) {
    let Some(result) = &sessions.scan_result else {
        return;
    };
    let received = result
        .lock()
        .ok()
        .and_then(|receiver| receiver.try_recv().ok());
    if let Some(list) = received {
        sessions.list = list;
        sessions.revision += 1;
        sessions.scan_result = None;
    }
}

fn poll_export(mut sessions: ResMut<Sessions>) {
    let ExportStatus::Running { name, .. } = &sessions.export else {
        return;
    };
    if sessions
        .last_export_poll
        .is_some_and(|last| last.elapsed() < EXPORT_POLL_INTERVAL)
    {
        return;
    }
    let name = name.clone();
    let Some(exporter) = sessions.exporter.clone() else {
        return;
    };
    sessions.last_export_poll = Some(Instant::now());
    let result = with_activity(|env, _| {
        let exporter = exporter.as_obj();
        let state = env.call_method(exporter, "state", "()I", &[])?.i()?;
        let percent = env.call_method(exporter, "percent", "()I", &[])?.i()?;
        let message = call_string(env, exporter, "message")?;
        Ok((state, percent, message))
    });
    sessions.export = match result {
        Ok((EXPORT_RUNNING, percent, _)) => ExportStatus::Running { name, percent },
        Ok((EXPORT_DONE, _, message)) => {
            info!("sessions: exported to {message}");
            ExportStatus::Done(message)
        }
        Ok((EXPORT_FAILED, _, message)) => {
            error!("sessions: export failed: {message}");
            ExportStatus::Failed(message)
        }
        Ok(_) => ExportStatus::Idle,
        Err(err) => ExportStatus::Failed(err.to_string()),
    };
}
