//! 自身目录定位 / Locating application-owned directories.
//!
//! 默认（非 portable）基于 [`directories`] crate 获取本项目（WhiteBox-Forge）的
//! 配置 / 数据 / 缓存 / 运行时 / 状态目录。所有入口都遵循 **fail-close**：平台无法
//! 确定目录时返回 [`DirError`]，**绝不**静默回退到当前目录、临时目录或空路径。
//!
//! By default (non-portable) we resolve this project's config / data / cache / runtime /
//! state directories via the [`directories`] crate. Every entry point is **fail-close**:
//! if the platform cannot determine a directory we return [`DirError`] instead of silently
//! falling back to the current directory, a temp directory, or an empty path.
//!
//! # 便携构建 / Portable builds
//!
//! 启用 `portable` feature 后，**所有目录都定位到可执行文件所在目录**下，只向“自己目录”
//! 写入，不触碰 OS 用户目录：
//! - `config_dir()` / `preference_dir()` → `<exe_dir>/config`
//! - `data_dir()` → `<exe_dir>/data`
//! - `cache_dir()` → `<exe_dir>/cache`
//! - `runtime_dir()` → `<exe_dir>/run`
//! - `state_dir()` → `<exe_dir>/state`
//!
//! 此模式下 [`project_dirs`] / [`base_dirs`] 不提供（它们返回 `directories` 类型，与
//! 便携根无关）。
//!
//! With the `portable` feature, every directory lives under the executable's own
//! directory and nothing is written to OS user directories.
//!
//! # 平台 / Platforms
//!
//! 桌面平台（非 Android/iOS）正常工作；**移动端（Android/iOS）暂以 `todo!()` 占位**，
//! 因为移动端的目录布局尚未定义。这是显式的“未实现”，不是静默回退。
//! Desktop targets work normally; **Android/iOS currently `todo!()`** because their
//! directory layouts are not defined yet. That is an explicit "unimplemented", not a
//! silent fallback.

use std::path::{Path, PathBuf};

#[cfg(all(
    not(feature = "portable"),
    not(any(target_os = "android", target_os = "ios"))
))]
use directories::{BaseDirs, ProjectDirs};

use super::DirError;

/// 反向域名限定符 / Reverse-DNS qualifier（仅 macOS 使用）。
pub const QUALIFIER: &str = "io";
/// 组织名 / Organization name（macOS 与 Windows 使用）。
pub const ORGANIZATION: &str = "WhiteBox";
/// 应用名 / Application name.
pub const APPLICATION: &str = "WhiteBox-Forge";

/// 本项目“自身目录”的聚合 / Aggregate of the app-owned directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppDirs {
    /// 配置目录 / Config directory.
    pub config: PathBuf,
    /// 本地配置目录（不随漫游同步）/ Local config directory.
    pub config_local: PathBuf,
    /// 数据目录 / Data directory.
    pub data: PathBuf,
    /// 本地数据目录（不随漫游同步）/ Local data directory.
    pub data_local: PathBuf,
    /// 缓存目录 / Cache directory.
    pub cache: PathBuf,
    /// 临时目录（可丢弃的中间产物）/ Temporary directory (disposable intermediates).
    pub tmp: PathBuf,
    /// 偏好目录 / Preference directory.
    pub preference: PathBuf,
}

/// 可执行文件所在目录（便携根）/ The executable's directory (portable root).
///
/// fail-close：`current_exe()` 失败或没有父目录都返回 [`DirError`]。
/// Fail-close: `current_exe()` failure or a missing parent returns [`DirError`].
#[cfg(all(
    feature = "portable",
    not(any(target_os = "android", target_os = "ios"))
))]
fn portable_root() -> Result<PathBuf, DirError> {
    let exe = std::env::current_exe().map_err(|source| DirError::Io {
        path: PathBuf::from("<current_exe>"),
        source,
    })?;
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or(DirError::Unavailable("current_exe parent"))
}

/// 解析 [`ProjectDirs`] / Resolve [`ProjectDirs`]（仅桌面平台的非便携构建）。
///
/// 返回 `Err(DirError::Unavailable)` 表示平台无法给出应用目录（例如没有可用的
/// HOME / AppData）；这是 fail-close，调用方必须显式处理。
/// Returns `Err(DirError::Unavailable)` when the platform cannot provide app
/// directories (e.g. no HOME / AppData); callers must handle it explicitly.
#[cfg(all(
    not(feature = "portable"),
    not(any(target_os = "android", target_os = "ios"))
))]
pub fn project_dirs() -> Result<ProjectDirs, DirError> {
    ProjectDirs::from(QUALIFIER, ORGANIZATION, APPLICATION)
        .ok_or(DirError::Unavailable("ProjectDirs::from"))
}

