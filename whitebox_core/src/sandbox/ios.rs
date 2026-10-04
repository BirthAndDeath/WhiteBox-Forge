//! iOS 平台进程沙箱实现。
//!
//! iOS 应用已经运行在 Apple 自身的 app 沙箱（系统级强制访问控制）中，
//! 且应用进程无权自行收紧或扩展系统限制。因此任何能力都不覆盖 ——
//! 全部由 [`super::ReportSet::from_config`] 自动标为“请求了 → Error”。

use super::{CapabilityReport, ReportSet, SandboxConfig};

pub(super) fn setup(config: &SandboxConfig) -> Vec<CapabilityReport> {
    ReportSet::from_config(config).finish()
}