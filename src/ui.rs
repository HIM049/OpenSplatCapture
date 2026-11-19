use std::time::{Duration, Instant};

use bevy::{
    asset::{RenderAssetUsages, uuid::Uuid},
    camera::{NormalizedRenderTarget, RenderTarget},
    picking::{
        PickingSystems,
        pointer::{Location, PointerAction, PointerId, PointerInput},
    },
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages},
    text::FontWeight,
};
use bevy_mod_openxr::{openxr_session_running, resources::OxrInstance};
use bevy_mod_xr::spaces::XrPrimaryReferenceSpace;
use capture_format::Storage;
use rust_i18n::t;

use crate::{
    android::system_language,
    camera::Cameras,
    depth::DepthShared,
    input::{
        Haptic, LeftAimAnchor, LeftTrigger, RightAimAnchor, SelectButton, ToggleMenu,
        ToggleRecording,
    },
    pose::{ViewSpace, head_transform},
    preview::{Coverage, DetailCloud},
    recorder::{
        AdvanceStage, CancelRecording, CaptureMode, LastStop, Recorder, RecordingStarted, Stage,
        StopReason, free_bytes, recordings_root,
    },
    room::{MissingReason, RequestRoomSetup, RoomSetup, RoomStatus, SetupNote},
    sessions::{ExportStatus, SessionCommand, Sessions},
    tuning::{
        CANCEL_CONFIRM, HUD_FOLLOW_ANGLE, HUD_FOLLOW_DISTANCE, HUD_SETTLE_ANGLE,
        HUD_SETTLE_DISTANCE, HUD_SMOOTH_TIME,
    },
};

const IMAGE_SIZE: UVec2 = UVec2::new(1200, 820);
/// 面板在世界里的宽高（米），宽高比和贴图一致。
const PANEL_SIZE: Vec2 = Vec2::new(0.84, 0.574);
/// 面板出现在头显前方多远（米）。
const PANEL_DISTANCE: f32 = 0.9;
const HUD_IMAGE_SIZE: UVec2 = UVec2::new(1100, 420);
/// 提示窗在世界里的宽高（米）。
const HUD_SIZE: Vec2 = Vec2::new(0.5, 0.191);
/// 提示窗离头显的距离（米）和它在水平视线下方的角度（度）。
const HUD_DISTANCE: f32 = 0.8;
const HUD_PITCH: f32 = 22.0;
/// 列表里最多显示多少条录制。
const MAX_LISTED_SESSIONS: usize = 4;
const STATUS_INTERVAL: Duration = Duration::from_millis(250);
/// 操作反馈在录制页里停留的时间。
const NOTICE_DURATION: Duration = Duration::from_secs(2);
const POINTER_ID: PointerId =
    PointerId::Custom(Uuid::from_u128(0x6f70_656e_5f63_6170_7475_7265_7569_0001));
const LEFT_POINTER_ID: PointerId =
    PointerId::Custom(Uuid::from_u128(0x6f70_656e_5f63_6170_7475_7265_7569_0002));
/// 定时拍摄可选的帧率和它们的显示文字。
const RATES: [(u8, &str); 5] = [(1, "1"), (2, "2"), (3, "3"), (5, "5"), (10, "10")];

/// 正文字体（中英文共用）和 Material Icons 图标字体。
const TEXT_FONT: &[u8] = include_bytes!("../assets/fonts/NotoSansSC.ttf");
const ICON_FONT: &[u8] = include_bytes!("../assets/fonts/MaterialIconsOutlined.otf");
/// 导航用的 Material Icons 码位：home、video_library、settings。
const ICON_HOME: char = '\u{e88a}';
const ICON_RECORDINGS: char = '\u{e04a}';
const ICON_SETTINGS: char = '\u{e8b8}';

const SURFACE: Color = Color::srgb(0.23, 0.23, 0.23);
const RAIL: Color = Color::srgb(0.34, 0.34, 0.34);
const RAISED: Color = Color::srgb(0.3, 0.3, 0.3);
const CARD: Color = Color::srgb(0.47, 0.47, 0.47);
const LIGHT: Color = Color::WHITE;
const SOFT: Color = Color::srgb(0.74, 0.74, 0.74);
const MUTED: Color = Color::srgb(0.43, 0.43, 0.43);
const INK: Color = Color::srgb(0.07, 0.07, 0.07);
const INK_DIM: Color = Color::srgb(0.32, 0.32, 0.32);
const TEXT: Color = Color::srgb(0.96, 0.96, 0.96);
const DIM_TEXT: Color = Color::srgb(0.8, 0.8, 0.8);
const FAINT_TEXT: Color = Color::srgb(0.64, 0.64, 0.64);
const ACCENT: Color = Color::srgb(0.24, 0.49, 0.88);
const WARNING: Color = Color::srgb(0.98, 0.75, 0.33);
const HOVER: Color = Color::srgb(0.45, 0.72, 1.0);

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<UiCommand>()
            .init_resource::<PendingDelete>()
            .init_resource::<ListPage>()
            .init_resource::<ActiveTab>()
            .init_resource::<Notice>()
            .init_resource::<PointerRays>()
            .init_resource::<PendingCancel>()
            .init_resource::<UiLanguage>()
            .init_resource::<HudView>()
            .add_systems(Startup, (load_fonts, init_language, setup).chain())
            .add_systems(First, drive_pointer.in_set(PickingSystems::Input))
            .add_systems(
                Update,
                (
                    place_panel.run_if(openxr_session_running),
                    place_hud.run_if(openxr_session_running),
                    apply_commands,
                    apply_language,
                    refresh_hud_view,
                    update_texts,
                    update_looks,
                    update_layout,
                    rebuild_session_list,
                    draw_pointer_ray,
                )
                    .chain(),
            );
    }
}

/// 界面按钮触发的操作。
#[derive(Message, Clone, Debug)]
enum UiCommand {
    ShowTab(Tab),
    Set(SettingValue),
    Toggle(SwitchSetting),
    /// 选择开始方式：用已设定的房间，或先扫描。
    ChooseStart(bool),
    SetUpRoom,
    StartStop,
    /// 提示窗上的主按钮：开始前说明里的开始，或录制中的下一步。
    HudPrimary,
    /// 提示窗上的次按钮：取消录制。
    HudSecondary,
    Refresh,
    PreviousPage,
    NextPage,
    Export(String),
    Delete(String),
}

/// 主面板的页面。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Tab {
    #[default]
    Home,
    Recordings,
    Settings,
}

/// 界面语言。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Language {
    #[default]
    English,
    Chinese,
}

impl Language {
    fn locale(self) -> &'static str {
        match self {
            Language::English => "en",
            Language::Chinese => "zh-CN",
        }
    }
}

/// 分段选择里的一个选项。
#[derive(Clone, Copy, PartialEq, Debug)]
enum SettingValue {
    Language(Language),
    Mode(CaptureMode),
    Rate(u8),
    Storage(Storage),
}

/// 开关类的设置。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SwitchSetting {
    Controllers,
}

