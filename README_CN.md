# Open Splat Capture

[English](README.md) | 简体中文

一款运行在头显上的采集应用，用来拍摄训练 3D 高斯泼溅（3DGS）所需的数据。

它会录制头显的相机画面并记录头显位姿和深度，并通过引导系统提示哪些表面已经从哪些角度拍过。

目前仅支持 Meta Quest 3 和 Quest 3S。Pico 支持仍处于非常早期的计划阶段。


## 输出

从 [Releases](https://github.com/HIM049/OpenSplatCapture/releases) 页面下载 `colmap_export`。把导出的录制解压，然后转换成 COLMAP 工程：

```
colmap_export <session_dir> <output_dir>
```

输出包含 `images/` 和 `sparse/0/`（相机、图像，以及由深度融合出的点云），可以直接用于大多数 3DGS 训练工具。

录制格式也兼容 OpenQuestCapture 的工作流：运行 `tools/expand_session.py` 可以还原成原始格式，供它的重建流程使用。


## 关于开发

我对于 XR 或图形方面并不擅长，这个应用的很多代码是由 AI 辅助编写的。
所以这个应用并不完全是我自己的作品。
不过每个版本在发布前都会经过检查和测试。

如果遇到问题，欢迎提交 issue。


## 隐私

应用完全离线运行，不收集任何个人数据。详见 [PRIVACY.md](PRIVACY.md)。


## 构建

需要安装带 `aarch64-linux-android` 目标的 Rust、`cargo-ndk` 以及 Android SDK（NDK 27.0.12077973）。

```
.\android\gradlew.bat -p android assembleDebug
```

COLMAP 导出器是普通的 Rust 程序：

```
cargo build --release -p colmap_export
```


## 许可与致谢

感谢 samuelm2 的 [OpenQuestCapture](https://github.com/samuelm2/OpenQuestCapture) 和 [quest-3d-reconstruction](https://github.com/samuelm2/quest-3d-reconstruction)，为本项目提供了很大的帮助。

内置字体：Noto Sans SC（SIL Open Font License 1.1）、Material Icons（Apache License 2.0）。
