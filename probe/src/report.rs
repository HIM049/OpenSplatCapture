use std::{
    ffi::{CString, c_char, c_int},
    fmt::Write as _,
};

use bevy::prelude::*;

/// 一项检测的结果。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Ok,
    Warn,
    Fail,
    Info,
    Pending,
}

impl Status {
    pub fn name(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Warn => "warn",
            Status::Fail => "fail",
            Status::Info => "info",
            Status::Pending => "pending",
        }
    }
}

/// 检测项所属的分组，按声明顺序显示。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Section {
    Device,
    Runtime,
    Extensions,
    Tracking,
    Controls,
    Permissions,
    AndroidCamera,
    PicoCamera,
    Spatial,
}

impl Section {
    pub fn title(self) -> &'static str {
        match self {
            Section::Device => "设备",
            Section::Runtime => "OpenXR 运行时",
            Section::Extensions => "关键扩展",
            Section::Tracking => "6DoF 与时间",
            Section::Controls => "手柄",
            Section::Permissions => "权限",
            Section::AndroidCamera => "Android 相机接口",
            Section::PicoCamera => "PICO 相机接口",
            Section::Spatial => "空间数据（BD）",
        }
    }
}

pub struct Item {
    pub section: Section,
    pub key: String,
    pub status: Status,
    pub detail: String,
}

/// 一张检测得到的画面，显示在面板上。
pub struct Thumbnail {
    pub label: String,
    pub image: Handle<Image>,
}

/// 所有检测结果；面板和 JSON 报告都从这里取。
#[derive(Resource, Default)]
pub struct Report {
    pub items: Vec<Item>,
    /// 只写进报告和日志的大段文字：标题和内容。
    pub blocks: Vec<(String, String)>,
    pub thumbnails: Vec<Thumbnail>,
    /// 内容每变化一次加一，面板据此决定是否重画。
    pub generation: u64,
    /// 各项检测是否都已结束。
    pub done: bool,
}

/// 每帧都会变的项目，只更新报告，不写日志。
const LIVE_KEYS: [&str; 4] = ["头显位姿", "正在按下", "帧率", "时间换算"];
/// 日志里探测结果所用的 tag，提取脚本按它过滤。
const LOG_TAG: &[u8] = b"OSCProbe\0";

unsafe extern "C" {
    fn __android_log_write(priority: c_int, tag: *const c_char, text: *const c_char) -> c_int;
}

/// 以 OSCProbe 为 tag 写一行 INFO 日志。
pub fn log_line(text: &str) {
    let Ok(text) = CString::new(text.replace('\0', "")) else {
        return;
    };
    unsafe { __android_log_write(4, LOG_TAG.as_ptr() as *const c_char, text.as_ptr()) };
}

fn log_item(prefix: &str, item: &Item) {
    log_line(&format!(
        "{prefix} {} {} | {} | {}",
        item.status.name(),
        item.section.title(),
        item.key,
        item.detail
    ));
}

impl Report {
    /// 写入或更新一项结果，同一分组里的 key 只保留一条。
    pub fn set(
        &mut self,
        section: Section,
        key: impl Into<String>,
        status: Status,
        detail: impl Into<String>,
    ) {
        let key = key.into();
        let detail = detail.into();
        if let Some(item) = self
            .items
            .iter_mut()
            .find(|item| item.section == section && item.key == key)
        {
            if item.status != status || item.detail != detail {
                item.status = status;
                item.detail = detail;
                self.generation += 1;
                if !LIVE_KEYS.contains(&item.key.as_str()) {
                    log_item("ITEM", item);
                }
            }
            return;
        }
        let item = Item {
            section,
            key,
            status,
            detail,
        };
        if !LIVE_KEYS.contains(&item.key.as_str()) {
            log_item("ITEM", &item);
        }
        self.items.push(item);
        self.generation += 1;
    }

    /// 某一项的当前结果。
    pub fn item(&self, section: Section, key: &str) -> Option<&Item> {
        self.items
            .iter()
            .find(|item| item.section == section && item.key == key)
    }

    /// 把全部结果和大段文字逐行写进日志，最后写结束标记。
    pub fn log_final(&self) {
        for item in &self.items {
            log_item("FINAL", item);
        }
        for (title, text) in &self.blocks {
            for line in text.lines() {
                log_line(&format!("BLOCK {title} | {line}"));
            }
        }
        log_line("PROBE_DONE");
    }

    /// 写入或替换一段大段文字。
    pub fn block(&mut self, title: impl Into<String>, text: impl Into<String>) {
        let title = title.into();
        let text = text.into();
        match self.blocks.iter_mut().find(|(t, _)| *t == title) {
            Some((_, existing)) if *existing == text => return,
            Some((_, existing)) => *existing = text,
            None => self.blocks.push((title, text)),
        }
        self.generation += 1;
    }

    pub fn thumbnail(&mut self, label: impl Into<String>, image: Handle<Image>) {
        let label = label.into();
        self.thumbnails.retain(|t| t.label != label);
        self.thumbnails.push(Thumbnail { label, image });
        self.generation += 1;
    }

    /// 某一项当前的状态。
    pub fn status(&self, section: Section, key: &str) -> Option<Status> {
        self.item(section, key).map(|item| item.status)
    }

    /// 整份报告的 JSON 文本。
    pub fn to_json(&self) -> String {
        let mut out = String::from("{\n  \"items\": [\n");
        for (i, item) in self.items.iter().enumerate() {
            let _ = write!(
                out,
                "    {{\"section\": {}, \"key\": {}, \"status\": \"{}\", \"detail\": {}}}{}\n",
                json_string(item.section.title()),
                json_string(&item.key),
                item.status.name(),
                json_string(&item.detail),
                if i + 1 < self.items.len() { "," } else { "" }
            );
        }
        out.push_str("  ],\n  \"blocks\": {\n");
        for (i, (title, text)) in self.blocks.iter().enumerate() {
            let _ = write!(
                out,
                "    {}: {}{}\n",
                json_string(title),
                json_string(text),
                if i + 1 < self.blocks.len() { "," } else { "" }
            );
        }
        out.push_str("  }\n}\n");
        out
    }
}

fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// OpenXR 结果码的名称，PICO 私有的错误码也一并翻译。
pub fn xr_result(result: openxr::sys::Result) -> String {
    let raw = result.into_raw();
    let pico = match raw {
        1010033000 => Some("XR_CAMERA_IMAGE_NO_UPDATE_PICO"),
        -1010033000 => Some("XR_ERROR_CAMERA_UNAVAILABLE_PICO"),
        -1010033001 => Some("XR_ERROR_CAMERA_OCCUPIED_PICO"),
        -1010033002 => Some("XR_ERROR_CAMERA_CAPTURE_SESSION_CAPTURING_PICO"),
        -1010033003 => Some("XR_ERROR_CAMERA_CAPTURE_SESSION_NOT_CAPTURING_PICO"),
        -1010033004 => Some("XR_ERROR_CAMERA_ID_INVALID_PICO"),
        -1010033005 => Some("XR_ERROR_CAMERA_IMAGE_ID_INVALID_PICO"),
        -1010033006 => Some("XR_ERROR_CAMERA_PROPERTY_TYPE_INVALID_PICO"),
        -1010033007 => Some("XR_ERROR_CAMERA_CAPABILITY_TYPE_INVALID_PICO"),
        _ => None,
    };
    match pico {
        Some(name) => format!("{name} ({raw})"),
        None => format!("{result:?} ({raw})"),
    }
}
