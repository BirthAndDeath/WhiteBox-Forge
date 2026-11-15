//! 基于权限的平台目录保护 / Permission-based, platform-specific directory protection.
//!
//! # 语义 / Semantics
//!
//! “保护目录”= 把目录收紧为**仅属主可访问**，组 / 其他主体默认拒绝：
//! - Unix：`chmod 0700`（属主 rwx，组/其他无任何权限），并**读回校验**；
//! - Windows：写入一条**仅当前用户**、可继承的 DACL，设置 `PROTECTED`（不再继承父目录 ACE），
//!   并应用到**整棵子树**（已存在的子项也一并覆盖）。
//!
//! "Protect a directory" means tightening it to **owner-only** access, denying group/other
//! by default: `chmod 0700` on Unix (with read-back verification), or a **current-user-only**,
//! inheritable DACL with `PROTECTED` (no parent inheritance) on Windows, applied to the
//! whole subtree so existing children are also covered.
//!
//! # Fail-close 原则 / Fail-close policy
//!
//! 1. 默认拒绝：保护后组 / 其他主体**默认无权访问**。
//! 2. 绝不静默失败：目录不存在、不是目录、权限施加失败、施加后校验不符、平台不支持——
//!    全部返回 `Err(ProtectError)`，**不会**“尽力而为后返回 Ok”。
//!
//! 1. Default-deny: after protection, group/other have **no access**.
//! 2. Never fail silently: missing path, non-directory, failed syscall, failed read-back
//!    verification, or unsupported platform all return `Err(ProtectError)` — we never
//!    return `Ok` after a best-effort attempt.
//!
//! 平台分派由内部宏 [`platform_protect`] 在编译期完成，每种平台只编译一份实现。
//! Platform dispatch is done at compile time by the internal [`platform_protect`] macro;
//! only one implementation is compiled per target.

use std::fmt;
use std::path::{Path, PathBuf};

/// 目录保护失败原因 / Reasons a directory could not be protected.
///
/// 注意：所有错误都必须由调用方显式处理（fail-close），本模块不会把任何失败降级为
/// `Ok(())`。部分变体只在特定平台出现，跨平台编译时未使用的变体属正常情况。
/// Note: callers must handle every error (fail-close); this module never degrades a
/// failure into `Ok(())`. Some variants are platform-specific, so being unused on a given
/// target is expected.
#[non_exhaustive]
#[derive(Debug)]
pub enum ProtectError {
    /// 路径不存在 / Path does not exist.
    NotFound(PathBuf),
    /// 路径存在但不是目录 / Path exists but is not a directory.
    NotADirectory(PathBuf),
    /// 路径是符号链接（拒绝，避免保护到非预期目标）/ Path is a symlink (refused to avoid
    /// protecting an unintended target).
    Symlink(PathBuf),
    /// 子树内存在 reparse point（junction/symlink 等）/ The subtree contains a reparse
    /// point (junction/symlink, etc.).
    ReparsePoint(PathBuf),
    /// 施加期间路径被替换（inode 变化）/ The path was replaced while protecting (inode changed).
    Changed(PathBuf),
    /// 文件系统操作失败（metadata / chmod 等）/ Filesystem operation failed.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// 施加后读回校验不符（Unix）/ Read-back verification mismatch (Unix).
    VerificationFailed {
        path: PathBuf,
        expected: u32,
        actual: u32,
    },
    /// 目标平台无法确定平台实现（fail-close 而非静默跳过）/ No implementation on this target.
    UnsupportedPlatform,
    /// 平台 API 返回错误（Windows）/ Platform API returned an error (Windows).
    Platform {
        path: PathBuf,
        /// Win32 错误码 / Win32 error code.
        code: u32,
        /// 失败的动作 / The operation that failed.
        action: &'static str,
    },
}

