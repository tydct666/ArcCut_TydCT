use crate::{model::Project, storage, timeline};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs,
    io::{BufReader, Read},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

static WRITER: OnceLock<Mutex<()>> = OnceLock::new();
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    #[serde(default)]
    pub revision: u64,
    pub project_id: String,
    pub name: String,
    pub source_path: String,
    pub status: String,
    pub created_ms: u128,
    pub updated_ms: u128,
    pub path: String,
}
fn now() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
fn clean(text: &str) -> String {
    let s: String = text
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .take(80)
        .collect();
    let s = s.trim().trim_end_matches('.');
    if s.is_empty() {
        "录屏项目".into()
    } else {
        s.into()
    }
}
fn title(source: &str) -> String {
    clean(
        Path::new(source)
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .as_ref(),
    )
}
#[cfg(windows)]
fn stamp() -> String {
    #[repr(C)]
    #[derive(Default)]
    struct LocalTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        millis: u16,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetLocalTime(time: *mut LocalTime);
    }
    let mut t = LocalTime::default();
    unsafe { GetLocalTime(&mut t) };
    format!(
        "{:04}{:02}{:02}_{:02}{:02}{:02}",
        t.year, t.month, t.day, t.hour, t.minute, t.second
    )
}
#[cfg(not(windows))]
fn stamp() -> String {
    now().to_string()
}
pub fn root() -> Result<PathBuf, String> {
    if let Some(path) = storage::settings().0.archive_directory {
        return Ok(PathBuf::from(path));
    }
    Ok(std::env::current_exe()
        .map_err(|e| e.to_string())?
        .parent()
        .ok_or("工具目录不可用")?
        .join("项目存档"))
}
fn meta_path(path: &Path) -> Result<PathBuf, String> {
    Ok(path
        .parent()
        .ok_or("存档路径无效")?
        .join("archive-state.json"))
}
fn small_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    if file.metadata().map_err(|e| e.to_string())?.len() > 1024 * 1024 {
        return Err("存档记录过大".into());
    }
    serde_json::from_reader(BufReader::new(file.take(1024 * 1024 + 1))).map_err(|e| e.to_string())
}
fn metadata(path: &Path) -> Result<Entry, String> {
    if !path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case("project.arkcut.json"))
    {
        return Err("这不是自动存档主文件".into());
    }
    let mut entry: Entry =
        small_json(&meta_path(path)?).map_err(|e| format!("读取存档记录失败：{e}"))?;
    entry.path = path.to_string_lossy().into();
    Ok(entry)
}
fn write_meta(entry: &Entry) -> Result<(), String> {
    storage::atomic(
        &meta_path(Path::new(&entry.path))?,
        &serde_json::to_vec_pretty(entry).map_err(|e| e.to_string())?,
    )
}
pub fn begin(source: &str) -> Result<Entry, String> {
    let root = root()?;
    fs::create_dir_all(&root)
        .map_err(|e| format!("ARCHIVE_LOCATION: 工具目录无法建立项目存档，请选择可写位置：{e}"))?;
    let id = uuid::Uuid::new_v4().to_string();
    let name = title(source);
    let folder = root.join(format!("{}_{}_{}", name, stamp(), &id[..8]));
    fs::create_dir(&folder).map_err(|e| format!("ARCHIVE_LOCATION: 无法创建项目目录：{e}"))?;
    let entry = Entry {
        revision: 0,
        project_id: id,
        name,
        source_path: source.into(),
        status: "importing".into(),
        created_ms: now(),
        updated_ms: now(),
        path: folder.join("project.arkcut.json").to_string_lossy().into(),
    };
    write_meta(&entry)?;
    Ok(entry)
}
pub fn mark(path: &str, status: &str) -> Result<(), String> {
    if !["importing", "cancelled", "failed", "ready"].contains(&status) {
        return Err("无效存档状态".into());
    }
    let mut e = metadata(Path::new(path))?;
    e.status = status.into();
    e.updated_ms = now();
    write_meta(&e)
}
#[cfg(test)]
fn valid(bytes: &[u8]) -> Result<Project, String> {
    if bytes.len() > 128 * 1024 * 1024 {
        return Err("存档过大".into());
    }
    let p: Project = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    timeline::build(&p)?;
    Ok(p)
}
const MAX_PROJECT_BYTES: u64 = 128 * 1024 * 1024;
fn project_file(path: &Path) -> Result<fs::File, String> {
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    if file.metadata().map_err(|e| e.to_string())?.len() > MAX_PROJECT_BYTES {
        return Err("存档过大，原文件未修改".into());
    }
    Ok(file)
}
fn valid_path(path: &Path) -> Result<Project, String> {
    let file = project_file(path)?;
    let resource = crate::resource::ResourceController::new(std::sync::Arc::new(
        std::sync::atomic::AtomicBool::new(false),
    ));
    let _memory = resource.reserve_memory(
        file.metadata()
            .map_err(|e| e.to_string())?
            .len()
            .saturating_mul(8),
        "读取工程",
    )?;
    let project: Project =
        serde_json::from_reader(BufReader::new(file.take(MAX_PROJECT_BYTES + 1)))
            .map_err(|e| e.to_string())?;
    timeline::build(&project)?;
    Ok(project)
}
pub fn save(project: &Project, path: &str, status: &str) -> Result<u64, String> {
    let _guard = WRITER
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| "存档写入器不可用")?;
    timeline::build(project)?;
    let path = Path::new(path);
    let mut entry = metadata(path)?;
    if entry.project_id != project.project_id {
        return Err("存档与项目标识不一致".into());
    }
    storage::protect_inputs(project, path)?;
    // A new archive has no main file yet. Every existing main must validate
    // before replacement: read failures and future schemas are not new files.
    let existing = match fs::symlink_metadata(path) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(format!(
                "ARCHIVE_VALIDATION: 无法检查现有存档，原文件未修改：{error}"
            ))
        }
    };
    if existing {
        match valid_path(path) {
            Ok(old) => {
                if old.project_id != project.project_id {
                    return Err("存档已属于其他项目".into());
                }
                if old.revision > project.revision {
                    return Ok(old.revision);
                }
                let changed = storage::hash(&old)? != storage::hash(project)?;
                drop(old);
                if changed {
                    storage::atomic_copy(
                        path,
                        &path.with_file_name("project.previous.arkcut.json"),
                    )?
                }
            }
            Err(error) if error.starts_with("RESOURCE_") || error.starts_with("存档过大") => {
                return Err(error)
            }
            Err(error) => {
                return Err(format!(
                    "ARCHIVE_VALIDATION: 现有存档无法验证，原文件未修改：{error}"
                ))
            }
        }
    }
    storage::atomic_json(path, project).map_err(|e| format!("自动保存失败：{e}"))?;
    entry.revision = project.revision;
    entry.name = project
        .name
        .clone()
        .unwrap_or_else(|| title(&project.source.fingerprint.path));
    entry.status = status.into();
    entry.updated_ms = now();
    write_meta(&entry)?;
    if status == "ready" {
        remember(path)?
    }
    Ok(project.revision)
}
fn installation() -> Result<PathBuf, String> {
    std::env::current_exe()
        .map_err(|e| e.to_string())?
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "工具目录不可用".into())
}
fn index_path_for(data: &Path, installation: &Path) -> PathBuf {
    use sha2::{Digest, Sha256};
    let folder = installation
        .canonicalize()
        .unwrap_or_else(|_| installation.to_path_buf());
    let identity = folder.to_string_lossy().to_string();
    #[cfg(windows)]
    let identity = identity.to_lowercase();
    data.join("recent-projects")
        .join(format!("{:x}.json", Sha256::digest(identity.as_bytes())))
}
fn remember(path: &Path) -> Result<(), String> {
    let index = index_path_for(&storage::root(), &installation()?);
    let path = path.to_string_lossy().to_string();
    let mut paths: Vec<String> = small_json(&index).unwrap_or_default();
    paths.retain(|p| !p.eq_ignore_ascii_case(&path));
    paths.insert(0, path);
    paths.truncate(60);
    fs::create_dir_all(index.parent().ok_or("最近项目目录不可用")?).map_err(|e| e.to_string())?;
    storage::atomic(
        &index,
        &serde_json::to_vec(&paths).map_err(|e| e.to_string())?,
    )
}
pub fn recent() -> Result<Vec<Entry>, String> {
    recent_for(&installation()?, &storage::root())
}
fn recent_for(installation: &Path, data: &Path) -> Result<Vec<Entry>, String> {
    // A fresh portable folder must not inherit another copy's history or shared
    // custom archive setting. Local archives remain discoverable after a move.
    let root = installation.join("项目存档");
    let mut paths: Vec<String> =
        small_json(&index_path_for(data, installation)).unwrap_or_default();
    if root.exists() {
        for folder in fs::read_dir(root).map_err(|e| e.to_string())? {
            let folder = folder.map_err(|e| e.to_string())?;
            if folder.file_type().map_err(|e| e.to_string())?.is_dir()
                && !folder.file_type().map_err(|e| e.to_string())?.is_symlink()
            {
                paths.push(
                    folder
                        .path()
                        .join("project.arkcut.json")
                        .to_string_lossy()
                        .into(),
                )
            }
        }
    }
    let mut entries = vec![];
    for path in paths {
        let path = Path::new(&path);
        if let Ok(e) = metadata(path) {
            if e.status == "ready"
                && (path.is_file()
                    || path
                        .with_file_name("project.previous.arkcut.json")
                        .is_file())
            {
                entries.push(e)
            }
        }
    }
    entries.sort_by(|a, b| {
        b.revision
            .cmp(&a.revision)
            .then_with(|| b.updated_ms.cmp(&a.updated_ms))
    });
    let mut ids = std::collections::HashSet::new();
    entries.retain(|e| ids.insert(e.project_id.clone()));
    entries.sort_by_key(|e| std::cmp::Reverse(e.updated_ms));
    entries.truncate(30);
    Ok(entries)
}
pub fn open(path: &str) -> Result<Value, String> {
    let path = Path::new(path);
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Header {
        schema_version: u32,
    }
    if let Ok(file) = project_file(path) {
        if serde_json::from_reader::<_, Header>(BufReader::new(file.take(MAX_PROJECT_BYTES + 1)))
            .ok()
            .is_some_and(|h| h.schema_version != 1)
        {
            return Err("不支持的项目版本，原文件未修改".into());
        }
    }
    let owner = metadata(path).ok();
    let parsed = match valid_path(path) {
        Ok(p) => Some(p).filter(|p| owner.as_ref().is_none_or(|e| e.project_id == p.project_id)),
        Err(error) if error.starts_with("RESOURCE_") || error.starts_with("存档过大") => {
            return Err(error)
        }
        Err(_) => None,
    };
    let mut recovered = false;
    let project = if let Some(p) = parsed {
        p
    } else {
        let owned = metadata(path).map_err(|_| "PROJECT_CORRUPT: 项目无法解析，原文件未修改")?;
        let previous = path.with_file_name("project.previous.arkcut.json");
        let p = valid_path(&previous)?;
        if p.project_id != owned.project_id {
            return Err("上一完整版本属于其他项目，未恢复".into());
        }
        if path.is_file() {
            storage::atomic_copy(
                path,
                &path.with_file_name(format!("project.corrupt-{}.json", now())),
            )?
        }
        storage::atomic_json(path, &p)?;
        recovered = true;
        p
    };
    let mut messages: Vec<String> = std::iter::once(&project.source)
        .chain(project.music.iter().map(|m| &m.media))
        .filter_map(|m| crate::media::verify_media(m).err())
        .collect();
    if recovered {
        messages.push("已从上一份完整存档恢复；损坏文件已保留".into())
    }
    if let Ok(mut entry) = metadata(path) {
        entry.status = "ready".into();
        entry.revision = project.revision;
        entry.name = project
            .name
            .clone()
            .unwrap_or_else(|| title(&project.source.fingerprint.path));
        entry.updated_ms = now();
        write_meta(&entry)?;
        remember(path)?;
        return Ok(
            json!({"project":project,"path":path,"warnings":messages,"recovered":recovered}),
        );
    }
    let mut adopted = project;
    let entry = begin(&adopted.source.fingerprint.path)?;
    adopted.project_id = entry.project_id.clone();
    save(&adopted, &entry.path, "ready")?;
    Ok(json!({"project":adopted,"path":entry.path,"warnings":messages,"recovered":recovered}))
}
pub fn create(project: &Project) -> Result<Entry, String> {
    let mut entry = begin(&project.source.fingerprint.path)?;
    entry.project_id = project.project_id.clone();
    write_meta(&entry)?;
    save(project, &entry.path, "ready")?;
    entry.status = "ready".into();
    Ok(entry)
}
pub fn set_location(parent: &str) -> Result<PathBuf, String> {
    let parent = Path::new(parent);
    if !parent.is_absolute() || !parent.is_dir() {
        return Err("请选择已有的可写文件夹".into());
    }
    let root = if parent.file_name().is_some_and(|s| s == "项目存档") {
        parent.to_path_buf()
    } else {
        parent.join("项目存档")
    };
    fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let test = root.join(format!(".write-check-{}", uuid::Uuid::new_v4()));
    storage::atomic(&test, b"ArkCut")?;
    fs::remove_file(test).map_err(|e| e.to_string())?;
    let mut settings = storage::settings().0;
    settings.archive_directory = Some(root.to_string_lossy().into());
    storage::save_settings(&settings)?;
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recent_history_is_local_to_each_portable_copy() {
        let base = std::env::temp_dir().join(format!("arkcut-recent-{}", uuid::Uuid::new_v4()));
        let data = base.join("data");
        let first = base.join("first");
        let fresh = base.join("fresh");
        let external = base.join("external/project");
        for folder in [&data, &first, &fresh, &external] {
            fs::create_dir_all(folder).unwrap();
        }
        let path = external.join("project.arkcut.json");
        let entry = Entry {
            revision: 1,
            project_id: "external".into(),
            name: "外部项目".into(),
            source_path: "fixture".into(),
            status: "ready".into(),
            created_ms: 1,
            updated_ms: 2,
            path: path.to_string_lossy().into(),
        };
        fs::write(&path, b"fixture").unwrap();
        write_meta(&entry).unwrap();
        let remembered = serde_json::to_vec(&vec![entry.path.clone()]).unwrap();
        fs::write(data.join("recent-projects.json"), &remembered).unwrap();
        // A legacy shared index never populates a newly extracted copy.
        assert!(recent_for(&fresh, &data).unwrap().is_empty());
        let first_index = index_path_for(&data, &first);
        fs::create_dir_all(first_index.parent().unwrap()).unwrap();
        fs::write(&first_index, &remembered).unwrap();
        assert_eq!(recent_for(&first, &data).unwrap()[0].project_id, "external");
        assert!(recent_for(&fresh, &data).unwrap().is_empty());
        // An existing local archive remains discoverable after moving its copy.
        let local = fresh.join("项目存档/local");
        fs::create_dir_all(&local).unwrap();
        fs::copy(&path, local.join("project.arkcut.json")).unwrap();
        fs::copy(meta_path(&path).unwrap(), local.join("archive-state.json")).unwrap();
        let moved = base.join("moved");
        fs::rename(&fresh, &moved).unwrap();
        assert_eq!(
            PathBuf::from(&recent_for(&moved, &data).unwrap()[0].path),
            moved
                .join("项目存档")
                .join("local")
                .join("project.arkcut.json")
        );
        fs::remove_file(&path).unwrap();
        assert!(recent_for(&first, &data).unwrap().is_empty());
        assert_eq!(
            fs::read(data.join("recent-projects.json")).unwrap(),
            remembered
        );
        fs::remove_dir_all(base).unwrap();
    }
    #[test]
    fn existing_unverifiable_main_never_overwritten() {
        let base =
            std::env::temp_dir().join(format!("arkcut-archive-invalid-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&base).unwrap();
        let path = base.join("project.arkcut.json");
        let mut p = crate::timeline::tests::project();
        let entry = Entry {
            revision: 0,
            project_id: p.project_id.clone(),
            name: "fixture".into(),
            source_path: "fixture".into(),
            status: "importing".into(),
            created_ms: now(),
            updated_ms: now(),
            path: path.to_string_lossy().into(),
        };
        write_meta(&entry).unwrap();
        // NotFound is the only allowed unverified-main case.
        save(&p, path.to_str().unwrap(), "importing").unwrap();
        p.revision = 1;
        save(&p, path.to_str().unwrap(), "importing").unwrap();
        let previous = base.join("project.previous.arkcut.json");
        let previous_bytes = fs::read(&previous).unwrap();
        let state = base.join("archive-state.json");
        let state_bytes = fs::read(&state).unwrap();
        let mut future = p.clone();
        future.schema_version = 2;
        for invalid in [
            serde_json::to_vec(&future).unwrap(),
            b"{broken json".to_vec(),
        ] {
            fs::write(&path, &invalid).unwrap();
            p.revision += 1;
            let error = save(&p, path.to_str().unwrap(), "ready").unwrap_err();
            assert!(error.starts_with("ARCHIVE_VALIDATION:"), "{error}");
            assert_eq!(fs::read(&path).unwrap(), invalid);
            assert_eq!(fs::read(&previous).unwrap(), previous_bytes);
            assert_eq!(fs::read(&state).unwrap(), state_bytes);
        }
        fs::remove_dir_all(base).unwrap();
    }
    #[test]
    fn serialized_save_recovery_and_cancelled_index() {
        let base =
            std::env::temp_dir().join(format!("arkcut-archive-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&base).unwrap();
        let path = base.join("project.arkcut.json");
        let mut p = crate::timeline::tests::project();
        let entry = Entry {
            revision: 0,
            project_id: p.project_id.clone(),
            name: "fixture".into(),
            source_path: "fixture".into(),
            status: "importing".into(),
            created_ms: now(),
            updated_ms: now(),
            path: path.to_string_lossy().into(),
        };
        write_meta(&entry).unwrap();
        save(&p, path.to_str().unwrap(), "importing").unwrap();
        p.revision = 2;
        p.name = Some("重命名".into());
        save(&p, path.to_str().unwrap(), "ready").unwrap();
        let mut stale = p.clone();
        stale.revision = 1;
        assert_eq!(save(&stale, path.to_str().unwrap(), "ready").unwrap(), 2);
        assert_eq!(valid(&fs::read(&path).unwrap()).unwrap().revision, 2);
        assert_eq!(
            valid(&fs::read(base.join("project.previous.arkcut.json")).unwrap())
                .unwrap()
                .revision,
            0
        );
        p.revision = 3;
        save(&p, path.to_str().unwrap(), "ready").unwrap();
        mark(path.to_str().unwrap(), "cancelled").unwrap();
        assert_eq!(metadata(&path).unwrap().status, "cancelled");
        fs::write(&path, b"broken").unwrap();
        let restored = open(path.to_str().unwrap()).unwrap();
        assert_eq!(restored["recovered"], true);
        assert_eq!(restored["project"]["revision"], 2);
        assert!(fs::read_dir(&base).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("project.corrupt-")));
        let mut unsupported = p.clone();
        unsupported.schema_version = 2;
        let future = serde_json::to_vec(&unsupported).unwrap();
        fs::write(&path, &future).unwrap();
        assert!(open(path.to_str().unwrap()).unwrap_err().contains("不支持"));
        assert_eq!(fs::read(&path).unwrap(), future);
        assert!(metadata(&base.join("project.previous.arkcut.json")).is_err());
        fs::remove_dir_all(base).unwrap();
    }
    #[test]
    fn concurrent_revisions_keep_the_newest_complete_project() {
        let base =
            std::env::temp_dir().join(format!("arkcut-archive-race-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&base).unwrap();
        let path = base.join("project.arkcut.json");
        let p = crate::timeline::tests::project();
        let e = Entry {
            revision: 0,
            project_id: p.project_id.clone(),
            name: "race".into(),
            source_path: "fixture".into(),
            status: "importing".into(),
            created_ms: now(),
            updated_ms: now(),
            path: path.to_string_lossy().into(),
        };
        write_meta(&e).unwrap();
        save(&p, path.to_str().unwrap(), "ready").unwrap();
        let workers: Vec<_> = (1..=20)
            .rev()
            .map(|revision| {
                let mut p = p.clone();
                p.revision = revision;
                let path = path.clone();
                std::thread::spawn(move || save(&p, path.to_str().unwrap(), "ready").unwrap())
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(valid(&fs::read(&path).unwrap()).unwrap().revision, 20);
        assert!(valid(&fs::read(base.join("project.previous.arkcut.json")).unwrap()).is_ok());
        fs::remove_dir_all(base).unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn locked_target_preserves_main_and_can_retry() {
        use std::os::windows::fs::OpenOptionsExt;
        let base =
            std::env::temp_dir().join(format!("arkcut-archive-lock-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&base).unwrap();
        let path = base.join("project.arkcut.json");
        let mut p = crate::timeline::tests::project();
        let e = Entry {
            revision: 0,
            project_id: p.project_id.clone(),
            name: "lock".into(),
            source_path: "fixture".into(),
            status: "importing".into(),
            created_ms: now(),
            updated_ms: now(),
            path: path.to_string_lossy().into(),
        };
        write_meta(&e).unwrap();
        save(&p, path.to_str().unwrap(), "ready").unwrap();
        let guard = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        p.revision = 1;
        assert!(save(&p, path.to_str().unwrap(), "ready").is_err());
        drop(guard);
        assert_eq!(valid(&fs::read(&path).unwrap()).unwrap().revision, 0);
        save(&p, path.to_str().unwrap(), "ready").unwrap();
        assert_eq!(valid(&fs::read(&path).unwrap()).unwrap().revision, 1);
        assert_eq!(
            valid(&fs::read(base.join("project.previous.arkcut.json")).unwrap())
                .unwrap()
                .revision,
            0
        );
        fs::remove_dir_all(base).unwrap();
    }
}