/// 解析 [`BaseDirs`] / Resolve [`BaseDirs`]（仅桌面平台的非便携构建）。
///
/// 与 [`project_dirs`] 一样 fail-close：无法确定即报错。
/// Fail-close like [`project_dirs`]: error when it cannot be determined.
#[cfg(all(
    not(feature = "portable"),
    not(any(target_os = "android", target_os = "ios"))
))]
pub fn base_dirs() -> Result<BaseDirs, DirError> {
    BaseDirs::new().ok_or(DirError::Unavailable("BaseDirs::new"))
}

/// `app_dirs` 的便携实现：全部位于可执行文件目录下 / portable impl: under the exe dir.
#[cfg(all(
    feature = "portable",
    not(any(target_os = "android", target_os = "ios"))
))]
fn app_dirs_impl() -> Result<AppDirs, DirError> {
    let root = portable_root()?;
    Ok(AppDirs {
        config: root.join("config"),
        config_local: root.join("config"),
        data: root.join("data"),
        data_local: root.join("data"),
        cache: root.join("cache"),
        tmp: root.join("tmp"),
        preference: root.join("config"),
    })
}

/// `app_dirs` 的默认实现：来自 OS 用户目录 / default impl: OS user directories.
#[cfg(all(
    not(feature = "portable"),
    not(any(target_os = "android", target_os = "ios"))
))]
fn app_dirs_impl() -> Result<AppDirs, DirError> {
    let p = project_dirs()?;
    Ok(AppDirs {
        config: p.config_dir().to_path_buf(),
        config_local: p.config_local_dir().to_path_buf(),
        data: p.data_dir().to_path_buf(),
        data_local: p.data_local_dir().to_path_buf(),
        cache: p.cache_dir().to_path_buf(),
        tmp: tmp_dir_impl()?,
        preference: p.preference_dir().to_path_buf(),
    })
}

/// 移动端占位：目录布局未定义 / mobile placeholder: directory layout undefined.
#[cfg(any(target_os = "android", target_os = "ios"))]
fn app_dirs_impl() -> Result<AppDirs, DirError> {
    todo!("mobile directory layout is not implemented yet (Android/iOS)")
}

/// 一次性获取全部应用目录 / Resolve all application directories at once.
pub fn app_dirs() -> Result<AppDirs, DirError> {
    app_dirs_impl()
}

/// 配置目录 / Config directory.
pub fn config_dir() -> Result<PathBuf, DirError> {
    Ok(app_dirs()?.config)
}

/// 数据目录 / Data directory.
pub fn data_dir() -> Result<PathBuf, DirError> {
    Ok(app_dirs()?.data)
}

/// 缓存目录 / Cache directory.
pub fn cache_dir() -> Result<PathBuf, DirError> {
    Ok(app_dirs()?.cache)
}

/// 临时目录 / Temporary directory（可丢弃的中间产物）。
///
/// 便携：`<exe_dir>/tmp`。非便携：优先每用户 runtime 目录（Linux 的
/// `$XDG_RUNTIME_DIR/<app>`），否则回退到 OS 临时目录（Windows/macOS 的 temp 本身即
/// 每用户）。移动端 `todo!()`。
///
/// Linux 缺少 `XDG_RUNTIME_DIR` 时会回退到**共享 `/tmp`**；敏感数据请用 `cache_dir`/
/// `runtime_dir`，不要用本目录。
/// Portable: `<exe_dir>/tmp`. Non-portable: prefer the per-user runtime dir (Linux
/// `$XDG_RUNTIME_DIR/<app>`), else the OS temp dir (already per-user on Windows/macOS).
/// Mobile: `todo!()`. Without `XDG_RUNTIME_DIR` on Linux this falls back to shared
/// `/tmp`; use `cache_dir`/`runtime_dir` for sensitive data instead.
pub fn tmp_dir() -> Result<PathBuf, DirError> {
    tmp_dir_impl()
}

/// `tmp_dir` 的便携实现 / portable impl of `tmp_dir`.
#[cfg(all(
    feature = "portable",
    not(any(target_os = "android", target_os = "ios"))
))]
fn tmp_dir_impl() -> Result<PathBuf, DirError> {
    Ok(portable_root()?.join("tmp"))
}

/// `tmp_dir` 的默认实现 / default impl of `tmp_dir`.
#[cfg(all(
    not(feature = "portable"),
    not(any(target_os = "android", target_os = "ios"))
))]
fn tmp_dir_impl() -> Result<PathBuf, DirError> {
    // 优先每用户 runtime 目录；拿不到时回退 OS 临时目录（不因缺 HOME 而失败）。
    // Prefer the per-user runtime dir; fall back to the OS temp dir (do not fail merely
    // because HOME/ProjectDirs is unavailable).
    Ok(project_dirs()
        .ok()
        .and_then(|p| p.runtime_dir().map(Path::to_path_buf))
        .unwrap_or_else(std::env::temp_dir))
}