impl fmt::Display for ProtectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProtectError::NotFound(p) => write!(f, "directory does not exist: {}", p.display()),
            ProtectError::NotADirectory(p) => write!(f, "path is not a directory: {}", p.display()),
            ProtectError::Symlink(p) => {
                write!(f, "refusing to protect a symlink: {}", p.display())
            }
            ProtectError::ReparsePoint(p) => write!(
                f,
                "refusing to protect a subtree containing a reparse point: {}",
                p.display()
            ),
            ProtectError::Changed(p) => {
                write!(f, "path changed while protecting: {}", p.display())
            }
            ProtectError::Io { path, source } => {
                write!(f, "filesystem error on {}: {source}", path.display())
            }
            ProtectError::VerificationFailed {
                path,
                expected,
                actual,
            } => write!(
                f,
                "permission verification failed on {}: expected {expected:#o}, got {actual:#o}",
                path.display()
            ),
            ProtectError::UnsupportedPlatform => {
                write!(f, "directory protection is not supported on this platform")
            }
            ProtectError::Platform { path, code, action } => write!(
                f,
                "Win32 {action} failed on {} with error code {code}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ProtectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ProtectError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// 把 IO 错误归类为 `NotFound` 或 `Io`（便于调用方区分“不存在”与“其它失败”）。
/// Classify an IO error into `NotFound` or `Io` so callers can distinguish the two.
fn io_error(path: &Path, source: std::io::Error) -> ProtectError {
    if source.kind() == std::io::ErrorKind::NotFound {
        ProtectError::NotFound(path.to_path_buf())
    } else {
        ProtectError::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

/// 平台宏 / Platform dispatch macro.
///
/// 这是“基于权限的平台宏”的落点：编译期按目标平台展开到对应实现；没有实现的平台
/// 展开为 `Err(UnsupportedPlatform)`（fail-close），绝不静默返回 `Ok`。
/// This is the "permission-based platform macro": it expands at compile time to the
/// matching implementation; targets without one expand to `Err(UnsupportedPlatform)`
/// (fail-close), never a silent `Ok`.
#[cfg(unix)]
macro_rules! platform_protect {
    ($path:expr, $meta:expr) => {
        protect_unix($path, $meta)
    };
}

#[cfg(windows)]
macro_rules! platform_protect {
    ($path:expr, $meta:expr) => {
        protect_windows($path, $meta)
    };
}

#[cfg(not(any(unix, windows)))]
macro_rules! platform_protect {
    ($path:expr, $meta:expr) => {{
        // 显式消费参数，避免未使用告警 / consume the arguments to avoid unused warnings.
        let _ = ($path, $meta);
        Err(ProtectError::UnsupportedPlatform)
    }};
}

/// 保护一个目录：收紧为仅属主可访问 / Protect a directory: tighten it to owner-only.
///
/// fail-close 保证 / Fail-close guarantees:
/// - 路径不存在 → [`ProtectError::NotFound`]；
/// - 是符号链接 → [`ProtectError::Symlink`]（避免保护到非预期目标）；
/// - 不是目录 → [`ProtectError::NotADirectory`]；
/// - 平台施加失败 / 校验不符 / 不支持 → 相应错误。
///
/// - missing path → `NotFound`;
/// - symlink → `Symlink` (refuse to protect an unintended target);
/// - non-directory → `NotADirectory`;
/// - failed syscall / failed verification / unsupported → respective error.
///
/// # 示例 / Example
/// ```no_run
/// use whitebox_core::dir::protect_dir;
/// protect_dir("/path/to/private-dir").expect("directory must be protected");
/// ```
pub fn protect_dir(path: impl AsRef<Path>) -> Result<(), ProtectError> {
    let path = path.as_ref();
    // 平台无关的前置校验：必须在任何副作用之前完成，保证错误分类一致。
    // 用 symlink_metadata 而非 metadata：绝不跟随符号链接，避免 TOCTOU 把保护施加到别处。
    // Platform-independent pre-check, before any side effect. Use symlink_metadata (not
    // metadata) so we never follow symlinks and cannot protect an unintended target.
    let meta = std::fs::symlink_metadata(path).map_err(|e| io_error(path, e))?;
    if meta.file_type().is_symlink() {
        return Err(ProtectError::Symlink(path.to_path_buf()));
    }
    if !meta.is_dir() {
        return Err(ProtectError::NotADirectory(path.to_path_buf()));
    }
    // 编译期平台分派 / Compile-time platform dispatch.
    platform_protect!(path, &meta)
}

/// Unix：以 `O_NOFOLLOW|O_DIRECTORY` 打开目录句柄后 `fchmod 0700` + 读回校验。
///
/// 用句柄而非路径，消除“先检查、再按路径 chmod”的 TOCTOU；`O_NOFOLLOW` 同时拒绝
/// 最终组件是符号链接。`fstat` 的 `(st_dev, st_ino)` 还会与打开前的 `meta` 比对，
/// 若路径在两步之间被换成另一个真实目录则报 `Changed`。
/// Unix: open the directory with `O_NOFOLLOW|O_DIRECTORY`, then `fchmod 0700` and verify.
/// Using a handle (not the path) removes the check-then-chmod TOCTOU; `O_NOFOLLOW` also
/// rejects a symlink as the final component. The opened `(st_dev, st_ino)` is compared
/// with the pre-open `meta`, so swapping the path to another real directory reports
/// `Changed`.
#[cfg(unix)]
fn protect_unix(path: &Path, meta: &std::fs::Metadata) -> Result<(), ProtectError> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    let expected = (meta.dev(), meta.ino());
    let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|_| ProtectError::Io {
        path: path.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path contains an interior NUL",
        ),
    })?;
    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(open_error(path, std::io::Error::last_os_error()));
    }

    let applied = (|| -> Result<(), ProtectError> {
        if unsafe { libc::fchmod(fd, 0o700) } != 0 {
            return Err(io_error(path, std::io::Error::last_os_error()));
        }
        // 读回校验：某些文件系统/挂载选项可能静默忽略 chmod，必须显式确认。
        // Read-back: some filesystems/mount options may silently ignore chmod.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd, &mut stat) } != 0 {
            return Err(io_error(path, std::io::Error::last_os_error()));
        }
        // 路径在预检查与 open 之间被换成另一个真实目录 → inode 不一致。
        // The path was swapped to another real directory between the pre-check and open.
        if (stat.st_dev as u64, stat.st_ino as u64) != expected {
            return Err(ProtectError::Changed(path.to_path_buf()));
        }
        let actual = (stat.st_mode as u32) & 0o777;
        if actual != 0o700 {
            return Err(ProtectError::VerificationFailed {
                path: path.to_path_buf(),
                expected: 0o700,
                actual,
            });
        }
        Ok(())
    })();

    unsafe { libc::close(fd) };
    applied
}