/// 分段选项的显示文字：固定文字或语言表里的键。
#[derive(Clone, Copy)]
enum Caption {
    Fixed(&'static str),
    Key(&'static str),
}

#[derive(Resource, Default)]
struct UiLanguage(Language);

#[derive(Resource, Default)]
struct ActiveTab(Tab);

/// 已点过一次删除、等待再次确认的会话。
#[derive(Resource, Default)]
struct PendingDelete(Option<String>);

/// 取消录制的按钮点过一次、等待再次确认的时刻。
#[derive(Resource, Default)]
struct PendingCancel(Option<Instant>);

impl PendingCancel {
    fn waiting(&self) -> bool {
        self.0
            .is_some_and(|clicked| clicked.elapsed() < CANCEL_CONFIRM)
    }
}

/// 录制列表当前显示第几页，从 0 开始。
#[derive(Resource, Default)]
struct ListPage(usize);

/// 最近一次操作反馈的语言表键和它出现的时刻。
#[derive(Resource, Default)]
struct Notice(Option<(&'static str, Instant)>);

#[derive(Resource)]
struct IconFont(Handle<Font>);

/// 提示窗当前要显示的内容。
#[derive(Resource, Default)]
struct HudView {
    title: String,
    hint: String,
    tips: String,
    left: String,
    right: String,
    progress: Option<f32>,
    primary: Option<String>,
    secondary: Option<String>,
}

#[derive(Resource)]
struct Panel {
    entity: Entity,
    target: NormalizedRenderTarget,
    visible: bool,
    placed: bool,
}

/// 显示阶段和进度的提示窗，面板隐藏时出现，跟在视线下方。
#[derive(Resource)]
struct Hud {
    entity: Entity,
    target: NormalizedRenderTarget,
    visible: bool,
    /// 提示窗当前跟随的头部位置和水平朝向。
    anchor: Option<(Vec3, Vec3)>,
    /// 正在追向新的头部位置。
    following: bool,
    /// 追赶时位置和朝向的变化速度。
    velocity: (Vec3, Vec3),
}

/// 一只手的射线的起点和它在面板或提示窗上的落点，用于绘制；方向为零表示这只手没有射线。
#[derive(Default)]
struct PointerRay {
    origin: Vec3,
    direction: Vec3,
    hit: Option<Vec3>,
}

/// 右手和左手的射线。
#[derive(Resource, Default)]
struct PointerRays([PointerRay; 2]);

/// 内容来自语言表、只随语言变化的文字。
#[derive(Component)]
struct Tr(&'static str);

/// 内容随状态变化的文字。
#[derive(Component, Clone, Copy)]
enum Dyn {
    HomeWarning,
    RoomSubtitle,
    TimedNote,
    RecordingNote,
    StartLabel,
    FreeSpace,
    RecordingsNote,
    LastStop,
    HudTitle,
    HudHint,
    HudTips,
    HudLeft,
    HudRight,
    HudPrimary,
    HudSecondary,
}

/// 背景色随状态变化的元素。
#[derive(Component, Clone, Copy)]
enum Look {
    Rail(Tab),
    Card(bool),
    Start,
    Segment(SettingValue),
    SwitchTrack(SwitchSetting),
    SwitchKnob(SwitchSetting),
}

/// 文字颜色随状态变化的元素。
#[derive(Component, Clone, Copy)]
enum Ink {
    Rail(Tab),
    CardTitle(bool),
    CardSubtitle(bool),
    Start,
    Segment(SettingValue),
}

/// 显示与否或布局随状态变化的元素。
#[derive(Component, Clone, Copy)]
enum Layout {
    Page(Tab),
    ModeRow(CaptureMode),
    HomeCards,
    TimedNote,
    SetupRoom,
    /// 文字为空时隐藏。
    HideEmpty,
    SwitchTrack(SwitchSetting),
    HudStats,
    Progress,
    ProgressFill,
    HudPrimary,
    HudSecondary,
}

#[derive(Component)]
struct SessionList;

fn text_font(size: f32, weight: FontWeight) -> TextFont {
    TextFont {
        font_size: FontSize::Px(size),
        weight,
        ..default()
    }
}

fn label(text: impl Into<String>, size: f32, weight: FontWeight, color: Color) -> impl Bundle {
    (
        Text::new(text),
        text_font(size, weight),
        TextColor(color),
        Pickable::IGNORE,
    )
}

/// 一段来自语言表的文字，切换语言时自动更新。
fn tr(key: &'static str, size: f32, weight: FontWeight, color: Color) -> impl Bundle {
    (label(t!(key), size, weight, color), Tr(key))
}

fn icon(font: &Handle<Font>, glyph: char, size: f32, color: Color) -> impl Bundle {
    (
        Text::new(glyph.to_string()),
        TextFont {
            font: font.clone().into(),
            font_size: FontSize::Px(size),
            ..default()
        },
        TextColor(color),
        Pickable::IGNORE,
    )
}

/// 生成一个可点击的圆角块：悬停时显示描边，点击时发出给定的操作，内容由调用方添加。
fn clickable<'a>(
    parent: &'a mut ChildSpawnerCommands,
    node: Node,
    background: Color,
    command: UiCommand,
) -> EntityCommands<'a> {
    let mut entity = parent.spawn((
        node,
        BackgroundColor(background),
        Outline::new(px(3.0), px(2.0), Color::NONE),
    ));
    entity
        .observe(
            move |mut click: On<Pointer<Click>>, mut commands: MessageWriter<UiCommand>| {
                click.propagate(false);
                commands.write(command.clone());
            },
        )
        .observe(
            |over: On<Pointer<Over>>, mut outlines: Query<&mut Outline>| {
                if let Ok(mut outline) = outlines.get_mut(over.entity) {
                    outline.color = HOVER;
                }
            },
        )
        .observe(|out: On<Pointer<Out>>, mut outlines: Query<&mut Outline>| {
            if let Ok(mut outline) = outlines.get_mut(out.entity) {
                outline.color = Color::NONE;
            }
        });
    entity
}

/// 胶囊形按钮的布局。
fn pill(horizontal: f32, vertical: f32) -> Node {
    Node {
        padding: UiRect::axes(px(horizontal), px(vertical)),
        border_radius: BorderRadius::all(px(999.0)),
        justify_content: JustifyContent::Center,
        align_items: AlignItems::Center,
        ..default()
    }
}

fn column(gap: f32) -> Node {
    Node {
        flex_direction: FlexDirection::Column,
        row_gap: px(gap),
        ..default()
    }
}

fn spacer() -> Node {
    Node {
        flex_grow: 1.0,
        ..default()
    }
}

/// 生成一块显示界面的平面：界面先画到透明贴图上，再贴到世界里 `world_size` 大小的矩形上。
/// 返回矩形实体、界面相机和指针用的渲染目标。
fn spawn_surface(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    image_size: UVec2,
    world_size: Vec2,
    order: isize,
) -> (Entity, Entity, NormalizedRenderTarget) {
    let mut image = Image::new_fill(
        Extent3d {
            width: image_size.x,
            height: image_size.y,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[0, 0, 0, 0],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    image.texture_descriptor.usage =
        TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST | TextureUsages::RENDER_ATTACHMENT;
    let image = images.add(image);

    let render_target = RenderTarget::Image(image.clone().into());
    let target = render_target
        .normalize(None)
        .expect("image render targets always normalize");
    let ui_camera = commands
        .spawn((
            Camera2d,
            Camera { order, ..default() },
            Msaa::Off,
            render_target,
        ))
        .id();
    let entity = commands
        .spawn((
            Mesh3d(meshes.add(Rectangle::new(world_size.x, world_size.y))),
            MeshMaterial3d(materials.add(StandardMaterial {
                base_color_texture: Some(image),
                unlit: true,
                cull_mode: None,
                alpha_mode: AlphaMode::Blend,
                ..default()
            })),
            Transform::from_xyz(0.0, 1.3, -PANEL_DISTANCE),
            Visibility::Hidden,
        ))
        .id();
    (entity, ui_camera, target)
}

/// 正文字体放进默认字体的位置，没有指定字体的文字都用它；图标字体单独登记。
fn load_fonts(mut fonts: ResMut<Assets<Font>>, mut commands: Commands) {
    if let Err(err) = fonts.insert(
        &Handle::<Font>::default(),
        Font::from_bytes(TEXT_FONT.to_vec()),
    ) {
        error!("ui: loading the text font failed: {err}");
    }
    commands.insert_resource(IconFont(fonts.add(Font::from_bytes(ICON_FONT.to_vec()))));
}

/// 系统语言是中文时用中文界面，否则用英文。
fn init_language(mut language: ResMut<UiLanguage>) {
    if system_language().is_some_and(|code| code.starts_with("zh")) {
        language.0 = Language::Chinese;
    }
    rust_i18n::set_locale(language.0.locale());
}

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    icons: Res<IconFont>,
) {
    let (entity, ui_camera, target) = spawn_surface(
        &mut commands,
        &mut meshes,
        &mut materials,
        &mut images,
        IMAGE_SIZE,
        PANEL_SIZE,
        -1,
    );
    commands
        .spawn((
            Node {
                width: percent(100),
                height: percent(100),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                ..default()
            },
            UiTargetCamera(ui_camera),
        ))
        .with_children(|root| {
            root.spawn((
                Node {
                    flex_direction: FlexDirection::Column,
                    padding: UiRect::all(px(10.0)),
                    row_gap: px(10.0),
                    border_radius: BorderRadius::all(px(999.0)),
                    ..default()
                },
                BackgroundColor(RAIL),
            ))
            .with_children(|rail| {
                for (tab, glyph) in [
                    (Tab::Home, ICON_HOME),
                    (Tab::Recordings, ICON_RECORDINGS),
                    (Tab::Settings, ICON_SETTINGS),
                ] {
                    let node = Node {
                        width: px(76.0),
                        height: px(76.0),
                        ..pill(0.0, 0.0)
                    };
                    clickable(rail, node, Color::NONE, UiCommand::ShowTab(tab))
                        .insert(Look::Rail(tab))
                        .with_child((icon(&icons.0, glyph, 42.0, TEXT), Ink::Rail(tab)));
                }
            });
            root.spawn((
                Node {
                    flex_grow: 1.0,
                    height: percent(100),
                    margin: UiRect::left(px(24.0)),
                    padding: UiRect::all(px(44.0)),
                    border_radius: BorderRadius::all(px(56.0)),
                    flex_direction: FlexDirection::Column,
                    ..default()
                },
                BackgroundColor(SURFACE),
            ))
            .with_children(|content| {
                spawn_home(content);
                spawn_recordings(content);
                spawn_settings(content);
            });
        });
    commands.insert_resource(Panel {
        entity,
        target,
        visible: true,
        placed: false,
    });

    let (entity, hud_camera, target) = spawn_surface(
        &mut commands,
        &mut meshes,
        &mut materials,
        &mut images,
        HUD_IMAGE_SIZE,
        HUD_SIZE,
        -2,
    );
    commands
        .spawn((
            Node {
                width: percent(100),
                height: percent(100),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(px(36.0)),
                border_radius: BorderRadius::all(px(48.0)),
                ..default()
            },
            BackgroundColor(SURFACE),
            UiTargetCamera(hud_camera),
        ))
        .with_children(|root| {
            root.spawn((label("", 38.0, FontWeight::BOLD, TEXT), Dyn::HudTitle));
            root.spawn((
                label("", 26.0, FontWeight::MEDIUM, TEXT),
                Dyn::HudHint,
                Node {
                    margin: UiRect::top(px(12.0)),
                    ..default()
                },
            ));
            root.spawn((
                label("", 24.0, FontWeight::NORMAL, DIM_TEXT),
                Dyn::HudTips,
                Layout::HideEmpty,
                Node {
                    margin: UiRect::top(px(10.0)),
                    ..default()
                },
            ));
            root.spawn(spacer());
            root.spawn(Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::FlexEnd,
                column_gap: px(18.0),
                ..default()
            })
            .with_children(|bottom| {
                bottom
                    .spawn((
                        Node {
                            flex_grow: 1.0,
                            flex_basis: px(0.0),
                            ..column(10.0)
                        },
                        Layout::HudStats,
                    ))
                    .with_children(|stats| {
                        stats
                            .spawn(Node {
                                flex_direction: FlexDirection::Row,
                                justify_content: JustifyContent::SpaceBetween,
                                ..default()
                            })
                            .with_children(|line| {
                                line.spawn((
                                    label("", 22.0, FontWeight::NORMAL, DIM_TEXT),
                                    Dyn::HudLeft,
                                ));
                                line.spawn((
                                    label("", 22.0, FontWeight::NORMAL, DIM_TEXT),
                                    Dyn::HudRight,
                                ));
                            });
                        stats
                            .spawn((
                                Node {
                                    height: px(14.0),
                                    border_radius: BorderRadius::all(px(7.0)),
                                    ..default()
                                },
                                BackgroundColor(MUTED),
                                Layout::Progress,
                            ))
                            .with_child((
                                Node {
                                    height: percent(100),
                                    width: percent(0),
                                    border_radius: BorderRadius::all(px(7.0)),
                                    ..default()
                                },
                                BackgroundColor(ACCENT),
                                Layout::ProgressFill,
                            ));
                    });
                clickable(bottom, pill(30.0, 14.0), MUTED, UiCommand::HudSecondary)
                    .insert(Layout::HudSecondary)
                    .with_child((
                        label("", 26.0, FontWeight::SEMIBOLD, TEXT),
                        Dyn::HudSecondary,
                    ));
                clickable(bottom, pill(34.0, 14.0), LIGHT, UiCommand::HudPrimary)
                    .insert(Layout::HudPrimary)
                    .with_child((label("", 26.0, FontWeight::BOLD, INK), Dyn::HudPrimary));
            });
        });
    commands.insert_resource(Hud {
        entity,
        target,
        visible: false,
        anchor: None,
        following: false,
        velocity: default(),
    });
    commands.spawn(POINTER_ID);
    commands.spawn(LEFT_POINTER_ID);
}

fn page(tab: Tab) -> impl Bundle {
    (
        Node {
            flex_grow: 1.0,
            ..column(0.0)
        },
        Layout::Page(tab),
    )
}

/// 主页：选择开始方式，开始或结束录制。
fn spawn_home(content: &mut ChildSpawnerCommands) {
    content.spawn(page(Tab::Home)).with_children(|page| {
        page.spawn(tr("home.title", 44.0, FontWeight::BOLD, TEXT));
        page.spawn((
            label("", 22.0, FontWeight::NORMAL, WARNING),
            Dyn::HomeWarning,
            Layout::HideEmpty,
            Node {
                margin: UiRect::top(px(10.0)),
                ..default()
            },
        ));
        page.spawn((
            Node {
                margin: UiRect::top(px(28.0)),
                ..column(16.0)
            },
            Layout::HomeCards,
        ))
        .with_children(|cards| {
            for room in [true, false] {
                let node = Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: px(16.0),
                    padding: UiRect::axes(px(30.0), px(24.0)),
                    border_radius: BorderRadius::all(px(32.0)),
                    ..default()
                };
                clickable(cards, node, CARD, UiCommand::ChooseStart(room))
                    .insert(Look::Card(room))
                    .with_children(|card| {
                        card.spawn((
                            Node {
                                flex_grow: 1.0,
                                flex_basis: px(0.0),
                                ..column(6.0)
                            },
                            Pickable::IGNORE,
                        ))
                        .with_children(|texts| {
                            let title = if room {
                                "home.room.title"
                            } else {
                                "home.scan.title"
                            };
                            texts.spawn((
                                tr(title, 30.0, FontWeight::SEMIBOLD, TEXT),
                                Ink::CardTitle(room),
                            ));
                            if room {
                                texts.spawn((
                                    label("", 22.0, FontWeight::NORMAL, DIM_TEXT),
                                    Dyn::RoomSubtitle,
                                    Ink::CardSubtitle(true),
                                ));
                            } else {
                                texts.spawn((
                                    tr("home.scan.subtitle", 22.0, FontWeight::NORMAL, DIM_TEXT),
                                    Ink::CardSubtitle(false),
                                ));
                            }
                        });
                        if room {
                            clickable(card, pill(24.0, 10.0), SOFT, UiCommand::SetUpRoom)
                                .insert(Layout::SetupRoom)
                                .with_child(tr("home.room.setup", 24.0, FontWeight::SEMIBOLD, INK));
                        }
                    });
            }
        });
        page.spawn((
            label("", 24.0, FontWeight::NORMAL, DIM_TEXT),
            Dyn::TimedNote,
            Layout::TimedNote,
            Node {
                margin: UiRect::top(px(28.0)),
                ..default()
            },
        ));
        page.spawn(spacer());
        page.spawn((
            label("", 22.0, FontWeight::NORMAL, DIM_TEXT),
            Dyn::RecordingNote,
            Layout::HideEmpty,
            Node {
                align_self: AlignSelf::Center,
                margin: UiRect::bottom(px(12.0)),
                ..default()
            },
        ));
        let node = Node {
            height: px(84.0),
            ..pill(0.0, 0.0)
        };
        clickable(page, node, LIGHT, UiCommand::StartStop)
            .insert(Look::Start)
            .with_child((
                label("", 30.0, FontWeight::BOLD, INK),
                Dyn::StartLabel,
                Ink::Start,
            ));
    });
}

/// 录制页：剩余空间、导出和删除录制。
fn spawn_recordings(content: &mut ChildSpawnerCommands) {
    content.spawn(page(Tab::Recordings)).with_children(|page| {
        page.spawn(Node {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: px(16.0),
            ..default()
        })
        .with_children(|header| {
            header.spawn(tr("recordings.title", 44.0, FontWeight::BOLD, TEXT));
            header.spawn(spacer());
            header.spawn((
                label("", 22.0, FontWeight::NORMAL, DIM_TEXT),
                Dyn::FreeSpace,
            ));
            clickable(header, pill(24.0, 10.0), SOFT, UiCommand::Refresh).with_child(tr(
                "recordings.refresh",
                22.0,
                FontWeight::SEMIBOLD,
                INK,
            ));
        });
        page.spawn((
            label("", 22.0, FontWeight::NORMAL, DIM_TEXT),
            Dyn::RecordingsNote,
            Layout::HideEmpty,
            Node {
                margin: UiRect::top(px(10.0)),
                ..default()
            },
        ));
        page.spawn((
            Node {
                margin: UiRect::top(px(18.0)),
                ..column(10.0)
            },
            SessionList,
        ));
        page.spawn(spacer());
        page.spawn((
            label("", 20.0, FontWeight::NORMAL, FAINT_TEXT),
            Dyn::LastStop,
            Layout::HideEmpty,
        ));
    });
}

/// 设置页：每行左边是名称和说明，右边是开关或分段选择。
fn spawn_settings(content: &mut ChildSpawnerCommands) {
    content.spawn(page(Tab::Settings)).with_children(|page| {
        page.spawn(tr("settings.title", 44.0, FontWeight::BOLD, TEXT));
        page.spawn(Node {
            margin: UiRect::top(px(16.0)),
            ..column(4.0)
        })
        .with_children(|rows| {
            setting_row(rows, "settings.language", "settings.language.desc", None).with_children(
                |row| {
                    segments(
                        row,
                        &[
                            (
                                SettingValue::Language(Language::English),
                                Caption::Fixed("English"),
                            ),
                            (
                                SettingValue::Language(Language::Chinese),
                                Caption::Fixed("中文"),
                            ),
                        ],
                    );
                },
            );
            setting_row(rows, "settings.mode", "settings.mode.desc", None).with_children(|row| {
                segments(
                    row,
                    &[
                        (
                            SettingValue::Mode(CaptureMode::ByPose),
                            Caption::Key("settings.mode.by_pose"),
                        ),
                        (
                            SettingValue::Mode(CaptureMode::Timed),
                            Caption::Key("settings.mode.timed"),
                        ),
                    ],
                );
            });
            setting_row(
                rows,
                "settings.rate",
                "settings.rate.desc",
                Some(CaptureMode::Timed),
            )
            .with_children(|row| {
                let options: Vec<_> = RATES
                    .iter()
                    .map(|&(rate, text)| (SettingValue::Rate(rate), Caption::Fixed(text)))
                    .collect();
                segments(row, &options);
            });
            setting_row(rows, "settings.storage", "settings.storage.desc", None).with_children(
                |row| {
                    segments(
                        row,
                        &[
                            (
                                SettingValue::Storage(Storage::Compressed),
                                Caption::Fixed("JPEG"),
                            ),
                            (
                                SettingValue::Storage(Storage::Raw),
                                Caption::Key("settings.storage.raw"),
                            ),
                        ],
                    );
                },
            );
            setting_row(
                rows,
                "settings.controllers",
                "settings.controllers.desc",
                None,
            )
            .with_children(|row| switch(row, SwitchSetting::Controllers));
        });
    });
}

/// 一行设置的名称和说明；`mode` 指定时只在该拍摄方式下显示。
fn setting_row<'a>(
    rows: &'a mut ChildSpawnerCommands,
    title: &'static str,
    description: &'static str,
    mode: Option<CaptureMode>,
) -> EntityCommands<'a> {
    let mut row = rows.spawn(Node {
        flex_direction: FlexDirection::Row,
        align_items: AlignItems::Center,
        column_gap: px(20.0),
        padding: UiRect::axes(px(4.0), px(12.0)),
        ..default()
    });
    if let Some(mode) = mode {
        row.insert(Layout::ModeRow(mode));
    }
    row.with_children(|row| {
        row.spawn(Node {
            flex_grow: 1.0,
            flex_basis: px(0.0),
            ..column(4.0)
        })
        .with_children(|texts| {
            texts.spawn(tr(title, 28.0, FontWeight::SEMIBOLD, TEXT));
            texts.spawn(tr(description, 20.0, FontWeight::NORMAL, DIM_TEXT));
        });
    });
    row
}

