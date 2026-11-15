//! worker stderr 日志 / Worker stderr logging.
//!
//! 父进程把 worker 的 stderr 通过管道交给独立线程：转义后异步追加到按天轮换的文件；
//! 单文件超过 1 MiB 轮换，按文件名中的 UTC 天数保留 7 天。guest stderr 由 WASI 配置
//! 隔离（CLI 为 Null），因此这里只应出现宿主/引擎诊断。
//!
//! The parent drains the worker's stderr on a dedicated thread: escape it and append
//! asynchronously to day-rotated files; rotate at 1 MiB per file and keep 7 days by the UTC
//! day in the file name. Guest stderr is isolated by the WASI config (Null in the CLI), so
//! this log only carries host/engine diagnostics.

use std::io::Read;
use std::path::{Path, PathBuf};

/// 单个 worker 日志文件上限 / 轮换保留天数：来自用户配置 `logging.json`。
/// Per-file worker log cap / rotation retention: from user config `logging.json`.
const SECS_PER_DAY: u64 = 24 * 60 * 60;

/// 串行化“查大小 / 轮换 / 追加 / 清理”，避免并发日志线程互相踩踏。
/// Serialize size-check / rotation / append / cleanup across log threads.
static WORKER_LOG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// worker 日志目录：`<cache_dir>`（每用户；portable 为 `<exe_dir>/cache`）。
/// Worker log directory: `<cache_dir>` (per-user; portable `<exe_dir>/cache`).
#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn worker_log_dir() -> Option<PathBuf> {
    let dir = crate::dir::cache_dir().ok()?;
    crate::dir::ensure_dir(&dir).ok()?;
    Some(dir)
}

#[cfg(any(target_os = "android", target_os = "ios"))]
fn worker_log_dir() -> Option<PathBuf> {
    None
}

/// UTC 天数（用于按天轮换/清理）/ UTC day number for daily rotation/cleanup.
fn utc_day(now: std::time::SystemTime) -> Option<u64> {
    now.duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() / SECS_PER_DAY)
}

/// 当天的活动日志 / the active log for `day`.
fn active_worker_log(dir: &Path, day: u64) -> PathBuf {
    dir.join(format!("worker-{day}.log"))
}

/// 删除达到保留期的轮换日志（只匹配 `worker-<day>...log`，不碰当天文件）。
/// Delete rotated logs at the retention limit (only `worker-<day>...log`).
fn cleanup_old_worker_logs(dir: &Path, today: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(day) = name
            .strip_prefix("worker-")
            .and_then(|s| s.strip_suffix(".log"))
            .and_then(|s| s.split('-').next())
            .and_then(|s| s.parse::<u64>().ok())
        else {
            continue;
        };
        if today.saturating_sub(day) >= crate::config::logging().retention_days {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// 单文件达到上限：改名为带时间戳的轮换文件，并清理过期日志。
/// Rotate a full file to a timestamped name, then clean expired logs.
fn rotate_worker_log(dir: &Path, active: &Path, day: u64) -> std::io::Result<()> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let rotated = dir.join(format!("worker-{day}-{}-{nanos}.log", std::process::id()));
    std::fs::rename(active, &rotated)?;
    cleanup_old_worker_logs(dir, day);
    Ok(())
}

/// 追加一条已转义日志；超过单文件上限则轮换后继续。
/// Append one escaped record; rotate and continue when the per-file cap is reached.
fn append_worker_log(dir: &Path, day: u64, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;

    let active = active_worker_log(dir, day);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&active)?;
    let len = file.metadata()?.len();
    let cap = crate::config::logging().per_file_bytes;
    if len.saturating_add(bytes.len() as u64) > cap {
        drop(file);
        rotate_worker_log(dir, &active, day)?;
        file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&active)?;
    }
    file.write_all(bytes)?;
    Ok(())
}

/// 转义日志字节：`\`、控制字符与非 ASCII 字节都转为可见形式，杜绝终端/日志注入。
/// Escape log bytes: backslash, control chars, and non-ASCII bytes become visible,
/// preventing terminal/log injection.
fn escape_worker_log(input: &[u8], out: &mut Vec<u8>) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &b in input {
        match b {
            b'\\' => out.extend_from_slice(b"\\\\"),
            0x20..=0x7e => out.push(b),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            _ => {
                out.extend_from_slice(b"\\x");
                out.push(HEX[(b >> 4) as usize]);
                out.push(HEX[(b & 0x0f) as usize]);
            }
        }
    }
}

/// 后台排空 worker stderr，转义后异步追加到按天轮换的日志；失败时停止记录但仍排空。
/// Drain worker stderr in the background, escape it, and append asynchronously to
/// day-rotated logs; on failure, stop recording but keep draining.
pub(crate) fn spawn_worker_log_drain(
    mut reader: impl Read + Send + 'static,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let dir = worker_log_dir();
        if let Some(dir) = dir.as_deref() {
            if let Ok(_guard) = WORKER_LOG_LOCK.lock() {
                if let Some(today) = utc_day(std::time::SystemTime::now()) {
                    cleanup_old_worker_logs(dir, today);
                }
            }
        }
        let mut stopped = dir.is_none();
        let mut chunk = [0u8; 8 * 1024];
        let mut escaped = Vec::with_capacity(chunk.len() * 4 + 1);
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if stopped {
                        // 仍要排空，避免 worker 写满 stderr 管道阻塞。
                        continue;
                    }
                    escaped.clear();
                    escape_worker_log(&chunk[..n], &mut escaped);
                    escaped.push(b'\n'); // 受控换行，注入字符已被转义
                    let Some(dir) = dir.as_deref() else {
                        stopped = true;
                        continue;
                    };
                    let Some(day) = utc_day(std::time::SystemTime::now()) else {
                        stopped = true;
                        continue;
                    };
                    let Ok(_guard) = WORKER_LOG_LOCK.lock() else {
                        stopped = true;
                        continue;
                    };
                    if append_worker_log(dir, day, &escaped).is_err() {
                        stopped = true;
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    })
}
