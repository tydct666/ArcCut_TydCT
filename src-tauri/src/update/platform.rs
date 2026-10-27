use super::protocol::{no_links, Result};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::Path,
    time::{Duration, Instant},
};
pub fn atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    no_links(path)?;
    let parent = path.parent().ok_or("更新路径无父目录")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let tmp = parent.join(format!(".write-{}.tmp", uuid::Uuid::new_v4()));
    let mut f = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&tmp)
        .map_err(|e| e.to_string())?;
    f.write_all(bytes)
        .and_then(|_| f.sync_all())
        .map_err(|e| e.to_string())?;
    drop(f);
    #[cfg(windows)]
    {
        let a = wide(&tmp);
        let b = wide(path);
        let began = Instant::now();
        loop {
            if unsafe { MoveFileExW(a.as_ptr(), b.as_ptr(), 0x1 | 0x8) } != 0 {
                break;
            }
            let error = std::io::Error::last_os_error();
            if !matches!(error.raw_os_error(), Some(5 | 32 | 33))
                || began.elapsed() > Duration::from_secs(3)
            {
                let _ = fs::remove_file(&tmp);
                return Err(format!(
                    "写入更新记录失败（{}）：{error}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    #[cfg(not(windows))]
    fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}
pub fn json<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    atomic(
        path,
        &serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?,
    )
}
pub fn lock(path: &Path) -> Result<File> {
    no_links(path)?;
    fs::create_dir_all(path.parent().ok_or("无锁目录")?).map_err(|e| e.to_string())?;
    let mut o = OpenOptions::new();
    o.create(true).truncate(false).read(true).write(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.share_mode(0);
    }
    o.open(path)
        .map_err(|_| "更新目录正在使用、被占用或不可写，请关闭其他 ArkCut 后重试".into())
}
pub fn shared_instance(root: &Path) -> Result<File> {
    let p = super::transaction::control(root)?.join("instance.lock");
    no_links(&p)?;
    if !p.exists() {
        let _ = OpenOptions::new().create_new(true).write(true).open(&p);
    }
    let mut o = OpenOptions::new();
    o.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.share_mode(1);
    }
    o.open(p).map_err(|_| "ArkCut 正在更新，请稍后重试".into())
}
pub fn space(root: &Path, needed: u64) -> Result<()> {
    #[cfg(feature = "update-test-hooks")]
    if std::env::var("ARKCUT_UPDATE_TEST_LOW_SPACE").as_deref() == Ok("1") {
        return Err("更新磁盘空间不足（测试）".into());
    }
    #[cfg(windows)]
    {
        let mut available = 0u64;
        let mut total = 0u64;
        let mut free = 0u64;
        let path = wide(root);
        if unsafe { GetDiskFreeSpaceExW(path.as_ptr(), &mut available, &mut total, &mut free) } == 0
        {
            return Err("无法读取更新目录可用空间".into());
        }
        if available < needed.saturating_add(512 * 1024 * 1024) {
            return Err(format!(
                "更新空间不足，需要另有约 {:.2} GB 可用空间",
                (needed + 512 * 1024 * 1024) as f64 / 1e9
            ));
        }
    }
    Ok(())
}
pub fn idle_priority() {
    #[cfg(windows)]
    unsafe {
        SetThreadPriority(GetCurrentThread(), -1);
    }
}
pub fn wait_parent(pid: u32, dir: &Path, nonce: &str) -> Result<()> {
    #[cfg(windows)]
    unsafe {
        let handle = if pid == 0 {
            std::ptr::null_mut()
        } else {
            OpenProcess(0x100000, 0, pid)
        };
        if pid != 0 && handle.is_null() {
            return Err("无法确认旧程序退出，更新未开始".into());
        }
        let ack = json(
            &dir.join("helper-ready.json"),
            &serde_json::json!({"nonce":nonce,"pid":std::process::id()}),
        );
        if let Err(e) = ack {
            if !handle.is_null() {
                CloseHandle(handle);
            }
            return Err(e);
        }
        if !handle.is_null() {
            let result = WaitForSingleObject(handle, 120_000);
            CloseHandle(handle);
            if result != 0 {
                return Err("旧程序仍在运行，更新未开始".into());
            }
        }
    }
    Ok(())
}
pub fn hidden(command: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
}
pub fn wait_child(child: &mut std::process::Child, seconds: u64) -> Result<()> {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(seconds) {
        if let Some(exit) = child.try_wait().map_err(|e| e.to_string())? {
            return if exit.success() {
                Ok(())
            } else {
                Err("新版本首次启动失败".into())
            };
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("新版本启动检查超时".into())
}
#[cfg(windows)]
fn wide(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str().encode_wide().chain(Some(0)).collect()
}
#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn MoveFileExW(a: *const u16, b: *const u16, flags: u32) -> i32;
    fn GetDiskFreeSpaceExW(p: *const u16, a: *mut u64, t: *mut u64, f: *mut u64) -> i32;
    fn GetCurrentThread() -> isize;
    fn SetThreadPriority(h: isize, p: i32) -> i32;
    fn OpenProcess(a: u32, i: i32, p: u32) -> *mut std::ffi::c_void;
    fn WaitForSingleObject(h: *mut std::ffi::c_void, t: u32) -> u32;
    fn CloseHandle(h: *mut std::ffi::c_void) -> i32;
}
