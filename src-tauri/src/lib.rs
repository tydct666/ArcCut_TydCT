pub mod archive;
pub mod components;
pub mod audio;
pub mod detector;
pub mod selection;
pub mod transition;
pub mod diagnostics;
#[cfg(feature = "smart-cut")]
mod edge_pcm;
pub mod encoder;
pub mod export_record;
pub mod media;
pub mod model;
pub mod process;
pub mod render;
pub mod repair;
pub mod resource;
#[cfg(feature = "smart-cut")]
mod smart;
#[cfg(feature = "smart-cut")]
mod smart_mp4;
pub mod source_export;
pub mod storage;
pub mod update;
pub mod timeline;
use model::*;
use process::*;
use serde_json::{json, Value};
use std::{
    fs,
    path::PathBuf,
    sync::{atomic::Ordering, Arc, Mutex},
};
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub sequence: u64,
    pub job_id: String,
    pub project_id: String,
    pub revision: u64,
    pub stage: String,
    pub status: String,
    pub processed_us: Option<Us>,
    pub message: String,
    #[serde(flatten)]
    pub metric: Metric,
    pub elapsed_ms: u64,
}
struct Job {
    job_id: String,
    project_id: String,
    revision: u64,
    stage: String,
    operation: String,
    status: String,
    processed_us: Option<Us>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    result: Option<Value>,
    error: Option<String>,
    metric: Metric,
    started: std::time::Instant,
    previews: Vec<Value>,
    imported_project: Option<Value>,
    resource: resource::Summary,
}
#[derive(Default)]
struct Inner {
    job: Option<Job>,
    events: Vec<Event>,
    sequence: u64,
    pinned: Vec<String>,
    pinned_leases: Vec<storage::ActiveFile>,
    cache_limit: u64,
}
#[derive(Clone, Default)]
pub struct Engine {
    inner: Arc<Mutex<Inner>>,
}
fn parse<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, String> {
    serde_json::from_value(v).map_err(|e| format!("INVALID_REQUEST: {e}"))
}
fn prepare_detection(r: &Runner, p: &Project, phase: &str) -> Result<(), String> {
    let video = p
        .source
        .streams
        .iter()
        .find(|s| s.index == p.video_index)
        .ok_or("缺少视频流")?;
    let pixels = (video.width as u64).saturating_mul(video.height as u64);
    r.prepare_stage(
        phase,
        64 * resource::MIB + pixels.saturating_mul(48),
        pixels.saturating_mul(12) + 4 * resource::MIB,
    )?;
    Ok(())
}
fn analyze_budgeted(r: &Runner, p: &Project) -> Result<detector::Analysis, String> {
    prepare_detection(r, p, "detect")?;
    detector::analyze(r, p)
}
impl Engine {
    fn resource_update(&self, job_id: &str, summary: resource::Summary) {
        let mut i = self.inner.lock().unwrap();
        let Some(job) = i.job.as_mut().filter(|j| j.job_id == job_id) else {
            return;
        };
        let changed = job.resource.policy_changes != summary.policy_changes
            || job.resource.notice != summary.notice;
        job.resource = summary.clone();
        drop(i);
        // Resource observations must never replace the real media phase/progress.
        // The JSONL file is bounded; the UI receives only the current summary.
        let root = storage::root().join("logs");
        let _ = fs::create_dir_all(&root);
        let file = root.join("resources.jsonl");
        if file.metadata().is_ok_and(|m| m.len() > 5 * 1024 * 1024) {
            let _ = fs::remove_file(root.join("resources.2.jsonl"));
            let _ = fs::rename(
                root.join("resources.1.jsonl"),
                root.join("resources.2.jsonl"),
            );
            let _ = fs::rename(&file, root.join("resources.1.jsonl"));
        }
        use std::io::Write;
        if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(file) {
            let _ = writeln!(
                f,
                "{}",
                json!({"jobId":job_id,"policyChanged":changed,"resource":summary})
            );
        }
    }
    fn metric(&self, m: Metric) {
        let mut i = self.inner.lock().unwrap();
        let mut changed = false;
        let detail = m.detail.clone();
        if let Some(j) = &mut i.job {
            changed = j.metric.phase != m.phase || j.metric.detail != m.detail;
            j.metric = m;
        }
        drop(i);
        if changed && !detail.is_empty() {
            self.event("phase", "running", None, &detail);
        }
    }
    fn event(&self, stage: &str, status: &str, processed: Option<Us>, message: &str) {
        let mut i = self.inner.lock().unwrap();
        i.sequence += 1;
        let e = Event {
            sequence: i.sequence,
            job_id: i.job.as_ref().map(|j| j.job_id.clone()).unwrap_or_default(),
            project_id: i
                .job
                .as_ref()
                .map(|j| j.project_id.clone())
                .unwrap_or_default(),
            revision: i.job.as_ref().map(|j| j.revision).unwrap_or(0),
            stage: stage.into(),
            status: status.into(),
            processed_us: processed,
            message: message.into(),
            metric: i.job.as_ref().map(|j| j.metric.clone()).unwrap_or_default(),
            elapsed_ms: i
                .job
                .as_ref()
                .map(|j| j.started.elapsed().as_millis() as u64)
                .unwrap_or(0),
        };
        if let Some(j) = &mut i.job {
            j.stage = stage.into();
            j.processed_us = processed;
        }
        i.events.push(e.clone());
        if i.events.len() > 1000 {
            i.events.remove(0);
        }
        drop(i);
        if !message.is_empty() {
            let root = storage::root().join("logs");
            let _ = fs::create_dir_all(&root);
            let file = root.join("arkcut.log");
            if file
                .metadata()
                .map(|m| m.len() > 10 * 1024 * 1024)
                .unwrap_or(false)
            {
                let _ = fs::remove_file(root.join("arkcut.4.log"));
                for n in (1..4).rev() {
                    let _ = fs::rename(
                        root.join(format!("arkcut.{n}.log")),
                        root.join(format!("arkcut.{}.log", n + 1)),
                    );
                }
                let _ = fs::rename(&file, root.join("arkcut.1.log"));
            }
            use std::io::Write;
            if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(file) {
                let _ = writeln!(f, "{}", serde_json::to_string(&e).unwrap_or_default());
            }
        }
    }
    pub fn request(&self, op: &str, payload: Value) -> Result<Value, String> {
        match op {
            "outputExists" => Ok(json!(payload["path"]
                .as_str()
                .is_some_and(|p| std::path::Path::new(p).is_file()))),
            "exportLogs" => diagnostics::export(
                std::path::Path::new(payload["path"].as_str().ok_or("缺少日志路径")?),
                payload["jobId"].as_str().unwrap_or(""),
            ),
            "repairReport" => repair::read(&parse::<Project>(payload["project"].clone())?),
            "confirmReview" => {
                let p: Project = parse(payload["project"].clone())?;
                let mut report = repair::read(&p)?;
                if report.is_null() || report["signature"] != export_record::signature(&p)? {
                    return Err("检查记录已变化，请重新检查".into());
                }
                let issues = report["issues"].as_array().ok_or("没有待确认问题")?;
                if issues.is_empty() || issues.iter().any(|v| v["kind"] != "suspectedFrame") {
                    return Err("明确异常需要修复，不能确认跳过".into());
                }
                report["confirmedIssues"] = json!(storage::hash(&issues)?);
                repair::save(&p, &report)?;
                Ok(report)
            }
            "exportState" => {
                let p: Project = parse(payload["project"].clone())?;
                let path = payload["path"].as_str().ok_or("缺少输出路径")?;
                let valid = export_record::check(&p, path)?;
                Ok(json!({"valid":valid,"path":if valid{Some(path)}else{None}}))
            }
            "clientLog" => {
                self.event(
                    payload["stage"].as_str().unwrap_or("interface"),
                    payload["status"].as_str().unwrap_or("running"),
                    None,
                    payload["message"].as_str().unwrap_or(""),
                );
                Ok(json!(true))
            }
            "deferCleanup" => {
                storage::defer_cleanup(payload["projectId"].as_str().ok_or("缺少工程 ID")?)?;
                Ok(json!(true))
            }
            "status" => {
                let i = self.inner.lock().unwrap();
                let after = payload["after"].as_u64().unwrap_or(0);
                let include_result = payload["includeResult"].as_bool().unwrap_or(true);
                Ok(
                    json!({"job":i.job.as_ref().map(|j|json!({"jobId":j.job_id,"projectId":j.project_id,"revision":j.revision,"stage":j.stage,"operation":j.operation,"status":j.status,"processedUs":j.processed_us,"result":if include_result{j.result.as_ref()}else{None},"error":j.error,"phase":j.metric.phase,"completedUnits":j.metric.completed_units,"totalUnits":j.metric.total_units,"unit":j.metric.unit,"detail":j.metric.detail,"elapsedMs":j.started.elapsed().as_millis(),"resource":j.resource,"previews":j.previews,"importedProject":if payload["includeImported"]==true{j.imported_project.as_ref()}else{None}})),"resource":i.job.as_ref().map(|j|&j.resource),"events":i.events.iter().filter(|e|e.sequence>after).collect::<Vec<_>>()}),
                )
            }
            "cancel" => {
                let mut i = self.inner.lock().unwrap();
                if let Some(j) = &mut i.job {
                    if j.status == "running" {
                        j.cancel.store(true, Ordering::Relaxed);
                        j.status = "cancelling".into();
                    }
                }
                Ok(json!(true))
            }
            "archiveBegin" => Ok(json!(archive::begin(
                payload["sourcePath"].as_str().ok_or("缺少素材路径")?
            )?)),
            "archiveSave" => Ok(
                json!({"revision":archive::save(&parse::<Project>(payload["project"].clone())?,payload["path"].as_str().ok_or("缺少存档路径")?,"ready")?}),
            ),
            "archiveCreate" => Ok(json!(archive::create(&parse::<Project>(
                payload["project"].clone()
            )?)?)),
            "archiveRecent" => Ok(json!(archive::recent()?)),
            "archiveOpen" => archive::open(payload["path"].as_str().ok_or("缺少存档路径")?),
            "archiveMark" => {
                archive::mark(
                    payload["path"].as_str().ok_or("缺少存档路径")?,
                    payload["status"].as_str().ok_or("缺少处理状态")?,
                )?;
                Ok(json!(true))
            }
            "archiveLocation" => Ok(json!(archive::set_location(
                payload["path"].as_str().ok_or("缺少存档目录")?
            )?)),
            "archiveRoot" => Ok(json!(archive::root()?)),
            "openArchiveFolder" => {
                let path = archive::root()?;
                fs::create_dir_all(&path).map_err(|e| e.to_string())?;
                std::process::Command::new("explorer.exe")
                    .arg(path)
                    .spawn()
                    .map_err(|e| e.to_string())?;
                Ok(json!(true))
            }
            "plan" => Ok(json!(timeline::build(&parse::<Project>(payload)?)?)),
            "filter" => {
                let p: Project = parse(payload["project"].clone())?;
                let plan = if payload["plan"].is_object() {
                    let plan: Plan = parse(payload["plan"].clone())?;
                    if plan.project_id != p.project_id || plan.revision != p.revision {
                        return Err("渲染计划不属于当前快照".into());
                    }
                    plan
                } else {
                    timeline::build(&p)?
                };
                let a = payload["startUs"].as_i64().unwrap_or(0);
                let b = payload["endUs"].as_i64().unwrap_or(plan.duration_us);
                let (f, audio) = render::filter(&p, &plan, (a, b), payload["preview"] == true)?;
                Ok(json!({"filter":f,"audio":audio,"plan":plan}))
            }
            "openOutput" => {
                let path = PathBuf::from(payload["path"].as_str().ok_or("缺少输出路径")?);
                if !path.is_file() {
                    return Err("OUTPUT_MISSING: 已导出文件已移走或删除，请重新导出".into());
                }
                let parent = path.parent().ok_or("无效目录")?;
                if !parent.is_dir() {
                    return Err("输出目录不存在".into());
                }
                std::process::Command::new("explorer.exe")
                    .arg("/select,")
                    .arg(&path)
                    .spawn()
                    .map_err(|e| e.to_string())?;
                Ok(json!(true))
            }
            "save" => {
                let p: Project = parse(payload["project"].clone())?;
                storage::save(&p, payload["path"].as_str().ok_or("缺少保存路径")?)?;
                Ok(json!(true))
            }
            "open" => {
                let (p, w) = storage::open(payload["path"].as_str().ok_or("缺少工程路径")?)?;
                Ok(json!({"project":p,"warnings":w}))
            }
            "releaseCache" => {
                if self.busy() {
                    return Err("请先停止当前任务".into());
                }
                let p: Project = parse(payload["project"].clone())?;
                let report = repair::read(&p)?;
                let mut keep = self.inner.lock().unwrap().pinned.clone();
                if !report.is_null() && report["state"] != "passed" {
                    let dir = storage::project_dir(&p.project_id)?;
                    let marker = dir.join(".repair-retain.json");
                    storage::atomic_json(
                        &marker,
                        &json!({"rule":repair::RULE,"expiresAfterDays":7}),
                    )?;
                    keep.push(marker.to_string_lossy().into());
                    for entry in fs::read_dir(&dir).map_err(|e| e.to_string())?.flatten() {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        if name.starts_with("smart-video-")
                            || name.starts_with("smart-piece-")
                            || name.starts_with("plan-")
                        {
                            keep.push(entry.path().to_string_lossy().into());
                        }
                    }
                }
                Ok(json!(storage::clear(&p.project_id, &keep)?))
            }
            "clearCache" => {
                let i = self.inner.lock().unwrap();
                if i.job
                    .as_ref()
                    .is_some_and(|j| j.status == "running" || j.status == "cancelling")
                {
                    return Err("请取消当前任务后清理缓存".into());
                }
                let count = storage::clear(
                    payload["projectId"].as_str().ok_or("缺少工程 ID")?,
                    &i.pinned,
                )?;
                Ok(json!(count))
            }
            "pin" => {
                let paths: Vec<String> = parse(payload)?;
                let cache = fs::canonicalize(storage::cache_root()).ok();
                let leases = paths
                    .iter()
                    .filter_map(|p| fs::canonicalize(p).ok())
                    .filter(|p| cache.as_ref().is_some_and(|c| p.starts_with(c)))
                    .map(|p| storage::active_file(&p))
                    .collect::<Result<Vec<_>, _>>()?;
                let mut i = self.inner.lock().unwrap();
                i.pinned = paths;
                i.pinned_leases = leases;
                Ok(json!(true))
            }
            "settings" => {
                let (s, warning) = storage::settings();
                let mut used = 0u64;
                if let Ok(dirs) = fs::read_dir(storage::cache_root()) {
                    for dir in dirs.flatten() {
                        if dir.path().join(".arkcut-cache-v1").is_file() {
                            if let Ok(files) = fs::read_dir(dir.path()) {
                                used += files
                                    .flatten()
                                    .filter_map(|f| f.metadata().ok())
                                    .filter(|m| m.is_file())
                                    .map(|m| m.len())
                                    .sum::<u64>();
                            }
                        }
                    }
                }
                Ok(json!({"settings":s,"warning":warning,"cacheBytes":used}))
            }
            "recoverCache" => {
                if self.busy() {
                    return Err("当前有任务，不能回收缓存".into());
                }
                Ok(json!(storage::recover()?))
            }
            "saveSettings" => {
                if self.busy() {
                    return Err("请等待或取消当前任务后更改缓存配置".into());
                }
                let s: storage::Settings = parse(payload)?;
                storage::save_settings(&s)?;
                self.inner.lock().unwrap().cache_limit = s.cache_limit;
                Ok(json!(true))
            }
            "cacheLimit" => {
                let value = payload.as_u64().ok_or("无效缓存上限")?;
                let mut s = storage::settings().0;
                s.cache_limit = value;
                storage::save_settings(&s)?;
                self.inner.lock().unwrap().cache_limit = value;
                Ok(json!(true))
            }
            "capabilities" => {
                let r = Runner::discover(Arc::new(|_, _| {}))?;
                let (data, _) = r.run(false, &args(&["-version"]), 1024 * 1024)?;
                Ok(
                    json!({"ffmpeg":String::from_utf8_lossy(&data).lines().next(),"software":["h264","hevc","vp9","av1"],"hardware":encoder::hardware(&r),"detection":"cn-mobile-20x9-v4-events / cn-pc-16x9-v5-events","cacheRoot":storage::cache_root(),"version":env!("CARGO_PKG_VERSION")}),
                )
            }
            "probe" | "import" | "music" | "musicBatch" | "analyze" | "preview"
            | "previewBatch" | "prepare" | "checkOutput" | "export" | "sourceProxy"
            | "thumbnail" | "relink" | "selectVideo" => self.start(op.to_string(), payload),
            _ => Err(format!("未知操作 {op}")),
        }
    }
    fn start(&self, op: String, mut payload: Value) -> Result<Value, String> {
        if (op == "probe" || op == "import") && payload["_projectId"].as_str().is_none() {
            payload["_projectId"] = json!(uuid::Uuid::new_v4().to_string());
        }
        let project = payload
            .get("project")
            .cloned()
            .map(parse::<Project>)
            .transpose()?;
        if op == "export" && payload["formatChangeAccepted"] != true {
            if let Some(reason) = project.as_ref().and_then(source_export::change_reason) {
                return Err(format!(
                    "FORMAT_CHANGE_CONFIRMATION: {reason}；请接受格式变更后再导出"
                ));
            }
        }
        let id = uuid::Uuid::new_v4().to_string();
        let engine = self.clone();
        let jobid = id.clone();
        let callback = Arc::new(move |stage: &str, us: Option<Us>| {
            engine.event(
                stage,
                "running",
                us,
                if [
                    "WARN:",
                    "ENCODER:",
                    "ENCODER_POLICY:",
                    "SMART_RENDER:",
                    "PREVIEW_BUDGET:",
                    "正在以较低内存占用生成预览",
                    "QUALITY_SAMPLE:",
                    "SIZE_REFERENCE:",
                    "OUTPUT_SIZE:",
                    "CACHE_",
                    "SMART_IO:",
                    "PROCESS_IO:",
                    "PROCESS_ERROR:",
                ]
                .iter()
                .any(|prefix| stage.starts_with(prefix))
                {
                    stage
                } else {
                    ""
                },
            )
        });
        let mut runner = Runner::discover(callback)?;
        if payload["repairAllowed"] == false {
            runner.repair_used.store(true, Ordering::Relaxed);
        }
        let engine = self.clone();
        runner.metrics = Arc::new(move |m| engine.metric(m));
        {
            let mut i = self.inner.lock().unwrap();
            if i.job
                .as_ref()
                .is_some_and(|j| j.status == "running" || j.status == "cancelling")
            {
                return Err("JOB_BUSY: 当前已有任务，请取消或稍后执行".into());
            }
            i.job = Some(Job {
                job_id: id.clone(),
                project_id: project
                    .as_ref()
                    .map(|p| p.project_id.clone())
                    .unwrap_or_else(|| payload["_projectId"].as_str().unwrap_or("").into()),
                revision: project.as_ref().map(|p| p.revision).unwrap_or(0),
                stage: op.clone(),
                operation: op.clone(),
                status: "running".into(),
                processed_us: None,
                cancel: runner.cancel.clone(),
                result: None,
                error: None,
                metric: Metric {
                    phase: op.clone(),
                    ..Metric::default()
                },
                started: std::time::Instant::now(),
                previews: vec![],
                imported_project: None,
                resource: runner.resource().summary(),
            });
        }
        self.event(&op, "running", None, &format!("开始 {op}"));
        let engine = self.clone();
        std::thread::spawn(move || {
            let recovery_project = project.clone();
            let observer = engine.clone();
            let observed_id = jobid.clone();
            let mut monitor = runner.resource().monitor(Arc::new(move |summary| {
                observer.resource_update(&observed_id, summary)
            }));
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                runner.prepare_stage(&op, 32 * resource::MIB, 8 * resource::MIB)?;
                let cache = storage::cache_root();
                fs::create_dir_all(&cache)
                    .map_err(|e| format!("RESOURCE_DISK: 缓存目录不可用：{e}"))?;
                let _cache_volume = runner.reserve_disk(&cache, 0, "任务缓存卷")?;
                engine.work(&runner, &op, &payload, project)
            }))
            .unwrap_or_else(|_| Err("INTERNAL: 后台任务异常，工程数据保留".into()));
            if op == "export" {
                if let (Some(p), Err(error)) = (&recovery_project, &result) {
                    if !error.starts_with("CANCELLED")
                        && repair::read(p).map_or(true, |v| v.is_null() || v["state"] == "passed")
                    {
                        if let Ok(report) =
                            repair::report(p, p, "", &[repair::failure(error)], &[], "needsReview")
                        {
                            let _ = repair::save(p, &report);
                        }
                    }
                }
            }
            runner.resource().refresh(false);
            monitor.stop();
            engine.resource_update(&jobid, runner.resource().summary());
            let status = match &result {
                Ok(_) => "completed",
                Err(s) if s.starts_with("CANCELLED") => "cancelled",
                _ => "failed",
            };
            if op == "import" && status != "completed" {
                if let Some(path) = payload["archivePath"].as_str() {
                    if let Err(error) = archive::mark(path, status) {
                        engine.event("archive", "failed", None, &error);
                    }
                }
            }
            engine.event(
                &op,
                status,
                None,
                &result
                    .as_ref()
                    .map(|_| format!("完成 {op}"))
                    .unwrap_or_else(|e| e.clone()),
            );
            let mut i = engine.inner.lock().unwrap();
            if let Some(j) = &mut i.job {
                if j.job_id == jobid {
                    j.status = status.into();
                    match result {
                        Ok(v) => j.result = Some(v),
                        Err(e) => {
                            finish_preview_items(&mut j.previews, status, &e, false);
                            j.error = Some(e);
                        }
                    }
                }
            }
        });
        Ok(json!({"jobId":id}))
    }
    fn work(
        &self,
        r: &Runner,
        op: &str,
        payload: &Value,
        project: Option<Project>,
    ) -> Result<Value, String> {
        if op == "musicBatch" {
            let paths = payload["paths"].as_array().ok_or("缺少音乐列表")?;
            let mut list = vec![];
            for (i, path) in paths.iter().enumerate() {
                r.check()?;
                r.measure(
                    "musicImport",
                    i as i64,
                    Some(paths.len() as i64),
                    "tracks",
                    &format!("导入音乐 · {} / {}", i + 1, paths.len()),
                );
                let media = media::probe(r, path.as_str().ok_or("无效音乐路径")?, false)?;
                list.push(Music {
                    id: uuid::Uuid::new_v4().to_string(),
                    media,
                });
                r.measure(
                    "musicImport",
                    (i + 1) as i64,
                    Some(paths.len() as i64),
                    "tracks",
                    "音乐导入完成",
                );
            }
            return Ok(json!(list));
        }
        if op == "probe" || op == "import" || op == "music" {
            let path = payload["path"].as_str().ok_or("缺少素材路径")?;
            let m = media::probe(r, path, op != "music")?;
            return if op == "probe" || op == "import" {
                let mut p = Project::new(m)?;
                p.project_id = payload["_projectId"]
                    .as_str()
                    .unwrap_or(&p.project_id)
                    .to_string();
                if op == "import" {
                    if let Some(path) = payload["archivePath"].as_str() {
                        p.name = Some(
                            archive::recent()
                                .unwrap_or_default()
                                .iter()
                                .find(|e| e.project_id == p.project_id)
                                .map(|e| e.name.clone())
                                .unwrap_or_else(|| {
                                    std::path::Path::new(&p.source.fingerprint.path)
                                        .file_stem()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                        .into()
                                }),
                        );
                        archive::save(&p, path, "importing")?;
                    }
                    if let Some(j) = &mut self.inner.lock().unwrap().job {
                        j.imported_project = Some(json!(p));
                    }
                    let result = match analyze_budgeted(r, &p) { Ok(result)=>result,Err(error)=>{p.detection.status=if error.starts_with("CANCELLED"){"cancelled"}else{"failed"}.into();if let Some(path)=payload["archivePath"].as_str(){let _=archive::save(&p,path,&p.detection.status);}return Err(error)} };
                    r.check()?;
                    p.pauses = result.pauses;
                    p.detection.geometry = result.geometry;
                    p.detection.status = result.status;
                    p.detection.region = result.region;
                    p.detection.profile = Some(result.version);
                    p.detection.version = detector::ALGORITHM_VERSION.into();
                    p.revision += 1;
                    if let Some(path) = payload["archivePath"].as_str() {
                        r.measure("projectSave", 0, Some(1), "project", "保存项目");
                        archive::save(&p, path, "ready")?;
                        r.measure("projectSave", 1, Some(1), "project", "识别结果已自动存档");
                    }
                    for warning in result.warnings {
                        self.event("analysis", "running", None, &warning);
                    }
                }
                Ok(json!(p))
            } else {
                Ok(json!(Music {
                    id: uuid::Uuid::new_v4().to_string(),
                    media: m
                }))
            };
        }
        let mut p = project.ok_or("缺少工程快照")?;
        if op == "selectVideo" {
            let index = payload["index"].as_u64().ok_or("缺少视频流序号")? as u32;
            let m = media::probe_selected(r, &p.source.fingerprint.path, true, Some(index))?;
            p.video_index = index;
            p.source = m;
            p.pauses.clear();
            p.revision += 1;
            return Ok(json!(p));
        }
        if op == "relink" {
            let path = payload["path"].as_str().ok_or("缺少重新定位路径")?;
            let m = media::probe(r, path, true)?;
            if m.duration_us != p.source.duration_us
                || m.fingerprint.sample_hash != p.source.fingerprint.sample_hash
                || m.fingerprint.size != p.source.fingerprint.size
            {
                return Err("RELINK_MISMATCH: 所选文件内容或时长不同，不替换原引用".into());
            }
            p.source = m;
            p.revision += 1;
            return Ok(json!(p));
        }
        media::verify_media(&p.source)?;
        for m in &p.music {
            media::verify_media(&m.media)?;
        }
        if op == "analyze" {
            return Ok(json!(analyze_budgeted(r, &p)?));
        }
        let dir = storage::project_dir(&p.project_id)?;
        if op == "thumbnail" {
            let t = payload["timeUs"].as_i64().ok_or("缺少缩略图时间")?;
            let path = dir.join(format!("frame-{t}.png"));
            media::frame_png(r, &p, t, &path)?;
            return Ok(json!({"path":path}));
        }
        let source_proxy = op == "sourceProxy";
        if source_proxy {
            p.pauses.clear();
            p.music.clear();
            p.mix.tails = false;
            p.mix.game_gain = 1.;
        }
        if op == "previewBatch" {
            let base = timeline::build(&p)?;
            return self.batch(r, &p, &base, payload);
        }
        if op == "export" {
            if let Some(reason) = source_export::change_reason(&p) {
                (r.progress)(&format!("WARN: {reason}"), None);
            }
            source_export::lock(&mut p)?;
        }
        let mut plan = self.prepare_plan(r, &p)?;
        if op == "prepare" {
            return Ok(json!(plan));
        }
        if op == "checkOutput" {
            let path = PathBuf::from(payload["path"].as_str().ok_or("缺少校验路径")?);
            let range = (
                payload["startUs"].as_i64().unwrap_or(0),
                payload["endUs"].as_i64().unwrap_or(plan.duration_us),
            );
            prepare_detection(r, &p, "outputBoundaryAudit")?;
            return detector::check_output(r, &p, &plan, &path, range);
        }
        if op == "previewBatch" {
            return self.batch(r, &p, &plan, payload);
        }
        let range = if op == "preview" && payload["full"] != true {
            let at = payload["positionUs"]
                .as_i64()
                .unwrap_or_else(|| plan.removed.first().map(|c| c.output_us).unwrap_or(0));
            let half = payload["halfSeconds"].as_i64().unwrap_or(3).clamp(3, 6) * SECOND;
            ((at - half).max(0), (at + half).min(plan.duration_us))
        } else {
            (0, plan.duration_us)
        };
        let target = if op == "export" {
            Some(PathBuf::from(
                payload["path"].as_str().ok_or("缺少导出路径")?,
            ))
        } else {
            None
        };
        self.render_item(
            r,
            &mut p,
            &mut plan,
            range,
            op != "export",
            target.as_deref(),
            false,
        )
    }
    pub fn busy(&self) -> bool {
        self.inner
            .lock()
            .unwrap()
            .job
            .as_ref()
            .is_some_and(|j| j.status == "running" || j.status == "cancelling")
    }
}

