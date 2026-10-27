use crate::{model::*, process::*};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::Path,
};
const MAX_INDEX_BYTES: usize = 64 * 1024 * 1024;
const MAX_INDEX_CSV: u64 = 256 * 1024 * 1024;
const INDEX_MAGIC: &[u8; 8] = b"ARKIDX02";
struct FrameIndex {
    origin: Us,
    duration: Us,
    pts: Vec<Us>,
}
fn checked_probe_log(log: &str) -> Result<(), String> {
    // Probes use an error-only log level. Keep explicit lower-level messages
    // harmless if a decoder/library nevertheless emits one on stderr.
    let errors = log
        .lines()
        .filter(|line| {
            !line.trim().is_empty()
                && (["[error]", "[fatal]", "[panic]"]
                    .iter()
                    .any(|level| line.contains(level))
                    || !["[warning]", "[info]", "[verbose]", "[debug]", "[trace]"]
                        .iter()
                        .any(|level| line.contains(level)))
        })
        .collect::<Vec<_>>()
        .join("\n");
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "DECODE_FAILED: 素材解码报告错误，无法确认完整帧索引：{}",
            errors.chars().take(1600).collect::<String>()
        ))
    }
}
fn source_unchanged(fp: &Fingerprint) -> Result<(), String> {
    if fingerprint(&fp.path)? != *fp {
        Err("SOURCE_CHANGED: 建立帧索引期间素材已变化，请重新导入".into())
    } else {
        Ok(())
    }
}
fn load_index(r: &Runner, path: &Path, key: &str) -> Result<Option<FrameIndex>, String> {
    if !path.is_file() {
        return Ok(None);
    }
    let _active = crate::storage::active_file(path)?;
    let result = (|| -> Result<FrameIndex, String> {
        let file = File::open(path).map_err(|e| e.to_string())?;
        let size = file.metadata().map_err(|e| e.to_string())?.len();
        if size < 128 || size > MAX_INDEX_BYTES as u64 + 128 {
            return Err("索引缓存长度无效".into());
        }
        let mut reader = BufReader::new(file);
        let mut header = [0u8; 96];
        reader.read_exact(&mut header).map_err(|e| e.to_string())?;
        if &header[..8] != INDEX_MAGIC || &header[8..72] != key.as_bytes() {
            return Err("索引缓存身份不匹配".into());
        }
        let origin = i64::from_le_bytes(header[72..80].try_into().unwrap());
        let duration = i64::from_le_bytes(header[80..88].try_into().unwrap());
        let count = u64::from_le_bytes(header[88..96].try_into().unwrap());
        if count == 0 || count > MAX_INDEX_BYTES as u64 / 8 || size != 128 + count * 8 {
            return Err("索引缓存帧数无效".into());
        }
        let _memory = r.reserve_memory(count * 8, "读取帧索引缓存")?;
        let mut pts = Vec::new();
        pts.try_reserve_exact(count as usize)
            .map_err(|_| "RESOURCE_MEMORY: 无法分配帧索引，请关闭部分应用后重试")?;
        let mut hash = Sha256::new();
        hash.update(header);
        let mut bytes = [0u8; 8];
        for i in 0..count {
            if i % 8192 == 0 {
                r.check()?;
            }
            reader.read_exact(&mut bytes).map_err(|e| e.to_string())?;
            hash.update(bytes);
            let value = i64::from_le_bytes(bytes);
            if value < 0 || pts.last().is_some_and(|p| *p >= value) {
                return Err("索引缓存时间戳无效".into());
            }
            pts.push(value)
        }
        let mut checksum = [0u8; 32];
        reader
            .read_exact(&mut checksum)
            .map_err(|e| e.to_string())?;
        if hash.finalize().as_slice() != checksum || pts[0] != 0 || duration <= *pts.last().unwrap()
        {
            return Err("索引缓存完整性校验失败".into());
        }
        Ok(FrameIndex {
            origin,
            duration,
            pts,
        })
    })();
    match result {
        Ok(index) => Ok(Some(index)),
        Err(e) if e.starts_with("RESOURCE_") || e.starts_with("CANCELLED") => Err(e),
        Err(_) => {
            let _ = fs::remove_file(path);
            Ok(None)
        }
    }
}
fn save_index(r: &Runner, path: &Path, key: &str, index: &FrameIndex) -> Result<(), String> {
    let dir = path.parent().ok_or("索引缓存目录无效")?;
    let expected = 128 + index.pts.len() as u64 * 8;
    let _active = crate::storage::active_file(path)?;
    crate::storage::quota(dir, expected, crate::storage::settings().0.cache_limit, &[])?;
    let temp = crate::storage::temp_file(dir, "index-save")?;
    let _disk = r.reserve_disk(temp.path(), expected, "保存帧索引缓存")?;
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp.path())
        .map_err(|e| e.to_string())?;
    let mut writer = BufWriter::new(file);
    let mut hash = Sha256::new();
    let mut header = [0u8; 96];
    header[..8].copy_from_slice(INDEX_MAGIC);
    header[8..72].copy_from_slice(key.as_bytes());
    header[72..80].copy_from_slice(&index.origin.to_le_bytes());
    header[80..88].copy_from_slice(&index.duration.to_le_bytes());
    header[88..96].copy_from_slice(&(index.pts.len() as u64).to_le_bytes());
    writer.write_all(&header).map_err(|e| e.to_string())?;
    hash.update(header);
    for (i, t) in index.pts.iter().enumerate() {
        if i % 8192 == 0 {
            r.check()?;
        }
        let bytes = t.to_le_bytes();
        writer.write_all(&bytes).map_err(|e| e.to_string())?;
        hash.update(bytes)
    }
    writer
        .write_all(&hash.finalize())
        .and_then(|_| writer.flush())
        .and_then(|_| writer.get_ref().sync_all())
        .map_err(|e| e.to_string())?;
    drop(writer);
    r.check()?;
    if path.exists() {
        return Ok(());
    }
    fs::rename(temp.path(), path).map_err(|e| e.to_string())
}
fn frame_index(
    r: &Runner,
    fp: &Fingerprint,
    stream: &Stream,
    duration: Us,
) -> Result<FrameIndex, String> {
    let pixels = stream.width as u64 * stream.height as u64;
    // A validated cache only needs its PTS allocation. Do not require the
    // decoder's full memory budget until a real decode is necessary.
    r.prepare_stage("frameIndexCache", 128 * 1024, 0)?;
    let dir = crate::storage::index_dir()?;
    let key = crate::storage::hash(
        &serde_json::json!({"algorithm":"decoded-display-pts-v3-checked-decode","source":fp,"stream":stream,"decoder":fingerprint(&r.ffprobe.to_string_lossy())?}),
    )?;
    let cache = dir.join(format!("frame-index-{key}.idx"));
    if let Some(index) = load_index(r, &cache, &key)? {
        source_unchanged(fp)?;
        (r.progress)("cache", None);
        r.measure(
            "frameIndex",
            index.duration,
            Some(index.duration),
            "us",
            "已校验并复用真实帧索引",
        );
        return Ok(index);
    }
    r.check()?;
    let threads = r.resource().prepare_stage_limited(
        "frameIndex",
        (160 * 1024 * 1024u64).saturating_add(pixels.saturating_mul(24)),
        pixels.saturating_mul(8).max(4 * 1024 * 1024),
        8,
    )?;
    let cache_limit = crate::storage::settings().0.cache_limit;
    let spool_limit = MAX_INDEX_CSV.min(cache_limit);
    let estimated = ((duration.max(SECOND) as u128 * 240 * 80 / SECOND as u128)
        .min(spool_limit as u128) as u64)
        .max(65536);
    crate::storage::quota(&dir, estimated, cache_limit, &[])?;
    let spool = crate::storage::temp_file(&dir, "index-decode")?;
    let disk = r.reserve_disk(spool.path(), estimated, "帧索引临时数据")?;
    let decoder_log = r.run_to_file(
        true,
        &args(&[
            "-v",
            "level+error",
            "-threads",
            &threads.to_string(),
            "-select_streams",
            &stream.index.to_string(),
            "-show_frames",
            "-show_entries",
            "frame=best_effort_timestamp_time,pkt_duration_time,duration_time",
            "-of",
            "csv=p=0",
            &fp.path,
        ]),
        spool.path(),
        spool_limit,
    )?;
    checked_probe_log(&decoder_log)?;
    crate::storage::quota(&dir, 0, cache_limit, &[])?;
    let mut reader = BufReader::new(File::open(spool.path()).map_err(|e| e.to_string())?);
    let mut line = Vec::new();
    line.try_reserve_exact(8192)
        .map_err(|_| "RESOURCE_MEMORY: 无法分配索引解析缓冲")?;
    let mut pts = Vec::new();
    let mut index_memory = Vec::new();
    let mut last_duration = 0;
    loop {
        r.check()?;
        line.clear();
        let read = reader
            .by_ref()
            .take(8193)
            .read_until(b'\n', &mut line)
            .map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        if line.len() > 8192 {
            return Err("DECODE_FAILED: 帧索引行超过安全上限".into());
        }
        let text = std::str::from_utf8(&line).map_err(|_| "DECODE_FAILED: 帧索引编码无效")?;
        let mut parts = text.trim_end().split(',');
        if let Some(t) = parts
            .next()
            .and_then(|x| x.parse::<f64>().ok())
            .filter(|t| t.is_finite())
        {
            if pts.len() >= MAX_INDEX_BYTES / 8 {
                return Err("RESOURCE_MEMORY: 帧索引超过有界内存限额，请使用较短素材".into());
            }
            if pts.len() == pts.capacity() {
                let additional = pts.capacity()
                        .max(8192)
                        .min(MAX_INDEX_BYTES / 8 - pts.len());
                // Account only for the index that is actually being built.
                // Keep the global entry cap and fallible allocation; a four-minute
                // 120 fps recording needs ~256 KiB, not a 128 MiB up-front lease.
                index_memory.push(r.reserve_memory(additional as u64 * 8, "扩展真实帧索引")?);
                pts.try_reserve_exact(additional)
                .map_err(|_| "RESOURCE_MEMORY: 无法扩充分帧索引，请关闭部分应用后重试")?
            }
            pts.push((t * SECOND as f64).round() as Us);
            if let Some(dt) = parts
                .find_map(|x| x.parse::<f64>().ok())
                .filter(|t| t.is_finite())
            {
                last_duration = (dt * SECOND as f64).round() as Us
            }
        }
    }
    drop(reader);
    drop(spool);
    drop(disk);
    if pts.is_empty() {
        return Err("DECODE_FAILED: 没有有效显示帧".into());
    }
    let origin = pts[0];
    for chunk in pts.chunks_mut(8192) {
        r.check()?;
        for t in chunk {
            *t = t.checked_sub(origin).ok_or("DECODE_FAILED: 帧时间戳溢出")?;
        }
    }
    if !pts.windows(2).all(|w| w[0] <= w[1]) {
        pts.sort_unstable()
    }
    pts.dedup();
    r.check()?;
    if last_duration <= 0 && pts.len() > 1 {
        last_duration = pts[pts.len() - 1]
            .checked_sub(pts[pts.len() - 2])
            .ok_or("DECODE_FAILED: 帧间隔溢出")?
    }
    if last_duration <= 0 {
        last_duration = 33333
    }
    let duration = pts
        .last()
        .unwrap()
        .checked_add(last_duration)
        .ok_or("DECODE_FAILED: 帧时长溢出")?;
    let index = FrameIndex {
        origin,
        duration,
        pts,
    };
    source_unchanged(fp)?;
    if let Err(e) = save_index(r, &cache, &key, &index) {
        if e.starts_with("RESOURCE_") || e.starts_with("CANCELLED") {
            return Err(e);
        }
        (r.progress)(&format!("WARN: 帧索引已完成，缓存未保存：{e}"), None)
    }
    Ok(index)
}
pub fn fingerprint(path: &str) -> Result<Fingerprint, String> {
    let canonical = fs::canonicalize(path).map_err(|e| format!("SOURCE_MISSING: {path}: {e}"))?;
    let meta = canonical.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("请选择媒体文件".into());
    }
    let mut f = File::open(&canonical).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut b = vec![0; 65536];
    for at in [0, meta.len() / 2, meta.len().saturating_sub(65536)] {
        f.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
        let n = f.read(&mut b).map_err(|e| e.to_string())?;
        hasher.update(&b[..n]);
    }
    Ok(Fingerprint {
        path: canonical.to_string_lossy().to_string(),
        size: meta.len(),
        modified_ms: meta
            .modified()
            .map_err(|e| e.to_string())?
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
        sample_hash: format!("{:x}", hasher.finalize()),
    })
}
fn seconds(v: &Value) -> Us {
    v.as_str()
        .and_then(|x| x.parse::<f64>().ok())
        .map(|s| (s * SECOND as f64).round() as Us)
        .unwrap_or(0)
}
fn string(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}
pub fn probe(r: &Runner, path: &str, video_required: bool) -> Result<Media, String> {
    probe_selected(r, path, video_required, None)
}
pub fn probe_selected(
    r: &Runner,
    path: &str,
    video_required: bool,
    selected: Option<u32>,
) -> Result<Media, String> {
    let fp = fingerprint(path)?;
    (r.progress)("probe", None);
    r.measure("metadata", 0, None, "state", "读取媒体信息");
    let (data, metadata_log) = r.run(
        true,
        &args(&[
            "-v",
            "level+error",
            "-show_streams",
            "-show_format",
            "-of",
            "json",
            path,
        ]),
        8 * 1024 * 1024,
    )?;
    checked_probe_log(&metadata_log)?;
    let v: Value = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
    let raw = v["streams"].as_array().ok_or("未找到可解码媒体流")?;
    let streams: Vec<Stream> = raw
        .iter()
        .filter(|s| s["codec_type"] == "video" || s["codec_type"] == "audio")
        .filter(|s| s["disposition"]["attached_pic"] != 1)
        .map(|s| Stream {
            index: s["index"].as_u64().unwrap_or(0) as u32,
            kind: string(&s["codec_type"]),
            codec: string(&s["codec_name"]),
            language: s["tags"]["language"].as_str().unwrap_or("und").into(),
            channels: s["channels"].as_u64().unwrap_or(0) as u32,
            channel_layout: string(&s["channel_layout"]),
            width: s["width"].as_u64().unwrap_or(0) as u32,
            height: s["height"].as_u64().unwrap_or(0) as u32,
            rate: string(&s["r_frame_rate"]),
            time_base: string(&s["time_base"]),
            start_us: seconds(&s["start_time"]),
            default: s["disposition"]["default"] == 1,
        })
        .collect();
    let select = |kind: &str| {
        streams
            .iter()
            .find(|s| s.kind == kind && s.default)
            .or_else(|| streams.iter().find(|s| s.kind == kind))
            .map(|s| s.index)
    };
    let video_index = if let Some(i) = selected {
        if !streams.iter().any(|s| s.index == i && s.kind == "video") {
            return Err("无效视频流索引".into());
        }
        Some(i)
    } else {
        select("video")
    };
    let audio_index = select("audio");
    if video_required && video_index.is_none() {
        return Err("NO_VIDEO: 文件没有有效视频流".into());
    }
    if !video_required && audio_index.is_none() {
        return Err("NO_AUDIO: 文件没有有效音轨".into());
    }
    let hdr = raw
        .iter()
        .any(|s| s["color_transfer"] == "smpte2084" || s["color_transfer"] == "arib-std-b67");
    // Until a validated tone mapper is shipped, reject HDR instead of silently producing incorrect colours.
    if hdr {
        return Err("HDR_UNSUPPORTED: 当前构建尚未验证 HDR→SDR 转换，请先转换成 SDR 后导入".into());
    }
    let mut duration = seconds(&v["format"]["duration"]);
    let mut origin = 0;
    let mut pts = vec![];
    if let Some(index) = video_index {
        let selected_stream = streams.iter().find(|s| s.index == index).unwrap();
        r.measure("frameIndex", 0, Some(duration), "us", "建立真实帧索引");
        r.metric_state.lock().unwrap().origin = selected_stream.start_us;
        let index = frame_index(r, &fp, selected_stream, duration)?;
        origin = index.origin;
        duration = index.duration;
        pts = index.pts;
        r.measure("frameIndex", duration, Some(duration), "us", "帧索引已完成");
    } else {
        duration = duration.max(
            raw.iter()
                .map(|s| seconds(&s["duration"]))
                .max()
                .unwrap_or(0),
        );
    }
    if duration <= 0 {
        return Err("ZERO_DURATION: 媒体时长为零或无法读取".into());
    }
    let mut warnings = vec![];
    if let Some(i) = video_index {
        if let Some(s) = raw.iter().find(|s| s["index"].as_u64() == Some(i as u64)) {
            if s["tags"]["rotate"].as_str().is_some()
                || s["side_data_list"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|x| x["rotation"].is_number()))
            {
                warnings.push("包含旋转元数据，渲染按显示方向自动旋转".into());
            }
            if s["sample_aspect_ratio"]
                .as_str()
                .is_some_and(|x| x != "1:1" && x != "N/A")
            {
                warnings.push("非方形像素将转换为显示比例".into());
            }
        }
    }
    Ok(Media {
        title: v["format"]["tags"]["title"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .map(str::to_owned),
        fingerprint: fp,
        duration_us: duration,
        format: string(&v["format"]["format_name"]),
        streams,
        video_index,
        audio_index,
        origin_us: origin,
        frame_pts: pts,
        hdr,
        warnings,
    })
}
pub fn verify_media(media: &Media) -> Result<(), String> {
    let now = fingerprint(&media.fingerprint.path)?;
    if now != media.fingerprint {
        return Err(format!(
            "SOURCE_CHANGED: 素材指纹变化，请重新定位并确认 {}",
            media.fingerprint.path
        ));
    }
    Ok(())
}
pub fn extension(path: &str) -> String {
    Path::new(path)
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase()
}
pub fn frame_png(r: &Runner, p: &Project, t: Us, path: &Path) -> Result<(), String> {
    r.ff(args(&[
        "-v",
        "error",
        "-y",
        "-ss",
        &format!("{:.6}", (t + p.source.origin_us) as f64 / SECOND as f64),
        "-i",
        &p.source.fingerprint.path,
        "-map",
        &format!("0:{}", p.video_index),
        "-frames:v",
        "1",
        "-vf",
        "scale=320:-2",
        "-update",
        "1",
        &path.to_string_lossy(),
    ]))?;
    Ok(())
}

