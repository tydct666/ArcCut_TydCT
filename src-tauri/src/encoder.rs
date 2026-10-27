use crate::{model::*, process::*, source_export::Format, storage};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::HashMap,
    fs,
    sync::{Mutex, OnceLock},
};
static AVAILABLE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
const QUALITY_VERSION: &str = "source-hq-sample-cq-v6-resource-independent-lookahead";

/// Strict export fallback. Resource policy changes worker counts only; it never
/// selects a lossy quantizer or a cheaper quality preset.
pub fn lossless_options(threads: usize, codec: &str) -> Result<Vec<String>, String> {
    let workers = threads.max(1).to_string();
    let mut out = args(&["-threads", &workers]);
    out.extend(match codec {
        "libx264" => args(&["-preset", "fast", "-qp", "0", "-bf", "0"]),
        "libx265" => args(&[
            "-preset",
            "fast",
            "-x265-params",
            &format!("lossless=1:pools={workers}:frame-threads={workers}:log-level=error"),
        ]),
        "libvpx-vp9" => args(&[
            "-lossless",
            "1",
            "-b:v",
            "0",
            "-cpu-used",
            "4",
            "-row-mt",
            "1",
        ]),
        "libaom-av1" => args(&[
            "-aom-params",
            "lossless=1",
            "-crf",
            "0",
            "-b:v",
            "0",
            "-cpu-used",
            "8",
            "-row-mt",
            "1",
        ]),
        "ffv1" => args(&[
            "-level",
            "3",
            "-coder",
            "1",
            "-context",
            "1",
            "-slicecrc",
            "1",
        ]),
        _ => return Err(format!("SOURCE_LOSSLESS: {codec} 没有已验证的无损画面模式")),
    });
    Ok(out)
}

