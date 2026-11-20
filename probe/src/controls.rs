use std::time::{Duration, Instant};

use bevy::prelude::*;
use bevy_mod_openxr::{
    action_binding::OxrSendActionBindings,
    action_set_attaching::OxrAttachActionSet,
    action_set_syncing::{OxrActionSetSyncSet, OxrSyncActionSet},
    openxr_session_running,
    resources::OxrInstance,
    session::OxrSession,
};
use bevy_mod_xr::session::{XrSessionCreated, session_available};
use openxr::sys;

use crate::report::{Report, Section, Status, xr_result};

/// 依次尝试建议绑定的交互配置，这几种配置的按键路径相同。
const PROFILES: [&str; 4] = [
    "/interaction_profiles/bytedance/pico4_controller",
    "/interaction_profiles/bytedance/pico_ultra_controller_bd",
    "/interaction_profiles/bytedance/pico_neo3_controller",
    "/interaction_profiles/oculus/touch_controller",
];

pub struct ControlsPlugin;

impl Plugin for ControlsPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<ProbeCommand>()
            .add_systems(Startup, create_actions.run_if(session_available))
            .add_systems(OxrSendActionBindings, suggest_bindings)
            .add_systems(XrSessionCreated, attach_set)
            .add_systems(
                PreUpdate,
                sync_actions
                    .before(OxrActionSetSyncSet)
                    .run_if(openxr_session_running),
            )
            .add_systems(Update, read_buttons.run_if(openxr_session_running));
    }
}

/// 手柄按键触发的操作。
#[derive(Message, Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProbeCommand {
    /// 左 Y：重新检测相机。
    RetryCameras,
    /// 左 X：打开系统的房间设定。
    CaptureScene,
    /// 右 A：把面板移到眼前。
    Recenter,
}

/// 一个按键动作：名称、绑定的路径和按下时发出的操作。
struct Button {
    name: &'static str,
    path: &'static str,
    command: Option<ProbeCommand>,
    action: openxr::Action<bool>,
}

#[derive(Resource)]
struct Actions {
    set: openxr::ActionSet,
    buttons: Vec<Button>,
}

fn create_actions(instance: Res<OxrInstance>, mut commands: Commands, mut report: ResMut<Report>) {
    let table: [(&str, &str, Option<ProbeCommand>); 9] = [
        (
            "Y",
            "/user/hand/left/input/y/click",
            Some(ProbeCommand::RetryCameras),
        ),
        (
            "X",
            "/user/hand/left/input/x/click",
            Some(ProbeCommand::CaptureScene),
        ),
        ("左菜单", "/user/hand/left/input/menu/click", None),
        ("左扳机", "/user/hand/left/input/trigger/value", None),
        ("左侧键", "/user/hand/left/input/squeeze/value", None),
        (
            "A",
            "/user/hand/right/input/a/click",
            Some(ProbeCommand::Recenter),
        ),
        ("B", "/user/hand/right/input/b/click", None),
        ("右扳机", "/user/hand/right/input/trigger/value", None),
        ("右侧键", "/user/hand/right/input/squeeze/value", None),
    ];
    let result = (|| -> openxr::Result<Actions> {
        let set = instance.create_action_set("probe", "Probe", 0)?;
        let mut buttons = Vec::new();
        for (i, (name, path, command)) in table.into_iter().enumerate() {
            let action = set.create_action(&format!("button_{i}"), name, &[])?;
            buttons.push(Button {
                name,
                path,
                command,
                action,
            });
        }
        Ok(Actions { set, buttons })
    })();
    match result {
        Ok(actions) => commands.insert_resource(actions),
        Err(err) => report.set(Section::Controls, "动作", Status::Fail, err.to_string()),
    }
}