#[cfg(test)]
mod index_tests {
    use super::*;
    #[test]
    fn probe_errors_reject_silent_success_but_not_explicit_warnings() {
        assert!(checked_probe_log("").is_ok());
        assert!(
            checked_probe_log("[h264 @ 123] [warning] harmless warning\n[info] stream info")
                .is_ok()
        );
        assert!(checked_probe_log("[h264 @ 123] [error] invalid NAL unit")
            .unwrap_err()
            .starts_with("DECODE_FAILED"));
        assert!(checked_probe_log("decoder reported an unlabelled error").is_err());
        assert!(checked_probe_log("[error] packet contains literal [warning] text").is_err());
    }
    #[test]
    fn old_stream_metadata_defaults_channel_layout() {
        let old = serde_json::json!({"index":0,"kind":"audio","codec":"aac","language":"und","channels":2,"width":0,"height":0,"rate":"0/0","timeBase":"1/48000","startUs":0,"default":true});
        let stream: Stream = serde_json::from_value(old).unwrap();
        assert_eq!(stream.channel_layout, "");
    }
    #[test]
    fn index_cache_checks_identity_length_and_checksum() {
        let r = Runner::discover(std::sync::Arc::new(|_, _| {})).unwrap();
        let root = std::env::temp_dir().join(format!("arkcut-index-{}", uuid::Uuid::new_v4()));
        let dir = root.join("indexes");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(".arkcut-cache-v1"), b"owned").unwrap();
        let path = dir.join("frame-index-test.idx");
        let key = "a".repeat(64);
        let index = FrameIndex {
            origin: 100,
            duration: 100000,
            pts: vec![0, 16667, 33334, 80000],
        };
        save_index(&r, &path, &key, &index).unwrap();
        let loaded = load_index(&r, &path, &key).unwrap().unwrap();
        assert_eq!(loaded.pts, index.pts);
        assert_eq!(loaded.origin, 100);
        assert_eq!(loaded.duration, 100000);
        let mut bytes = fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        fs::write(&path, &bytes).unwrap();
        assert!(load_index(&r, &path, &key).unwrap().is_none());
        assert!(!path.exists());
        save_index(&r, &path, &key, &index).unwrap();
        assert!(load_index(&r, &path, &"b".repeat(64)).unwrap().is_none());
        fs::remove_dir_all(root).unwrap();
    }
}