impl Engine {
    fn prepare_plan(&self, r: &Runner, p: &Project) -> Result<Plan, String> {
        let dir = storage::project_dir(&p.project_id)?;
        let mut identities = vec![repair::identity(r, &p.source.fingerprint.path)?];
        for m in &p.music {
            identities.push(repair::identity(r, &m.media.fingerprint.path)?);
        }
        let key = storage::hash(&json!([prepared_plan_key(p)?, identities]))?;
        let path = dir.join(format!("plan-{key}.json"));
        if let Ok(data) = fs::read(&path) {
            if let Ok(mut plan) = serde_json::from_slice::<Plan>(&data) {
                if plan.version == PLAN_VERSION {
                    // Output names, export settings and save revisions do not
                    // change the edit or audio processing. Rebind cached work
                    // to this snapshot so consumers still get the current ID.
                    plan.project_id = p.project_id.clone();
                    plan.revision = p.revision;
                    return Ok(plan);
                }
            }
        }
        let mut plan = timeline::build(p)?;
        if plan.duration_us <= 0 {
            return Err("全部内容已删除，请恢复至少一段".into());
        }
        audio::enrich(r, p, &mut plan)?;
        storage::atomic(
            &path,
            &serde_json::to_vec(&plan).map_err(|e| e.to_string())?,
        )?;
        Ok(plan)
    }
    fn render_item(
        &self,
        r: &Runner,
        p: &mut Project,
        plan: &mut Plan,
        range: (Us, Us),
        preview: bool,
        output: Option<&std::path::Path>,
        defer_revision: bool,
    ) -> Result<Value, String> {
        let dir = storage::project_dir(&p.project_id)?;
        let before = p.clone();
        let source_sha = repair::identity(r, &p.source.fingerprint.path)?;
        let mut identities = vec![source_sha.clone()];
        for m in &p.music {
            identities.push(repair::identity(r, &m.media.fingerprint.path)?);
        }
        let local_keep:Vec<_>=plan.keep.iter().filter_map(|k|{let a=range.0.max(k.output_us);let b=range.1.min(k.output_us+k.end_us-k.start_us);(b>a).then_some(json!({"sourceStart":k.start_us+a-k.output_us,"sourceEnd":k.start_us+b-k.output_us,"outputStart":a}))}).collect();
        let local_tails: Vec<_> = plan
            .tails
            .iter()
            .filter(|t| t.output_us < range.1 && t.output_us + t.duration_us > range.0)
            .collect();
        let local_music:Vec<_>=plan.music.iter().filter(|m|m.output_us<range.1&&m.output_us+m.duration_us>range.0).map(|m|json!({"placement":m,"media":p.music[m.track].media.fingerprint,"gain":plan.loudness_gain[m.track]})).collect();
        let music_end = plan
            .music
            .last()
            .map(|m| m.output_us + m.duration_us)
            .unwrap_or(0)
            .min(plan.duration_us);
        let fade_end = if music_end - 2 * SECOND < range.1 {
            Some(music_end)
        } else {
            None
        };
        let key = storage::hash(
            &json!({"render":"pts-int-events-envelope-9-hq-h264-p3-la12","previewVersion":if preview{"fast-960-cq26-p1-pts-v5"}else{"native-raster-pts-hq-h264-aac-v4-p3-la12"},"batchRevisionHandoff":defer_revision,"source":p.source.fingerprint,"streams":[Some(p.video_index),p.audio_index],"keep":local_keep,"tails":local_tails,"music":local_music,"musicEnd":fade_end,"gameGain":p.mix.game_gain,"musicGain":p.mix.music_gain,"range":range,"proxy":preview,"export":if preview{None}else{Some(&p.export)}}),
        )?;
        let related: Vec<_> = p
            .pauses
            .iter()
            .filter(|v| {
                plan.removed.iter().any(|c| {
                    c.output_us >= range.0
                        && c.output_us < range.1
                        && v.start_us <= c.end_us
                        && v.end_us >= c.start_us
                })
            })
            .collect();
        let key = storage::hash(&json!([
            key,
            identities,
            repair::RULE,
            &p.detection,
            related
        ]))?;
        let final_path = if preview {
            dir.join(format!("{key}.mp4"))
        } else {
            let target = output
                .ok_or("缺少导出路径")?
                .with_extension(render::container(p));
            storage::protect_inputs(p, &target)?;
            storage::unique_output(&target)?
        };
        let audit_path = dir.join(format!("audit-{key}.json"));
        let cached_audit = fs::metadata(&audit_path)
            .ok()
            .filter(|m| m.len() < 8 * 1024 * 1024)
            .and_then(|_| fs::read(&audit_path).ok())
            .filter(|b| b.len() < 8 * 1024 * 1024)
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        let cache_valid = if preview && final_path.exists() {
            if let Some(record) = &cached_audit {
                let (bytes, sha) = export_record::digest_checked(&final_path, || r.check())?;
                let mut proof = record.clone();
                if let Some(object) = proof.as_object_mut() {
                    object.remove("integrity");
                }
                record["bytes"] == bytes
                    && record["sha256"] == sha
                    && record["integrity"] == storage::hash(&proof)?
                    && record["rule"] == repair::RULE
                    && record["revised"] != true
                    && record["audit"]["needsReview"] != true
                    && record["audit"]["residualFrames"] == 0
                    && record["audit"]["candidateFrames"] == 0
            } else {
                false
            }
        } else {
            false
        };
        if cache_valid {
            let _ = fs::File::open(&final_path)
                .and_then(|f| f.set_modified(std::time::SystemTime::now()));
            self.event("cache", "running", None, "预览缓存命中，未启动编码");
            let audit = cached_audit.map(|v| v["audit"].clone());
            return Ok(
                json!({"path":final_path,"plan":plan,"projectId":p.project_id,"revision":p.revision,"startUs":range.0,"endUs":range.1,"mediaStartUs":range.0,"mediaEndUs":range.1,"cached":true,"quickPreview":preview,"audit":audit}),
            );
        }
        if preview {
            let _ = fs::remove_file(&final_path);
            let _ = fs::remove_file(&audit_path);
        }
        let estimate = (range.1 - range.0).max(0) as u64 * 500000 / 1_000_000;
        if preview {
            let i = self.inner.lock().unwrap();
            storage::quota(
                &dir,
                estimate.max(1024 * 1024),
                storage::settings().0.cache_limit,
                &i.pinned,
            )?;
        }
        let owned_temp = storage::temp_file(final_path.parent().ok_or("无效输出路径")?, "render")?;
        let temp = owned_temp.path().to_path_buf();
        let mut audit = Value::Null;
        let original_revision = p.revision;
        let mut effective_range = range;
        let mut completed_export = None;
        let mut issues = vec![];
        let result: Result<(), String> = (|| {
            for attempt in 0..2 {
                issues.clear();
                *r.validated_audit.lock().unwrap() = None;
                render::render(r, p, plan, effective_range, preview, &temp)?;
                let complete_audit = r.validated_audit.lock().unwrap().take();
                if let Some((path, result)) = complete_audit.filter(|(path, _)| path == &temp) {
                    let _ = path;
                    audit = result;
                } else if p
                    .pauses
                    .iter()
                    .any(|x| x.enabled) && detector::can_check(p)
                {
                    prepare_detection(r, p, "outputBoundaryAudit")?;
                    audit = detector::check_output(r, p, plan, &temp, effective_range)?;
                } else {
                    audit = json!({"residualFrames":0,"candidateFrames":0,"checks":[],"status":"skipped","scope":"no applicable pause audit","detectionStatus":p.detection.status});
                }
                issues = repair::audit_issues(p, plan, &audit);
                if audit["residualFrames"].as_u64().unwrap_or(0)
                    + audit["transitionResidualFrames"].as_u64().unwrap_or(0) == 0 {
                    if !preview && !issues.is_empty() {
                        let previous = repair::read(p)?;
                        let confirmed = previous["sourceSha256"] == source_sha
                            && previous["signature"] == export_record::signature(p)?
                            && previous["confirmedIssues"] == storage::hash(&issues)?;
                        if !confirmed {
                            return Err("PAUSE_REVIEW_REQUIRED: 剪辑点有疑似画面，请查看问题后确认或调整边界".into());
                        }
                        for issue in &mut issues {
                            issue["status"] = json!("userConfirmed");
                        }
                    }
                    break;
                }
                if attempt == 1 || r.repair_used.swap(true, Ordering::Relaxed) {
                    return Err(
                        "PAUSE_REVIEW_REQUIRED: 局部复检仍有残帧，请查看问题并调整边界".into(),
                    );
                }
                r.measure(
                    "sourceBoundaryReview",
                    0,
                    None,
                    "state",
                    "检测到残帧，重新复核源事件边界并渲染",
                );
                prepare_detection(r, p, "sourceBoundaryReview")?;
                let corrected = detector::repair_boundaries(r, p, &issues)?;
                if corrected.revision == p.revision {
                    return Err(
                        "PAUSE_REVIEW_REQUIRED: 边界由人工指定或证据不足，请查看问题并调整时间戳"
                            .into(),
                    );
                }
                *p = corrected;
                repair::save(
                    p,
                    &repair::report(
                        &before,
                        p,
                        &source_sha,
                        &issues,
                        &boundary_changes(&before, p),
                        "repairing",
                    )?,
                )?;
                // Batch ranges and cache keys belong to the original snapshot. Hand
                // the new project back before encoding or caching under that old key.
                if defer_revision {
                    return Err("PLAN_REVISED".into());
                }
                *plan = self.prepare_plan(r, p)?;
                effective_range.1 = if !preview {
                    plan.duration_us
                } else {
                    effective_range.1.min(plan.duration_us)
                };
                r.measure(
                    "repairRecheck",
                    0,
                    None,
                    "state",
                    "正在重新检查已修正的剪辑点",
                );
                let _ = fs::remove_file(&temp);
            }
            r.check()?;
            if preview {
                let i = self.inner.lock().unwrap();
                storage::quota(&dir, 0, storage::settings().0.cache_limit, &i.pinned)?;
            }
            r.measure("commit", 0, Some(1), "file", "提交已验证文件");
            if !preview {
                let (bytes, sha256) = export_record::digest_checked(&temp, || r.check())?;
                completed_export = Some(export_record::ExportRecord {
                    signature: export_record::signature(p)?,
                    path: final_path.to_string_lossy().into(),
                    bytes,
                    sha256,
                });
            }
            r.check()?;
            if final_path.exists() {
                return Err("OUTPUT_EXISTS: 目标在渲染期间被创建，请另存新名".into());
            }
            let (bytes, sha) = if let Some(record) = &completed_export {
                (record.bytes, record.sha256.clone())
            } else {
                export_record::digest_checked(&temp, || r.check())?
            };
            let mut record = json!({"rule":repair::RULE,"bytes":bytes,"sha256":sha,"audit":audit,"revised":before.revision!=p.revision});
            record["integrity"] = json!(storage::hash(&record)?);
            storage::atomic_json(&audit_path, &record)?;
            fs::rename(&temp, &final_path).map_err(|e| format!("OUTPUT_COMMIT: {e}"))?;
            r.measure("commit", 1, Some(1), "file", "文件已提交");
            Ok(())
        })();
        if let Err(e) = result {
            let _ = fs::remove_file(&temp);
            if e != "PLAN_REVISED" && !e.starts_with("CANCELLED") {
                if issues.is_empty() {
                    issues.push(repair::failure(&e));
                }
                if let Ok(report) = repair::report(
                    &before,
                    p,
                    &source_sha,
                    &issues,
                    &boundary_changes(&before, p),
                    "needsReview",
                ) {
                    let _ = repair::save(p, &report);
                }
            }
            return Err(e);
        }
        if !preview || !issues.is_empty() || before.revision != p.revision {
            if let Ok(report) = repair::report(
                &before,
                p,
                &source_sha,
                &issues,
                &boundary_changes(&before, p),
                if issues.iter().all(|v| v["status"] == "userConfirmed") {
                    "passed"
                } else {
                    "needsReview"
                },
            ) {
                let _ = repair::save(p, &report);
            }
        }
        Ok(
            json!({"path":final_path,"plan":plan,"projectId":p.project_id,"revision":p.revision,"startUs":effective_range.0,"endUs":effective_range.1,"mediaStartUs":effective_range.0,"mediaEndUs":effective_range.1,"cached":false,"quickPreview":preview,"audit":audit,"completedExport":completed_export,"effectiveProject":if original_revision!=p.revision{Some(p)}else{None}}),
        )
    }
    fn publish_previews(&self, items: &[Value]) {
        if let Some(j) = &mut self.inner.lock().unwrap().job {
            j.previews = items.to_vec();
        }
    }
    fn batch(
        &self,
        r: &Runner,
        p: &Project,
        plan: &Plan,
        payload: &Value,
    ) -> Result<Value, String> {
        let half = payload["halfSeconds"].as_i64().unwrap_or(3).clamp(3, 6) * SECOND;
        let selected = payload["selectedId"].as_str().unwrap_or("");
        let mut items = vec![];
        for (i, c) in plan.removed.iter().enumerate() {
            let ids: Vec<_> = p
                .pauses
                .iter()
                .filter(|x| x.enabled && x.start_us >= c.start_us && x.end_us <= c.end_us)
                .map(|x| x.id.clone())
                .collect();
            items.push(json!({"id":format!("cut-{}-{}",c.start_us,c.end_us),"pauseIds":ids,"index":i,"cutUs":c.output_us,"sourceStartUs":c.start_us,"sourceEndUs":c.end_us,"startUs":(c.output_us-half).max(0),"endUs":(c.output_us+half).min(plan.duration_us),"projectId":p.project_id,"revision":p.revision,"status":"queued","path":null,"cached":false}));
        }
        if items.is_empty() {
            items.push(json!({"id":"opening","pauseIds":[],"index":0,"cutUs":0,"startUs":0,"endUs":(2*half).min(plan.duration_us),"projectId":p.project_id,"revision":p.revision,"status":"queued","path":null,"cached":false}));
        }
        self.publish_previews(&items);
        let begun = self
            .inner
            .lock()
            .unwrap()
            .job
            .as_ref()
            .map(|j| j.started)
            .unwrap_or_else(std::time::Instant::now);
        let mut ready = 0;
        let mut first_ms = None;
        let prepared = self.prepare_plan(r, p)?;
        let plan = &prepared;
        let mut rendering = p.clone();
        let mut snapshot = plan.clone();
        let mut limits = None;
        let mut render_groups = 0;
        loop {
            // Re-sample between completed media processes. Available physical
            // memory already accounts for the app/player and other applications.
            r.resource().refresh(false);
            if let Err(e) = r.check() {
                let status = if e.starts_with("CANCELLED") { "cancelled" } else { "failed" };
                finish_preview_items(&mut items, status, &e, false);
                self.publish_previews(&items);
                return Err(e);
            }
            let next_limits = preview_group_limits(&r.resource().summary().sample, limits);
            limits = Some(next_limits);
            let Some((a, b, indexes)) = preview_groups(&items, next_limits, selected).into_iter().next() else { break; };
            render_groups += 1;
            for &i in &indexes {
                items[i]["status"] = json!("rendering");
            }
            self.publish_previews(&items);
            r.measure(
                "batchPreview",
                ready,
                Some(items.len() as i64),
                "clips",
                &format!("预览准备 · {ready} / {}", items.len()),
            );
            let result =
                self.render_item(r, &mut rendering, &mut snapshot, (a, b), true, None, true);
            if rendering.revision != p.revision {
                let revised_plan = timeline::build(&rendering)?;
                let reason = "检测到暂停残帧，已重新复核并校正源区间边界";
                finish_preview_items(&mut items, "cancelled", reason, true);
                self.publish_previews(&items);
                self.event("sourceBoundaryRevision", "completed", None, reason);
                return Ok(
                    json!({"outcome":"revised","previews":items,"plan":revised_plan,
                    "effectiveProject":rendering,"correction":{"reason":"pause-residual",
                    "message":reason,"baseRevision":p.revision,"changes":boundary_changes(p,&rendering)},
                    "interrupted":r.check().is_err(),"ready":0,"total":items.len(),
                    "renderGroups":render_groups,"firstReadyMs":first_ms,
                    "elapsedMs":begun.elapsed().as_millis(),"projectId":p.project_id,"revision":p.revision}),
                );
            }
            match result {
                Ok(result) => {
                    // Full per-frame evidence is already integrity-checked and
                    // stored beside the cached clip. Repeating it in every item
                    // creates quadratic job snapshots (223 MB for 38 cuts), and
                    // every status poll clones and serializes that data again.
                    let audit = preview_audit_summary(&result["audit"]);
                    for &i in &indexes {
                        items[i]["status"] = json!("ready");
                        for field in ["path", "mediaStartUs", "mediaEndUs", "cached", "quickPreview"] {
                            items[i][field] = result[field].clone();
                        }
                        items[i]["audit"] = audit.clone();
                        items[i]["readyMs"] = json!(begun.elapsed().as_millis());
                        ready += 1;
                    }
                    if first_ms.is_none() {
                        first_ms = Some(begun.elapsed().as_millis());
                    }
                }
                Err(e) => {
                    if let Err(cancelled) = r.check() {
                        let status = if cancelled.starts_with("CANCELLED") { "cancelled" } else { "failed" };
                        finish_preview_items(&mut items, status, &cancelled, false);
                        self.publish_previews(&items);
                        return Err(cancelled);
                    }
                    for &i in &indexes {
                        items[i]["status"] = json!("failed");
                        items[i]["error"] = json!(e);
                    }
                }
            }
            self.publish_previews(&items);
            r.measure(
                "batchPreview",
                ready,
                Some(items.len() as i64),
                "clips",
                "短预览逐段就绪",
            );
        }
        Ok(
            json!({"outcome":"ready","previews":items,"plan":plan,"ready":ready,"total":items.len(),"renderGroups":render_groups,"firstReadyMs":first_ms,"elapsedMs":begun.elapsed().as_millis(),"projectId":p.project_id,"revision":p.revision}),
        )
    }
}

