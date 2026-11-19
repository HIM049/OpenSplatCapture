use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use bevy::prelude::*;
use bevy_mod_openxr::{
    action_binding::{OxrSendActionBindings, OxrSuggestActionBinding},
    action_set_attaching::OxrAttachActionSet,
    action_set_syncing::{OxrActionSetSyncSet, OxrSyncActionSet},
    openxr_session_running,
    resources::OxrInstance,
    session::OxrSession,
};
use bevy_mod_xr::{
    session::{XrPreDestroySession, XrSessionCreated, session_available},
    spaces::XrSpace,
};
use openxr::Posef;

use crate::tuning::{CLICK_AMPLITUDE, CLICK_PULSE, PULSE_GAP};

const TOUCH_PROFILE: &str = "/interaction_profiles/oculus/touch_controller";

pub struct InputPlugin;

impl Plugin for InputPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<ToggleRecording>()
            .add_message::<ManualShot>()
            .add_message::<ToggleMenu>()
            .add_message::<Haptic>()
            .init_resource::<SelectButton>()
            .init_resource::<HoldButton>()
            .init_resource::<LeftTrigger>()
            .init_resource::<RecordKey>()
            .add_systems(
                Startup,
                (spawn_anchors, create_actions.run_if(session_available)),
            )
            .add_systems(OxrSendActionBindings, suggest_bindings)
            .add_systems(XrSessionCreated, (attach_set, create_spaces))
            .add_systems(XrPreDestroySession, remove_spaces)
            .add_systems(
                PreUpdate,
                sync_actions
                    .before(OxrActionSetSyncSet)
                    .run_if(openxr_session_running),
            )
            .add_systems(
                Update,
                read_buttons
                    .in_set(InputSystems)
                    .run_if(openxr_session_running),
            )
            .add_systems(PostUpdate, play_haptics.run_if(openxr_session_running));
    }
}

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct InputSystems;

/// 要开始或结束录制。
#[derive(Message)]
pub struct ToggleRecording;

/// 用户按下了手动拍照的按键。
#[derive(Message)]
pub struct ManualShot;

/// 左手柄的震动提示。
#[derive(Message, Clone, Copy)]
pub enum Haptic {
    /// 拍了一张照片。
    Shot,
    /// 阶段变了，或开始前被拦下、打开了说明。
    Stage,
    /// 录制开始或结束。
    Record,
    /// 长按录制键的倒计时过了一秒。
    Tick,
    /// 左手或右手的射线点击了界面。
    ClickLeft,
    ClickRight,
}

/// 用户按下了显示或隐藏面板的按键。
#[derive(Message)]
pub struct ToggleMenu;

/// 右手扳机当前是否按下。
#[derive(Resource, Default)]
pub struct SelectButton {
    pub pressed: bool,
}

/// 左手扳机当前是否按下。
#[derive(Resource, Default)]
pub struct LeftTrigger {
    pub pressed: bool,
}

/// 录制键（左 Y）从什么时候开始一直按着，没按时为 None。
#[derive(Resource, Default)]
pub struct RecordKey {
    pub held_since: Option<Instant>,
}

/// 左手侧键当前是否握住。
#[derive(Resource, Default)]
pub struct HoldButton {
    pub pressed: bool,
}

/// 跟随左手柄握持位姿的实体。
#[derive(Component)]
pub struct LeftHandAnchor;

/// 跟随右手柄瞄准位姿的实体，-Z 方向是射线方向。
#[derive(Component)]
pub struct RightAimAnchor;

/// 跟随左手柄瞄准位姿的实体，-Z 方向是射线方向。
#[derive(Component)]
pub struct LeftAimAnchor;

#[derive(Resource)]
struct ControllerActions {
    set: openxr::ActionSet,
    left_pose: openxr::Action<Posef>,
    right_pose: openxr::Action<Posef>,
    right_aim: openxr::Action<Posef>,
    left_aim: openxr::Action<Posef>,
    record: openxr::Action<bool>,
    shoot: openxr::Action<bool>,
    toggle_menu: openxr::Action<bool>,
    select: openxr::Action<bool>,
    hold: openxr::Action<bool>,
    haptic: openxr::Action<openxr::Haptic>,
    right_haptic: openxr::Action<openxr::Haptic>,
}

/// 左右手柄握持位姿的空间。
#[derive(Resource)]
pub struct ControllerSpaces {
    pub left: XrSpace,
    pub right: XrSpace,
}

