use crate::{media, model::*};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
};
#[derive(serde::Serialize, serde::Deserialize)]
struct ActiveLease {
    pid: u32,
    path: PathBuf,
    temporary: bool,
}
struct CacheMaintenance {
    _file: fs::File,
    #[cfg(not(windows))]
    path: PathBuf,
}
#[cfg(not(windows))]
impl Drop for CacheMaintenance {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
#[cfg(windows)]
fn maintenance_lock(dir: &Path) -> Result<CacheMaintenance, String> {
    use std::os::windows::fs::OpenOptionsExt;
    let path = dir.join(".arkcut-maintenance.lock");
    let began = std::time::Instant::now();
    loop {
        // The OS owns the lock lifetime, including process crashes. An old
        // empty/PID lock file is safely opened only after its handle is gone.
        match fs::OpenOptions::new()
            .write(true)
            .create(true)
            .access_mode(0x40000000 | 0x00010000) // GENERIC_WRITE | DELETE
            .share_mode(0)
            .custom_flags(0x04000000) // FILE_FLAG_DELETE_ON_CLOSE
            .open(&path)
        {
            Ok(file) => return Ok(CacheMaintenance { _file: file }),
            Err(e) if matches!(e.raw_os_error(), Some(5 | 32 | 33)) => {
                if began.elapsed() > std::time::Duration::from_secs(2) {
                    return Err("CACHE_BUSY: 缓存正在由另一任务维护，请稍后重试".into());
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}
#[cfg(not(windows))]
fn maintenance_lock(dir: &Path) -> Result<CacheMaintenance, String> {
    let path = dir.join(".arkcut-maintenance.lock");
    let began = std::time::Instant::now();
    loop {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                if let Err(e) = write!(file, "{}", std::process::id()) {
                    let _ = fs::remove_file(&path);
                    return Err(e.to_string());
                }
                return Ok(CacheMaintenance { _file: file, path });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let dead = fs::read_to_string(&path)
                    .ok()
                    .and_then(|s| s.parse::<u32>().ok())
                    .is_some_and(|pid| !alive(pid));
                if dead {
                    let _ = fs::remove_file(&path);
                    continue;
                }
                if began.elapsed() > std::time::Duration::from_secs(2) {
                    return Err("CACHE_BUSY: 缓存正在由另一任务维护，请稍后重试".into());
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}
pub struct ActiveFile {
    lease: PathBuf,
    retain: bool,
}
impl Drop for ActiveFile {
    fn drop(&mut self) {
        if !self.retain {
            let _ = fs::remove_file(&self.lease);
        }
    }
}
fn lease_file(path: &Path, temporary: bool) -> Result<ActiveFile, String> {
    let parent =
        fs::canonicalize(path.parent().ok_or("无效缓存文件路径")?).map_err(|e| e.to_string())?;
    let _maintenance = maintenance_lock(&parent)?;
    let target = parent.join(path.file_name().ok_or("无效缓存文件名")?);
    let lease = parent.join(format!(".arkcut-active-{}.json", uuid::Uuid::new_v4()));
    let bytes = serde_json::to_vec(&ActiveLease {
        pid: std::process::id(),
        path: target,
        temporary,
    })
    .map_err(|e| e.to_string())?;
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&lease)
        .map_err(|e| e.to_string())?;
    if let Err(e) = file.write_all(&bytes) {
        drop(file);
        let _ = fs::remove_file(&lease);
        return Err(e.to_string());
    }
    Ok(ActiveFile {
        lease,
        retain: false,
    })
}
/// Protect a cache file across this and other ArkCut instances, even before it exists.
pub fn active_file(path: &Path) -> Result<ActiveFile, String> {
    lease_file(path, false)
}
pub struct OwnedTemp {
    path: PathBuf,
    lease: ActiveFile,
}
impl OwnedTemp {
    pub fn path(&self) -> &Path {
        &self.path
    }
}
impl Drop for OwnedTemp {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_file(&self.path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                self.lease.retain = true;
            }
        }
    }
}
pub fn temp_file(dir: &Path, prefix: &str) -> Result<OwnedTemp, String> {
    if prefix.is_empty()
        || !prefix
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err("非法临时文件前缀".into());
    }
    let path = dir.join(format!(".arkcut-{prefix}-{}.tmp", uuid::Uuid::new_v4()));
    let lease = lease_file(&path, true)?;
    Ok(OwnedTemp { path, lease })
}
pub fn index_dir() -> Result<PathBuf, String> {
    let dir = cache_root().join("frame-index-v2");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let _maintenance = maintenance_lock(&dir)?;
    fs::write(dir.join(".arkcut-cache-v1"), b"ArkCut owned cache").map_err(|e| e.to_string())?;
    Ok(dir)
}
fn read_lease(path: &Path) -> Option<ActiveLease> {
    if path.metadata().ok()?.len() > 16384 {
        return None;
    }
    serde_json::from_reader(BufReader::new(fs::File::open(path).ok()?.take(16385))).ok()
}
fn active_paths(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".arkcut-active-")
        })
        .filter_map(|e| read_lease(&e.path()))
        .filter(|lease| alive(lease.pid))
        .map(|lease| lease.path)
        .collect()
}
fn same_path(a: &Path, b: &Path) -> bool {
    a.to_string_lossy()
        .trim_start_matches("\\\\?\\")
        .eq_ignore_ascii_case(b.to_string_lossy().trim_start_matches("\\\\?\\"))
}
fn protected(path: &Path, pinned: &[String], active: &[PathBuf]) -> bool {
    pinned.iter().any(|p| same_path(Path::new(p), path))
        || active.iter().any(|p| same_path(p, path))
}
fn metadata_file(path: &Path) -> bool {
    path.file_name().is_some_and(|n| {
        let n = n.to_string_lossy();
        n.starts_with(".arkcut-") || n == ".session.json" || n == ".cleanup-pending.json"
    })
}
pub fn root() -> PathBuf {
    std::env::var_os("ARKCUT_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir)
                .join("ArkCut")
        })
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_directory: Option<String>,
    pub cache_directory: Option<String>,
    pub cache_limit: u64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            archive_directory: None,
            cache_directory: None,
            cache_limit: 5 * 1024 * 1024 * 1024,
        }
    }
}
pub fn settings() -> (Settings, Option<String>) {
    let path = root().join("settings.json");
    match fs::File::open(&path) {
        Ok(file) => match file
            .metadata()
            .ok()
            .filter(|m| m.len() <= 64 * 1024)
            .and_then(|_| {
                serde_json::from_reader::<_, Settings>(BufReader::new(file.take(64 * 1024 + 1)))
                    .ok()
            }) {
            Some(s) if s.cache_limit >= 64 * 1024 * 1024 => (s, None),
            _ => {
                let backup = path.with_extension(format!("corrupt-{}.json", uuid::Uuid::new_v4()));
                let _ = fs::rename(&path, &backup);
                (
                    Settings::default(),
                    Some(format!(
                        "配置损坏，已备份到 {} 并恢复默认",
                        backup.display()
                    )),
                )
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Settings::default(), None),
        Err(e) => (Settings::default(), Some(format!("配置无法读取：{e}"))),
    }
}
pub fn save_settings(s: &Settings) -> Result<(), String> {
    if s.cache_limit < 64 * 1024 * 1024 {
        return Err("缓存限额最小 64 MB".into());
    }
    if let Some(dir) = &s.cache_directory {
        let p = Path::new(dir);
        if !p.is_absolute() || !p.is_dir() {
            return Err("缓存目录必须是已有的绝对路径目录".into());
        }
        let dedicated = p.join("ArkCutCache");
        fs::create_dir_all(&dedicated).map_err(|e| format!("CACHE_DIRECTORY: {e}"))?;
    }
    fs::create_dir_all(root()).map_err(|e| e.to_string())?;
    atomic(
        &root().join("settings.json"),
        &serde_json::to_vec(s).map_err(|e| e.to_string())?,
    )
}
pub fn cache_root() -> PathBuf {
    settings()
        .0
        .cache_directory
        .map(|p| PathBuf::from(p).join("ArkCutCache"))
        .unwrap_or_else(|| root().join("cache"))
}
pub fn project_dir(id: &str) -> Result<PathBuf, String> {
    uuid::Uuid::parse_str(id).map_err(|_| "非法工程 ID")?;
    let p = cache_root().join(id);
    fs::create_dir_all(&p).map_err(|e| e.to_string())?;
    let _maintenance = maintenance_lock(&p)?;
    fs::write(p.join(".arkcut-cache-v1"), b"ArkCut owned cache").map_err(|e| e.to_string())?;
    fs::write(
        p.join(".session.json"),
        format!("{{\"pid\":{}}}", std::process::id()),
    )
    .map_err(|e| e.to_string())?;
    Ok(p)
}
pub fn hash<T: serde::Serialize>(v: &T) -> Result<String, String> {
    struct HashWriter(Sha256);
    impl Write for HashWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = HashWriter(Sha256::new());
    serde_json::to_writer(&mut writer, v).map_err(|e| e.to_string())?;
    Ok(format!("{:x}", writer.0.finalize()))
}
pub fn atomic_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), String> {
    atomic_json_limited(path, value, 128 * 1024 * 1024)
}
pub(crate) fn atomic_json_limited<T: serde::Serialize>(
    path: &Path,
    value: &T,
    limit: u64,
) -> Result<(), String> {
    struct BoundedWriter {
        file: fs::File,
        written: u64,
        limit: u64,
    }
    impl Write for BoundedWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() as u64 > self.limit.saturating_sub(self.written) {
                return Err(std::io::Error::other(format!(
                    "PROJECT_TOO_LARGE: JSON 写入超过 {} 字节上限，原文件未修改",
                    self.limit
                )));
            }
            let count = self.file.write(bytes)?;
            self.written += count as u64;
            Ok(count)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.file.flush()
        }
    }
    let tmp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let mut created = false;
    let result = (|| {
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| format!("WRITE_FAILED: {e}"))?;
        created = true;
        let mut writer = BufWriter::new(BoundedWriter {
            file,
            written: 0,
            limit,
        });
        serde_json::to_writer_pretty(&mut writer, value).map_err(|e| e.to_string())?;
        writer
            .flush()
            .and_then(|_| writer.get_ref().file.sync_all())
            .map_err(|e| e.to_string())?;
        drop(writer);
        fs::rename(&tmp, path).map_err(|e| e.to_string())
    })();
    if result.is_err() && created {
        let _ = fs::remove_file(tmp);
    }
    result
}
pub fn atomic_copy(source: &Path, target: &Path) -> Result<(), String> {
    let tmp = target.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let mut created = false;
    let result = (|| {
        let mut input = fs::File::open(source).map_err(|e| e.to_string())?;
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| format!("WRITE_FAILED: {e}"))?;
        created = true;
        std::io::copy(&mut input, &mut output)
            .and_then(|_| output.sync_all())
            .map_err(|e| e.to_string())?;
        drop(output);
        drop(input);
        fs::rename(&tmp, target).map_err(|e| e.to_string())
    })();
    if result.is_err() && created {
        let _ = fs::remove_file(tmp);
    }
    result
}
/// Optional cache transfer: only this task's owned temporary may be moved.
/// A failed hash/cancellation leaves no published cache and never touches input media.
pub fn transfer_cache_checked(
    source: &OwnedTemp,
    target: &Path,
    mut check: impl FnMut() -> Result<(), String>,
) -> Result<(u64, String, bool), String> {
    check()?;
    let tmp = temp_file(target.parent().ok_or("无效缓存目录")?, "cache-transfer")?;
    match fs::rename(source.path(), tmp.path()) {
        Ok(()) => {
            let (bytes, digest) = crate::export_record::digest_checked(tmp.path(), &mut check)?;
            check()?;
            fs::rename(tmp.path(), target).map_err(|e| e.to_string())?;
            Ok((bytes, digest, true))
        }
        Err(e) if matches!(e.raw_os_error(), Some(17 | 18)) => {
            let (bytes, digest) = copy_digest_checked(source.path(), target, check)?;
            Ok((bytes, digest, false))
        }
        Err(e) => Err(e.to_string()),
    }
}
/// One bounded pass computes the digest of exactly the bytes written.
pub fn copy_digest_checked(
    source: &Path,
    target: &Path,
    mut check: impl FnMut() -> Result<(), String>,
) -> Result<(u64, String), String> {
    if same_path(source, target) {
        return Err("缓存源和目标不能相同".into());
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1); // FILE_SHARE_READ: deny concurrent writes/deletes.
    }
    let mut input = options.open(source).map_err(|e| e.to_string())?;
    let before = input.metadata().map_err(|e| e.to_string())?;
    let tmp = temp_file(target.parent().ok_or("无效缓存目录")?, "cache-copy")?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp.path())
        .map_err(|e| e.to_string())?;
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut digest = Sha256::new();
    let mut bytes = 0u64;
    loop {
        check()?;
        let n = input.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        output.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
        digest.update(&buffer[..n]);
        bytes += n as u64;
    }
    let after = input.metadata().map_err(|e| e.to_string())?;
    if bytes != before.len()
        || after.len() != bytes
        || before.modified().ok() != after.modified().ok()
    {
        return Err("CACHE_CHANGED: 复制时源文件发生变化".into());
    }
    output.sync_all().map_err(|e| e.to_string())?;
    drop(output);
    check()?;
    fs::rename(tmp.path(), target).map_err(|e| e.to_string())?;
    Ok((bytes, format!("{:x}", digest.finalize())))
}
pub fn atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let mut created = false;
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| format!("WRITE_FAILED: {e}"))?;
        created = true;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        drop(file);
        fs::rename(&tmp, path).map_err(|e| e.to_string())
    })();
    if result.is_err() && created {
        let _ = fs::remove_file(tmp);
    }
    result
}
pub fn protect_inputs(p: &Project, path: &Path) -> Result<(), String> {
    let full = if path.exists() {
        fs::canonicalize(path).map_err(|e| e.to_string())?
    } else {
        let parent = fs::canonicalize(path.parent().ok_or("无效输出目录")?)
            .map_err(|e| format!("OUTPUT_DIRECTORY: {e}"))?;
        parent.join(path.file_name().ok_or("无效文件名")?)
    };
    let target = full.to_string_lossy().to_lowercase();
    for fp in
        std::iter::once(&p.source.fingerprint).chain(p.music.iter().map(|m| &m.media.fingerprint))
    {
        if target == fp.path.to_lowercase() {
            return Err("INPUT_PROTECTED: 不允许覆盖任何输入视频或音乐".into());
        }
    }
    Ok(())
}
pub fn save(p: &Project, path: &str) -> Result<(), String> {
    crate::timeline::build(p)?;
    let path = Path::new(path);
    protect_inputs(p, path)?;
    if !path.to_string_lossy().ends_with(".arkcut.json") {
        return Err("工程扩展名必须为 .arkcut.json".into());
    }
    atomic_json(path, p)
}
pub fn open(path: &str) -> Result<(Project, Vec<String>), String> {
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    let bytes = file.metadata().map_err(|e| e.to_string())?.len();
    if bytes > 128 * 1024 * 1024 {
        return Err("工程文件过大".into());
    }
    let resource = crate::resource::ResourceController::new(std::sync::Arc::new(
        std::sync::atomic::AtomicBool::new(false),
    ));
    let _memory = resource.reserve_memory(bytes.saturating_mul(8), "读取工程")?;
    let p: Project = serde_json::from_reader(BufReader::new(file.take(128 * 1024 * 1024 + 1)))
        .map_err(|e| format!("PROJECT_CORRUPT: 工程无法解析，原文件未修改：{e}"))?;
    crate::timeline::build(&p)?;
    let missing = std::iter::once(&p.source)
        .chain(p.music.iter().map(|m| &m.media))
        .filter_map(|m| media::verify_media(m).err())
        .collect();
    Ok((p, missing))
}
pub fn defer_cleanup(id: &str) -> Result<(), String> {
    atomic(
        &project_dir(id)?.join(".cleanup-pending.json"),
        b"{\"retryOnNextStart\":true}",
    )
}
pub fn clear(id: &str, pinned: &[String]) -> Result<u64, String> {
    let dir = project_dir(id)?;
    let _maintenance = maintenance_lock(&dir)?;
    let active = active_paths(&dir);
    let mut total = 0;
    for entry in fs::read_dir(&dir).map_err(|e| e.to_string())? {
        let p = entry.map_err(|e| e.to_string())?.path();
        if metadata_file(&p)
            || !p.is_file()
            || fs::symlink_metadata(&p)
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(true)
            || protected(&p, pinned, &active)
        {
            continue;
        }
        total += p.metadata().map(|m| m.len()).unwrap_or(0);
        fs::remove_file(p).map_err(|e| e.to_string())?;
    }
    let _ = fs::remove_file(dir.join(".cleanup-pending.json"));
    Ok(total)
}
pub fn quota(dir: &Path, expected: u64, limit: u64, pinned: &[String]) -> Result<(), String> {
    if expected > limit {
        return Err("CACHE_QUOTA: 预览预计超过缓存限额，请调整设置或缩短预览范围".into());
    }
    let mut files = vec![];
    let mut total = 0;
    for project in fs::read_dir(dir.parent().ok_or("无效缓存根目录")?).map_err(|e| e.to_string())?
    {
        let project = project.map_err(|e| e.to_string())?;
        if project.file_type().map_err(|e| e.to_string())?.is_symlink()
            || !project.path().join(".arkcut-cache-v1").is_file()
        {
            continue;
        }
        let active = active_paths(&project.path());
        let foreign_session = fs::File::open(project.path().join(".session.json"))
            .ok()
            .filter(|f| f.metadata().is_ok_and(|m| m.len() <= 16384))
            .and_then(|f| {
                serde_json::from_reader::<_, serde_json::Value>(BufReader::new(f.take(16385))).ok()
            })
            .and_then(|v| v["pid"].as_u64())
            .is_some_and(|pid| pid != std::process::id() as u64 && alive(pid as u32));
        for e in fs::read_dir(project.path()).map_err(|e| e.to_string())? {
            let e = e.map_err(|e| e.to_string())?;
            if e.file_type().map_err(|e| e.to_string())?.is_symlink() {
                continue;
            }
            let m = e.metadata().map_err(|e| e.to_string())?;
            if !m.is_file() {
                continue;
            }
            total += m.len();
            if !foreign_session
                && e.path()
                    .extension()
                    .is_some_and(|s| s == "mp4" || s == "idx" || s == "json")
                && !metadata_file(&e.path())
                && !protected(&e.path(), pinned, &active)
            {
                files.push((m.modified().ok(), e.path(), m.len()));
            }
        }
    }
    files.sort_by_key(|x| x.0);
    for (_, path, size) in files {
        if total.saturating_add(expected) <= limit {
            break;
        }
        let parent = path.parent().ok_or("无效缓存路径")?;
        let _maintenance = maintenance_lock(parent)?;
        if protected(&path, pinned, &active_paths(parent)) {
            continue;
        }
        if fs::remove_file(path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
    if total.saturating_add(expected) > limit {
        Err("CACHE_QUOTA: 缓存空间不足，正在播放的文件已保留".into())
    } else {
        Ok(())
    }
}
#[cfg(windows)]
fn alive(pid: u32) -> bool {
    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
        fn GetExitCodeProcess(handle: *mut std::ffi::c_void, code: *mut u32) -> i32;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
        fn GetLastError() -> u32;
    }
    unsafe {
        let h = OpenProcess(0x1000, 0, pid);
        if h.is_null() {
            return GetLastError() == 5; // Access denied is not proof the owner exited.
        }
        let mut code = 0;
        let ok = GetExitCodeProcess(h, &mut code);
        CloseHandle(h);
        ok != 0 && code == 259
    }
}
#[cfg(not(windows))]
fn alive(_pid: u32) -> bool {
    true
}
pub fn recover() -> Result<u64, String> {
    let root = cache_root();
    if !root.exists() {
        return Ok(0);
    }
    let mut count = 0;
    for project in fs::read_dir(root).map_err(|e| e.to_string())? {
        let project = project.map_err(|e| e.to_string())?;
        if project.file_type().map_err(|e| e.to_string())?.is_symlink()
            || !project.path().join(".arkcut-cache-v1").is_file()
        {
            continue;
        }
        let _maintenance = maintenance_lock(&project.path())?;
        // An interrupted task may only leave its own explicitly leased temp.
        // Never infer deletable user files from a directory or filename alone.
        for entry in fs::read_dir(project.path())
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
        {
            if !entry
                .file_name()
                .to_string_lossy()
                .starts_with(".arkcut-active-")
            {
                continue;
            }
            let Some(lease) = read_lease(&entry.path()) else {
                continue;
            };
            if alive(lease.pid) {
                continue;
            }
            let parent = fs::canonicalize(project.path()).map_err(|e| e.to_string())?;
            let owned = lease.path.parent().is_some_and(|p| same_path(p, &parent))
                && lease.path.file_name().is_some_and(|n| {
                    n.to_string_lossy().starts_with(".arkcut-")
                        && n.to_string_lossy().ends_with(".tmp")
                });
            if lease.temporary && owned {
                if let Ok(m) = fs::symlink_metadata(&lease.path) {
                    if m.file_type().is_symlink() {
                        continue;
                    }
                    count += m.len();
                    if fs::remove_file(&lease.path).is_err() {
                        continue;
                    }
                }
            }
            let _ = fs::remove_file(entry.path());
        }
        let active = active_paths(&project.path());
        if !active.is_empty() {
            continue;
        }
        let lease = project.path().join(".session.json");
        let pid = fs::File::open(&lease)
            .ok()
            .filter(|f| f.metadata().is_ok_and(|m| m.len() <= 16384))
            .and_then(|f| {
                serde_json::from_reader::<_, serde_json::Value>(BufReader::new(f.take(16385))).ok()
            })
            .and_then(|v| v["pid"].as_u64())
            .map(|p| p as u32);
        let old = lease
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|d| d.as_secs() > 60);
        let pending = project.path().join(".cleanup-pending.json").is_file();
        // Incomplete repair can resume after restart. These entries still obey
        // the normal cache quota and must pass full content checks on reuse.
        if !pending
            && project
                .path()
                .join(".repair-retain.json")
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age.as_secs() < 7 * 24 * 3600)
        {
            continue;
        }
        if (!old && !pending) || pid.is_none() || alive(pid.unwrap()) {
            continue;
        }
        for f in fs::read_dir(project.path()).map_err(|e| e.to_string())? {
            let f = f.map_err(|e| e.to_string())?;
            if f.file_type().map_err(|e| e.to_string())?.is_symlink()
                || !f.path().is_file()
                || metadata_file(&f.path())
            {
                continue;
            }
            count += f.metadata().map(|m| m.len()).unwrap_or(0);
            fs::remove_file(f.path()).map_err(|e| format!("CACHE_RECOVERY: {e}"))?;
        }
        for name in [".cleanup-pending.json", ".session.json", ".arkcut-cache-v1"] {
            let _ = fs::remove_file(project.path().join(name));
        }
        drop(_maintenance);
        let _ = fs::remove_dir(project.path());
    }
    Ok(count)
}
pub fn unique_output(path: &Path) -> Result<PathBuf, String> {
    if !path.exists() {
        return Ok(path.into());
    }
    let stem = path.file_stem().ok_or("无效输出文件名")?.to_string_lossy();
    let ext = path.extension().unwrap_or_default().to_string_lossy();
    for i in 2..10000 {
        let p = path.with_file_name(format!("{stem} ({i}).{ext}"));
        if !p.exists() {
            return Ok(p);
        }
    }
    Err("无法生成新的输出文件名".into())
}
#[cfg(windows)]
pub fn available(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(
            path: *const u16,
            available: *mut u64,
            total: *mut u64,
            free: *mut u64,
        ) -> i32;
    }
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut free = 0;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            path.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok != 0 {
        Some(free)
    } else {
        None
    }
}
#[cfg(not(windows))]
pub fn available(_: &Path) -> Option<u64> {
    None
}

