use base64::{engine::general_purpose::STANDARD, Engine as _};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
};
pub type Result<T> = std::result::Result<T, String>;
pub const MAX_INDEX: u64 = 4 * 1024 * 1024;
pub const MAX_ARCHIVE: u64 = 2 * 1024 * 1024 * 1024 - 1;
pub const MAX_EXPANDED: u64 = 4 * 1024 * 1024 * 1024;
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Key {
    pub id: String,
    pub key: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    pub endpoint: String,
    pub asset_prefix: String,
    pub redirect_hosts: Vec<String>,
    pub keys: Vec<Key>,
    pub channel: String,
}
impl Config {
    pub fn embedded() -> Result<Self> {
        #[cfg(feature = "update-test-hooks")]
        if let Some(p) = std::env::var_os("ARKCUT_UPDATE_TEST_CONFIG") {
            return read_json(Path::new(&p), MAX_INDEX);
        }
        serde_json::from_str(
            include_str!("../../../release/update-config.json").trim_start_matches('\u{feff}'),
        )
        .map_err(|e| e.to_string())
    }
    pub fn url(&self, s: &str, asset: bool) -> Result<reqwest::Url> {
        let u = reqwest::Url::parse(s).map_err(|_| "更新地址无效")?;
        if u.scheme() != "https"
            || !u.username().is_empty()
            || u.password().is_some()
            || u.fragment().is_some()
        {
            return Err("更新只允许可信 HTTPS 地址".into());
        }
        if asset && !s.starts_with(&self.asset_prefix) {
            return Err("更新资产不属于指定发布仓库".into());
        }
        if !self
            .redirect_hosts
            .iter()
            .any(|h| Some(h.as_str()) == u.host_str())
        {
            return Err("更新地址不在许可来源内".into());
        }
        Ok(u)
    }
}
#[derive(Clone, Deserialize, Serialize, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwnedFile {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Asset {
    pub url: String,
    pub bytes: u64,
    pub sha256: String,
    pub root: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Release {
    pub schema: u32,
    pub product: String,
    pub version: String,
    pub platform: String,
    pub channel: String,
    pub min_updater: String,
    pub data_schema: u32,
    pub notes: String,
    pub source_commit: String,
    pub asset: Asset,
    pub unpacked_bytes: u64,
    pub components: std::collections::BTreeMap<String, String>,
    pub files: Vec<OwnedFile>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Envelope {
    pub key_id: String,
    pub payload: String,
    pub signature: String,
}
pub fn verify(bytes: &[u8], config: &Config, current: Option<&str>) -> Result<(Envelope, Release)> {
    if bytes.len() as u64 > MAX_INDEX {
        return Err("更新清单过大".into());
    }
    let envelope: Envelope = serde_json::from_slice(bytes).map_err(|_| "更新清单格式不正确")?;
    let key = config
        .keys
        .iter()
        .find(|k| k.id == envelope.key_id)
        .ok_or("未知更新签名密钥")?;
    let raw = STANDARD.decode(&key.key).map_err(|_| "更新验证公钥无效")?;
    let raw: [u8; 32] = raw.try_into().map_err(|_| "更新验证公钥长度错误")?;
    let key = VerifyingKey::from_bytes(&raw).map_err(|_| "更新验证公钥无效")?;
    let payload = STANDARD
        .decode(&envelope.payload)
        .map_err(|_| "更新清单编码无效")?;
    let signature = STANDARD
        .decode(&envelope.signature)
        .map_err(|_| "更新签名编码无效")?;
    let signature = Signature::try_from(signature.as_slice()).map_err(|_| "更新签名长度错误")?;
    key.verify_strict(&payload, &signature)
        .map_err(|_| "更新签名校验失败，已保留当前版本")?;
    let release: Release =
        serde_json::from_slice(&payload).map_err(|_| "已签名更新清单格式无效")?;
    validate(&release, config, current)?;
    Ok((envelope, release))
}
pub fn validate(r: &Release, c: &Config, current: Option<&str>) -> Result<()> {
    if r.schema != 1
        || r.product != "ArkCut"
        || r.platform != "windows-x86_64"
        || r.channel != "stable"
        || c.channel != "stable"
        || r.data_schema != 1
    {
        return Err("更新协议、平台或数据版本不兼容".into());
    }
    let version = semver::Version::parse(&r.version).map_err(|_| "更新版本号无效")?;
    if !version.pre.is_empty() || !version.build.is_empty() {
        return Err("稳定版不接受预发布更新".into());
    }
    let minimum = semver::Version::parse(&r.min_updater).map_err(|_| "最低更新器版本无效")?;
    if let Some(current) = current {
        let old = semver::Version::parse(current).map_err(|e| e.to_string())?;
        if version <= old {
            return Err("不是更新版本，已拒绝降级或重复安装".into());
        }
        if minimum > old {
            return Err("此版本需要先手动升级更新器".into());
        }
    }
    c.url(&r.asset.url, true)?;
    safe_relative(&r.asset.root)?;
    if r.asset.root.contains('/')
        || r.asset.bytes == 0
        || r.asset.bytes > MAX_ARCHIVE
        || !valid_hash(&r.asset.sha256)
        || r.unpacked_bytes > MAX_EXPANDED
        || r.files.is_empty()
        || r.files.len() > 10000
        || r.notes.len() > 16000
        || r.source_commit.len() > 64
    {
        return Err("更新包大小或清单限制不满足".into());
    }
    if !r.components.contains_key("ffmpeg") || !r.components.contains_key("webview2") {
        return Err("更新清单缺少运行组件版本".into());
    }
    let mut names = BTreeSet::new();
    let mut total = 0u64;
    for f in &r.files {
        owned_path(&f.path)?;
        if !names.insert(f.path.to_lowercase()) || f.bytes > MAX_ARCHIVE || !valid_hash(&f.sha256) {
            return Err("更新文件重复或描述无效".into());
        }
        total = total.checked_add(f.bytes).ok_or("更新大小溢出")?;
    }
    if total != r.unpacked_bytes
        || ![
            "arkcut.exe",
            "arkcutupdater.exe",
            "manifest.json",
        ]
        .iter()
        .all(|n| names.contains(*n))
    {
        return Err("更新清单不完整或展开大小不符".into());
    }
    if r.components.get("webview2").is_some_and(|v|v.starts_with("evergreen>=")) && names.iter().any(|n|n.starts_with("media/")||n.starts_with("webview2/")) {
        return Err("轻量更新包不能携带视频或固定界面运行组件".into());
    }
    Ok(())
}
pub fn valid_hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
pub fn safe_relative(s: &str) -> Result<()> {
    if s.is_empty()
        || s.len() > 220
        || s.contains(['\\', ':', '\0'])
        || s.starts_with('/')
        || s.ends_with('/')
    {
        return Err("不安全的更新路径".into());
    }
    for part in s.split('/') {
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        let device = stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|s| s.chars().count() == 1 && "123456789¹²³".contains(s));
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.ends_with(['.', ' '])
            || part
                .chars()
                .any(|c| c.is_control() || "<>\"|?*".contains(c))
            || ["CON", "PRN", "AUX", "NUL", "CLOCK$"].contains(&stem.as_str())
            || device
        {
            return Err("更新路径包含保留名称或越界字符".into());
        }
    }
    Ok(())
}
pub fn owned_path(s: &str) -> Result<()> {
    safe_relative(s)?;
    let top = s.split('/').next().unwrap();
    let file = [
        "ArkCut.exe",
        "ArkCutUpdater.exe",
        "manifest.json",
        "README.md",
        "LICENSE",
        "使用说明.txt",
    ]
    .contains(&s);
    let directory =
        ["WebView2", "media", "licenses", "sources", "docs"].contains(&top) && s.contains('/');
    if !file && !directory {
        return Err("更新清单试图替换非程序文件".into());
    }
    Ok(())
}
pub fn no_links(path: &Path) -> Result<()> {
    for ancestor in path.ancestors() {
        if let Ok(m) = fs::symlink_metadata(ancestor) {
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if m.file_attributes() & 0x400 != 0 {
                    return Err("更新路径包含重解析点".into());
                }
            }
            if m.file_type().is_symlink() {
                return Err("更新路径包含链接".into());
            }
        }
    }
    Ok(())
}
pub fn child(root: &Path, relative: &str) -> Result<PathBuf> {
    safe_relative(relative)?;
    let path = root.join(relative);
    no_links(&path)?;
    Ok(path)
}
pub fn hash(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut h = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        h.update(&buffer[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}
pub fn matches(path: &Path, f: &OwnedFile) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() == f.bytes)
        && hash(path).is_ok_and(|h| h == f.sha256)
}
pub fn read_json<T: serde::de::DeserializeOwned>(p: &Path, limit: u64) -> Result<T> {
    let bytes = read_bounded(p, limit)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|e| e.to_string())?
        .trim_start_matches('\u{feff}');
    serde_json::from_str(text).map_err(|e| e.to_string())
}
pub fn read_bounded(p: &Path, limit: u64) -> Result<Vec<u8>> {
    no_links(p)?;
    let file = fs::File::open(p).map_err(|e| e.to_string())?;
    if file.metadata().map_err(|e| e.to_string())?.len() > limit {
        return Err("更新元数据超过限制".into());
    }
    let mut v = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut v)
        .map_err(|e| e.to_string())?;
    if v.len() as u64 > limit {
        return Err("更新元数据超过限制".into());
    }
    Ok(v)
}