/// 移动端占位 / mobile placeholder.
#[cfg(any(target_os = "android", target_os = "ios"))]
fn tmp_dir_impl() -> Result<PathBuf, DirError> {
    todo!("mobile tmp directory is not implemented yet (Android/iOS)")
}

/// 偏好目录 / Preference directory.
pub fn preference_dir() -> Result<PathBuf, DirError> {
    Ok(app_dirs()?.preference)
}

/// `runtime_dir` 的便携实现 / portable impl of `runtime_dir`.
#[cfg(all(
    feature = "portable",
    not(any(target_os = "android", target_os = "ios"))
))]
fn runtime_dir_impl() -> Result<PathBuf, DirError> {
    Ok(portable_root()?.join("run"))
}

/// `runtime_dir` 的默认实现 / default impl of `runtime_dir`.
#[cfg(all(
    not(feature = "portable"),
    not(any(target_os = "android", target_os = "ios"))
))]
fn runtime_dir_impl() -> Result<PathBuf, DirError> {
    project_dirs()?
        .runtime_dir()
        .map(Path::to_path_buf)
        .ok_or(DirError::Unavailable("ProjectDirs::runtime_dir"))
}

/// 移动端占位 / mobile placeholder.
#[cfg(any(target_os = "android", target_os = "ios"))]
fn runtime_dir_impl() -> Result<PathBuf, DirError> {
    todo!("mobile runtime directory is not implemented yet (Android/iOS)")
}

/// 运行时目录 / Runtime directory（临时、会话结束即清理）。
///
/// 非便携构建下部分平台没有该目录 → `Err(DirError::Unavailable)`；
/// 便携构建下为 `<exe_dir>/run`；移动端为 `todo!()`。
/// Not available on every platform in non-portable builds → `Err(DirError::Unavailable)`;
/// `<exe_dir>/run` in portable builds; `todo!()` on mobile.
pub fn runtime_dir() -> Result<PathBuf, DirError> {
    runtime_dir_impl()
}

/// `state_dir` 的便携实现 / portable impl of `state_dir`.
#[cfg(all(
    feature = "portable",
    not(any(target_os = "android", target_os = "ios"))
))]
fn state_dir_impl() -> Result<PathBuf, DirError> {
    Ok(portable_root()?.join("state"))
}

/// `state_dir` 的默认实现 / default impl of `state_dir`.
#[cfg(all(
    not(feature = "portable"),
    not(any(target_os = "android", target_os = "ios"))
))]
fn state_dir_impl() -> Result<PathBuf, DirError> {
    project_dirs()?
        .state_dir()
        .map(Path::to_path_buf)
        .ok_or(DirError::Unavailable("ProjectDirs::state_dir"))
}

/// 移动端占位 / mobile placeholder.
#[cfg(any(target_os = "android", target_os = "ios"))]
fn state_dir_impl() -> Result<PathBuf, DirError> {
    todo!("mobile state directory is not implemented yet (Android/iOS)")
}

/// 状态目录 / State directory（跨会话保留但不宜同步）。
///
/// 非便携构建下部分平台没有该目录 → `Err(DirError::Unavailable)`；
/// 便携构建下为 `<exe_dir>/state`；移动端为 `todo!()`。
/// Not available on every platform in non-portable builds → `Err(DirError::Unavailable)`;
/// `<exe_dir>/state` in portable builds; `todo!()` on mobile.
pub fn state_dir() -> Result<PathBuf, DirError> {
    state_dir_impl()
}

/// 确保目录存在（含父目录）/ Ensure the directory exists (including parents).
///
/// Unix 以 `0700`（仅属主）创建，避免 `create_dir_all` 默认权限到 `protect_dir`
/// 生效之间被其他用户写入；Windows 无 std 级“带 DACL 创建”，沿用 `create_dir_all`
/// （继承父 ACL），需要更强隔离时随后调用 [`crate::dir::protect_dir`]。
/// On Unix the directory is created with `0700` (owner-only), avoiding a window where
/// other users could write before `protect_dir`. Windows has no std API to create with a
/// DACL, so it inherits the parent ACL; call `protect_dir` afterwards for stronger isolation.
///
/// fail-close：创建失败原样返回 [`DirError::Io`]，不吞错。
/// Fail-close: creation failures surface as [`DirError::Io`].
pub fn ensure_dir(path: &Path) -> Result<(), DirError> {
    #[cfg(unix)]
    let result = {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
    };
    #[cfg(not(unix))]
    let result = std::fs::create_dir_all(path);

    result.map_err(|source| DirError::Io {
        path: path.to_path_buf(),
        source,
    })
}