/// 把 `open` 的 errno 归类（ELOOP→Symlink、ENOTDIR→NotADirectory、其余走 Io）。
/// Classify `open` errno (ELOOP→Symlink, ENOTDIR→NotADirectory, otherwise Io).
#[cfg(unix)]
fn open_error(path: &Path, err: std::io::Error) -> ProtectError {
    match err.raw_os_error() {
        Some(libc::ELOOP) => ProtectError::Symlink(path.to_path_buf()),
        Some(libc::ENOTDIR) => ProtectError::NotADirectory(path.to_path_buf()),
        _ => io_error(path, err),
    }
}

/// Windows：仅当前用户可访问的受保护 DACL / Windows: a protected, current-user-only DACL.
///
/// 步骤 / Steps:
/// 1. 打开当前进程令牌，取当前用户 SID；
/// 2. 构造“当前用户 = `FILE_ALL_ACCESS`，可继承”的 ACE；
/// 3. `SetEntriesInAclW` 生成只含该 ACE 的 ACL；
/// 4. `TreeSetNamedSecurityInfoW` 把 DACL 应用到**整棵子树**并设置 `PROTECTED`（阻断父目录继承）；
///    已存在的子项也会被覆盖。该 API 在任一对象缺少 `READ_CONTROL`/`WRITE_DAC` 时**全部不施加**。
///
/// 1. Open the current process token and read the current user SID;
/// 2. build an inheritable ACE granting the current user `FILE_ALL_ACCESS`;
/// 3. materialize a single-ACE ACL with `SetEntriesInAclW`;
/// 4. apply it via `TreeSetNamedSecurityInfoW` to the whole subtree with `PROTECTED`
///    (no inheritance), so existing children are covered too. If the caller lacks
///    `READ_CONTROL`/`WRITE_DAC` on any object, none of the updates are applied.
#[cfg(windows)]
fn protect_windows(path: &Path, _meta: &std::fs::Metadata) -> Result<(), ProtectError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_SUCCESS, GetLastError, HANDLE, LocalFree,
    };
    use windows_sys::Win32::Security::Authorization::{
        EXPLICIT_ACCESS_W, NO_MULTIPLE_TRUSTEE, ProgressInvokeNever, SE_FILE_OBJECT, SET_ACCESS,
        SetEntriesInAclW, TREE_SEC_INFO_SET, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
        TreeSetNamedSecurityInfoW,
    };
    use windows_sys::Win32::Security::{
        ACL, DACL_SECURITY_INFORMATION, GetTokenInformation, PROTECTED_DACL_SECURITY_INFORMATION,
        SUB_CONTAINERS_AND_OBJECTS_INHERIT, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    // TreeSet 会遍历整棵子树；先拒绝任何 reparse point，避免 ACL 传播逃逸到树外。
    // TreeSet walks the whole subtree; reject any reparse point first so ACL propagation
    // cannot escape the tree.
    reject_reparse_points(path)?;

    // 1) 打开当前进程令牌 / open the current process token.
    let mut token: HANDLE = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(ProtectError::Platform {
            path: path.to_path_buf(),
            code: unsafe { GetLastError() },
            action: "OpenProcessToken",
        });
    }

    // 2) 查询当前用户 SID（先问长度，再按长度分配）/ read the current user SID.
    let mut needed: u32 = 0;
    let _ = unsafe {
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed)
    };
    if needed == 0 {
        let code = unsafe { GetLastError() };
        unsafe { CloseHandle(token) };
        return Err(ProtectError::Platform {
            path: path.to_path_buf(),
            code,
            action: "GetTokenInformation(size)",
        });
    }
    let mut buf = vec![0u8; needed as usize];
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buf.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    };
    if ok == 0 {
        let code = unsafe { GetLastError() };
        unsafe { CloseHandle(token) };
        return Err(ProtectError::Platform {
            path: path.to_path_buf(),
            code,
            action: "GetTokenInformation",
        });
    }
    // `buf` 是字节缓冲，不保证指针对齐，故用 read_unaligned 读取 TOKEN_USER。
    // `buf` is byte-allocated and not guaranteed aligned; read TOKEN_USER via read_unaligned.
    let sid = unsafe { std::ptr::read_unaligned(buf.as_ptr().cast::<TOKEN_USER>()) }
        .User
        .Sid;
    // 令牌用完即关闭，避免句柄泄漏 / close the token as soon as possible.
    unsafe { CloseHandle(token) };

    // 3) 仅当前用户的 ACE（可继承）/ a single inheritable ACE for the current user.
    let ea = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_ALL_ACCESS,
        grfAccessMode: SET_ACCESS,
        grfInheritance: SUB_CONTAINERS_AND_OBJECTS_INHERIT,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            // TrusteeForm == TRUSTEE_IS_SID 时，ptstrName 实际是 SID 指针。
            // With TRUSTEE_IS_SID, ptstrName actually points at the SID.
            ptstrName: sid.cast(),
        },
    };

    let mut acl: *mut ACL = std::ptr::null_mut();
    let rc = unsafe { SetEntriesInAclW(1, &ea, std::ptr::null(), &mut acl) };
    if rc != ERROR_SUCCESS {
        return Err(ProtectError::Platform {
            path: path.to_path_buf(),
            code: rc,
            action: "SetEntriesInAclW",
        });
    }

    // 4) 应用 DACL 并标记 PROTECTED（不继承父目录），整棵子树生效（含已存在子项）
    //    Apply the DACL to the whole subtree and mark it PROTECTED (no inheritance).
    let mut wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let rc = unsafe {
        TreeSetNamedSecurityInfoW(
            wide.as_mut_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(), // 不改 owner / keep current owner
            std::ptr::null_mut(), // 不改 group / keep current group
            acl,                  // 新 DACL / new DACL
            std::ptr::null(),     // 不改 SACL / keep SACL
            TREE_SEC_INFO_SET,    // 设置（含子树）/ set (including subtree)
            None,                 // 无进度回调 / no progress callback
            ProgressInvokeNever,  // 不中途调用回调 / never invoke
            std::ptr::null(),     // 无回调参数 / no callback args
        )
    };
    // ACL 由 SetEntriesInAclW 分配，必须用 LocalFree 释放。
    // The ACL is allocated by SetEntriesInAclW and must be released with LocalFree.
    unsafe { LocalFree(acl.cast()) };

    if rc != ERROR_SUCCESS {
        return Err(ProtectError::Platform {
            path: path.to_path_buf(),
            code: rc,
            action: "TreeSetNamedSecurityInfoW",
        });
    }
    Ok(())
}