fn spawn_anchors(mut commands: Commands) {
    commands.spawn((LeftHandAnchor, Transform::default(), Visibility::default()));
    commands.spawn((RightAimAnchor, Transform::default(), Visibility::default()));
    commands.spawn((LeftAimAnchor, Transform::default(), Visibility::default()));
}

fn create_actions(instance: Res<OxrInstance>, mut commands: Commands) {
    let result = (|| -> openxr::Result<ControllerActions> {
        let set = instance.create_action_set("capture", "Capture", 0)?;
        Ok(ControllerActions {
            left_pose: set.create_action("left_pose", "Left Grip Pose", &[])?,
            right_pose: set.create_action("right_pose", "Right Grip Pose", &[])?,
            right_aim: set.create_action("right_aim", "Right Aim Pose", &[])?,
            left_aim: set.create_action("left_aim", "Left Aim Pose", &[])?,
            record: set.create_action("record", "Record / Next Stage", &[])?,
            shoot: set.create_action("shoot", "Take Photo", &[])?,
            toggle_menu: set.create_action("toggle_menu", "Toggle Menu", &[])?,
            select: set.create_action("select", "Select", &[])?,
            hold: set.create_action("hold", "Continuous Capture", &[])?,
            haptic: set.create_action("haptic", "Haptic Feedback", &[])?,
            right_haptic: set.create_action("right_haptic", "Right Haptic Feedback", &[])?,
            set,
        })
    })();
    match result {
        Ok(actions) => commands.insert_resource(actions),
        Err(err) => error!("input: creating actions failed: {err}"),
    }
}

fn suggest_bindings(
    actions: Option<Res<ControllerActions>>,
    mut bindings: MessageWriter<OxrSuggestActionBinding>,
) {
    let Some(actions) = actions else {
        return;
    };
    let table: [(openxr::sys::Action, &[&str]); 11] = [
        (
            actions.left_pose.as_raw(),
            &["/user/hand/left/input/grip/pose"],
        ),
        (
            actions.right_pose.as_raw(),
            &["/user/hand/right/input/grip/pose"],
        ),
        (
            actions.right_aim.as_raw(),
            &["/user/hand/right/input/aim/pose"],
        ),
        (
            actions.left_aim.as_raw(),
            &["/user/hand/left/input/aim/pose"],
        ),
        (actions.record.as_raw(), &["/user/hand/left/input/y/click"]),
        (
            actions.shoot.as_raw(),
            &["/user/hand/left/input/trigger/value"],
        ),
        (
            actions.toggle_menu.as_raw(),
            &["/user/hand/left/input/menu/click"],
        ),
        (
            actions.select.as_raw(),
            &["/user/hand/right/input/trigger/value"],
        ),
        (
            actions.hold.as_raw(),
            &["/user/hand/left/input/squeeze/value"],
        ),
        (actions.haptic.as_raw(), &["/user/hand/left/output/haptic"]),
        (
            actions.right_haptic.as_raw(),
            &["/user/hand/right/output/haptic"],
        ),
    ];
    for (action, paths) in table {
        bindings.write(OxrSuggestActionBinding {
            action,
            interaction_profile: TOUCH_PROFILE.into(),
            bindings: paths.iter().map(|p| (*p).into()).collect(),
        });
    }
}

fn attach_set(
    actions: Option<Res<ControllerActions>>,
    mut attach: MessageWriter<OxrAttachActionSet>,
) {
    if let Some(actions) = actions {
        attach.write(OxrAttachActionSet(actions.set.clone()));
    }
}

/// 创建手柄位姿空间，并让锚点实体跟随它们。
fn create_spaces(
    actions: Option<Res<ControllerActions>>,
    session: Res<OxrSession>,
    left_anchor: Single<Entity, With<LeftHandAnchor>>,
    aim_anchor: Single<Entity, With<RightAimAnchor>>,
    left_aim_anchor: Single<Entity, With<LeftAimAnchor>>,
    mut commands: Commands,
) {
    let Some(actions) = actions else {
        return;
    };
    let space = |action: &openxr::Action<Posef>| {
        session.create_action_space(action, openxr::Path::NULL, Isometry3d::IDENTITY)
    };
    match (
        space(&actions.left_pose),
        space(&actions.right_pose),
        space(&actions.right_aim),
        space(&actions.left_aim),
    ) {
        (Ok(left), Ok(right), Ok(aim), Ok(left_aim)) => {
            commands.insert_resource(ControllerSpaces { left, right });
            commands.entity(*left_anchor).insert(left);
            commands.entity(*aim_anchor).insert(aim);
            commands.entity(*left_aim_anchor).insert(left_aim);
        }
        (left, right, aim, left_aim) => error!(
            "input: creating controller spaces failed: {:?} {:?} {:?} {:?}",
            left.err(),
            right.err(),
            aim.err(),
            left_aim.err()
        ),
    }
}