fn prepared_plan_key(p: &Project) -> Result<String, String> {
    storage::hash(&json!({
        "projectId":p.project_id,"source":p.source,"streams":[Some(p.video_index),p.audio_index],
        "pauses":p.pauses,"music":p.music,"mix":p.mix,"detection":p.detection,
        "planVersion":PLAN_VERSION,"audio":"bridge-envelope-v3-ebur128"
    }))
}

fn preview_group_limits(sample: &resource::Sample, previous: Option<(usize, Us)>) -> (usize, Us) {
    let small = (3, 12 * SECOND);
    let large = (24, 30 * SECOND);
    let Some(memory) = sample.memory.as_ref().filter(|_| sample.reliable) else { return small; };
    let headroom = memory.available.saturating_sub(resource::critical_reserve(memory.total))
        .min(memory.commit_available.saturating_sub(512 * resource::MIB));
    // Hysteresis avoids switching on every small fluctuation. These are live
    // headroom thresholds, not total-RAM or hardware tiers.
    let threshold = if previous == Some(large) {1536 * resource::MIB} else {2 * resource::GIB};
    if headroom < threshold {small} else {large}
}

fn preview_groups(items: &[Value], (max_cuts, max_duration): (usize, Us), selected: &str) -> Vec<(Us, Us, Vec<usize>)> {
    let mut groups: Vec<(Us, Us, Vec<usize>)> = vec![];
    for (i, item) in items.iter().enumerate().filter(|(_, item)|item["status"]=="queued") {
        let a=item["startUs"].as_i64().unwrap(); let b=item["endUs"].as_i64().unwrap();
        if let Some(last)=groups.last_mut() {
            if a<=last.1 && last.2.len()<max_cuts && b.max(last.1)-last.0<=max_duration {
                last.1=last.1.max(b);last.2.push(i);continue;
            }
        }
        groups.push((a,b,vec![i]));
    }
    if let Some(at)=groups.iter().position(|g|g.2.iter().any(|&i|items[i]["id"]==selected
        || items[i]["pauseIds"].as_array().is_some_and(|ids|ids.iter().any(|id|id.as_str()==Some(selected))))) {
        let first=groups.remove(at);groups.insert(0,first);
    }
    groups
}