/// 递归拒绝子树内会造成遍历逃逸的 reparse point（symlink / junction / 挂载点）。
///
/// `TreeSetNamedSecurityInfoW` 会遍历整棵子树；若树内存在**重定向目录**，ACL 传播可能
/// 逃逸到树外。这里在施加前显式拒绝（fail-close）。普通文件上的 reparse 属性（如云占位）
/// 不参与目录遍历，故不拒绝。注意：扫描与施加之间仍存在一个极小的路径竞态；调用方应
/// 保证目录树不被不可信方并发改写。
/// Recursively reject traversal-escaping reparse points (symlink / junction / mount point).
/// TreeSet walks the whole subtree, so a **reparse directory** could let ACL propagation
/// escape; reject explicitly (fail-close) before applying. Reparse attributes on regular
/// files (e.g. cloud placeholders) are not traversed as directories, so they are allowed.
/// A tiny scan/apply race remains, so callers must keep the tree from being concurrently
/// rewritten by untrusted parties.
#[cfg(windows)]
fn reject_reparse_points(path: &Path) -> Result<(), ProtectError> {
    use std::os::windows::fs::MetadataExt;

    // FILE_ATTRIBUTE_REPARSE_POINT = 0x0400（Win32 文件属性位）。
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;

    let meta = std::fs::symlink_metadata(path).map_err(|e| io_error(path, e))?;
    let reparse = meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    // 只拒绝会被当作目录继续遍历的重定向：symlink 或 reparse 目录（junction/挂载点）。
    // Only reject redirects that would be walked as directories: symlinks or reparse dirs.
    if meta.file_type().is_symlink() || (reparse && meta.is_dir()) {
        return Err(ProtectError::ReparsePoint(path.to_path_buf()));
    }
    if meta.is_dir() {
        for entry in std::fs::read_dir(path).map_err(|e| io_error(path, e))? {
            let entry = entry.map_err(|e| io_error(path, e))?;
            reject_reparse_points(&entry.path())?;
        }
    }
    Ok(())
}

