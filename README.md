# Open Splat Capture

English | [简体中文](README_CN.md)

<img width="100%" alt="10_3" src="https://github.com/user-attachments/assets/4e2976d6-850e-4f5b-ba11-584e1de89338" />
<img width="720" alt="1367f27533eaf30a8b16c53910656afb" src="https://github.com/user-attachments/assets/ed1dd9c9-8eef-4d07-8093-89f0ca9a6064" />

A capture app for VR headsets that records the data needed to train 3D Gaussian Splatting (3DGS).

It records the headset's camera images along with the pose and depth for every photo, which helps improve COLMAP reconstruction quality.
A guide system built on the room's surfaces marks which surfaces have been captured, and from which angles.

This app currently only supports Meta Quest 3 / 3S. Pico support is still at a very early stage.

Download it throuth [Meta Store](https://www.meta.com/experiences/open-splat-capture/1375280245676821/)


## Output

In the app, press **Export** on a recording in the recordings list. It is saved as a zip file to `Download/OpenSplatCapture/` on the headset.
Transfer the file to your computer using a file manager app's wireless transfer, ADB, or any other method, then run the tool below to convert it to COLMAP data.

Download `colmap_export` from the [Releases](https://github.com/HIM049/OpenSplatCapture/releases) page. Unzip an exported recording, then convert it to a COLMAP project:

```
colmap_export <session_dir> <output_dir>
```

The output has `images/` and `sparse/0/` (cameras, images and a point cloud fused from depth), ready for most 3DGS trainers.

Recordings also stay compatible with the OpenQuestCapture by running this convert script: `tools/expand_session.py` expands them back to the raw layout for its pipeline.


## About Development

This project is a utility built to solve a practical need. Since XR and graphics programming are not my primary tech stack, the core codebase was largely generated with AI assistance. I was primarily responsible for technology selection, solution and architecture design, as well as the UI and design assets.

While it is not a project I developed by my self, every release is reviewed and tested on actual hardware to ensure core workflows remain stable and reliable. If there are some problems you meet, please open an issue.


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