fn preview_audit_summary(audit: &Value) -> Value {
    if !audit.is_object() {return Value::Null;}
    let mut summary = serde_json::Map::new();
    for key in ["profile", "residualFrames", "candidateFrames", "needsReview", "status", "scope", "detectionStatus"] {
        if let Some(value) = audit.get(key) {summary.insert(key.into(), value.clone());}
    }
    let checks = audit["checks"].as_array();
    summary.insert("checkedCuts".into(), json!(checks.map_or(0, |v|v.len())));
    summary.insert("checkedFrames".into(), json!(checks.map_or(0, |v|v.iter().map(|c|c["frames"].as_array().map_or(0, |f|f.len())).sum::<usize>())));
    Value::Object(summary)
}

fn finish_preview_items(items: &mut [Value], status: &str, reason: &str, invalidate_ready: bool) {
    for item in items {
        if item["status"] == "queued"
            || item["status"] == "rendering"
            || invalidate_ready && item["status"] == "ready"
        {
            item["status"] = json!(status);
            item["error"] = json!(reason);
            if invalidate_ready {
                item["path"] = Value::Null;
            }
        }
    }
}

fn boundary_changes(before: &Project, after: &Project) -> Vec<Value> {
    let mut ids: Vec<_> = before
        .pauses
        .iter()
        .chain(after.pauses.iter())
        .map(|p| p.id.clone())
        .collect();
    ids.sort();
    ids.dedup();
    ids.into_iter()
        .filter_map(|id| {
            let a = before.pauses.iter().find(|p| p.id == id);
            let b = after.pauses.iter().find(|p| p.id == id);
            let bounds = |p: &Pause| json!({"startUs":p.start_us,"endUs":p.end_us});
            if a.map(|p| (p.start_us, p.end_us)) == b.map(|p| (p.start_us, p.end_us)) {
                None
            } else {
                Some(json!({"pauseId":id,"before":a.map(bounds),"after":b.map(bounds)}))
            }
        })
        .collect()
}

