//! 录制会话的目录布局、文件格式和数值约定，与 OpenQuestCapture 的输出兼容。

pub mod convert;
pub mod coverage;
pub mod gate;
pub mod geometry;
pub mod layout;
pub mod records;
pub mod room;
pub mod time;

pub use layout::{Side, Storage};
pub use time::TimeBase;