fn segments(row: &mut ChildSpawnerCommands, options: &[(SettingValue, Caption)]) {
    row.spawn((
        Node {
            flex_direction: FlexDirection::Row,
            padding: UiRect::all(px(4.0)),
            border_radius: BorderRadius::all(px(999.0)),
            ..default()
        },
        BackgroundColor(MUTED),
    ))
    .with_children(|track| {
        for &(value, caption) in options {
            clickable(track, pill(22.0, 8.0), Color::NONE, UiCommand::Set(value))
                .insert(Look::Segment(value))
                .with_children(|option| {
                    let (text, key) = match caption {
                        Caption::Fixed(text) => (text.to_owned(), None),
                        Caption::Key(key) => (t!(key).into_owned(), Some(key)),
                    };
                    let mut text = option.spawn((
                        label(text, 22.0, FontWeight::SEMIBOLD, TEXT),
                        Ink::Segment(value),
                    ));
                    if let Some(key) = key {
                        text.insert(Tr(key));
                    }
                });
        }
    });
}

fn switch(row: &mut ChildSpawnerCommands, setting: SwitchSetting) {
    let node = Node {
        width: px(84.0),
        height: px(44.0),
        padding: UiRect::all(px(5.0)),
        border_radius: BorderRadius::all(px(999.0)),
        align_items: AlignItems::Center,
        ..default()
    };
    clickable(row, node, MUTED, UiCommand::Toggle(setting))
        .insert((Look::SwitchTrack(setting), Layout::SwitchTrack(setting)))
        .with_child((
            Node {
                width: px(34.0),
                height: px(34.0),
                border_radius: BorderRadius::all(px(999.0)),
                ..default()
            },
            BackgroundColor(LIGHT),
            Look::SwitchKnob(setting),
            Pickable::IGNORE,
        ));
}

