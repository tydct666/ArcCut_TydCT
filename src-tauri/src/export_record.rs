//! A successful output is tied to content, target and a full file digest.
use crate::{model::Project, source_export, storage};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::Path};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportRecord {
    pub signature: String,
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}
pub fn signature(p: &Project) -> Result<String, String> {
    let mut content = p.clone();
    content.completed_export = None;
    content.name = None;
    content.output_path = None;
    content.revision = 0;
    content.source.title = None;
    for item in &mut content.music {
        item.media.title = None;
    }
    source_export::lock(&mut content)?;
    content.export.preview_edge = 1280;
    storage::hash(&content)
}
pub fn digest(path: &Path) -> Result<(u64, String), String> {
    digest_checked(path, || Ok(()))
}
pub fn digest_checked(
    path: &Path,
    mut check: impl FnMut() -> Result<(), String>,
) -> Result<(u64, String), String> {
    let mut file =
        fs::File::open(path).map_err(|e| format!("OUTPUT_MISSING: 无法读取已导出文件：{e}"))?;
    let before = file.metadata().map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut bytes = 0;
    loop {
        check()?;
        let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        bytes += n as u64;
        hasher.update(&buffer[..n]);
    }
    let after = file.metadata().map_err(|e| e.to_string())?;
    if bytes != before.len()
        || after.len() != bytes
        || before.modified().ok() != after.modified().ok()
    {
        return Err("OUTPUT_CHANGED: 校验期间成片发生变化，请重试".into());
    }
    Ok((bytes, format!("{:x}", hasher.finalize())))
}
pub fn create(p: &Project, path: &Path) -> Result<ExportRecord, String> {
    let (bytes, sha256) = digest(path)?;
    Ok(ExportRecord {
        signature: signature(p)?,
        path: path.to_string_lossy().into(),
        bytes,
        sha256,
    })
}
pub fn check(p: &Project, target: &str) -> Result<bool, String> {
    let Some(record) = &p.completed_export else {
        return Ok(false);
    };
    if record.signature != signature(p)? {
        return Ok(false);
    }
    let (Ok(a), Ok(b)) = (fs::canonicalize(&record.path), fs::canonicalize(target)) else {
        return Ok(false);
    };
    if a != b {
        return Ok(false);
    }
    let (bytes, hash) = digest(&a)?;
    Ok(bytes == record.bytes && hash == record.sha256)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn receipt_survives_only_non_content_edits() {
        let mut p = crate::timeline::tests::project();
        let original = signature(&p).unwrap();
        p.name = Some("renamed".into());
        p.revision += 1;
        p.output_path = Some("elsewhere.mp4".into());
        p.export.preview_edge = 960;
        assert_eq!(original, signature(&p).unwrap());
        p.mix.game_gain = 0.4;
        assert_ne!(original, signature(&p).unwrap());
    }
    #[test]
    fn receipt_checks_content_target_and_external_replacement() {
        let dir = std::env::temp_dir().join(format!("arkcut-receipt-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let file = dir.join("movie.mp4");
        fs::write(&file, b"validated file").unwrap();
        let mut p = crate::timeline::tests::project();
        p.completed_export = Some(create(&p, &file).unwrap());
        let target = file.to_str().unwrap();
        assert!(check(&p, target).unwrap());
        let before = p.clone();
        p.mix.game_gain = 0.23;
        assert!(!check(&p, target).unwrap());
        p = before;
        assert!(check(&p, target).unwrap());
        let other = dir.join("elsewhere.mp4");
        fs::copy(&file, &other).unwrap();
        assert!(!check(&p, other.to_str().unwrap()).unwrap());
        fs::write(&file, b"replaced! file").unwrap();
        assert_eq!(
            fs::metadata(&file).unwrap().len(),
            p.completed_export.as_ref().unwrap().bytes
        );
        assert!(!check(&p, target).unwrap());
        fs::remove_file(&file).unwrap();
        assert!(!check(&p, target).unwrap());
        fs::remove_file(&other).unwrap();
        fs::remove_dir(&dir).unwrap();
    }
    #[test]
    fn receipt_hash_is_cancellable() {
        let path = std::env::temp_dir().join(format!("arkcut-hash-{}", uuid::Uuid::new_v4()));
        fs::write(&path, vec![0; 2 * 1024 * 1024]).unwrap();
        let mut checks = 0;
        let result = digest_checked(&path, || {
            checks += 1;
            if checks > 1 {
                Err("CANCELLED".into())
            } else {
                Ok(())
            }
        });
        assert_eq!(result.unwrap_err(), "CANCELLED");
        fs::remove_file(path).unwrap();
    }
}
