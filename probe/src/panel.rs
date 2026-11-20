use std::time::{Duration, Instant};

use bevy::{
    asset::RenderAssetUsages,
    camera::RenderTarget,
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages},
    text::FontWeight,
};
use bevy_mod_openxr::{openxr_session_running, resources::OxrInstance};
use bevy_mod_xr::spaces::XrPrimaryReferenceSpace;

use crate::{
    controls::ProbeCommand,
    report::{Report, Section, Status},
    runtime::{ViewSpace, head_transform},
};

const TEXT_FONT: &[u8] = include_bytes!("../../assets/fonts/NotoSansSC.ttf");
const IMAGE_SIZE: UVec2 = UVec2::new(1800, 1100);
/// 面板在世界里的宽高（米），宽高比和贴图一致。
const PANEL_SIZE: Vec2 = Vec2::new(1.44, 0.88);
/// 面板出现在头显前方多远（米）。
const PANEL_DISTANCE: f32 = 1.2;
/// 面板内容最快多久重画一次。
const REFRESH_INTERVAL: Duration = Duration::from_millis(250);

const SURFACE: Color = Color::srgba(0.11, 0.11, 0.12, 0.96);
const TEXT: Color = Color::srgb(0.93, 0.93, 0.93);
const DIM_TEXT: Color = Color::srgb(0.66, 0.66, 0.68);

pub struct PanelPlugin;

impl Plugin for PanelPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, (load_font, setup).chain())
            .add_systems(Update, refresh)
            .add_systems(Update, place.run_if(openxr_session_running));
    }
}

#[derive(Resource)]
struct Panel {
    entity: Entity,
    rows: Entity,
    thumbnails: Entity,
    placed: bool,
}

fn load_font(mut fonts: ResMut<Assets<Font>>) {
    if let Err(err) = fonts.insert(
        &Handle::<Font>::default(),
        Font::from_bytes(TEXT_FONT.to_vec()),
    ) {
        error!("probe: loading the font failed: {err}");
    }
}

fn font(size: f32, weight: FontWeight) -> TextFont {
    TextFont {
        font_size: FontSize::Px(size),
        weight,
        ..default()
    }
}

fn status_color(status: Status) -> Color {
    match status {
        Status::Ok => Color::srgb(0.30, 0.80, 0.45),
        Status::Warn => Color::srgb(0.96, 0.72, 0.22),
        Status::Fail => Color::srgb(0.94, 0.34, 0.31),
        Status::Info => Color::srgb(0.50, 0.62, 0.82),
        Status::Pending => Color::srgb(0.55, 0.55, 0.58),
    }
}

/// 生成面板：界面画到贴图上，再贴到世界里的矩形上。
fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    let mut image = Image::new_fill(
        Extent3d {
            width: IMAGE_SIZE.x,
            height: IMAGE_SIZE.y,
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
    let ui_camera = commands
        .spawn((
            Camera2d,
            Camera {
                order: -1,
                ..default()
            },
            Msaa::Sample4,
            RenderTarget::Image(image.clone().into()),
        ))
        .id();
    let entity = commands
        .spawn((
            Mesh3d(meshes.add(Rectangle::new(PANEL_SIZE.x, PANEL_SIZE.y))),
            MeshMaterial3d(materials.add(StandardMaterial {
                base_color_texture: Some(image),
                unlit: true,
                cull_mode: None,
                alpha_mode: AlphaMode::Blend,
                ..default()
            })),
            Transform::from_xyz(0.0, 1.3, -PANEL_DISTANCE),
        ))
        .id();

    let mut rows = Entity::PLACEHOLDER;
    let mut thumbnails = Entity::PLACEHOLDER;
    commands
        .spawn((
            Node {
                width: percent(100),
                height: percent(100),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(px(40.0)),
                row_gap: px(20.0),
                border_radius: BorderRadius::all(px(40.0)),
                ..default()
            },
            BackgroundColor(SURFACE),
            UiTargetCamera(ui_camera),
        ))
        .with_children(|root| {
            root.spawn((
                Text::new(format!(
                    "Open Splat Capture 设备检测 v{}",
                    env!("CARGO_PKG_VERSION")
                )),
                font(44.0, FontWeight::BOLD),
                TextColor(TEXT),
            ));
            rows = root
                .spawn(Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: px(12.0),
                    ..default()
                })
                .id();
            thumbnails = root
                .spawn(Node {
                    flex_direction: FlexDirection::Row,
                    column_gap: px(16.0),
                    ..default()
                })
                .id();
            root.spawn(Node {
                flex_grow: 1.0,
                ..default()
            });
            root.spawn((
                Text::new(
                    "左Y 重测相机 · 左X 打开房间设定 · 右A 面板移到眼前 · 其余按键用于测试手柄",
                ),
                font(26.0, FontWeight::NORMAL),
                TextColor(DIM_TEXT),
            ));
        });
    commands.insert_resource(Panel {
        entity,
        rows,
        thumbnails,
        placed: false,
    });
}

/// 面板上的一行：名称、状态和结论。
struct Row {
    name: &'static str,
    status: Status,
    text: String,
}