/// 处理面板的显示切换：录制开始或弹出开始前说明时收起面板，开始被拦下时打开面板回到主页；
/// 每次显示时把它放到头显正前方、面向用户。打开面板会收起开始前说明。
fn place_panel(
    mut toggles: MessageReader<ToggleMenu>,
    mut started: MessageReader<RecordingStarted>,
    mut recorder: ResMut<Recorder>,
    mut tab: ResMut<ActiveTab>,
    mut edges: Local<(bool, bool)>,
    mut panel: ResMut<Panel>,
    mut transforms: Query<(&mut Transform, &mut Visibility)>,
    instance: Res<OxrInstance>,
    base: Option<Res<XrPrimaryReferenceSpace>>,
    view: Option<Res<ViewSpace>>,
) {
    let toggled = toggles.read().count() % 2 == 1;
    if toggled {
        panel.visible = !panel.visible;
        panel.placed = false;
        if panel.visible && recorder.intro_open {
            recorder.intro_open = false;
        }
    }
    let (was_intro, was_blocked) = *edges;
    *edges = (recorder.intro_open, recorder.start_blocked);
    let intro_shown = recorder.intro_open && !was_intro;
    if (started.read().count() > 0 || intro_shown) && panel.visible {
        panel.visible = false;
    }
    if recorder.start_blocked && !was_blocked {
        tab.0 = Tab::Home;
        if !panel.visible {
            panel.visible = true;
            panel.placed = false;
        }
    }
    let Ok((mut transform, mut visibility)) = transforms.get_mut(panel.entity) else {
        return;
    };
    if !panel.visible {
        *visibility = Visibility::Hidden;
        return;
    }
    if panel.placed {
        return;
    }
    let (Some(base), Some(view)) = (base, view) else {
        return;
    };
    let Some(head) = head_transform(&instance, &base, &view) else {
        return;
    };
    let forward = (head.rotation * Vec3::NEG_Z)
        .with_y(0.0)
        .try_normalize()
        .unwrap_or(Vec3::NEG_Z);
    let position = head.translation + forward * PANEL_DISTANCE - Vec3::Y * 0.1;
    *transform = Transform::from_translation(position).looking_to(forward, Vec3::Y);
    *visibility = Visibility::Visible;
    panel.placed = true;
}