/// 逐个交互配置建议绑定，并记录每个配置是否被运行时接受。
fn suggest_bindings(
    actions: Option<Res<Actions>>,
    instance: Res<OxrInstance>,
    mut report: ResMut<Report>,
) {
    let Some(actions) = actions else {
        return;
    };
    for profile in PROFILES {
        let result = (|| -> Result<(), String> {
            let profile_path = instance
                .string_to_path(profile)
                .map_err(|e| e.to_string())?;
            let bindings: Vec<sys::ActionSuggestedBinding> = actions
                .buttons
                .iter()
                .map(|b| {
                    Ok(sys::ActionSuggestedBinding {
                        action: b.action.as_raw(),
                        binding: instance.string_to_path(b.path)?,
                    })
                })
                .collect::<openxr::Result<_>>()
                .map_err(|e| e.to_string())?;
            let info = sys::InteractionProfileSuggestedBinding {
                ty: sys::InteractionProfileSuggestedBinding::TYPE,
                next: std::ptr::null(),
                interaction_profile: profile_path,
                count_suggested_bindings: bindings.len() as u32,
                suggested_bindings: bindings.as_ptr(),
            };
            let result = unsafe {
                (instance.fp().suggest_interaction_profile_bindings)(instance.as_raw(), &info)
            };
            if result.into_raw() < 0 {
                return Err(xr_result(result));
            }
            Ok(())
        })();
        let name = profile.rsplit('/').next().unwrap_or(profile);
        match result {
            Ok(()) => report.set(Section::Controls, name, Status::Ok, "绑定被接受"),
            Err(err) => report.set(Section::Controls, name, Status::Warn, err),
        }
    }
}

fn attach_set(actions: Option<Res<Actions>>, mut attach: MessageWriter<OxrAttachActionSet>) {
    if let Some(actions) = actions {
        attach.write(OxrAttachActionSet(actions.set.clone()));
    }
}

fn sync_actions(actions: Option<Res<Actions>>, mut sync: MessageWriter<OxrSyncActionSet>) {
    if let Some(actions) = actions {
        sync.write(OxrSyncActionSet(actions.set.clone()));
    }
}

/// 显示每个按键的按下状态和当前生效的交互配置，按下有功能的键时发出对应操作。
fn read_buttons(
    actions: Option<Res<Actions>>,
    instance: Res<OxrInstance>,
    session: Res<OxrSession>,
    mut report: ResMut<Report>,
    mut commands: MessageWriter<ProbeCommand>,
    mut seen: Local<Vec<&'static str>>,
    mut last_profile: Local<Option<Instant>>,
) {
    let Some(actions) = actions else {
        return;
    };
    let mut pressed = Vec::new();
    for button in &actions.buttons {
        let Ok(state) = button.action.state(&session, openxr::Path::NULL) else {
            continue;
        };
        if state.current_state {
            pressed.push(button.name);
            if !seen.contains(&button.name) {
                seen.push(button.name);
            }
            if state.changed_since_last_sync
                && let Some(command) = button.command
            {
                commands.write(command);
            }
        }
    }
    report.set(
        Section::Controls,
        "正在按下",
        Status::Info,
        if pressed.is_empty() {
            "无".to_string()
        } else {
            pressed.join(" ")
        },
    );
    let all = actions.buttons.len();
    report.set(
        Section::Controls,
        "按过的键",
        if seen.len() == all {
            Status::Ok
        } else {
            Status::Pending
        },
        format!("{}/{all} · {}", seen.len(), seen.join(" ")),
    );

    if last_profile.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
        return;
    }
    *last_profile = Some(Instant::now());
    for (hand, key) in [
        ("/user/hand/left", "左手配置"),
        ("/user/hand/right", "右手配置"),
    ] {
        let text = instance
            .string_to_path(hand)
            .and_then(|path| session.current_interaction_profile(path))
            .and_then(|profile| {
                if profile == openxr::Path::NULL {
                    Ok("无（手柄未连接或未识别）".to_string())
                } else {
                    instance.path_to_string(profile)
                }
            });
        match text {
            Ok(text) => report.set(Section::Controls, key, Status::Info, text),
            Err(err) => report.set(Section::Controls, key, Status::Fail, err.to_string()),
        }
    }
}
