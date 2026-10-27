use super::{platform, protocol::*, transaction, CONTROL, VERSION};
use serde::Serialize;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub current_version: String,
    pub status: String,
    pub version: Option<String>,
    pub notes: String,
    pub downloaded: u64,
    pub total: u64,
    pub error: Option<String>,
    pub configured: bool,
}
#[derive(Clone)]
pub struct Updater {
    pub gate: Arc<Mutex<()>>,
    root: PathBuf,
    config: Config,
    state: Arc<Mutex<Status>>,
    offer: Arc<Mutex<Option<(Envelope, Release)>>>,
    cancel: Arc<AtomicBool>,
}
impl Updater {
    pub fn new(root: PathBuf, config: Config) -> Self {
        let this = Self {
            gate: Arc::new(Mutex::new(())),
            root,
            config,
            state: Arc::new(Mutex::new(Status {
                current_version: VERSION.into(),
                status: "idle".into(),
                version: None,
                notes: String::new(),
                downloaded: 0,
                total: 0,
                error: None,
                configured: true,
            })),
            offer: Arc::new(Mutex::new(None)),
            cancel: Arc::new(AtomicBool::new(false)),
        };
        if let Ok(bytes) = read_bounded(&this.root.join(CONTROL).join("offer.json"), MAX_INDEX) {
            if let Ok((e, r)) = verify(&bytes, &this.config, Some(VERSION)) {
                let cached = this.root.join(CONTROL).join("archive.zip");
                let ready = fs::metadata(cached).is_ok_and(|m| m.len() == r.asset.bytes);
                this.offered(e, r, ready);
            }
        }
        if let Ok(value) =
            read_json::<serde_json::Value>(&this.root.join(CONTROL).join("last-result.json"), 16384)
        {
            if value["success"] == false {
                this.state.lock().unwrap().error = value["message"].as_str().map(str::to_owned);
            }
        }
        this
    }
    pub fn status(&self) -> Status {
        self.state.lock().unwrap().clone()
    }
    pub fn report_action_error(&self, error: &str) {
        self.state.lock().unwrap().error = Some(error.to_owned());
    }
    fn offered(&self, e: Envelope, r: Release, ready: bool) {
        let mut s = self.state.lock().unwrap();
        s.status = if ready { "ready" } else { "available" }.into();
        s.version = Some(r.version.clone());
        s.notes = r.notes.clone();
        s.total = r.asset.bytes;
        s.downloaded = if ready { r.asset.bytes } else { 0 };
        s.error = None;
        *self.offer.lock().unwrap() = Some((e, r));
    }
    fn error(&self, e: String) {
        let mut s = self.state.lock().unwrap();
        s.status = if self.cancel.load(Ordering::Relaxed) {
            "cancelled"
        } else {
            "error"
        }
        .into();
        s.error = Some(e);
    }
    fn running(&self) -> bool {
        ["checking", "downloading", "cancelling", "installing"]
            .contains(&self.status().status.as_str())
    }
    fn client(&self, download: bool) -> Result<reqwest::Client> {
        let hosts = self.config.redirect_hosts.clone();
        #[allow(unused_mut)]
        let mut builder = reqwest::Client::builder()
            .https_only(true)
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(if download { 1800 } else { 15 }))
            .user_agent(format!("ArkCut/{VERSION} windows-x86_64"))
            .redirect(reqwest::redirect::Policy::custom(move |a| {
                let u = a.url();
                if a.previous().len() >= 6
                    || u.scheme() != "https"
                    || !u.username().is_empty()
                    || u.password().is_some()
                    || !hosts.iter().any(|h| Some(h.as_str()) == u.host_str())
                {
                    a.error("更新重定向来源不可信")
                } else {
                    a.follow()
                }
            }));
        #[cfg(feature = "update-test-hooks")]
        if let Some(p) = std::env::var_os("ARKCUT_UPDATE_TEST_CA") {
            builder = builder.add_root_certificate(
                reqwest::Certificate::from_pem(&read_bounded(Path::new(&p), 65536)?)
                    .map_err(|e| e.to_string())?,
            );
        }
        builder.build().map_err(|_| "无法初始化安全更新连接".into())
    }
    pub fn check(&self, automatic: bool) -> Result<Status> {
        let _gate = self.gate.lock().unwrap();
        if self.running() || (automatic && self.status().status == "ready") {
            return Ok(self.status());
        }
        let dir = transaction::control(&self.root)?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if automatic {
            if let Ok(last) = read_json::<u64>(&dir.join("last-check.json"), 64) {
                if now < last.saturating_add(86400) {
                    return Ok(self.status());
                }
            }
        }
        platform::json(&dir.join("last-check.json"), &now)?;
        self.cancel.store(false, Ordering::Relaxed);
        {
            let mut s = self.state.lock().unwrap();
            s.status = "checking".into();
            s.error = None;
        }
        let this = self.clone();
        std::thread::spawn(move || {
            platform::idle_priority();
            let result = runtime(async {
                this.config.url(&this.config.endpoint, false)?;
                let mut response = this
                    .client(false)?
                    .get(&this.config.endpoint)
                    .send()
                    .await
                    .map_err(network_error)?;
                if response.status().as_u16() == 404 {
                    return Err("发布端点暂无可用更新，请稍后重试；当前版本可正常使用".into());
                }
                if !response.status().is_success() {
                    return Err(format!(
                        "检查更新失败（HTTP {}），可稍后重试",
                        response.status().as_u16()
                    ));
                }
                if response.content_length().is_some_and(|n| n > MAX_INDEX) {
                    return Err("更新清单超过大小限制".into());
                }
                let mut bytes = Vec::new();
                while let Some(chunk) = response.chunk().await.map_err(network_error)? {
                    if bytes.len() as u64 + chunk.len() as u64 > MAX_INDEX {
                        return Err("更新清单过大".into());
                    }
                    bytes.extend_from_slice(&chunk);
                }
                let (e, r) = verify(&bytes, &this.config, None)?;
                let version = semver::Version::parse(&r.version).map_err(|e| e.to_string())?;
                let current = semver::Version::parse(VERSION).unwrap();
                if version < current {
                    return Err("发布端点返回旧版本，已拒绝降级".into());
                }
                if version == current {
                    let mut s = this.state.lock().unwrap();
                    s.status = "current".into();
                    s.error = None;
                    s.version = None;
                    return Ok(());
                }
                validate(&r, &this.config, Some(VERSION))?;
                platform::atomic(&dir.join("offer.json"), &bytes)?;
                let ready = transaction::verify_archive(&dir.join("archive.zip"), &r).is_ok();
                this.offered(e, r, ready);
                Ok(())
            });
            if let Err(e) = result {
                this.error(e)
            }
        });
        Ok(self.status())
    }
    pub fn download(&self) -> Result<Status> {
        let _gate = self.gate.lock().unwrap();
        if self.running() {
            return Err("更新操作正在进行".into());
        }
        let (envelope, r) = self.offer.lock().unwrap().clone().ok_or("请先检查更新")?;
        validate(&r, &self.config, Some(VERSION))?;
        let dir = transaction::control(&self.root)?;
        platform::space(
            &self.root,
            r.asset
                .bytes
                .saturating_add(r.unpacked_bytes.saturating_mul(2)),
        )?;
        self.cancel.store(false, Ordering::Relaxed);
        {
            let mut s = self.state.lock().unwrap();
            s.status = "downloading".into();
            s.downloaded = 0;
            s.total = r.asset.bytes;
            s.error = None;
        }
        let this = self.clone();
        std::thread::spawn(move || {
            platform::idle_priority();
            let result = runtime(async {
                let _lock = platform::lock(&dir.join("download.lock"))?;
                let final_path = dir.join("archive.zip");
                if transaction::verify_archive(&final_path, &r).is_ok() {
                    this.offered(envelope.clone(), r.clone(), true);
                    return Ok(());
                }
                let partial = dir.join("archive.part");
                no_links(&partial)?;
                let mut file = OpenOptions::new()
                    .create(true)
                    .truncate(true)
                    .write(true)
                    .open(&partial)
                    .map_err(|_| "无法写入更新下载，请检查权限")?;
                let mut response =
                    cancellable(this.client(true)?.get(&r.asset.url).send(), &this.cancel).await?;
                if !response.status().is_success() {
                    return Err(format!(
                        "下载失败（HTTP {}），当前版本未改变",
                        response.status().as_u16()
                    ));
                }
                if response
                    .content_length()
                    .is_some_and(|n| n != r.asset.bytes)
                {
                    return Err("下载长度与签名清单不符".into());
                }
                let mut count = 0u64;
                let mut last = std::time::Instant::now();
                loop {
                    let Some(chunk) = cancellable(response.chunk(), &this.cancel).await? else {
                        break;
                    };
                    count += chunk.len() as u64;
                    if count > r.asset.bytes {
                        return Err("下载超过签名限额".into());
                    }
                    file.write_all(&chunk)
                        .map_err(|_| "写入更新失败，请检查剩余空间")?;
                    if last.elapsed() > Duration::from_millis(100) {
                        this.state.lock().unwrap().downloaded = count;
                        last = std::time::Instant::now();
                    }
                    if count % (4 * 1024 * 1024) < 65536 {
                        platform::space(&this.root, r.asset.bytes.saturating_sub(count))?;
                        std::thread::sleep(Duration::from_millis(2));
                    }
                }
                file.sync_all().map_err(|e| e.to_string())?;
                drop(file);
                if this.cancel.load(Ordering::Relaxed) {
                    return Err("已取消更新下载".into());
                }
                transaction::verify_archive(&partial, &r)?;
                if final_path.exists() {
                    no_links(&final_path)?;
                    fs::remove_file(&final_path).map_err(|e| e.to_string())?
                }
                fs::rename(&partial, &final_path).map_err(|e| e.to_string())?;
                this.offered(envelope, r, true);
                Ok(())
            });
            if let Err(e) = result {
                let partial = dir.join("archive.part");
                if no_links(&partial).is_ok() {
                    let _ = fs::remove_file(partial);
                }
                this.error(e)
            }
        });
        Ok(self.status())
    }
    pub fn cancel(&self) -> Status {
        let mut s = self.state.lock().unwrap();
        if s.status == "downloading" {
            self.cancel.store(true, Ordering::Relaxed);
            s.status = "cancelling".into();
        }
        s.clone()
    }
    pub fn prepare_install(&self, busy: impl FnOnce() -> bool) -> Result<Status> {
        let _gate = self.gate.lock().unwrap();
        if busy() {
            return Err("媒体任务仍在处理，更新将在空闲后安装".into());
        }
        if self.status().status != "ready" {
            return Err("请先完成更新下载".into());
        }
        let dir = transaction::control(&self.root)?;
        let bytes = read_bounded(&dir.join("offer.json"), MAX_INDEX)?;
        let (_, r) = verify(&bytes, &self.config, Some(VERSION))?;
        transaction::verify_archive(&dir.join("archive.zip"), &r)?;
        platform::space(&self.root, r.unpacked_bytes.saturating_mul(2))?;
        self.state.lock().unwrap().status = "installing".into();
        if let Err(e) = start_helper(&self.root, "--install") {
            self.error(e.clone());
            return Err(e);
        }
        Ok(self.status())
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
}
fn network_error(error: reqwest::Error) -> String {
    if error.is_timeout() {
        "更新连接超时，请重试".into()
    } else {
        "无法连接更新服务器，请检查网络后重试；本地剪辑仍可使用".into()
    }
}
fn runtime<T>(future: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?
        .block_on(future)
}
async fn cancellable<T>(
    future: impl std::future::Future<Output = std::result::Result<T, reqwest::Error>>,
    cancel: &AtomicBool,
) -> Result<T> {
    let mut future = std::pin::pin!(future);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("已取消更新下载，当前版本未改变".into());
        }
        if let Ok(value) = tokio::time::timeout(Duration::from_millis(100), &mut future).await {
            return value.map_err(network_error);
        }
    }
}
pub fn start_helper(root: &Path, mode: &str) -> Result<()> {
    let dir = transaction::control(root)?;
    let helper = if mode == "--recover" {
        transaction::journal(root)?
            .and_then(|j| {
                j.changes
                    .into_iter()
                    .find(|c| c.path == "ArkCutUpdater.exe")
                    .and_then(|c| c.old)
            })
            .ok_or("恢复记录缺少更新助手")?
    } else {
        transaction::old_files(root)?
            .into_iter()
            .find(|f| f.path == "ArkCutUpdater.exe")
            .ok_or("当前版本缺少更新助手，请手动获取完整包")?
    };
    if !valid_hash(&helper.sha256) {
        return Err("助手描述无效".into());
    }
    let copy = dir.join(format!("helper-{}.exe", &helper.sha256[..16]));
    no_links(&copy)?;
    if !copy.exists() {
        if !matches(&root.join("ArkCutUpdater.exe"), &helper) {
            return Err("更新助手校验失败，请重新解压完整包".into());
        }
        fs::copy(root.join("ArkCutUpdater.exe"), &copy).map_err(|e| e.to_string())?;
    }
    if !matches(&copy, &helper) {
        return Err("暂存更新助手校验失败".into());
    }
    let recovery = format!(
        "@echo off\r\n\"%~dp0helper-{}.exe\" --recover \"%~dp0..\" 0 manual\r\n",
        &helper.sha256[..16]
    );
    platform::atomic(&dir.join("恢复更新.cmd"), recovery.as_bytes())?;
    let nonce = uuid::Uuid::new_v4().to_string();
    let mut command = Command::new(&copy);
    platform::hidden(&mut command);
    command
        .arg(mode)
        .arg(root)
        .arg(std::process::id().to_string())
        .arg(&nonce);
    let mut child = command
        .spawn()
        .map_err(|e| format!("无法启动更新助手：{e}"))?;
    let began = std::time::Instant::now();
    while began.elapsed() < Duration::from_secs(8) {
        if let Ok(ready) = read_json::<serde_json::Value>(&dir.join("helper-ready.json"), 4096) {
            if ready["nonce"] == nonce && ready["pid"] == child.id() {
                return Ok(());
            }
        }
        if child.try_wait().map_err(|e| e.to_string())?.is_some() {
            return Err("更新助手未就绪，当前程序保持运行".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    Err("更新助手响应超时，当前程序保持运行".into())
}
pub fn health_token() -> Option<String> {
    let args: Vec<_> = std::env::args().collect();
    args.windows(2)
        .find(|p| p[0] == "--arkcut-health")
        .map(|p| p[1].clone())
}
pub fn validate_health(root: &Path) -> Result<String> {
    let token = health_token().ok_or("不是更新健康检查")?;
    let j = transaction::journal(root)?.ok_or("缺少更新事务")?;
    let (_, r) = verify(
        &serde_json::to_vec(&j.envelope).map_err(|e| e.to_string())?,
        &Config::embedded()?,
        None,
    )?;
    if j.state != "awaitingHealth" || j.token != token || r.version != VERSION {
        return Err("更新健康检查不匹配".into());
    }
    for part in ["health-data", "health-profile", "health.json"] {
        no_links(&root.join(CONTROL).join(part))?;
    }
    Ok(token)
}
pub fn health_ready(root: &Path) -> Result<()> {
    let token = validate_health(root)?;
    #[cfg(feature = "update-test-hooks")]
    if std::env::var("ARKCUT_UPDATE_TEST_FAIL_HEALTH").as_deref() == Ok("1") {
        return Err("测试首次启动失败".into());
    }
    platform::json(
        &root.join(CONTROL).join("health.json"),
        &serde_json::json!({"ready":true,"token":token,"version":VERSION}),
    )
}