/// 让 `current` 平缓地追向 `target`：起步和停下都是渐变的，`smooth_time` 是大致的追赶时间。
fn smooth_damp(
    current: Vec3,
    target: Vec3,
    velocity: &mut Vec3,
    smooth_time: f32,
    dt: f32,
) -> Vec3 {
    let omega = 2.0 / smooth_time;
    let x = omega * dt;
    let decay = 1.0 / (1.0 + x + 0.48 * x * x + 0.235 * x * x * x);
    let change = current - target;
    let step = (*velocity + change * omega) * dt;
    *velocity = (*velocity - step * omega) * decay;
    target + (change + step) * decay
}

/// 面板隐藏时让提示窗出现在视线下方；头部转开或走开一段后平缓地跟过去，追上后停住。
fn place_hud(
    panel: Res<Panel>,
    mut hud: ResMut<Hud>,
    mut transforms: Query<(&mut Transform, &mut Visibility)>,
    instance: Res<OxrInstance>,
    base: Option<Res<XrPrimaryReferenceSpace>>,
    view: Option<Res<ViewSpace>>,
    time: Res<Time>,
) {
    let Ok((mut transform, mut visibility)) = transforms.get_mut(hud.entity) else {
        return;
    };
    let head = match (base, view) {
        (Some(base), Some(view)) if !panel.visible => head_transform(&instance, &base, &view),
        _ => None,
    };
    let Some(head) = head else {
        if hud.visible {
            hud.visible = false;
            hud.anchor = None;
            hud.following = false;
            hud.velocity = default();
            *visibility = Visibility::Hidden;
        }
        return;
    };
    let forward = (head.rotation * Vec3::NEG_Z)
        .with_y(0.0)
        .try_normalize()
        .unwrap_or(Vec3::NEG_Z);
    let hud = &mut *hud;
    let (mut origin, mut facing) = hud.anchor.unwrap_or((head.translation, forward));
    if origin.distance(head.translation) > HUD_FOLLOW_DISTANCE
        || facing.angle_between(forward) > HUD_FOLLOW_ANGLE.to_radians()
    {
        hud.following = true;
    }
    if hud.following {
        let dt = time.delta_secs();
        origin = smooth_damp(
            origin,
            head.translation,
            &mut hud.velocity.0,
            HUD_SMOOTH_TIME,
            dt,
        );
        facing = smooth_damp(facing, forward, &mut hud.velocity.1, HUD_SMOOTH_TIME, dt)
            .with_y(0.0)
            .try_normalize()
            .unwrap_or(forward);
        if origin.distance(head.translation) <= HUD_SETTLE_DISTANCE
            && facing.angle_between(forward) <= HUD_SETTLE_ANGLE.to_radians()
        {
            hud.following = false;
            hud.velocity = default();
        }
    }
    hud.anchor = Some((origin, facing));
    let pitch = HUD_PITCH.to_radians();
    let direction = facing * pitch.cos() - Vec3::Y * pitch.sin();
    *transform = Transform::from_translation(origin + direction * HUD_DISTANCE)
        .looking_to(direction, Vec3::Y);
    if !hud.visible {
        *visibility = Visibility::Visible;
        hud.visible = true;
    }
}

/// 射线与一块 `size` 大小的矩形的交点：沿射线的距离和以左上角为原点的归一化坐标。
fn ray_hit(
    surface: &GlobalTransform,
    size: Vec2,
    origin: Vec3,
    direction: Vec3,
) -> Option<(f32, Vec2)> {
    let to_surface = surface.affine().inverse();
    let origin = to_surface.transform_point3(origin);
    let direction = to_surface.transform_vector3(direction);
    if direction.z.abs() <= 1e-6 {
        return None;
    }
    let distance = -origin.z / direction.z;
    let local = origin + direction * distance;
    (distance > 0.0 && local.x.abs() <= size.x / 2.0 && local.y.abs() <= size.y / 2.0).then(|| {
        (
            distance,
            Vec2::new(local.x / size.x + 0.5, 0.5 - local.y / size.y),
        )
    })
}

/// 把手柄射线在面板或提示窗上的落点和扳机状态转成界面指针输入，点到界面时震一下；
/// 左手只在没有录制时有射线。
fn drive_pointer(
    panel: Res<Panel>,
    hud: Res<Hud>,
    select: Res<SelectButton>,
    left_trigger: Res<LeftTrigger>,
    recorder: Res<Recorder>,
    right_aim: Query<&GlobalTransform, With<RightAimAnchor>>,
    left_aim: Query<&GlobalTransform, With<LeftAimAnchor>>,
    transforms: Query<&GlobalTransform>,
    mut rays: ResMut<PointerRays>,
    mut last: Local<[Option<(NormalizedRenderTarget, Vec2)>; 2]>,
    mut was_pressed: Local<[bool; 2]>,
    mut pointer_inputs: MessageWriter<PointerInput>,
    mut haptics: MessageWriter<Haptic>,
) {
    const OFF_SURFACE: Vec2 = Vec2::splat(-100.0);
    let surfaces = [
        (
            panel.visible && panel.placed,
            panel.entity,
            &panel.target,
            PANEL_SIZE,
            IMAGE_SIZE,
        ),
        (
            hud.visible,
            hud.entity,
            &hud.target,
            HUD_SIZE,
            HUD_IMAGE_SIZE,
        ),
    ];
    let hands = [
        (
            POINTER_ID,
            right_aim.single().ok(),
            select.pressed,
            Haptic::ClickRight,
        ),
        (
            LEFT_POINTER_ID,
            left_aim.single().ok().filter(|_| recorder.active.is_none()),
            left_trigger.pressed,
            Haptic::ClickLeft,
        ),
    ];
    for (hand, (id, aim, trigger, click)) in hands.into_iter().enumerate() {
        let ray = &mut rays.0[hand];
        *ray = PointerRay::default();
        let mut nearest: Option<(f32, &NormalizedRenderTarget, Vec2)> = None;
        if let Some(aim) = aim.filter(|aim| aim.translation() != Vec3::ZERO) {
            ray.origin = aim.translation();
            ray.direction = aim.forward().as_vec3();
            for (shown, entity, target, size, image_size) in surfaces {
                if !shown {
                    continue;
                }
                let Ok(surface) = transforms.get(entity) else {
                    continue;
                };
                if let Some((distance, uv)) = ray_hit(surface, size, ray.origin, ray.direction)
                    && nearest.is_none_or(|(other, ..)| distance < other)
                {
                    nearest = Some((distance, target, uv * image_size.as_vec2()));
                }
            }
        }
        // 没有落点时指针留在上一次的界面上，移到界面之外。
        let (target, position) = match (nearest, &last[hand]) {
            (Some((distance, target, position)), _) => {
                ray.hit = Some(ray.origin + ray.direction * distance);
                (target.clone(), position)
            }
            (None, Some((target, _))) => (target.clone(), OFF_SURFACE),
            (None, None) => (panel.target.clone(), OFF_SURFACE),
        };

        let location = Location {
            target: target.clone(),
            position,
        };
        let delta = match &last[hand] {
            Some((last_target, last_position)) if *last_target == target => {
                Some(position - *last_position).filter(|delta| *delta != Vec2::ZERO)
            }
            _ => Some(Vec2::ZERO),
        };
        if let Some(delta) = delta {
            pointer_inputs.write(PointerInput::new(
                id,
                location.clone(),
                PointerAction::Move { delta },
            ));
            last[hand] = Some((target, position));
        }
        let pressed = trigger && (ray.hit.is_some() || was_pressed[hand]);
        if pressed != was_pressed[hand] {
            let action = if pressed {
                haptics.write(click);
                PointerAction::Press(PointerButton::Primary)
            } else {
                PointerAction::Release(PointerButton::Primary)
            };
            pointer_inputs.write(PointerInput::new(id, location, action));
            was_pressed[hand] = pressed;
        }
    }
}

