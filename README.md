# Open Splat Capture

English | [简体中文](README_CN.md)

A capture APP that run on your headset, to capture data for train 3D Gaussian Splatting.

It records the headset's camera images together with the headset pose and depth
with a guiding system to showing which surfaces have been captured from which angles. 

This app currently only support Meta Quest3 / 3s. Pico support are still in very early stage.

## Output

Download `colmap_export` from the [Releases](https://github.com/HIM049/OpenSplatCapture/releases) page. Unzip an exported recording, then convert it to a COLMAP project:

```
colmap_export <session_dir> <output_dir>
```

The output has `images/` and `sparse/0/` (cameras, images and a point cloud fused from depth), ready for most 3DGS trainers.

Recordings also stay compatible with the OpenQuestCapture by running this convert script: `tools/expand_session.py` expands them back to the raw layout for its pipeline.


## About development

I'm not an expert in XR or graphics. Much of the code in this app was written by AI agents.
So not all of this app is my own work.
Even so, I test every version on a real headset before releasing it.
  
If something doesn't work for you, please open an issue.


## Privacy

The app works fully offline and collects no personal data. See
[PRIVACY.md](PRIVACY.md).


## Building

Requires Rust with the `aarch64-linux-android` target, `cargo-ndk`, and the
Android SDK (NDK 27.0.12077973).

```
.\android\gradlew.bat -p android assembleDebug
```

The COLMAP exporter is a plain Rust program:

```
cargo build --release -p colmap_export
```


## License & Credits

Thanks to [OpenQuestCapture](https://github.com/samuelm2/OpenQuestCapture) and [quest-3d-reconstruction](https://github.com/samuelm2/quest-3d-reconstruction) by samuelm2. They helped me a lot.

Bundled fonts: Noto Sans SC (SIL Open Font License 1.1), Material Icons (Apache License 2.0).