/// Probe only the requested encoder. Cancellation/resource failures are not cached.
pub fn available(r: &Runner, codec: &str, lossless: bool) -> bool {
    if lossless && !["h264_nvenc", "hevc_nvenc"].contains(&codec) {
        return false;
    }
    if r.check().is_err() {
        return false;
    }
    let key = format!("{}:{codec}:{lossless}", r.ffmpeg.display());
    let cache = AVAILABLE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(value) = cache.lock().unwrap().get(&key) {
        return *value;
    }
    (r.progress)("encoderProbe", None);
    let mut cmd = args(&[
        "-v",
        "error",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=s=640x360:r=30:d=0.1",
        "-frames:v",
        "3",
        "-an",
        "-c:v",
        codec,
    ]);
    if lossless {
        cmd.extend(args(&[
            "-preset", "p1", "-tune", "lossless", "-rc", "constqp", "-qp", "0",
        ]));
    }
    cmd.extend(args(&["-f", "null", "-"]));
    let result = r.ff(cmd);
    if r.check().is_ok()
        && !result
            .as_ref()
            .err()
            .is_some_and(|e| e.contains("RESOURCE_") || e.to_lowercase().contains("memory"))
    {
        cache.lock().unwrap().insert(key, result.is_ok());
    }
    result.is_ok()
}
pub fn hardware(r: &Runner) -> Vec<String> {
    [
        "h264_nvenc",
        "h264_qsv",
        "h264_amf",
        "hevc_nvenc",
        "hevc_qsv",
        "hevc_amf",
        "av1_nvenc",
        "av1_qsv",
        "av1_amf",
    ]
    .into_iter()
    .filter(|codec| available(r, codec, false))
    .map(str::to_string)
    .collect()
}
pub fn compatible(container: &str, codec: &str) -> bool {
    crate::source_export::video_encoder(container, codec.split('_').next().unwrap_or(codec)).is_ok()
}
pub fn preferred(r: &Runner, family: &str, container: &str, lossless: bool) -> Option<String> {
    // Only the locally quality-tested NVENC preset is automatic. Unsupported
    // hardware uses the bounded software encoders below, without changing codec.
    ["nvenc"]
        .into_iter()
        .map(|device| format!("{family}_{device}"))
        .find(|codec| compatible(container, codec) && available(r, codec, lossless))
}
pub fn initialization_failure(error: &str) -> bool {
    let e = error.to_ascii_lowercase();
    if [
        "resource_",
        "cancelled",
        "memory",
        "no space",
        "disk",
        "output_",
        "write",
        "quality_sample",
    ]
    .iter()
    .any(|s| e.contains(s))
    {
        return false;
    }
    [
        "initializeencoder",
        "no capable devices",
        "unsupported device",
        "unsupported format",
        "does not support",
        "not supported",
        "cannot load nvcuda",
        "cannot load libcuda",
        "error while opening encoder",
        "openencodesessionex failed",
        "minimum required nvidia driver",
    ]
    .iter()
    .any(|s| e.contains(s))
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Quality {
    pub quantizer: u8,
    pub max_bitrate: u64,
    pub estimated_bitrate: u64,
    pub measured_ssim: f64,
    pub minimum_frame_ssim: f64,
    pub sample_frames: usize,
    pub sample_starts: Vec<Us>,
    pub calibrated: bool,
    pub av1_level: String,
}
fn initial_quantizer(codec: &str) -> u8 {
    match codec {
        "libx265" => 20,
        "libvpx-vp9" | "libaom-av1" => 24,
        "libvpx" => 10,
        _ => 18,
    }
}
pub fn initial(p: &Project, f: &Format, codec: &str) -> Quality {
    let total =
        (f.reference_bytes as f64 * 8. * SECOND as f64 / p.source.duration_us.max(1) as f64) as u64;
    let max_bitrate = ((total as f64 * 1.3) as u64)
        .saturating_sub(f.audio_bitrate)
        // High-quality replacement audio can dominate tiny/low-bitrate files.
        // Do not take those extra audio bits away from the source video budget.
        .max((f.source_video_bitrate as f64 * 1.3) as u64)
        .max(128000);
    let video_time = p
        .source
        .frame_pts
        .last()
        .copied()
        .unwrap_or(p.source.duration_us)
        - p.source.frame_pts.first().copied().unwrap_or(0);
    let fps = p.source.frame_pts.len().saturating_sub(1) as f64 * SECOND as f64
        / video_time.max(1) as f64;
    let pixels = f.width as u64 * f.height as u64;
    let av1_level = if codec == "av1_nvenc" {
        if pixels <= 8_912_896
            && pixels as f64 * fps <= 534_773_760.
            && f.width <= 8192
            && f.height <= 4352
        {
            "5.1"
        } else {
            "6.3"
        }
    } else {
        ""
    }
    .to_string();
    Quality {
        quantizer: initial_quantizer(codec),
        max_bitrate,
        estimated_bitrate: max_bitrate,
        measured_ssim: 0.,
        minimum_frame_ssim: 0.,
        sample_frames: 0,
        sample_starts: vec![],
        calibrated: false,
        av1_level,
    }
}
pub fn preview(p: &Project, f: &Format, codec: &str) -> Quality {
    let mut q = initial(p, f, codec);
    q.quantizer = 26;
    q.max_bitrate = 0;
    let fps = p.source.frame_pts.len() as f64 * SECOND as f64 / p.source.duration_us.max(1) as f64;
    q.estimated_bitrate = (f.width as f64 * f.height as f64 * fps * 0.15).clamp(2_000_000., 16_000_000.) as u64;
    q
}
pub fn preview_options(r: &Runner, codec: &str, q: &Quality) -> Vec<String> {
    let mut options = match codec {
        "h264_nvenc" => args(&["-preset","p1","-tune","ll","-rc","vbr","-cq",&q.quantizer.to_string(),"-b:v","0","-rc-lookahead","0","-bf","0"]),
        "libx264" => args(&["-preset","ultrafast","-crf",&q.quantizer.to_string(),"-bf","0"]),
        _ => return self::options(r, codec, q),
    };
    options.extend(args(&["-threads", &r.threads().to_string()]));
    options
}
pub fn options(r: &Runner, codec: &str, q: &Quality) -> Vec<String> {
    options_with_threads(r.threads(), codec, q)
}
fn options_with_threads(threads: usize, codec: &str, q: &Quality) -> Vec<String> {
    let mut out = if codec.ends_with("_nvenc") {
        args(&[
            "-preset",
            if codec == "h264_nvenc" { "p3" } else { "p5" },
            "-tune",
            "hq",
            "-rc",
            "vbr",
            "-cq",
            &q.quantizer.to_string(),
            "-b:v",
            "0",
            "-spatial-aq",
            "1",
            "-aq-strength",
            "8",
            "-rc-lookahead",
            &if codec == "h264_nvenc" { 12 } else { 16 }.to_string(),
            "-bf",
            "2",
        ])
    } else {
        match codec {
            "libx264" => args(&["-preset", "fast", "-crf", &q.quantizer.to_string()]),
            "libx265" => args(&[
                "-preset",
                "fast",
                "-crf",
                &q.quantizer.to_string(),
                "-x265-params",
                &format!(
                    "pools={}:frame-threads={}:log-level=error",
                    threads,
                    threads.min(2)
                ),
            ]),
            "libvpx-vp9" | "libvpx" => args(&[
                "-crf",
                &q.quantizer.to_string(),
                "-b:v",
                &if q.max_bitrate > 0 {
                    (q.max_bitrate * 8 / 10).to_string()
                } else {
                    "0".into()
                },
                "-deadline",
                "good",
                "-cpu-used",
                "4",
                "-row-mt",
                "1",
            ]),
            "libaom-av1" => args(&[
                "-crf",
                &q.quantizer.to_string(),
                "-b:v",
                &if q.max_bitrate > 0 {
                    (q.max_bitrate * 8 / 10).to_string()
                } else {
                    "0".into()
                },
                "-cpu-used",
                "6",
                "-row-mt",
                "1",
            ]),
            _ => vec![],
        }
    };
    if q.max_bitrate > 0 {
        out.extend(args(&[
            "-maxrate",
            &q.max_bitrate.to_string(),
            "-bufsize",
            &q.max_bitrate.saturating_mul(2).to_string(),
        ]));
    }
    // Some NVENC drivers choose reserved AV1 auto-level 7.3 (seq_level_idx=23),
    // which libaom correctly refuses. Select a defined level from native size and
    // observed frame cadence; this changes no PTS and no encoded frame count.
    if codec == "av1_nvenc" {
        out.extend(args(&["-level", &q.av1_level]));
    }
    out.extend(args(&["-threads", &threads.to_string()]));
    out
}
fn sample_windows(p: &Project) -> Vec<(Us, Us)> {
    let end = p.source.duration_us.max(1);
    if end < 5 * SECOND {
        return vec![(p.source.frame_pts.first().copied().unwrap_or(0), end)];
    }
    let fractions: Vec<f64> = if end < 5 * SECOND {
        vec![0.]
    } else {
        vec![0.10, 0.50, 0.85]
    };
    let mut windows = vec![];
    for fraction in fractions {
        let desired = (end as f64 * fraction) as Us;
        let start = p
            .source
            .frame_pts
            .iter()
            .copied()
            .find(|t| *t >= desired)
            .unwrap_or(0);
        let stop = (start + 1_200_000).min(end);
        if stop > start && !windows.contains(&(start, stop)) {
            windows.push((start, stop));
        }
    }
    windows
}
fn sample_input(p: &Project, start: Us, end: Us) -> Vec<String> {
    let seek = if start >= 200000 {
        (start + p.source.origin_us - 200000).max(0)
    } else {
        0
    };
    let mut cmd = args(&["-copyts"]);
    if seek > 0 {
        cmd.extend(args(&[
            "-seek_timestamp",
            "1",
            "-ss",
            &crate::render::sec(seek),
        ]));
    }
    cmd.extend(args(&[
        "-t",
        &crate::render::sec((end + p.source.origin_us - seek + 100000).max(100000)),
        "-i",
        &p.source.fingerprint.path,
    ]));
    cmd
}
fn sample_filter(
    p: &Project,
    f: &Format,
    start: Us,
    end: Us,
    edited: Option<&Plan>,
) -> Result<String, String> {
    if let Some(plan) = edited {
        return Ok(format!(
            "{},format={}",
            crate::render::video_selection(p, plan, (0, plan.duration_us))?,
            f.pixel_format
        ));
    }
    Ok(format!(
        "settb=1/1000000,setpts=PTS-{},trim=start={}:end={},setpts=PTS-STARTPTS,format={}",
        p.source.origin_us,
        crate::render::sec(start),
        crate::render::sec(end),
        f.pixel_format
    ))
}
fn all_value(line: &str) -> Option<f64> {
    line.split_once("All:")
        .and_then(|(_, v)| v.split_whitespace().next())
        .and_then(|s| s.parse().ok())
        .filter(|v: &f64| v.is_finite())
}
/// Calibrate only representative short windows; never restart a completed full
/// encode to meet a byte target. A cached result is tied to source/tool/parameters.
pub fn calibrated(
    r: &Runner,
    p: &Project,
    plan: &Plan,
    f: &Format,
    codec: &str,
) -> Result<Quality, String> {
    let dir = storage::project_dir(&p.project_id)?;
    let tool = fs::metadata(&r.ffmpeg).map_err(|e| e.to_string())?;
    let edited = (p.source.duration_us < 5 * SECOND).then_some(plan);
    let mut identity = json!({"version":QUALITY_VERSION,"source":p.source.fingerprint,"sourceDuration":p.source.duration_us,"frameIndex":storage::hash(&p.source.frame_pts)?,"stream":p.video_index,"codec":codec,"pixel":f.pixel_format,"sar":f.sar,"colors":f.colors,"bytes":f.reference_bytes,"audioRate":f.audio_bitrate,"ffmpeg":r.ffmpeg,"toolSize":tool.len(),"toolModified":tool.modified().ok().and_then(|t|t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d|d.as_millis()),"threads":r.threads()});
    if let Some(plan) = edited {
        identity["shortEditedSample"] = json!({"version":1,"keep":plan.keep,"removed":plan.removed,"duration":plan.duration_us});
    }
    let key = storage::hash(&identity)?;
    let path = dir.join(format!("quality-{key}.json"));
    if path.metadata().is_ok_and(|m| m.len() < 64 * 1024) {
        if let Ok(bytes) = fs::read(&path) {
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if value["key"] == key {
                    if let Ok(q) = serde_json::from_value::<Quality>(value["quality"].clone()) {
                        if q.calibrated
                            && q.quantizer <= 40
                            && q.estimated_bitrate > 0
                            && q.measured_ssim >= 0.985
                            && q.minimum_frame_ssim >= 0.95
                            && q.sample_frames > 0
                            && value["digest"] == storage::hash(&q)?
                        {
                            (r.progress)("CACHE_HIT: 复用已验证的短片编码校准", None);
                            return Ok(q);
                        }
                    }
                }
            }
        }
    }
    r.measure(
        "encoderCalibration",
        0,
        None,
        "sample",
        "校准高质量压缩参数",
    );
    let windows = sample_windows(p);
    let mut q = initial(p, f, codec);
    for attempt in 0..3 {
        let mut bytes = 0u64;
        let mut duration = 0i64;
        let mut weighted_ssim = 0.;
        let mut frames = 0usize;
        let mut worst = 1.0f64;
        for (start, end) in &windows {
            r.check()?;
            let video_filter = sample_filter(p, f, *start, *end, edited)?;
            let sample = storage::temp_file(&dir, "quality-sample")?;
            let estimated = ((q.max_bitrate.max(f.source_video_bitrate) * 3 / 2) as f64
                * (*end - *start) as f64
                / SECOND as f64
                / 8.) as u64
                + 2 * 1024 * 1024;
            let _disk = r.reserve_disk(sample.path(), estimated, "编码校准样本")?;
            let mut cmd = args(&["-hide_banner", "-v", "warning", "-y"]);
            cmd.extend(sample_input(p, *start, *end));
            cmd.extend(args(&[
                "-map",
                &format!("0:{}", p.video_index),
                "-vf",
                &video_filter,
                "-an",
                "-c:v",
                codec,
            ]));
            cmd.extend(options(r, codec, &q));
            cmd.extend(args(&[
                "-pix_fmt",
                &f.pixel_format,
                "-fps_mode",
                "passthrough",
                "-enc_time_base:v",
                "1:1000000",
                "-map_metadata",
                "-1",
                "-f",
                "matroska",
                &sample.path().to_string_lossy(),
            ]));
            r.ff(cmd)?;
            let mut compare = args(&["-hide_banner", "-v", "info"]);
            compare.extend(sample_input(p, *start, *end));
            compare.extend(args(&["-i",&sample.path().to_string_lossy(),"-filter_complex_threads",&r.threads().min(2).to_string(),"-filter_complex",&format!("[0:{}]{video_filter},setpts=N[ref];[1:v:0]format={},settb=1/1000000,setpts=N[dist];[ref][dist]ssim=stats_file=-:shortest=1:repeatlast=0[v]",p.video_index,f.pixel_format),"-map","[v]","-an","-fps_mode","passthrough","-f","null","-"]));
            let (stats, stderr) = r.run(false, &compare, 2 * 1024 * 1024)?;
            let values: Vec<_> = String::from_utf8_lossy(&stats)
                .lines()
                .filter_map(all_value)
                .collect();
            let expected = p
                .source
                .frame_pts
                .iter()
                .filter(|t| {
                    **t >= *start
                        && **t < *end
                        && edited.is_none_or(|plan| {
                            plan.keep
                                .iter()
                                .any(|k| **t >= k.start_us && **t < k.end_us)
                        })
                })
                .count();
            if values.is_empty() || values.len() != expected {
                return Err(format!(
                    "QUALITY_SAMPLE: 校准帧未完整对齐，预期 {expected}，实际 {}；{}",
                    values.len(),
                    stderr
                        .chars()
                        .rev()
                        .take(300)
                        .collect::<String>()
                        .chars()
                        .rev()
                        .collect::<String>()
                ));
            }
            weighted_ssim += values.iter().sum::<f64>();
            frames += values.len();
            worst = worst.min(values.into_iter().fold(1., f64::min));
            bytes += fs::metadata(sample.path())
                .map_err(|e| e.to_string())?
                .len();
            duration += edited.map_or(end - start, |plan| plan.duration_us);
        }
        q.measured_ssim = weighted_ssim / frames.max(1) as f64;
        q.minimum_frame_ssim = worst;
        q.sample_frames = frames;
        q.sample_starts = windows.iter().map(|w| w.0).collect();
        q.estimated_bitrate = (bytes as f64 * 8. * SECOND as f64 / duration.max(1) as f64) as u64;
        q.calibrated = true;
        (r.progress)(&format!("QUALITY_SAMPLE: {codec}; q={}; maxrate={}; sampleRate={}; SSIM={:.6}; minimum={:.6}; frames={frames}",q.quantizer,q.max_bitrate,q.estimated_bitrate,q.measured_ssim,worst),None);
        if q.measured_ssim >= 0.985 && worst >= 0.95 {
            let data = json!({"key":key,"quality":q,"digest":storage::hash(&q)?});
            storage::atomic(
                &path,
                &serde_json::to_vec(&data).map_err(|e| e.to_string())?,
            )?;
            return Ok(q);
        }
        if attempt < 2 {
            q.quantizer = q.quantizer.saturating_sub(2);
            q.max_bitrate = if attempt == 0 {
                initial(p, f, codec).max_bitrate.saturating_mul(2)
            } else {
                0
            };
            (r.progress)(
                "WARN: 短片画质未达初始门槛，放宽体积约束后重新校准；不会反复整片编码。",
                None,
            );
        }
    }
    Err("QUALITY_SAMPLE: 代表片段未通过自动画质检查，未开始正式成片；原素材和已有输出已保留".into())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lossless_worker_limits_never_change_quality_controls() {
        for codec in ["libx264", "libx265", "libvpx-vp9", "libaom-av1", "ffv1"] {
            let normalize = |workers| {
                lossless_options(workers, codec)
                    .unwrap()
                    .into_iter()
                    .map(|x| {
                        if x.starts_with("lossless=1:pools=") {
                            "lossless=1:worker-controls:log-level=error".into()
                        } else {
                            x
                        }
                    })
                    .skip(2)
                    .collect::<Vec<_>>()
            };
            for n in [2, 4, 8, 16] {
                assert_eq!(normalize(1), normalize(n), "{codec}");
            }
        }
        assert!(lossless_options(1, "libvpx").is_err());
        let q = Quality {
            quantizer: 18,
            max_bitrate: 0,
            estimated_bitrate: 0,
            measured_ssim: 0.,
            minimum_frame_ssim: 0.,
            sample_frames: 0,
            sample_starts: vec![],
            calibrated: false,
            av1_level: "5.1".into(),
        };
        for codec in ["h264_nvenc", "hevc_nvenc", "av1_nvenc"] {
            let a = options_with_threads(1, codec, &q);
            let b = options_with_threads(16, codec, &q);
            // FFmpeg's generic thread limit is appended after quality options.
            assert_eq!(&a[..a.len() - 2], &b[..b.len() - 2]);
            assert_eq!(a[a.len() - 2], "-threads");
        }
    }
    #[test]
    fn resource_and_output_failures_never_trigger_encoder_retry() {
        for e in [
            "RESOURCE_MEMORY: not supported",
            "RESOURCE_DISK: error while opening encoder",
            "CANCELLED: 用户取消",
            "out of memory: OpenEncodeSessionEx failed",
            "OUTPUT_COLOR: not supported",
            "QUALITY_SAMPLE: not supported",
        ] {
            assert!(!initialization_failure(e), "{e}")
        }
        assert!(initialization_failure(
            "Error while opening encoder: unsupported format"
        ));
    }
    #[test]
    fn ssim_parser_rejects_invalid_measurements() {
        assert_eq!(all_value("n:1 Y:0.9 All:0.98765 (19.2)"), Some(0.98765));
        assert_eq!(all_value("All:nan"), None);
        assert_eq!(all_value("unrelated"), None);
    }
}
