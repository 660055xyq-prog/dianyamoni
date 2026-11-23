//! algo：核心算法层（滤波 / 内阻 / 电量 / 校准 / 关机 / 黑匣子 / 满电学习）。

pub mod blackbox;
pub mod calib;
pub mod filter;
pub mod full_learn;
pub mod ir;
pub mod shutdown;
pub mod soc;