/// 射线落在界面上时画到落点；面板显示而没有落点时只画一小段。
fn draw_pointer_ray(panel: Res<Panel>, rays: Res<PointerRays>, mut gizmos: Gizmos) {
    for ray in &rays.0 {
        if ray.direction == Vec3::ZERO {
            continue;
        }
        match ray.hit {
            Some(hit) => {
                gizmos.line(ray.origin, hit, Color::WHITE);
                gizmos.sphere(hit, 0.008, Color::WHITE);
            }
            None if panel.visible && panel.placed => gizmos.line(
                ray.origin,
                ray.origin + ray.direction * 0.3,
                Color::srgba(1.0, 1.0, 1.0, 0.4),
            ),
            None => {}
        }
    }
}

/// 当前的开始方式能否开始录制：按位姿录制选了已设定的房间时，房间要已找到或正在读取。
fn start_allowed(recorder: &Recorder, room: &RoomSetup) -> bool {
    recorder.mode == CaptureMode::Timed || !recorder.use_room || room.usable()
}

fn apply_commands(
    mut commands: MessageReader<UiCommand>,
    mut recorder: ResMut<Recorder>,
    mut language: ResMut<UiLanguage>,
    mut pending_delete: ResMut<PendingDelete>,
    mut page: ResMut<ListPage>,
    mut tab: ResMut<ActiveTab>,
    mut pending_cancel: ResMut<PendingCancel>,
    mut notice: ResMut<Notice>,
    mut sessions: ResMut<Sessions>,
    room: Res<RoomSetup>,
    mut session_commands: MessageWriter<SessionCommand>,
    mut advance: MessageWriter<AdvanceStage>,
    mut cancel: MessageWriter<CancelRecording>,
    mut room_requests: MessageWriter<RequestRoomSetup>,
    mut toggles: MessageWriter<ToggleRecording>,
) {
    for command in commands.read() {
        info!("ui: {command:?}");
        let was_pending = pending_delete.0.take();
        let was_page = page.0;
        match command {
            UiCommand::ShowTab(shown) => tab.0 = *shown,
            UiCommand::Set(SettingValue::Language(chosen)) => {
                if language.0 != *chosen {
                    language.0 = *chosen;
                }
            }
            UiCommand::Set(SettingValue::Mode(mode)) => recorder.mode = *mode,
            UiCommand::Set(SettingValue::Rate(rate)) => recorder.capture_fps = f32::from(*rate),
            UiCommand::Set(SettingValue::Storage(storage)) => recorder.storage = *storage,
            UiCommand::Toggle(SwitchSetting::Controllers) => {
                recorder.log_controllers = !recorder.log_controllers;
            }
            UiCommand::ChooseStart(use_room) => {
                if recorder.active.is_none() {
                    recorder.use_room = *use_room;
                    recorder.start_blocked = false;
                }
            }
            UiCommand::SetUpRoom => {
                room_requests.write(RequestRoomSetup);
            }
            UiCommand::StartStop => {
                if recorder.active.is_some() || start_allowed(&recorder, &room) {
                    toggles.write(ToggleRecording);
                }
            }
            UiCommand::HudPrimary if recorder.intro_open => {
                toggles.write(ToggleRecording);
            }
            UiCommand::HudPrimary => {
                if recorder.active.is_some() {
                    advance.write(AdvanceStage);
                }
            }
            UiCommand::HudSecondary if pending_cancel.waiting() => {
                pending_cancel.0 = None;
                cancel.write(CancelRecording);
            }
            UiCommand::HudSecondary => pending_cancel.0 = Some(Instant::now()),
            UiCommand::Refresh => {
                session_commands.write(SessionCommand::Refresh);
                notice.0 = Some(("recordings.refreshed", Instant::now()));
            }
            UiCommand::PreviousPage => page.0 = page.0.saturating_sub(1),
            UiCommand::NextPage => page.0 += 1,
            UiCommand::Export(name) => {
                session_commands.write(SessionCommand::Export(name.clone()));
            }
            UiCommand::Delete(name) if was_pending.as_ref() == Some(name) => {
                session_commands.write(SessionCommand::Delete(name.clone()));
            }
            UiCommand::Delete(name) => pending_delete.0 = Some(name.clone()),
        }
        // 删除确认状态或页码变化后要重画列表。
        if was_pending != pending_delete.0 || was_page != page.0 {
            sessions.revision += 1;
        }
    }
}

/// 切换语言后更新所有来自语言表的固定文字。
fn apply_language(language: Res<UiLanguage>, mut texts: Query<(&Tr, &mut Text)>) {
    if !language.is_changed() {
        return;
    }
    rust_i18n::set_locale(language.0.locale());
    for (key, mut text) in &mut texts {
        set_text(&mut text, &t!(key.0));
    }
}

/// 按录制状态组织提示窗的内容：空闲、开始前说明、各个阶段和定时录制。
fn refresh_hud_view(
    recorder: Res<Recorder>,
    coverage: Res<Coverage>,
    cloud: Res<DetailCloud>,
    room: Res<RoomSetup>,
    pending_cancel: Res<PendingCancel>,
    language: Res<UiLanguage>,
    mut view: ResMut<HudView>,
    mut last_update: Local<Option<Instant>>,
) {
    if !language.is_changed() && last_update.is_some_and(|last| last.elapsed() < STATUS_INTERVAL) {
        return;
    }
    *last_update = Some(Instant::now());

    let Some(active) = &recorder.active else {
        *view = if recorder.intro_open {
            HudView {
                title: t!("hud.intro.title").into_owned(),
                tips: t!("hud.intro.tips").into_owned(),
                primary: Some(t!("hud.next").into_owned()),
                ..default()
            }
        } else {
            HudView {
                title: t!("hud.ready.title").into_owned(),
                hint: t!("hud.ready.hint").into_owned(),
                ..default()
            }
        };
        return;
    };
    let fps = format!("{:.0}", recorder.capture_fps);
    let (title, hint, tips) = match (active.mode, active.stage) {
        (CaptureMode::Timed, _) => (
            t!("hud.timed.title"),
            t!("hud.timed.hint", fps = fps),
            t!("hud.timed.tips"),
        ),
        (CaptureMode::ByPose, Stage::Scan) => (
            t!("hud.step1.title"),
            t!("hud.step1.hint"),
            t!("hud.step1.tips"),
        ),
        (CaptureMode::ByPose, Stage::Detail) => (
            t!("hud.step2.title"),
            t!("hud.step2.hint"),
            t!("hud.step2.tips"),
        ),
        (CaptureMode::ByPose, Stage::Free) => (
            t!("hud.step3.title"),
            t!("hud.step3.hint"),
            t!("hud.step3.tips"),
        ),
    };
    let last_step = active.mode == CaptureMode::Timed || active.stage == Stage::Free;
    let scanning = active.mode == CaptureMode::ByPose && active.stage == Stage::Scan;
    let state = if scanning && room.status == RoomStatus::Checking && recorder.use_room {
        Some(t!("hud.state.reading_room"))
    } else if scanning && coverage.too_close() {
        Some(t!("hud.state.step_back"))
    } else {
        None
    };
    let photos = t!("hud.photos", photos = active.ticks).into_owned();
    let (enough, total) = (coverage.enough(), coverage.total());
    let (left, right, progress) = match (active.mode, active.stage) {
        (CaptureMode::Timed, _) => (String::new(), photos, None),
        (CaptureMode::ByPose, Stage::Scan) => (
            state
                .unwrap_or_else(|| t!("hud.points.scan", total = total))
                .into_owned(),
            String::new(),
            None,
        ),
        (CaptureMode::ByPose, Stage::Detail) => (
            t!(
                "hud.points.done",
                percent = enough * 100 / total.max(1),
                done = enough,
                total = total
            )
            .into_owned(),
            photos,
            Some(enough as f32 / total.max(1) as f32),
        ),
        (CaptureMode::ByPose, Stage::Free) => (
            t!("hud.points.cloud", points = cloud.points()).into_owned(),
            photos,
            None,
        ),
    };
    let secondary = if pending_cancel.waiting() {
        t!("hud.confirm_cancel")
    } else {
        t!("hud.cancel")
    };
    let primary = if last_step {
        t!("hud.finish")
    } else {
        t!("hud.next")
    };
    let primary = match recorder.advance_countdown {
        Some(seconds) => format!("{primary} ({seconds})"),
        None => primary.into_owned(),
    };
    *view = HudView {
        title: title.into_owned(),
        hint: hint.into_owned(),
        tips: tips.into_owned(),
        left,
        right,
        progress,
        primary: Some(primary),
        secondary: Some(secondary.into_owned()),
    };
}

