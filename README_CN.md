# Open Splat Capture

[English](README.md) | 简体中文

<img width="100%" alt="10_3" src="https://github.com/user-attachments/assets/4e2976d6-850e-4f5b-ba11-584e1de89338" />
<img width="720" alt="1367f27533eaf30a8b16c53910656afb" src="https://github.com/user-attachments/assets/ed1dd9c9-8eef-4d07-8093-89f0ca9a6064" />

一款运行在 VR 头显上的采集应用，用来拍摄训练 3D 高斯泼溅（3DGS）所需的数据。

它通过录制头显的相机画面，并记录每张照片拍摄时的位姿和深度数据，辅助提升 COLMAP 的重建质量；同时通过基于环境表面的引导系统，标记已被拍摄的表面与拍摄角度。

目前仅支持 Meta Quest 3 和 Quest 3S。Pico 支持仍处于非常早期的计划阶段。


## 输出

在应用的录制列表中点击 **导出** 后，录制会以 zip 格式保存到头显的 `Download/OpenSplatCapture/` 目录。
您可以选择使用文件管理应用的无线传输，ADB 调试，或其他方式将对应文件传输到电脑，再运行下面的工具以导出 COLMAP 数据。

从 [Releases](https://github.com/HIM049/OpenSplatCapture/releases) 页面下载 `colmap_export`。把导出的录制解压，然后转换成 COLMAP 工程：

```
colmap_export <session_dir> <output_dir>
```

输出包含 `images/` 和 `sparse/0/`（相机、图像，以及由深度融合出的点云），可以直接用于大多数 3DGS 训练工具。

录制格式也兼容 OpenQuestCapture 的工作流：运行 `tools/expand_session.py` 可以还原成原始格式，供它的重建流程使用。


## 关于开发

本项目是一个为了解决实际需求而构建的工具。鉴于我的主力技术栈并不在 XR 与图形学领域，本项目的核心代码主要由 AI 生成。我主要负责了技术选择、解决方案和架构设计、以及界面和设计资源的制作。

它并非我独立完成的工程作品，但所有发布版本均经过审查与实际设备测试，以确保核心流程的稳定可用。如果在使用中遇到问题，欢迎提交 Issue 反馈。


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