#[cfg(test)]
mod preview_completion_tests {
    use super::*;
    #[test]
    fn prepared_audio_cache_reuses_output_edits_but_invalidates_media_edits() {
        let media: Media = serde_json::from_value(json!({
            "fingerprint":{"path":"fixture.mp4","size":100,"modifiedMs":1,"sampleHash":"test"},
            "durationUs":1000000,"format":"mp4","streams":[],"videoIndex":0,
            "audioIndex":1,"originUs":0,"framePts":[0,500000],"hdr":false,"warnings":[]
        }))
        .unwrap();
        let p = Project::new(media).unwrap();
        let key = prepared_plan_key(&p).unwrap();
        let mut renamed = p.clone();
        renamed.name = Some("renamed".into());
        renamed.output_path = Some("other.mp4".into());
        renamed.revision += 1;
        renamed.export.quality = "lossless".into();
        renamed.export.preview_edge = 960;
        assert_eq!(key, prepared_plan_key(&renamed).unwrap());
        for change in 0..4 {
            let mut changed = p.clone();
            match change {
                0 => changed.mix.game_gain = 0.5,
                1 => changed.audio_index = None,
                2 => changed.source.frame_pts.push(900000),
                _ => changed.detection.activity_db = -35.,
            }
            assert_ne!(key, prepared_plan_key(&changed).unwrap());
        }
    }
    #[test]
    fn every_pending_item_becomes_terminal_and_revisions_invalidate_old_ready_media() {
        let mut items = vec![
            json!({"status":"ready","path":"old.mp4"}),
            json!({"status":"rendering"}),
            json!({"status":"queued"}),
            json!({"status":"failed"}),
        ];
        finish_preview_items(&mut items, "failed", "prepare failed", false);
        assert_eq!(items[0]["status"], "ready");
        assert!(items
            .iter()
            .all(|x| x["status"] != "queued" && x["status"] != "rendering"));
        finish_preview_items(&mut items, "cancelled", "revised", true);
        assert_eq!(items[0]["status"], "cancelled");
        assert!(items[0]["path"].is_null());
    }
    #[test]
    fn preview_snapshots_keep_verdicts_without_repeating_per_frame_evidence() {
        let audit=json!({"profile":"test","residualFrames":2,"candidateFrames":1,"needsReview":true,
            "checks":[{"frames":[{"state":"confirmed","evidence":["x".repeat(4096)]},{"state":"candidate"}]}]});
        let summary=preview_audit_summary(&audit);
        assert_eq!(summary["residualFrames"],2);assert_eq!(summary["candidateFrames"],1);
        assert_eq!(summary["needsReview"],true);assert_eq!(summary["checkedFrames"],2);assert_eq!(summary["checkedCuts"],1);
        assert!(summary.get("checks").is_none());assert!(serde_json::to_vec(&summary).unwrap().len()<256);
        assert_eq!(audit["checks"][0]["frames"][0]["evidence"][0].as_str().unwrap().len(),4096);
        assert_eq!(preview_audit_summary(&Value::Null),Value::Null);
    }
    #[test]
    fn preview_batches_follow_live_headroom_and_keep_all_pending_items() {
        let small=(3,12*SECOND); let large=(24,30*SECOND);
        let mut sample=resource::Sample { reliable:true, memory:Some(resource::Memory {
            total:4*resource::GIB, available:3584*resource::MIB, commit_available:4*resource::GIB,
        }), ..Default::default() };
        assert_eq!(preview_group_limits(&sample,None),large);
        // The same free resources permit the same batch on a larger machine.
        sample.memory.as_mut().unwrap().total=16*resource::GIB;
        assert_eq!(preview_group_limits(&sample,None),large);
        let reserve=resource::critical_reserve(16*resource::GIB);
        sample.memory.as_mut().unwrap().available=reserve+1792*resource::MIB;
        assert_eq!(preview_group_limits(&sample,None),small);
        assert_eq!(preview_group_limits(&sample,Some(large)),large);
        sample.memory.as_mut().unwrap().available=reserve+1024*resource::MIB;
        assert_eq!(preview_group_limits(&sample,Some(large)),small);
        sample.memory.as_mut().unwrap().available=8*resource::GIB;
        sample.memory.as_mut().unwrap().commit_available=resource::GIB;
        assert_eq!(preview_group_limits(&sample,None),small);
        sample.reliable=false;
        assert_eq!(preview_group_limits(&sample,None),small);

        let mut items:Vec<_>=(0..10).map(|i|json!({"id":format!("c{i}"),"pauseIds":[format!("p{i}")],
            "status":"queued","startUs":i*SECOND,"endUs":(i+6)*SECOND})).collect();
        items[0]["status"]=json!("ready");items[4]["status"]=json!("failed");
        let groups=preview_groups(&items,small,"p9");
        assert!(groups[0].2.contains(&9));
        assert!(groups.iter().all(|(a,b,ids)|b-a<=12*SECOND&&ids.len()<=3));
        let mut coverage:Vec<_>=groups.iter().flat_map(|g|g.2.iter().copied()).collect();coverage.sort_unstable();
        assert_eq!(coverage,vec![1,2,3,5,6,7,8,9]);
        for &i in &groups[0].2 {items[i]["status"]=json!("ready");}
        let remaining=preview_groups(&items,large,"p9");
        let remaining_count:usize=remaining.iter().map(|g|g.2.len()).sum();
        assert_eq!(remaining_count+groups[0].2.len(),8);
    }
}