#[cfg(test)]
mod resource_tests {
    use super::*;
    fn fixture() -> PathBuf {
        let path = std::env::temp_dir().join(format!("arkcut-storage-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path
    }
    #[cfg(windows)]
    #[test]
    fn maintenance_lock_is_handle_owned_and_recovers_empty_old_lock() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = fixture();
        let path = root.join(".arkcut-maintenance.lock");
        // Simulate an interrupted legacy PID write; its unlocked file must
        // never permanently block maintenance in subsequent app launches.
        fs::write(&path, b"").unwrap();
        let guard = maintenance_lock(&root).unwrap();
        assert!(fs::OpenOptions::new()
            .write(true)
            .share_mode(0)
            .open(&path)
            .is_err());
        drop(guard);
        assert!(!path.exists());
        drop(maintenance_lock(&root).unwrap());
        assert!(!path.exists());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn json_hash_and_atomic_streams_preserve_bytes() {
        let root = fixture();
        let value = serde_json::json!({"frames":[0,16667,33333],"text":"中文"});
        assert_eq!(
            hash(&value).unwrap(),
            format!("{:x}", Sha256::digest(serde_json::to_vec(&value).unwrap()))
        );
        let a = root.join("a.json");
        atomic_json(&a, &value).unwrap();
        assert_eq!(
            fs::read(&a).unwrap(),
            serde_json::to_vec_pretty(&value).unwrap()
        );
        let b = root.join("b.json");
        atomic_copy(&a, &b).unwrap();
        assert_eq!(fs::read(&a).unwrap(), fs::read(&b).unwrap());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn cache_copy_hash_cancel_and_locked_source_are_transactional() {
        let root = fixture();
        let source = root.join("input.bin");
        let target = root.join("cache.bin");
        fs::write(&source, vec![73u8; 3 * 1024 * 1024 + 7]).unwrap();
        let expected = crate::export_record::digest(&source).unwrap();
        let mut calls = 0;
        assert!(copy_digest_checked(&source, &target, || {
            calls += 1;
            if calls == 3 {
                Err("CANCELLED".into())
            } else {
                Ok(())
            }
        })
        .is_err());
        assert!(!target.exists());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        assert_eq!(crate::export_record::digest(&source).unwrap(), expected);
        let actual = copy_digest_checked(&source, &target, || {
            #[cfg(windows)]
            assert!(fs::OpenOptions::new().write(true).open(&source).is_err());
            Ok(())
        })
        .unwrap();
        assert_eq!(actual, expected);
        assert_eq!(crate::export_record::digest(&target).unwrap(), expected);
        assert!(copy_digest_checked(&source, &source, || Ok(())).is_err());
        let bad = root.join("folder");
        fs::create_dir(&bad).unwrap();
        assert!(copy_digest_checked(&source, &bad, || Ok(())).is_err());
        assert_eq!(crate::export_record::digest(&source).unwrap(), expected);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn owned_cache_move_never_leaves_partial_cache_after_cancel() {
        let root = fixture();
        let temp = temp_file(&root, "joined").unwrap();
        fs::write(temp.path(), b"validated video").unwrap();
        let target = root.join("cached.bin");
        let mut calls = 0;
        assert!(transfer_cache_checked(&temp, &target, || {
            calls += 1;
            if calls > 1 {
                Err("CANCELLED".into())
            } else {
                Ok(())
            }
        })
        .is_err());
        assert!(!target.exists());
        drop(temp);
        let temp = temp_file(&root, "joined").unwrap();
        fs::write(temp.path(), b"validated video").unwrap();
        let (bytes, digest, moved) = transfer_cache_checked(&temp, &target, || Ok(())).unwrap();
        assert!(moved);
        assert!(!temp.path().exists());
        drop(temp);
        assert_eq!(
            crate::export_record::digest(&target).unwrap(),
            (bytes, digest)
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn bounded_json_preserves_existing_file_and_cleans_only_owned_temp() {
        let root = fixture();
        let target = root.join("project.arkcut.json");
        let unrelated = root.join("unrelated.tmp");
        fs::write(&unrelated, b"keep").unwrap();
        atomic_json(&target, &serde_json::json!({"revision": 1})).unwrap();
        let before = fs::read(&target).unwrap();
        let large = serde_json::json!({"payload": "x".repeat(1024)});
        let error = atomic_json_limited(&target, &large, 64).unwrap_err();
        assert!(error.contains("PROJECT_TOO_LARGE"), "{error}");
        assert_eq!(fs::read(&target).unwrap(), before);
        assert_eq!(fs::read(&unrelated).unwrap(), b"keep");
        let new_target = root.join("new.arkcut.json");
        assert!(atomic_json_limited(&new_target, &large, 64).is_err());
        assert!(!new_target.exists());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
        // The limit is inclusive and accounts for UTF-8 bytes, not characters.
        let small = serde_json::json!("中文");
        let bytes = serde_json::to_vec_pretty(&small).unwrap();
        atomic_json_limited(&target, &small, bytes.len() as u64).unwrap();
        assert_eq!(fs::read(&target).unwrap(), bytes);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn owned_temps_and_active_cache_files_are_protected() {
        let root = fixture();
        let dir = root.join("project");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join(".arkcut-cache-v1"), b"owned").unwrap();
        let temp = temp_file(&dir, "test").unwrap();
        let temporary = temp.path().to_path_buf();
        fs::write(&temporary, b"owned temporary").unwrap();
        assert!(protected(&temporary, &[], &active_paths(&dir)));
        drop(temp);
        assert!(!temporary.exists());
        let active = dir.join("active.mp4");
        let old = dir.join("old.mp4");
        fs::write(&active, vec![0; 256 * 1024]).unwrap();
        fs::write(&old, vec![0; 128 * 1024]).unwrap();
        let guard = active_file(&active).unwrap();
        quota(&dir, 0, 300 * 1024, &[]).unwrap();
        assert!(active.exists());
        assert!(!old.exists());
        drop(guard);
        quota(&dir, 0, 128 * 1024, &[]).unwrap();
        assert!(!active.exists());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn oversized_project_rejected_before_reading_payload() {
        let root = fixture();
        let file = root.join("huge.arkcut.json");
        fs::File::create(&file)
            .unwrap()
            .set_len(128 * 1024 * 1024 + 1)
            .unwrap();
        assert_eq!(open(file.to_str().unwrap()).unwrap_err(), "工程文件过大");
        fs::remove_dir_all(root).unwrap();
    }
}
