use std::path::{Path, PathBuf};

pub const HMD_POSES: &str = "hmd_poses.csv";
pub const LEFT_CONTROLLER_POSES: &str = "left_controller_poses.csv";
pub const RIGHT_CONTROLLER_POSES: &str = "right_controller_poses.csv";
/// 用已设定的房间开始录制时，存下的房间网格和由它生成的表面点，仅供调试查看。
pub const ROOM_LAYOUT: &str = "room_layout.ply";
pub const ROOM_POINTS: &str = "room_points.ply";

/// 图像和深度在会话里的存储方式。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Storage {
    /// 相机帧存 JPEG，深度存压缩的 16 位原值；需要先展开才能交给上游管线。
    #[default]
    Compressed,
    /// 与 OpenQuestCapture 相同的未压缩文件。
    Raw,
}

impl Storage {
    pub fn depth_extension(self) -> &'static str {
        match self {
            Storage::Compressed => "depth16z",
            Storage::Raw => "raw",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    pub const BOTH: [Side; 2] = [Side::Left, Side::Right];

    pub fn prefix(self) -> &'static str {
        match self {
            Side::Left => "left",
            Side::Right => "right",
        }
    }

    /// 在深度纹理数组和各类成对数据里的下标。
    pub fn index(self) -> usize {
        match self {
            Side::Left => 0,
            Side::Right => 1,
        }
    }

    pub fn camera_dir(self, session: &Path) -> PathBuf {
        session.join(format!("{}_camera_raw", self.prefix()))
    }

    pub fn camera_image_format(self, session: &Path) -> PathBuf {
        session.join(format!("{}_camera_image_format.json", self.prefix()))
    }

    pub fn camera_characteristics(self, session: &Path) -> PathBuf {
        session.join(format!("{}_camera_characteristics.json", self.prefix()))
    }

    pub fn depth_dir(self, session: &Path) -> PathBuf {
        session.join(format!("{}_depth", self.prefix()))
    }

    pub fn depth_descriptors(self, session: &Path) -> PathBuf {
        session.join(format!("{}_depth_descriptors.csv", self.prefix()))
    }

    pub fn depth_file(self, session: &Path, unix_ms: i64, storage: Storage) -> PathBuf {
        self.depth_dir(session)
            .join(format!("{unix_ms}.{}", storage.depth_extension()))
    }
}
