use super::{platform, protocol::*, CONTROL};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
};
pub fn control(root: &Path) -> Result<PathBuf> {
    no_links(root)?;
    let dir = root.join(CONTROL);
    no_links(&dir)?;
    if dir.exists()
        && !dir.join("owner.txt").exists()
        && fs::read_dir(&dir)
            .map_err(|e| e.to_string())?
            .next()
            .is_some()
    {
        return Err("更新暂存目录含未知文件，请使用新的解压目录".into());
    }
    fs::create_dir_all(&dir)
        .map_err(|_| "程序目录不可写，当前版本仍可使用；请移到可写目录后更新")?;
    if dir.join("owner.txt").exists() {
        if read_bounded(&dir.join("owner.txt"), 100)? != b"ArkCut portable update v1\n" {
            return Err("更新目录标识不匹配".into());
        }
    } else {
        platform::atomic(&dir.join("owner.txt"), b"ArkCut portable update v1\n")?
    }
    Ok(dir)
}
pub fn remove_owned_tree(path: &Path) -> Result<()> {
    no_links(path)?;
    if !path.exists() {
        return Ok(());
    }
    if !path.is_dir() {
        return Err("预期更新子目录".into());
    }
    for entry in fs::read_dir(path).map_err(|e| e.to_string())? {
        let p = entry.map_err(|e| e.to_string())?.path();
        no_links(&p)?;
        if p.is_dir() {
            remove_owned_tree(&p)?
        } else {
            fs::remove_file(p).map_err(|e| e.to_string())?
        }
    }
    fs::remove_dir(path).map_err(|e| e.to_string())
}
pub fn old_files(root: &Path) -> Result<Vec<OwnedFile>> {
    let value: serde_json::Value = read_json(&root.join("manifest.json"), MAX_INDEX)?;
    if value["product"] != "ArkCut" {
        return Err("当前目录缺少可信的 ArkCut 程序清单，请手动重新解压完整包".into());
    }
    let array = value["files"].as_array().ok_or("当前程序文件清单缺失")?;
    if array.len() > 10000 {
        return Err("当前文件清单过大".into());
    }
    let mut list = Vec::new();
    let mut names = BTreeSet::new();
    for v in array {
        let mut f: OwnedFile = serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
        f.path = f.path.replace('\\', "/");
        owned_path(&f.path)?;
        if !valid_hash(&f.sha256)
            || !names.insert(f.path.to_lowercase())
            || f.path == "manifest.json"
        {
            return Err("当前文件清单重复或无效".into());
        }
        list.push(f);
    }
    let file = root.join("manifest.json");
    list.push(OwnedFile {
        path: "manifest.json".into(),
        bytes: fs::metadata(&file).map_err(|e| e.to_string())?.len(),
        sha256: hash(&file)?,
    });
    Ok(list)
}
pub fn verify_archive(path: &Path, r: &Release) -> Result<()> {
    no_links(path)?;
    if fs::metadata(path).map_err(|e| e.to_string())?.len() != r.asset.bytes
        || hash(path)? != r.asset.sha256
    {
        return Err("更新包长度或哈希不符，当前版本未改变".into());
    }
    Ok(())
}
pub fn extract(archive: &Path, dest: &Path, r: &Release) -> Result<()> {
    verify_archive(archive, r)?;
    if dest.exists() {
        return Err("更新解包目录已经存在".into());
    }
    no_links(dest)?;
    fs::create_dir(dest).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(File::open(archive).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    if zip.len() > 20000 {
        return Err("ZIP 条目过多".into());
    }
    let expected: BTreeMap<_, _> = r.files.iter().map(|f| (f.path.clone(), f)).collect();
    let mut seen = BTreeSet::new();
    let mut dirs = BTreeSet::new();
    let mut total = 0u64;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(|e| e.to_string())?;
        let name = f.name().map_err(|e| e.to_string())?.into_owned();
        if f.is_symlink()
            || f.encrypted()
            || f.unix_mode().is_some_and(|v| {
                let k = v & 0o170000;
                k != 0 && k != 0o100000 && k != 0o040000
            })
        {
            return Err("更新 ZIP 不允许链接、特殊文件或加密条目".into());
        }
        if f.is_dir() {
            let clean = name.trim_end_matches('/');
            safe_relative(clean)?;
            if clean != r.asset.root && !clean.starts_with(&(r.asset.root.clone() + "/")) {
                return Err("ZIP 目录越界".into());
            }
            if !dirs.insert(clean.to_lowercase()) {
                return Err("重复 ZIP 目录".into());
            }
            continue;
        }
        let relative = name
            .strip_prefix(&(r.asset.root.clone() + "/"))
            .ok_or("ZIP 顶层目录不符")?;
        owned_path(relative)?;
        let entry = expected.get(relative).ok_or("ZIP 包含清单外文件")?;
        if !seen.insert(relative.to_lowercase()) || f.size() != entry.bytes {
            return Err("ZIP 文件重复或长度不符".into());
        }
        let target = child(dest, relative)?;
        fs::create_dir_all(target.parent().unwrap()).map_err(|e| e.to_string())?;
        no_links(&target)?;
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&target)
            .map_err(|e| e.to_string())?;
        let mut written = 0u64;
        let mut buffer = [0u8; 65536];
        loop {
            let n = f.read(&mut buffer).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            written += n as u64;
            total += n as u64;
            if written > entry.bytes || total > r.unpacked_bytes {
                return Err("更新解包超过签名限额".into());
            }
            output.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
        }
        output.sync_all().map_err(|e| e.to_string())?;
        drop(output);
        if !matches(&target, entry) {
            return Err("解包文件哈希不符".into());
        }
    }
    if seen.len() != expected.len() || total != r.unpacked_bytes {
        return Err("ZIP 缺少程序文件".into());
    }
    let manifest: serde_json::Value = read_json(&dest.join("manifest.json"), MAX_INDEX)?;
    if manifest["version"] != r.version {
        return Err("包内版本与签名版本不一致".into());
    }
    let owned = old_files(dest)?;
    if owned.len() != r.files.len()
        || owned.iter().any(|f| {
            !expected
                .get(&f.path)
                .is_some_and(|e| e.bytes == f.bytes && e.sha256 == f.sha256)
        })
    {
        return Err("包内程序清单与签名不一致".into());
    }
    Ok(())
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    pub path: String,
    pub old: Option<OwnedFile>,
    pub new: Option<OwnedFile>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Journal {
    pub schema: u32,
    pub token: String,
    pub state: String,
    pub previous_version: String,
    pub target_version: String,
    pub envelope: Envelope,
    pub changes: Vec<Change>,
    pub completed: usize,
    pub error: Option<String>,
}
fn save(dir: &Path, j: &Journal) -> Result<()> {
    platform::json(&dir.join("transaction.json"), j)
}
pub fn journal(root: &Path) -> Result<Option<Journal>> {
    let path = root.join(CONTROL).join("transaction.json");
    if !path.exists() {
        return Ok(None);
    }
    read_json(&path, MAX_INDEX * 2).map(Some)
}
fn validate_journal(j: &Journal, c: &Config) -> Result<Release> {
    if j.schema != 1
        || j.token.len() != 36
        || uuid::Uuid::parse_str(&j.token).is_err()
        || j.changes.len() > 20000
    {
        return Err("更新恢复日志无效".into());
    }
    let (_, r) = verify(
        &serde_json::to_vec(&j.envelope).map_err(|e| e.to_string())?,
        c,
        None,
    )?;
    if r.version != j.target_version {
        return Err("更新恢复版本不匹配".into());
    }
    let map: BTreeMap<_, _> = r.files.iter().map(|x| (&x.path, x)).collect();
    let mut seen = BTreeSet::new();
    for x in &j.changes {
        owned_path(&x.path)?;
        if !seen.insert(x.path.to_lowercase()) {
            return Err("恢复路径重复".into());
        }
        for f in [&x.old, &x.new].into_iter().flatten() {
            if f.path != x.path || !valid_hash(&f.sha256) || f.bytes > MAX_ARCHIVE {
                return Err("恢复文件描述无效".into());
            }
        }
        if let Some(n) = &x.new {
            let expected = map.get(&x.path).ok_or("恢复文件不在签名清单内")?;
            if n.bytes != expected.bytes || n.sha256 != expected.sha256 {
                return Err("恢复文件签名不匹配".into());
            }
        }
    }
    if j.changes.iter().filter(|x| x.new.is_some()).count() != r.files.len() {
        return Err("恢复清单缺少签名文件".into());
    }
    Ok(r)
}
pub fn rollback(root: &Path, c: &Config, j: &mut Journal) -> Result<()> {
    validate_journal(j, c)?;
    let dir = control(root)?;
    j.state = "rollingBack".into();
    // The full undo plan was persisted before mutation. A transient log lock
    // must not prevent restoring files from that already durable plan.
    let _ = save(&dir, j);
    for change in j.changes.iter().rev() {
        let target = child(root, &change.path)?;
        let backup = child(&dir.join("backup"), &change.path)?;
        if backup.exists() {
            let old = change.old.as_ref().ok_or("未知旧文件备份")?;
            if !matches(&backup, old) {
                return Err("旧版本备份校验失败，请保留更新目录人工恢复".into());
            }
            if target.exists() {
                if !change.new.as_ref().is_some_and(|f| matches(&target, f)) {
                    return Err("程序文件在更新期间被修改，已保留备份，请关闭占用后恢复".into());
                }
                fs::remove_file(&target).map_err(|e| format!("回滚文件被占用：{e}"))?;
            }
            fs::create_dir_all(target.parent().unwrap()).map_err(|e| e.to_string())?;
            fs::rename(&backup, &target).map_err(|e| format!("回滚重命名失败：{e}"))?;
        } else if let Some(old) = &change.old {
            if !matches(&target, old) {
                return Err("原文件和备份状态不一致，保留现场供恢复".into());
            }
        } else if target.exists() {
            if !change.new.as_ref().is_some_and(|f| matches(&target, f)) {
                return Err("新增程序路径出现未知文件，拒绝删除".into());
            }
            fs::remove_file(&target).map_err(|e| e.to_string())?;
        }
    }
    j.state = "rolledBack".into();
    save(&dir, j)?;
    Ok(())
}
pub fn install<F: FnOnce(&Release, &str) -> Result<()>>(
    root: &Path,
    c: &Config,
    envelope: Envelope,
    r: &Release,
    health: F,
) -> Result<()> {
    let dir = control(root)?;
    let _lock = platform::lock(&dir.join("install.lock"))?;
    let _instances = platform::lock(&dir.join("instance.lock"))?;
    if let Some(j) = journal(root)? {
        if !["committed", "rolledBack"].contains(&j.state.as_str()) {
            return Err("上次更新尚未恢复，请先恢复旧版本".into());
        }
    }
    let old = old_files(root)?;
    let old_bytes: u64 = old.iter().map(|f| f.bytes).sum();
    platform::space(root, r.unpacked_bytes.saturating_add(old_bytes))?;
    let old_map: BTreeMap<_, _> = old.iter().map(|f| (f.path.clone(), f.clone())).collect();
    let new_map: BTreeMap<_, _> = r
        .files
        .iter()
        .map(|f| (f.path.clone(), f.clone()))
        .collect();
    let names: BTreeSet<_> = old_map.keys().chain(new_map.keys()).cloned().collect();
    let mut changes = Vec::new();
    for path in names {
        let target = child(root, &path)?;
        let previous = old_map.get(&path).cloned();
        if let Some(f) = &previous {
            if !matches(&target, f) {
                return Err(format!(
                    "程序文件已改变，请重新解压完整包后更新：{}",
                    f.path
                ));
            }
        } else if target.exists() {
            return Err(format!("更新路径已存在非程序文件，已保留：{path}"));
        }
        changes.push(Change {
            path: path.clone(),
            old: previous,
            new: new_map.get(&path).cloned(),
        });
    }
    for part in ["payload", "backup", "health-data", "health-profile"] {
        remove_owned_tree(&dir.join(part))?
    }
    extract(&dir.join("archive.zip"), &dir.join("payload"), r)?;
    // Run the verified new executable's component-only entry point before changing any installed file.
    // Components are versioned outside the application, so cancellation leaves the old program usable.
    if r.components.get("webview2").is_some_and(|v|v.starts_with("evergreen>=")) {
        let mut command=Command::new(dir.join("payload/ArkCut.exe"));
        command.arg("--prepare-components").arg("--component-ffmpeg").arg(&r.components["ffmpeg"]).arg("--component-webview2").arg(&r.components["webview2"]);
        platform::hidden(&mut command);
        let status=command.status().map_err(|e|format!("无法准备更新组件，当前版本未改变：{e}"))?;
        if !status.success(){return Err("更新组件准备未完成，当前版本未改变，可重试更新".into())}
    }
    fs::create_dir(dir.join("backup")).map_err(|e| e.to_string())?;
    let current: serde_json::Value = read_json(&root.join("manifest.json"), MAX_INDEX)?;
    let mut j = Journal {
        schema: 1,
        token: uuid::Uuid::new_v4().to_string(),
        state: "applying".into(),
        previous_version: current["version"].as_str().unwrap_or("").into(),
        target_version: r.version.clone(),
        envelope,
        changes,
        completed: 0,
        error: None,
    };
    save(&dir, &j)?;
    let result: Result<()> = (|| {
        for (index, change) in j.changes.iter().enumerate() {
            if change
                .old
                .as_ref()
                .zip(change.new.as_ref())
                .is_some_and(|(a, b)| a.bytes == b.bytes && a.sha256 == b.sha256)
            {
                continue;
            }
            let target = child(root, &change.path)?;
            if change.old.is_some() {
                let backup = child(&dir.join("backup"), &change.path)?;
                fs::create_dir_all(backup.parent().unwrap()).map_err(|e| e.to_string())?;
                fs::rename(&target, &backup)
                    .map_err(|e| format!("旧文件被占用或目录不可写：{e}"))?;
            }
            #[cfg(feature = "update-test-hooks")]
            if std::env::var("ARKCUT_UPDATE_TEST_INTERRUPT").ok() == Some(format!("backup:{index}"))
            {
                std::process::exit(86)
            }
            if change.new.is_some() {
                fs::create_dir_all(target.parent().unwrap()).map_err(|e| e.to_string())?;
                fs::rename(child(&dir.join("payload"), &change.path)?, &target)
                    .map_err(|e| format!("替换程序文件失败：{e}"))?;
            }
            #[cfg(feature = "update-test-hooks")]
            if std::env::var("ARKCUT_UPDATE_TEST_INTERRUPT").ok()
                == Some(format!("install:{index}"))
            {
                std::process::exit(86)
            }
            j.completed = index + 1;
            save(&dir, &j)?;
        }
        j.state = "awaitingHealth".into();
        save(&dir, &j)?;
        health(r, &j.token)?;
        j.state = "committed".into();
        save(&dir, &j)?;
        Ok(())
    })();
    if let Err(error) = result {
        j.error = Some(error.clone());
        let _ = save(&dir, &j);
        if let Err(recovery) = rollback(root, c, &mut j) {
            return Err(format!("{error}；恢复待处理：{recovery}"));
        }
        return Err(format!("{error}；已恢复旧版本，可重试更新"));
    }
    // Once committed, only the previous program files are needed for the
    // retention policy. Do not leave a full runtime duplicate and ZIP behind.
    let mut pending=Vec::new();
    for part in ["payload","health-data","health-profile"]{
        if remove_owned_tree(&dir.join(part)).is_err(){pending.push(part.to_owned());}
    }
    let archive=dir.join("archive.zip");
    if no_links(&archive).is_err()||fs::remove_file(&archive).is_err(){pending.push("archive.zip".into());}
    let _=platform::json(&dir.join("cleanup.json"),&serde_json::json!({"pending":pending}));
    Ok(())
}
pub fn recover(root: &Path, c: &Config) -> Result<()> {
    let dir = control(root)?;
    let _lock = platform::lock(&dir.join("install.lock"))?;
    let _instances = platform::lock(&dir.join("instance.lock"))?;
    if let Some(mut j) = journal(root)? {
        if !["committed", "rolledBack"].contains(&j.state.as_str()) {
            rollback(root, c, &mut j)?
        }
    }
    Ok(())
}
pub fn health_process(root: &Path, r: &Release, token: &str) -> Result<()> {
    let dir = control(root)?;
    let ack = dir.join("health.json");
    if ack.exists() {
        fs::remove_file(&ack).map_err(|e| e.to_string())?
    }
    let mut command = Command::new(root.join("ArkCut.exe"));
    command
        .arg("--arkcut-health")
        .arg(token)
        .env("ARKCUT_DATA_DIR", dir.join("health-data"))
        .env("WEBVIEW2_USER_DATA_FOLDER", dir.join("health-profile"))
        .env("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", "--mute-audio");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000004);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("新版本无法启动：{e}"))?;
    #[cfg(windows)]
    let group = match crate::resource::platform::ProcessGroup::attach(&child) {
        Ok(g) => g,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
    };
    let result = platform::wait_child(&mut child, 90);
    #[cfg(windows)]
    {
        group.terminate();
        drop(group);
    }
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return result;
    }
    let receipt: serde_json::Value = read_json(&ack, 4096).map_err(|_|"新版本未完成界面初始化，已启动回滚".to_string())?;
    if receipt["token"] != token || receipt["version"] != r.version || receipt["ready"] != true {
        return Err("新版本没有完成启动验证".into());
    }
    Ok(())
}
