//! 把一段录制导出成 COLMAP 工程：照片放进 images/，头显位姿换算成相机外参，深度融合出初始点云。

mod colmap;
mod json;
mod points;
mod pose;
mod session;

use std::{
    fs,
    path::{Path, PathBuf},
    process::ExitCode,
    time::Instant,
};

use capture_format::Side;

use crate::{
    colmap::{CameraModel, ImageEntry, Point3D},
    points::{ColorView, FusionSettings},
    pose::Extrinsics,
    session::Result,
};

const USAGE: &str = "\
usage: colmap_export <session_dir> <output_dir> [options]

options:
  --points <depth|none>  where points3D comes from (default: depth)
  --voxel <m>            fusion voxel size in metres (default: 0.02)
  --max-depth <m>        ignore depth farther than this along the view axis (default: 3.0)
  --min-frames <n>       keep voxels seen by at least this many depth frames (default: 3)
  --max-points <n>       evenly thin points3D down to at most n points";

/// points3D 的来源。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PointSource {
    Depth,
    None,
}

struct Options {
    session: PathBuf,
    output: PathBuf,
    points: PointSource,
    fusion: FusionSettings,
    max_points: Option<usize>,
}

fn parse_args(args: &[String]) -> Result<Options> {
    let mut positional = Vec::new();
    let mut points = PointSource::Depth;
    let mut fusion = FusionSettings::default();
    let mut max_points = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if !arg.starts_with("--") {
            positional.push(PathBuf::from(arg));
            continue;
        }
        let value = iter.next().ok_or_else(|| format!("{arg} needs a value"))?;
        let number = |v: &str| v.parse::<f64>().map_err(|_| format!("{arg}: not a number: {v}"));
        match arg.as_str() {
            "--points" => {
                points = match value.as_str() {
                    "depth" => PointSource::Depth,
                    "none" => PointSource::None,
                    other => return Err(format!("--points: unknown source {other}")),
                }
            }
            "--voxel" => fusion.voxel = number(value)? as f32,
            "--max-depth" => fusion.max_depth = number(value)? as f32,
            "--min-frames" => fusion.min_frames = number(value)? as u32,
            "--max-points" => max_points = Some(number(value)? as usize),
            _ => return Err(format!("unknown option {arg}")),
        }
    }
    let [session, output] = <[PathBuf; 2]>::try_from(positional)
        .map_err(|_| "expected a session directory and an output directory".to_string())?;
    if fusion.voxel.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
        return Err("--voxel must be positive".into());
    }
    Ok(Options {
        session,
        output,
        points,
        fusion,
        max_points,
    })
}

fn create_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(|e| format!("create {}: {e}", path.display()))
}

fn run(options: &Options) -> Result<()> {
    let session = &options.session;
    if !session.is_dir() {
        return Err(format!("not a directory: {}", session.display()));
    }
    if options.output.canonicalize().ok() == session.canonicalize().ok() {
        return Err("output directory must differ from the session directory".into());
    }
    let images_dir = options.output.join("images");
    let model_dir = options.output.join("sparse").join("0");
    create_dir(&images_dir)?;
    create_dir(&model_dir)?;

    let poses = session::load_hmd_poses(session)?;
    println!("{} headset poses", poses.len());

    let mut cameras = Vec::new();
    let mut images = Vec::new();
    let mut views = Vec::new();
    for side in Side::BOTH {
        let Some(camera) = session::load_camera(session, side)? else {
            continue;
        };
        let camera_id = cameras.len() as u32 + 1;
        let (mut exported, mut skipped) = (0, 0);
        for frame in session::list_frames(session, side)? {
            let Some(hmd) = poses.at(frame.unix_ms as f64) else {
                skipped += 1;
                continue;
            };
            let extrinsics = Extrinsics::from_unity_camera(&camera.mount.camera_pose(&hmd));
            let name = format!("{}_{}.jpg", frame.side.prefix(), frame.unix_ms);
            let target = images_dir.join(&name);
            fs::copy(&frame.path, &target).map_err(|e| format!("copy to {}: {e}", target.display()))?;
            images.push(ImageEntry {
                id: images.len() as u32 + 1,
                camera_id,
                name,
                extrinsics,
            });
            views.push(ColorView {
                extrinsics,
                intrinsics: camera.intrinsics,
                width: camera.width,
                height: camera.height,
                path: target,
            });
            exported += 1;
        }
        println!("{}: {exported} images, {skipped} without a headset pose", side.prefix());
        cameras.push(CameraModel {
            id: camera_id,
            width: camera.width,
            height: camera.height,
            params: camera.intrinsics,
        });
    }
    if images.is_empty() {
        return Err("no images with a headset pose".into());
    }

    let points = match options.points {
        PointSource::Depth => depth_points(options, &views)?,
        PointSource::None => Vec::new(),
    };

    let write = |name: &str, result: std::io::Result<()>| {
        result.map_err(|e| format!("write {}: {e}", model_dir.join(name).display()))
    };
    write("cameras.bin", colmap::write_cameras(&model_dir.join("cameras.bin"), &cameras))?;
    write("images.bin", colmap::write_images(&model_dir.join("images.bin"), &images))?;
    write("points3D.bin", colmap::write_points(&model_dir.join("points3D.bin"), &points))?;
    println!(
        "wrote {} cameras, {} images, {} points to {}",
        cameras.len(),
        images.len(),
        points.len(),
        options.output.display()
    );
    Ok(())
}

fn depth_points(options: &Options, views: &[ColorView]) -> Result<Vec<Point3D>> {
    let mut frames = Vec::new();
    for side in Side::BOTH {
        frames.extend(session::list_depth(&options.session, side)?);
    }
    if frames.is_empty() {
        eprintln!("no depth frames; points3D will be empty");
        return Ok(Vec::new());
    }
    let started = Instant::now();
    let positions = points::fuse(&frames, &options.fusion);
    println!(
        "fused {} depth frames into {} voxels in {:.1} s",
        frames.len(),
        positions.len(),
        started.elapsed().as_secs_f32()
    );
    let started = Instant::now();
    let mut points = points::colorize(&positions, views, f64::from(options.fusion.voxel));
    println!(
        "coloured {} points from {} photos in {:.1} s",
        points.len(),
        views.len(),
        started.elapsed().as_secs_f32()
    );
    if let Some(max) = options.max_points.filter(|&max| max > 0 && points.len() > max) {
        let total = points.len();
        points = (0..max).map(|i| i * total / max).map(|i| points[i]).collect();
        println!("thinned to {} points", points.len());
    }
    Ok(points)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return ExitCode::from(2);
    }
    match parse_args(&args).and_then(|options| run(&options)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