fn stop_reason(reason: StopReason) -> String {
    match reason {
        StopReason::User => t!("stop.user"),
        StopReason::Finished => t!("stop.finished"),
        StopReason::Cancelled => t!("stop.cancelled"),
        StopReason::SessionPaused => t!("stop.paused"),
        StopReason::StorageLow => t!("stop.storage"),
    }
    .into_owned()
}

fn missing_reason(reason: MissingReason) -> String {
    match reason {
        MissingReason::Unsupported => t!("room.reason.unsupported"),
        MissingReason::QueryFailed => t!("room.reason.query_failed"),
        MissingReason::TimedOut => t!("room.reason.timed_out"),
        MissingReason::Refused => t!("room.reason.refused"),
        MissingReason::NoSetup => t!("room.reason.none"),
        MissingReason::NoSurfaces => t!("room.reason.no_surfaces"),
    }
    .into_owned()
}

/// 刷新随状态变化的文字。
fn update_texts(
    recorder: Res<Recorder>,
    room: Res<RoomSetup>,
    cameras: Res<Cameras>,
    depth: Res<DepthShared>,
    sessions: Res<Sessions>,
    notice: Res<Notice>,
    language: Res<UiLanguage>,
    view: Res<HudView>,
    mut texts: Query<(&Dyn, &mut Text)>,
    mut last_update: Local<Option<Instant>>,
) {
    let due = last_update.is_none_or(|last| last.elapsed() >= STATUS_INTERVAL);
    if !due && !language.is_changed() && !view.is_changed() {
        return;
    }
    *last_update = Some(Instant::now());

    for (kind, mut text) in &mut texts {
        let value = match kind {
            Dyn::HomeWarning => {
                let mut lines = Vec::new();
                if !cameras.is_open() {
                    lines.push(t!("home.warn_camera"));
                }
                if !depth.is_ready() {
                    lines.push(t!("home.warn_depth"));
                }
                lines.join("\n")
            }
            Dyn::RoomSubtitle => match (&room.status, room.setup_note) {
                (RoomStatus::Found(count), _) => t!("home.room.found", count = count).into_owned(),
                (_, Some(SetupNote::Opening)) => t!("home.room.opening").into_owned(),
                (_, Some(SetupNote::Failed)) => t!("home.room.open_failed").into_owned(),
                (RoomStatus::Checking, None) => t!("home.room.checking").into_owned(),
                (RoomStatus::Unchecked, None) => t!("home.room.unchecked").into_owned(),
                (RoomStatus::Missing(reason), None) => {
                    t!("home.room.missing", reason = missing_reason(*reason)).into_owned()
                }
            },
            Dyn::TimedNote => {
                t!("home.timed", fps = format!("{:.0}", recorder.capture_fps)).into_owned()
            }
            Dyn::RecordingNote => match &recorder.active {
                Some(active) => t!(
                    "home.recording",
                    seconds = format!("{:.0}", active.elapsed().as_secs_f32()),
                    photos = active.ticks
                )
                .into_owned(),
                None => String::new(),
            },
            Dyn::StartLabel => {
                if recorder.active.is_some() {
                    t!("home.stop").into_owned()
                } else {
                    t!("home.start").into_owned()
                }
            }
            Dyn::FreeSpace => match free_bytes(&recordings_root()) {
                Some(bytes) => t!(
                    "recordings.free",
                    gb = format!("{:.1}", bytes as f64 / (1u64 << 30) as f64)
                )
                .into_owned(),
                None => t!("recordings.free_unknown").into_owned(),
            },
            Dyn::RecordingsNote => {
                let mut notes = Vec::new();
                match &sessions.export {
                    ExportStatus::Idle => {}
                    ExportStatus::Running { name, percent } => {
                        notes.push(t!("recordings.exporting", name = name, percent = percent));
                    }
                    ExportStatus::Done(path) => notes.push(t!("recordings.exported", path = path)),
                    ExportStatus::Failed(message) => {
                        notes.push(t!("recordings.export_failed", message = message));
                    }
                }
                if let Some((key, shown)) = notice.0
                    && shown.elapsed() < NOTICE_DURATION
                {
                    notes.push(t!(key));
                }
                notes.join("   ")
            }
            Dyn::LastStop => match &recorder.last_stop {
                None => String::new(),
                Some(LastStop::Stopped {
                    name,
                    reason,
                    seconds,
                    frames,
                    dropped,
                }) => {
                    let mut line = t!(
                        "recordings.last",
                        name = name,
                        reason = stop_reason(*reason),
                        seconds = format!("{seconds:.0}"),
                        photos = frames
                    )
                    .into_owned();
                    if *dropped > 0 {
                        line.push_str(&t!("recordings.dropped", count = dropped));
                    }
                    line
                }
                Some(LastStop::NotStarted(error)) => {
                    let reason = error
                        .clone()
                        .unwrap_or_else(|| stop_reason(StopReason::StorageLow));
                    t!("recordings.not_started", reason = reason).into_owned()
                }
            },
            Dyn::HudTitle => view.title.clone(),
            Dyn::HudHint => view.hint.clone(),
            Dyn::HudTips => view.tips.clone(),
            Dyn::HudLeft => view.left.clone(),
            Dyn::HudRight => view.right.clone(),
            Dyn::HudPrimary => view.primary.clone().unwrap_or_default(),
            Dyn::HudSecondary => view.secondary.clone().unwrap_or_default(),
        };
        set_text(&mut text, &value);
    }
}

/// 分段选项当前是否被选中。
fn is_selected(value: SettingValue, recorder: &Recorder, language: Language) -> bool {
    match value {
        SettingValue::Language(option) => option == language,
        SettingValue::Mode(mode) => mode == recorder.mode,
        SettingValue::Rate(rate) => f32::from(rate) == recorder.capture_fps.round(),
        SettingValue::Storage(storage) => storage == recorder.storage,
    }
}

fn switch_on(setting: SwitchSetting, recorder: &Recorder) -> bool {
    match setting {
        SwitchSetting::Controllers => recorder.log_controllers,
    }
}

fn set_color(color: &mut Color, value: Color) {
    if *color != value {
        *color = value;
    }
}

