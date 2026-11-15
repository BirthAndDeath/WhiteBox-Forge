//! 目录定位与保护 / Application directory location & protection.
//!
//! 子模块 / Submodules:
//! - [`get_dir`]：基于 [`directories`] 定位自身的配置 / 数据 / 缓存 / 运行时目录；
//! - [`protect_dir`]：基于权限、按平台宏分派的目录保护。
//!
//! - [`get_dir`]: locate the app's config / data / cache / runtime directories via
//!   [`directories`].
//! - [`protect_dir`]: permission-based, platform-dispatched directory protection.
//!
//! 设计原则 / Design principle: **fail-close**。任何无法确定 / 无法施加的情况都返回
//! 错误，绝不静默回退或静默成功。
//! Any inability to determine or enforce returns an error; never silently fall back or
//! silently succeed.

#[path = "get_dir.rs"]
pub mod get_dir;

#[path = "protect_dir.rs"]
pub mod protect_dir;

pub use get_dir::{
    APPLICATION, AppDirs, ORGANIZATION, QUALIFIER, app_dirs, cache_dir, config_dir, data_dir,
    ensure_dir, preference_dir, runtime_dir, state_dir, tmp_dir,
};
// 便携构建没有 OS 级 `ProjectDirs`/`BaseDirs`，移动端也未实现，故不导出。
// Portable builds have no OS-level `ProjectDirs`/`BaseDirs`, and mobile is unimplemented,
// so they are not exported in those configurations.
#[cfg(all(
    not(feature = "portable"),
    not(any(target_os = "android", target_os = "ios"))
))]
pub use get_dir::{base_dirs, project_dirs};
// `protect_dir` 在类型命名空间是模块、在值命名空间是函数，二者不冲突。
// `protect_dir` is a module in the type namespace and a function in the value namespace;
// they do not collide.
pub use protect_dir::{ProtectError, protect_dir};

use std::fmt;
use std::path::PathBuf;

/// 目录操作错误 / Directory operation error.
///
/// fail-close：无法确定目录时返回错误，而不是退回到一个"看起来能用的"路径。
/// Fail-close: return an error when a directory cannot be determined instead of falling
/// back to a path that merely "looks usable".
#[non_exhaustive]
#[derive(Debug)]
pub enum DirError {
    /// 平台无法确定该目录 / The platform could not determine this directory.
    ///
    /// 字段为失败来源 / the field names the failing source.
    Unavailable(&'static str),
    /// 文件系统操作失败 / Filesystem operation failed.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl fmt::Display for DirError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DirError::Unavailable(want) => {
                write!(f, "platform cannot determine directory: {want}")
            }
            DirError::Io { path, source } => {
                write!(f, "filesystem error on {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for DirError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DirError::Io { source, .. } => Some(source),
            DirError::Unavailable(_) => None,
        }
    }
}
