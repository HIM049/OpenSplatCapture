use std::time::{Duration, Instant};

use bevy::prelude::*;
use bevy_mod_openxr::openxr_session_running;

use crate::{
    android::{ALL_PERMISSIONS, call_helper, has_permission, request_permissions},
    report::{Report, Section, Status},
};

const INFO_CLASS: &str = "com.example.opencapture.probe.ProbeInfo";
/// 报告写到应用目录的间隔。
const SAVE_INTERVAL: Duration = Duration::from_secs(3);
/// 启动后至少等这么久才判断检测是否结束，让各项检测都有机会开始。
const DONE_MIN: Duration = Duration::from_secs(8);
/// 超过这个时间仍有检测没结束，也当作结束，以免提取脚本一直等。
const DONE_DEADLINE: Duration = Duration::from_secs(90);

pub struct DevicePlugin;

impl Plugin for DevicePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, read_device)
            .add_systems(
                Update,
                (request_once, check_permissions).run_if(openxr_session_running),
            )
            .add_systems(Update, save_report);
    }
}

/// 启动时间，各项检测据此安排先后。
#[derive(Resource)]
pub struct StartedAt(pub Instant);

/// 读取设备型号、系统版本、相关系统属性和硬件功能。
fn read_device(mut report: ResMut<Report>, mut commands: Commands) {
    commands.insert_resource(StartedAt(Instant::now()));
    match call_helper(INFO_CLASS, "device", &[]) {
        Ok(text) => {
            for line in text.lines() {
                if let Some((key, value)) = line.split_once('=') {
                    report.set(Section::Device, key, Status::Info, value);
                }
            }
        }
        Err(err) => report.set(Section::Device, "device", Status::Fail, err.to_string()),
    }
    let props = call_helper(INFO_CLASS, "props", &[]).unwrap_or_else(|err| err.to_string());
    for key in [
        "ro.pui.build.version",
        "ro.pico.os.version",
        "ro.build.display.id",
    ] {
        if let Some(value) = props
            .lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
        {
            report.set(Section::Device, key, Status::Info, value);
        }
    }
    report.block("系统属性（getprop 过滤后）", props);
    report.block(
        "系统功能（PackageManager 过滤后）",
        call_helper(INFO_CLASS, "features", &[]).unwrap_or_else(|err| err.to_string()),
    );
}

/// 会话开始后申请一次所有权限。
fn request_once(mut done: Local<bool>, mut report: ResMut<Report>) {
    if *done {
        return;
    }
    *done = true;
    let missing: Vec<&str> = ALL_PERMISSIONS
        .into_iter()
        .filter(|p| !has_permission(p))
        .collect();
    if !missing.is_empty()
        && let Err(err) = request_permissions(&missing)
    {
        report.set(
            Section::Permissions,
            "request",
            Status::Fail,
            err.to_string(),
        );
    }
}

/// 每秒刷新一次各权限的授予状态。
fn check_permissions(mut report: ResMut<Report>, mut last: Local<Option<Instant>>) {
    if last.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
        return;
    }
    *last = Some(Instant::now());
    for permission in ALL_PERMISSIONS {
        let granted = has_permission(permission);
        report.set(
            Section::Permissions,
            permission,
            if granted { Status::Ok } else { Status::Warn },
            if granted { "已授予" } else { "未授予" },
        );
    }
}

/// 定期把报告写到应用目录；检测结束时立即写一次，并把全部结果写进日志。
fn save_report(
    mut report: ResMut<Report>,
    started: Option<Res<StartedAt>>,
    mut last: Local<Option<(Instant, u64)>>,
    mut dir: Local<Option<String>>,
) {
    let finished = !report.done && started.is_some_and(|s| finished(&report, s.0.elapsed()));
    if finished {
        report.done = true;
        report.set(Section::Device, "检测", Status::Ok, "全部结束");
    }
    if !finished
        && last.is_some_and(|(t, generation)| {
            t.elapsed() < SAVE_INTERVAL || generation == report.generation
        })
    {
        return;
    }
    *last = Some((Instant::now(), report.generation));
    let dir =
        dir.get_or_insert_with(|| call_helper(INFO_CLASS, "filesDir", &[]).unwrap_or_default());
    if !dir.is_empty() {
        let path = format!("{dir}/probe_report.json");
        if let Err(err) = std::fs::write(&path, report.to_json()) {
            error!("probe: writing {path} failed: {err}");
        }
    }
    if finished {
        report.log_final();
    }
}

/// 相机和空间数据的检测都有了不再等待的结果，或者已经超过最长等待时间。
fn finished(report: &Report, elapsed: Duration) -> bool {
    if elapsed > DONE_DEADLINE {
        return true;
    }
    if elapsed < DONE_MIN {
        return false;
    }
    [
        Section::AndroidCamera,
        Section::PicoCamera,
        Section::Spatial,
    ]
    .into_iter()
    .all(|section| {
        let mut items = report
            .items
            .iter()
            .filter(|i| i.section == section)
            .peekable();
        items.peek().is_some() && items.all(|i| i.status != Status::Pending)
    })
}