/// 按状态更新导航、开始方式卡片、开始按钮、分段选择和开关的颜色。
fn update_looks(
    recorder: Res<Recorder>,
    room: Res<RoomSetup>,
    tab: Res<ActiveTab>,
    language: Res<UiLanguage>,
    mut backgrounds: Query<(&Look, &mut BackgroundColor)>,
    mut inks: Query<(&Ink, &mut TextColor)>,
) {
    let can_start = recorder.active.is_some() || start_allowed(&recorder, &room);
    for (look, mut background) in &mut backgrounds {
        let color = match *look {
            Look::Rail(shown) => {
                if shown == tab.0 {
                    LIGHT
                } else {
                    Color::NONE
                }
            }
            Look::Card(room_card) => {
                if recorder.use_room == room_card {
                    LIGHT
                } else {
                    CARD
                }
            }
            Look::Start => {
                if can_start {
                    LIGHT
                } else {
                    MUTED
                }
            }
            Look::Segment(value) => {
                if is_selected(value, &recorder, language.0) {
                    LIGHT
                } else {
                    Color::NONE
                }
            }
            Look::SwitchTrack(setting) => {
                if switch_on(setting, &recorder) {
                    LIGHT
                } else {
                    MUTED
                }
            }
            Look::SwitchKnob(setting) => {
                if switch_on(setting, &recorder) {
                    INK
                } else {
                    LIGHT
                }
            }
        };
        set_color(&mut background.0, color);
    }
    for (ink, mut text_color) in &mut inks {
        let color = match *ink {
            Ink::Rail(shown) => {
                if shown == tab.0 {
                    INK
                } else {
                    TEXT
                }
            }
            Ink::CardTitle(room_card) => {
                if recorder.use_room == room_card {
                    INK
                } else {
                    TEXT
                }
            }
            Ink::CardSubtitle(room_card) => {
                if recorder.use_room == room_card {
                    INK_DIM
                } else {
                    DIM_TEXT
                }
            }
            Ink::Start => {
                if can_start {
                    INK
                } else {
                    FAINT_TEXT
                }
            }
            Ink::Segment(value) => {
                if is_selected(value, &recorder, language.0) {
                    INK
                } else {
                    TEXT
                }
            }
        };
        set_color(&mut text_color.0, color);
    }
}

fn set_display(node: &mut Node, shown: bool) {
    let display = if shown { Display::Flex } else { Display::None };
    if node.display != display {
        node.display = display;
    }
}

/// 按状态决定页面、设置行、提示窗各部分是否显示，以及开关和进度条的位置。
fn update_layout(
    recorder: Res<Recorder>,
    room: Res<RoomSetup>,
    tab: Res<ActiveTab>,
    view: Res<HudView>,
    mut nodes: Query<(&Layout, &mut Node, Option<&Text>)>,
) {
    for (layout, mut node, text) in &mut nodes {
        match *layout {
            Layout::Page(shown) => set_display(&mut node, shown == tab.0),
            Layout::ModeRow(mode) => set_display(&mut node, mode == recorder.mode),
            Layout::HomeCards => set_display(&mut node, recorder.mode == CaptureMode::ByPose),
            Layout::TimedNote => set_display(&mut node, recorder.mode == CaptureMode::Timed),
            Layout::SetupRoom => {
                set_display(&mut node, matches!(room.status, RoomStatus::Missing(_)));
            }
            Layout::HideEmpty => {
                set_display(&mut node, text.is_some_and(|text| !text.0.is_empty()))
            }
            Layout::SwitchTrack(setting) => {
                let justify = if switch_on(setting, &recorder) {
                    JustifyContent::FlexEnd
                } else {
                    JustifyContent::FlexStart
                };
                if node.justify_content != justify {
                    node.justify_content = justify;
                }
            }
            Layout::HudStats => set_display(
                &mut node,
                !view.left.is_empty() || !view.right.is_empty() || view.progress.is_some(),
            ),
            Layout::Progress => set_display(&mut node, view.progress.is_some()),
            Layout::ProgressFill => {
                let width = percent(view.progress.unwrap_or(0.0).clamp(0.0, 1.0) * 100.0);
                if node.width != width {
                    node.width = width;
                }
            }
            Layout::HudPrimary => set_display(&mut node, view.primary.is_some()),
            Layout::HudSecondary => set_display(&mut node, view.secondary.is_some()),
        }
    }
}

fn set_text(text: &mut Text, value: &str) {
    if text.0 != value {
        text.0 = value.to_owned();
    }
}

/// 列表、页码、删除确认状态或语言变化后重建录制列表。
fn rebuild_session_list(
    sessions: Res<Sessions>,
    pending_delete: Res<PendingDelete>,
    language: Res<UiLanguage>,
    mut page: ResMut<ListPage>,
    list: Single<Entity, With<SessionList>>,
    mut shown: Local<Option<(u32, Language)>>,
    mut commands: Commands,
) {
    if *shown == Some((sessions.revision, language.0)) {
        return;
    }
    *shown = Some((sessions.revision, language.0));
    let pages = sessions.list.len().div_ceil(MAX_LISTED_SESSIONS).max(1);
    page.0 = page.0.min(pages - 1);
    let first = page.0 * MAX_LISTED_SESSIONS;

    let mut list = commands.entity(*list);
    list.despawn_related::<Children>();
    list.with_children(|rows| {
        if sessions.list.is_empty() {
            rows.spawn(label(
                t!("recordings.empty"),
                24.0,
                FontWeight::NORMAL,
                DIM_TEXT,
            ));
        }
        for session in sessions.list.iter().skip(first).take(MAX_LISTED_SESSIONS) {
            rows.spawn((
                Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: px(12.0),
                    padding: UiRect::axes(px(24.0), px(14.0)),
                    border_radius: BorderRadius::all(px(24.0)),
                    ..default()
                },
                BackgroundColor(RAISED),
            ))
            .with_children(|line| {
                line.spawn(Node {
                    flex_grow: 1.0,
                    flex_basis: px(0.0),
                    ..column(2.0)
                })
                .with_children(|texts| {
                    texts.spawn((
                        label(session.name.clone(), 24.0, FontWeight::SEMIBOLD, TEXT),
                        TextLayout {
                            linebreak: LineBreak::NoWrap,
                            ..default()
                        },
                    ));
                    texts.spawn(label(
                        t!(
                            "recordings.row",
                            size = format!("{:.0}", session.bytes as f64 / (1u64 << 20) as f64),
                            photos = session.frames
                        ),
                        20.0,
                        FontWeight::NORMAL,
                        DIM_TEXT,
                    ));
                });
                clickable(
                    line,
                    pill(22.0, 10.0),
                    SOFT,
                    UiCommand::Export(session.name.clone()),
                )
                .with_child(label(
                    t!("recordings.export"),
                    22.0,
                    FontWeight::SEMIBOLD,
                    INK,
                ));
                let confirming = pending_delete.0.as_ref() == Some(&session.name);
                let (background, color, key) = if confirming {
                    (WARNING, INK, "recordings.confirm_delete")
                } else {
                    (MUTED, TEXT, "recordings.delete")
                };
                clickable(
                    line,
                    pill(22.0, 10.0),
                    background,
                    UiCommand::Delete(session.name.clone()),
                )
                .with_child(label(t!(key), 22.0, FontWeight::SEMIBOLD, color));
            });
        }
        if pages > 1 {
            rows.spawn(Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: px(12.0),
                ..default()
            })
            .with_children(|line| {
                line.spawn(label(
                    t!("recordings.page", page = page.0 + 1, pages = pages),
                    22.0,
                    FontWeight::NORMAL,
                    DIM_TEXT,
                ));
                clickable(line, pill(22.0, 8.0), MUTED, UiCommand::PreviousPage).with_child(label(
                    t!("recordings.previous"),
                    22.0,
                    FontWeight::SEMIBOLD,
                    TEXT,
                ));
                clickable(line, pill(22.0, 8.0), MUTED, UiCommand::NextPage).with_child(label(
                    t!("recordings.next"),
                    22.0,
                    FontWeight::SEMIBOLD,
                    TEXT,
                ));
            });
        }
    });
}