/// 从完整报告里提炼出面板上的几项核心结论。
fn summarize(report: &Report) -> Vec<Row> {
    let item = |section, key| report.item(section, key);
    let pending = || (Status::Pending, "检测中".to_string());
    let from = |section, key| {
        item(section, key)
            .map(|i| (i.status, i.detail.clone()))
            .unwrap_or_else(pending)
    };
    let mut rows = Vec::new();
    let mut push = |name, (status, text): (Status, String)| rows.push(Row { name, status, text });

    let model = item(Section::Device, "model")
        .map(|i| i.detail.clone())
        .unwrap_or_default();
    let version = ["ro.pui.build.version", "ro.pico.os.version", "display"]
        .into_iter()
        .find_map(|key| item(Section::Device, key))
        .map(|i| i.detail.clone())
        .unwrap_or_default();
    push("设备", (Status::Info, format!("{model} · {version}")));

    push(
        "透视",
        match item(Section::Runtime, "混合模式") {
            Some(i) if i.status == Status::Ok => (Status::Ok, "支持".into()),
            Some(i) if i.status == Status::Warn => (Status::Fail, "不支持，只能不透明显示".into()),
            Some(i) => (i.status, i.detail.clone()),
            None => pending(),
        },
    );
    push(
        "6DoF",
        match item(Section::Tracking, "头显位姿") {
            Some(i) if i.status == Status::Ok => (Status::Ok, "位置和朝向都在跟踪".into()),
            Some(i) => (i.status, i.detail.clone()),
            None => pending(),
        },
    );
    push("时间同步", from(Section::Tracking, "时间换算"));

    let profile = item(Section::Controls, "左手配置")
        .map(|i| i.detail.rsplit('/').next().unwrap_or_default().to_string())
        .unwrap_or_default();
    push(
        "手柄",
        match item(Section::Controls, "按过的键") {
            Some(i) => (
                i.status,
                format!(
                    "{profile} · 已按 {}",
                    i.detail.split(' ').next().unwrap_or_default()
                ),
            ),
            None => (Status::Pending, format!("{profile} · 请依次按下各个按键")),
        },
    );

    push(
        "PICO 相机",
        match (
            item(Section::PicoCamera, "帧率"),
            item(Section::PicoCamera, "图像数据"),
        ) {
            (Some(fps), Some(data)) if fps.status == Status::Ok => {
                let size = data.detail.split(' ').next().unwrap_or_default();
                let rate = fps.detail.rsplit(" · ").next().unwrap_or_default();
                (Status::Ok, format!("可以取图 · {size} · {rate}"))
            }
            _ => from(Section::PicoCamera, "状态"),
        },
    );
    push("Android 相机", from(Section::AndroidCamera, "状态"));
    push(
        "房间数据",
        match item(Section::Spatial, "房间设定") {
            Some(i) => (i.status, i.detail.clone()),
            None => from(Section::Spatial, "状态"),
        },
    );
    push(
        "检测状态",
        if report.done {
            (Status::Ok, "检测完成，可以摘下头显".into())
        } else {
            (
                Status::Pending,
                "检测中，请戴着头显等待，并允许弹出的权限".into(),
            )
        },
    );
    rows
}

/// 内容有变化时重画面板，最快每 250 毫秒一次。
fn refresh(
    panel: Res<Panel>,
    report: Res<Report>,
    mut commands: Commands,
    mut last: Local<Option<(Instant, u64)>>,
) {
    if last.is_some_and(|(t, generation)| {
        t.elapsed() < REFRESH_INTERVAL || generation == report.generation
    }) {
        return;
    }
    *last = Some((Instant::now(), report.generation));
    commands.entity(panel.rows).despawn_related::<Children>();
    commands
        .entity(panel.thumbnails)
        .despawn_related::<Children>();
    commands.entity(panel.rows).with_children(|rows| {
        for row in summarize(&report) {
            rows.spawn(Node {
                flex_direction: FlexDirection::Row,
                column_gap: px(18.0),
                align_items: AlignItems::FlexStart,
                ..default()
            })
            .with_children(|line| {
                line.spawn((
                    Node {
                        width: px(22.0),
                        height: px(22.0),
                        margin: UiRect::top(px(12.0)),
                        flex_shrink: 0.0,
                        border_radius: BorderRadius::all(px(5.0)),
                        ..default()
                    },
                    BackgroundColor(status_color(row.status)),
                ));
                line.spawn((
                    Text::new(row.name),
                    font(34.0, FontWeight::BOLD),
                    TextColor(TEXT),
                    Node {
                        width: px(230.0),
                        flex_shrink: 0.0,
                        ..default()
                    },
                ));
                line.spawn((
                    Text::new(row.text),
                    font(34.0, FontWeight::NORMAL),
                    TextColor(DIM_TEXT),
                    Node {
                        flex_shrink: 1.0,
                        ..default()
                    },
                ));
            });
        }
    });
    commands.entity(panel.thumbnails).with_children(|row| {
        for thumbnail in &report.thumbnails {
            row.spawn(Node {
                flex_direction: FlexDirection::Column,
                row_gap: px(4.0),
                ..default()
            })
            .with_children(|cell| {
                cell.spawn((
                    ImageNode::new(thumbnail.image.clone()),
                    Node {
                        width: px(220.0),
                        ..default()
                    },
                ));
                cell.spawn((
                    Text::new(thumbnail.label.clone()),
                    font(20.0, FontWeight::NORMAL),
                    TextColor(DIM_TEXT),
                ));
            });
        }
    });
}

/// 头显位姿第一次有效时，以及按右 A 时，把面板放到视线正前方。
fn place(
    mut panel: ResMut<Panel>,
    mut commands_in: MessageReader<ProbeCommand>,
    instance: Res<OxrInstance>,
    base: Option<Res<XrPrimaryReferenceSpace>>,
    view: Option<Res<ViewSpace>>,
    mut transforms: Query<&mut Transform>,
) {
    let recenter = commands_in.read().any(|c| *c == ProbeCommand::Recenter);
    if panel.placed && !recenter {
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
    if let Ok(mut transform) = transforms.get_mut(panel.entity) {
        let position = head.translation + forward * PANEL_DISTANCE;
        *transform = Transform::from_translation(position).looking_to(forward, Vec3::Y);
    }
    panel.placed = true;
}