fn remove_spaces(
    anchors: Query<
        Entity,
        Or<(
            With<LeftHandAnchor>,
            With<RightAimAnchor>,
            With<LeftAimAnchor>,
        )>,
    >,
    mut commands: Commands,
) {
    commands.remove_resource::<ControllerSpaces>();
    for anchor in &anchors {
        commands.entity(anchor).remove::<XrSpace>();
    }
}

fn sync_actions(
    actions: Option<Res<ControllerActions>>,
    mut sync: MessageWriter<OxrSyncActionSet>,
) {
    if let Some(actions) = actions {
        sync.write(OxrSyncActionSet(actions.set.clone()));
    }
}

/// 读取按键：左 Y 键是否按着，左扳机手动拍照，
/// 握住左侧键连续拍摄，左菜单键切换面板，两个扳机的按下状态供界面点击用。
fn read_buttons(
    actions: Option<Res<ControllerActions>>,
    session: Res<OxrSession>,
    mut select: ResMut<SelectButton>,
    mut hold: ResMut<HoldButton>,
    mut left_trigger: ResMut<LeftTrigger>,
    mut record_key: ResMut<RecordKey>,
    mut manual_shot: MessageWriter<ManualShot>,
    mut toggle_menu: MessageWriter<ToggleMenu>,
) {
    let Some(actions) = actions else {
        return;
    };
    let is_pressed = |action: &openxr::Action<bool>| {
        action
            .state(&session, openxr::Path::NULL)
            .is_ok_and(|state| state.current_state)
    };
    let just_pressed = |action: &openxr::Action<bool>| {
        action
            .state(&session, openxr::Path::NULL)
            .is_ok_and(|state| state.changed_since_last_sync && state.current_state)
    };
    match (is_pressed(&actions.record), record_key.held_since) {
        (true, None) => record_key.held_since = Some(Instant::now()),
        (false, Some(_)) => record_key.held_since = None,
        _ => {}
    }
    if just_pressed(&actions.shoot) {
        manual_shot.write(ManualShot);
    }
    let trigger = is_pressed(&actions.shoot);
    if left_trigger.pressed != trigger {
        left_trigger.pressed = trigger;
    }
    if just_pressed(&actions.toggle_menu) {
        toggle_menu.write(ToggleMenu);
    }
    let pressed = is_pressed(&actions.select);
    if select.pressed != pressed {
        select.pressed = pressed;
    }
    let held = is_pressed(&actions.hold);
    if hold.pressed != held {
        hold.pressed = held;
    }
}

/// 把震动请求变成手柄的震动脉冲，多个脉冲之间留出间隔。
fn play_haptics(
    actions: Option<Res<ControllerActions>>,
    session: Res<OxrSession>,
    mut requests: MessageReader<Haptic>,
    mut queue: Local<VecDeque<(Instant, bool, Duration, f32)>>,
) {
    let now = Instant::now();
    for request in requests.read() {
        let (right, pulses, length, amplitude) = match request {
            Haptic::Shot => (false, 1, Duration::from_millis(30), 0.4),
            Haptic::Stage => (false, 2, Duration::from_millis(60), 0.7),
            Haptic::Record => (false, 1, Duration::from_millis(350), 1.0),
            Haptic::Tick => (false, 1, Duration::from_millis(50), 0.6),
            Haptic::ClickLeft => (false, 1, CLICK_PULSE, CLICK_AMPLITUDE),
            Haptic::ClickRight => (true, 1, CLICK_PULSE, CLICK_AMPLITUDE),
        };
        for pulse in 0..pulses {
            queue.push_back((now + (length + PULSE_GAP) * pulse, right, length, amplitude));
        }
    }
    let Some(actions) = actions else {
        queue.clear();
        return;
    };
    while let Some(&(at, right, length, amplitude)) = queue.front()
        && at <= now
    {
        queue.pop_front();
        let vibration = openxr::HapticVibration::new()
            .amplitude(amplitude)
            .frequency(openxr::sys::FREQUENCY_UNSPECIFIED)
            .duration(openxr::Duration::from_nanos(length.as_nanos() as i64));
        let action = if right {
            &actions.right_haptic
        } else {
            &actions.haptic
        };
        if let Err(err) = action.apply_feedback(&session, openxr::Path::NULL, &vibration) {
            error!("input: haptic feedback failed: {err}");
        }
    }
}
