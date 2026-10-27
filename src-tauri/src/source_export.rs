use crate::{media, model::*, process::*};
use serde_json::Value;

pub fn family(codec: &str) -> &str {
    match codec {
        "h264" | "hevc" | "av1" | "vp9" => codec,
        _ => "ffv1",
    }
}
fn source_video(p: &Project) -> Option<&Stream> {
    p.source
        .streams
        .iter()
        .find(|s| s.index == p.video_index && s.kind == "video")
}
pub fn multichannel_layout(p: &Project) -> Result<Option<String>, String> {
    let stream = p
        .audio_index
        .or(p.source.audio_index)
        .and_then(|ai| p.source.streams.iter().find(|s| s.index == ai));
    let Some(s) = stream.filter(|s| s.channels > 2) else {
        return Ok(None);
    };
    if s.channels > 8 {
        return Err("SOURCE_AUDIO: 暂不支持超过 8 声道的压缩音频".into());
    }
    let layout = if s.channel_layout.is_empty() {
        match s.channels {
            3 => "2.1",
            4 => "4.0",
            5 => "5.0",
            6 => "5.1",
            7 => "6.1",
            8 => "7.1",
            _ => return Err("SOURCE_AUDIO: 声道数无效".into()),
        }
        .to_string()
    } else {
        s.channel_layout.clone()
    };
    if layout.len() > 96
        || !layout
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+-.()_ ".contains(c))
    {
        return Err("SOURCE_AUDIO: 无法安全解析源声道布局".into());
    }
    Ok(Some(layout))
}
fn constant_frame_timing(p: &Project) -> bool {
    let Some((num, den)) = source_video(p).and_then(|s| s.rate.split_once('/')) else {
        return false;
    };
    let (Ok(num), Ok(den)) = (num.parse::<u64>(), den.parse::<u64>()) else {
        return false;
    };
    if num == 0 || den == 0 {
        return false;
    }
    let first = p.source.frame_pts.first().copied().unwrap_or(0) as i128;
    p.source.frame_pts.iter().enumerate().all(|(i, t)| {
        let expected = (i as i128 * 1_000_000 * den as i128 + num as i128 / 2) / num as i128;
        (*t as i128 - first - expected).abs() <= 1
    })
}
/// The source, not saved export preferences, determines the new automatic policy.
pub fn extension(p: &Project) -> String {
    let ext = media::extension(&p.source.fingerprint.path);
    let codec = family(source_video(p).map(|s| s.codec.as_str()).unwrap_or(""));
    if video_encoder(&ext, codec).is_ok() {
        ext
    } else if ["h264", "hevc"].contains(&codec) {
        "mp4".into()
    } else {
        "mkv".into()
    }
}
pub fn change_reason(p: &Project) -> Option<String> {
    let ext = media::extension(&p.source.fingerprint.path);
    let target = extension(p);
    let source_codec = source_video(p)?.codec.as_str();
    let codec = family(source_codec);
    let label = |value: &str| {
        if value == "webm" {
            "WebM".to_string()
        } else {
            value.to_uppercase()
        }
    };
    let mut changes = vec![];
    let mut reasons = vec![];
    if ext != target {
        changes.push(format!("{} → {}", label(&ext), label(&target)))
    }
    if source_codec != codec {
        changes.push(format!("{} → {}", label(source_codec), label(codec)));
        reasons.push("原视频编码不支持当前无损剪辑路径，改用 FFV1 保留画面像素".to_string())
    }
    if ext != target {
        if ["avi", "wmv", "flv"].contains(&ext.as_str()) {
            reasons.push("原封装不支持所需的无损编码与精确帧时序".into())
        } else {
            reasons.push(format!("原封装不支持 {} 无损画面编码", label(codec)))
        }
    }
    if changes.is_empty() {
        None
    } else {
        Some(format!("{}：{}", changes.join("，"), reasons.join("；")))
    }
}
pub fn muxer(ext: &str) -> &str {
    match ext {
        "mkv" => "matroska",
        "ts" | "mts" | "m2ts" => "mpegts",
        _ => ext,
    }
}
pub fn video_encoder(ext: &str, codec: &str) -> Result<&'static str, String> {
    let allowed = match ext {
        "mp4" => ["h264", "hevc", "av1", "vp9"].contains(&codec),
        "mov" => ["h264", "hevc"].contains(&codec),
        "webm" => ["vp8", "vp9", "av1"].contains(&codec),
        "mkv" => ["h264", "hevc", "av1", "vp9", "ffv1"].contains(&codec),
        "ts" | "mts" | "m2ts" => ["h264", "hevc"].contains(&codec),
        _ => false,
    };
    if !allowed {
        return Err(format!(
            "SOURCE_FORMAT: 暂不支持按原格式导出 {ext} / {codec}"
        ));
    }
    match codec {
        "ffv1" => Ok("ffv1"),
        "h264" => Ok("libx264"),
        "hevc" => Ok("libx265"),
        "av1" => Ok("libaom-av1"),
        "vp9" => Ok("libvpx-vp9"),
        "vp8" => Ok("libvpx"),
        _ => Err(format!("SOURCE_CODEC: 暂不支持按原视频编码导出 {codec}")),
    }
}
/// Applied at the engine boundary, including requests from old/custom projects.
pub fn lock(p: &mut Project) -> Result<(), String> {
    let codec = family(&source_video(p).ok_or("源视频流不存在")?.codec).to_string();
    let ext = extension(p);
    video_encoder(&ext, &codec)?;
    p.export.container = ext;
    p.export.codec = codec;
    p.export.quality = "lossless".into();
    p.export.acceleration = Some("auto".into());
    p.export.width = None;
    p.export.height = None;
    p.export.fps = None;
    Ok(())
}
#[derive(Clone)]
pub struct Format {
    pub video_codec: String,
    pub width: u32,
    pub height: u32,
    pub rate: String,
    pub cfr: bool,
    pub pixel_format: String,
    pub sar: (u64, u64),
    pub colors: Vec<(String, String)>,
    pub audio_codec: String,
    pub audio_encoder: String,
    pub audio_layout: Option<String>,
    pub audio_bitrate: u64,
    pub sample_rate: u32,
    pub channels: u32,
    pub reference_bytes: u64,
    pub reference_is_estimate: bool,
    pub source_video_bitrate: u64,
}
fn aspect(value: &Value) -> (u64, u64) {
    let p: Vec<u64> = value
        .as_str()
        .unwrap_or("1:1")
        .split(':')
        .filter_map(|s| s.parse().ok())
        .collect();
    if p.len() == 2 && p[0] > 0 && p[1] > 0 {
        (p[0], p[1])
    } else {
        (1, 1)
    }
}
fn number(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}
fn pixel_format(codec: &str, source: &str, quarter_turn: bool, preview: bool) -> String {
    let depth = if source.contains("16") {
        16
    } else if source.contains("14") {
        14
    } else if source.contains("12") {
        12
    } else if source.contains("10") || source.starts_with("p010") {
        10
    } else {
        8
    };
    let full = source.starts_with("yuvj");
    if preview || codec == "vp8" {
        return if full { "yuvj420p" } else { "yuv420p" }.into();
    }
    let depth = depth.min(if codec == "h264" { 10 } else { 12 });
    let chroma = if source.contains("444")
        || source.starts_with("gbr")
        || source.starts_with("rgb")
        || source.starts_with("bgr")
        || quarter_turn
            && (source.contains("422")
                || source.contains("440")
                || source.contains("411")
                || source.starts_with("nv16")
                || source.starts_with("p210"))
    {
        "444"
    } else if source.contains("422") || source.starts_with("nv16") || source.starts_with("p210") {
        "422"
    } else {
        "420"
    };
    if depth > 8 {
        format!("yuv{chroma}p{depth}le")
    } else if full {
        format!("yuvj{chroma}p")
    } else {
        format!("yuv{chroma}p")
    }
}
pub fn read(r: &Runner, p: &Project) -> Result<Format, String> {
    read_format(r, p, false)
}
pub const QUICK_PREVIEW_EDGE: u32 = 960;
pub fn preview_dimensions(width: u32, height: u32, sar: (u64, u64)) -> (u32, u32) {
    let display_width = width as f64 * sar.0 as f64 / sar.1.max(1) as f64;
    let factor = (QUICK_PREVIEW_EDGE as f64 / display_width.max(height as f64).max(1.)).min(1.);
    (((display_width * factor / 2. + 0.000001).floor() as u32 * 2).max(2),
     ((height as f64 * factor / 2. + 0.000001).floor() as u32 * 2).max(2))
}
pub fn read_preview(r: &Runner, p: &Project) -> Result<Format, String> {
    let mut format = read_format(r, p, true)?;
    (format.width, format.height) = preview_dimensions(format.width, format.height, format.sar);
    format.sar = (1, 1);
    Ok(format)
}
fn read_format(r: &Runner, p: &Project, preview: bool) -> Result<Format, String> {
    let (data, _) = r.run(
        true,
        &args(&[
            "-v",
            "error",
            "-show_streams",
            "-of",
            "json",
            &p.source.fingerprint.path,
        ]),
        4 * 1024 * 1024,
    )?;
    let value: Value = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
    let streams = value["streams"].as_array().ok_or("源格式信息缺失")?;
    let v = streams
        .iter()
        .find(|s| s["index"].as_u64() == Some(p.video_index as u64))
        .ok_or("源视频流不存在")?;
    let rotation = v["side_data_list"]
        .as_array()
        .and_then(|a| a.iter().find_map(|s| s["rotation"].as_f64()))
        .or_else(|| v["tags"]["rotate"].as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0.);
    let mut width = v["width"].as_u64().unwrap_or(0) as u32;
    let mut height = v["height"].as_u64().unwrap_or(0) as u32;
    if width == 0 || height == 0 {
        return Err("SOURCE_FORMAT: 源画面尺寸无效".into());
    }
    let mut sar = aspect(&v["sample_aspect_ratio"]);
    let quarter_turn = rotation.round().abs() as i32 % 180 == 90;
    if quarter_turn {
        std::mem::swap(&mut width, &mut height);
        std::mem::swap(&mut sar.0, &mut sar.1)
    }
    let source_pixel = v["pix_fmt"].as_str().ok_or("无法读取源像素格式")?;
    let codec = if preview {
        "h264"
    } else {
        family(v["codec_name"].as_str().unwrap_or(""))
    };
    let pixel_format = if preview {
        pixel_format(codec, source_pixel, quarter_turn, true)
    } else {
        let supported = match codec {
            "h264" => [
                "yuv420p",
                "yuv422p",
                "yuv444p",
                "yuvj420p",
                "yuvj422p",
                "yuvj444p",
                "yuv420p10le",
                "yuv422p10le",
                "yuv444p10le",
                "gray",
                "gray10le",
            ]
            .contains(&source_pixel),
            "hevc" | "vp9" | "av1" => [
                "yuv420p",
                "yuv422p",
                "yuv444p",
                "yuv420p10le",
                "yuv422p10le",
                "yuv444p10le",
                "yuv420p12le",
                "yuv422p12le",
                "yuv444p12le",
            ]
            .contains(&source_pixel),
            "ffv1" => [
                "yuv420p",
                "yuv422p",
                "yuv444p",
                "yuv420p10le",
                "yuv422p10le",
                "yuv444p10le",
                "yuv420p12le",
                "yuv422p12le",
                "yuv444p12le",
                "yuv420p16le",
                "yuv422p16le",
                "yuv444p16le",
                "bgr0",
                "bgra",
                "gbrp9le",
                "gbrp10le",
                "gbrp12le",
                "gbrp16le",
                "gray",
                "gray16le",
            ]
            .contains(&source_pixel),
            _ => false,
        };
        if !supported
            || quarter_turn
                && (source_pixel.contains("422")
                    || source_pixel.contains("440")
                    || source_pixel.contains("411"))
        {
            return Err(format!("SOURCE_LOSSLESS: 暂无法保证此像素格式与旋转组合的画质不变（{source_pixel} / {rotation}°）；已停止导出，原文件与项目保留"));
        }
        source_pixel.into()
    };
    if pixel_format != source_pixel {
        (r.progress)(
            &format!(
                "WARN: 为兼容{}，像素格式 {} 转为 {}；保持源分辨率、方向与帧时序。",
                if preview {
                    "预览播放器"
                } else {
                    "高质量压缩编码"
                },
                source_pixel,
                pixel_format
            ),
            None,
        )
    }
    if !preview && rotation.rem_euclid(90.).min(90. - rotation.rem_euclid(90.)) > 0.001 {
        return Err("SOURCE_LOSSLESS: 非直角旋转需要插值，无法保证像素不变，已停止导出".into());
    }
    if rotation.rem_euclid(90.).min(90. - rotation.rem_euclid(90.)) > 0.001 {
        (r.progress)("WARN: 源片含非直角旋转，按源显示方向插值显示。", None)
    }
    let mut colors: Vec<(String, String)> = [
        ("color_range", "-color_range"),
        ("color_space", "-colorspace"),
        ("color_primaries", "-color_primaries"),
        ("color_transfer", "-color_trc"),
        ("chroma_location", "-chroma_sample_location"),
    ]
    .into_iter()
    .filter_map(|(field, flag)| {
        v[field]
            .as_str()
            .filter(|s| !s.is_empty() && !matches!(*s, "unknown" | "unspecified" | "reserved"))
            .map(|s| (flag.to_string(), s.to_string()))
    })
    .collect();
    if preview && colors.iter().any(|(f, v)| f == "-colorspace" && v == "gbr") {
        colors.retain(|(f, _)| f != "-colorspace");
        colors.push(("-colorspace".into(), "bt709".into()));
        (r.progress)("WARN: RGB 源片转换为 BT.709 YUV 高质量压缩。", None)
    }
    let ai = p.audio_index.or(p.source.audio_index);
    let a = if p.audio_index.is_some() || !p.music.is_empty() {
        streams
            .iter()
            .find(|s| s["index"].as_u64() == ai.map(u64::from))
    } else {
        None
    };
    let source_rate = a
        .and_then(|s| number(&s["sample_rate"]))
        .filter(|n| *n > 0)
        .unwrap_or(48000) as u32;
    let channels = a
        .and_then(|s| s["channels"].as_u64())
        .filter(|n| *n > 0)
        .unwrap_or(2) as u32;
    let audio_layout = multichannel_layout(p)?;
    if let Some(layout) = &audio_layout {
        let actual = a
            .and_then(|s| s["channel_layout"].as_str())
            .unwrap_or("未知");
        if actual != layout {
            (r.progress)(&format!("WARN: 旧工程未完整记录声道布局，{} 按 {} 兼容映射，保留 {} 个声道；建议重新导入可恢复源布局。",actual,layout,channels),None)
        }
    }
    if channels > 8 {
        return Err("SOURCE_AUDIO: 暂不支持超过 8 声道的压缩音频".into());
    }
    let opus = !preview && extension(p) == "webm";
    let sample_rate = if opus {
        48000
    } else if [
        7350, 8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000, 64000, 88200, 96000,
    ]
    .contains(&source_rate)
    {
        source_rate
    } else {
        48000
    };
    if sample_rate != source_rate {
        (r.progress)(
            &format!("WARN: 为兼容压缩音频，采样率 {source_rate} 转为 {sample_rate} Hz。"),
            None,
        )
    }
    let audio_bitrate = if opus {
        if channels == 1 {
            128000
        } else {
            (channels as u64 * 96000).min(512000)
        }
    } else {
        if channels == 1 {
            192000
        } else {
            (channels as u64 * 160000).min(1024000)
        }
    };
    // Packet counts are not rescanned during export. Only exact byte tags are
    // treated as effective bytes; source file size remains an explicit estimate.
    let media_streams: Vec<_> = streams
        .iter()
        .filter(|s| matches!(s["codec_type"].as_str(), Some("video" | "audio")))
        .collect();
    let tagged: Option<Vec<u64>> = media_streams
        .iter()
        .map(|s| {
            number(&s["tags"]["NUMBER_OF_BYTES"])
                .or_else(|| number(&s["tags"]["NUMBER_OF_BYTES-eng"]))
        })
        .collect();
    let exact_bytes = tagged
        .filter(|v| !v.is_empty())
        .map(|v| v.into_iter().sum::<u64>())
        .filter(|n| *n > 0 && *n <= p.source.fingerprint.size);
    let reference_bytes = exact_bytes.unwrap_or(p.source.fingerprint.size);
    let estimated_rate =
        (reference_bytes as f64 * 8. * SECOND as f64 / p.source.duration_us.max(1) as f64) as u64;
    let source_video_bitrate = number(&v["bit_rate"])
        .filter(|n| *n > 0)
        .unwrap_or(estimated_rate.saturating_sub(audio_bitrate))
        .max(64000);
    Ok(Format {
        video_codec: codec.into(),
        width,
        height,
        rate: v["r_frame_rate"].as_str().unwrap_or("30/1").into(),
        cfr: constant_frame_timing(p),
        pixel_format,
        sar,
        colors,
        audio_codec: if opus { "opus" } else { "aac" }.into(),
        audio_encoder: if opus { "libopus" } else { "aac" }.into(),
        audio_layout,
        audio_bitrate,
        sample_rate,
        channels,
        reference_bytes,
        reference_is_estimate: exact_bytes.is_none(),
        source_video_bitrate,
    })
}
pub fn verify(format: &Format, streams: &[Value], has_audio: bool) -> Result<(), String> {
    let v = streams
        .iter()
        .find(|s| s["codec_type"] == "video")
        .ok_or("OUTPUT_FORMAT: 导出缺少视频流")?;
    if v["codec_name"] != format.video_codec
        || v["width"] != format.width
        || v["height"] != format.height
        || v["pix_fmt"] != format.pixel_format
    {
        return Err(format!("OUTPUT_FORMAT: 导出的编码、像素格式或画面尺寸不符，预期 {} / {} / {}×{}，实际 {} / {} / {}×{}",format.video_codec,format.pixel_format,format.width,format.height,v["codec_name"],v["pix_fmt"],v["width"],v["height"]));
    }
    let sar = aspect(&v["sample_aspect_ratio"]);
    if sar.0 as u128 * format.sar.1 as u128 != sar.1 as u128 * format.sar.0 as u128 {
        return Err("OUTPUT_FORMAT: 导出像素宽高比不符".into());
    }
    for (flag, expected) in &format.colors {
        let field = match flag.as_str() {
            "-color_range" => "color_range",
            "-colorspace" => "color_space",
            "-color_primaries" => "color_primaries",
            "-color_trc" => "color_transfer",
            _ => continue,
        };
        if v[field].as_str() != Some(expected) {
            return Err(format!("OUTPUT_COLOR: {field} 未保留源色彩信息"));
        }
    }
    if has_audio {
        let a = streams
            .iter()
            .find(|s| s["codec_type"] == "audio")
            .ok_or("OUTPUT_FORMAT: 导出缺少声音")?;
        if a["codec_name"] != format.audio_codec
            || a["channels"] != format.channels
            || number(&a["sample_rate"]) != Some(format.sample_rate as u64)
        {
            return Err("OUTPUT_FORMAT: 压缩音频格式不符".into());
        }
        // AAC/Opus encoder priming must be represented by the container, not by
        // shifting the edit timeline. Small codec padding is checked separately.
        let start = a["start_time"]
            .as_str()
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.);
        let video_start = v["start_time"]
            .as_str()
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.);
        if (start - video_start).abs() > 0.040001 {
            return Err(format!(
                "AV_START: 新增音画起点偏移 {:.3} ms",
                (start - video_start) * 1000.
            ));
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(ext: &str, codec: &str, audio: bool) -> Project {
        let mut p = crate::timeline::tests::project();
        p.source.fingerprint.path = format!("fixture.{ext}");
        p.source.streams[0].codec = codec.into();
        if audio {
            let mut a = p.source.streams[0].clone();
            a.index = 1;
            a.kind = "audio".into();
            a.codec = "aac".into();
            a.channels = 2;
            p.source.streams.push(a);
            p.audio_index = Some(1);
            p.source.audio_index = Some(1)
        }
        p
    }
    #[test]
    fn compressed_source_families_are_preserved_when_compatible() {
        for (ext, codec, target) in [
            ("mp4", "h264", "mp4"),
            ("mov", "hevc", "mov"),
            ("mp4", "av1", "mp4"),
            ("mp4", "vp9", "mp4"),
            ("webm", "vp9", "webm"),
            ("mov", "av1", "mkv"),
            ("ts", "hevc", "ts"),
        ] {
            let mut p = fixture(ext, codec, true);
            lock(&mut p).unwrap();
            assert_eq!(
                (
                    p.export.container.as_str(),
                    p.export.codec.as_str(),
                    p.export.quality.as_str()
                ),
                (target, codec, "lossless")
            );
        }
    }
    #[test]
    fn legacy_codecs_use_lossless_ffv1() {
        for (ext, codec, target) in [
            ("mp4", "mpeg4", "mkv"),
            ("avi", "mpeg4", "mkv"),
            ("mkv", "ffv1", "mkv"),
            ("avi", "huffyuv", "mkv"),
            ("wmv", "wmv2", "mkv"),
            ("flv", "flv1", "mkv"),
            ("webm", "vp8", "mkv"),
        ] {
            let mut p = fixture(ext, codec, true);
            lock(&mut p).unwrap();
            assert_eq!(
                (p.export.container.as_str(), p.export.codec.as_str()),
                (target, "ffv1")
            );
        }
    }
    #[test]
    fn old_lossless_projects_use_new_policy() {
        let mut p = fixture("mp4", "h264", true);
        p.export.quality = "lossless".into();
        p.export.codec = "ffv1".into();
        p.export.width = Some(1280);
        p.export.fps = Some(24);
        lock(&mut p).unwrap();
        assert_eq!(p.export.quality, "lossless");
        assert_eq!(p.export.codec, "h264");
        assert!(p.export.width.is_none() && p.export.fps.is_none());
        p.video_index = 99;
        assert!(lock(&mut p).is_err());
    }
    #[test]
    fn audio_does_not_force_webm_container_changes() {
        let mut p = fixture("webm", "vp9", true);
        assert_eq!(extension(&p), "webm");
        p.audio_index = None;
        assert_eq!(extension(&p), "webm");
        p.music.push(Music {
            id: "music".into(),
            media: p.source.clone(),
        });
        assert_eq!(extension(&p), "webm");
    }
    #[test]
    fn format_change_notices_explain_compression_policy() {
        assert!(change_reason(&fixture("mp4", "h264", true)).is_none());
        assert!(change_reason(&fixture("webm", "vp9", true)).is_none());
        assert_eq!(
            change_reason(&fixture("mp4", "mpeg4", true)).unwrap(),
            "MP4 → MKV，MPEG4 → FFV1：原视频编码不支持当前无损剪辑路径，改用 FFV1 保留画面像素；原封装不支持 FFV1 无损画面编码"
        );
        assert_eq!(change_reason(&fixture("avi","mpeg4",true)).unwrap(),"AVI → MKV，MPEG4 → FFV1：原视频编码不支持当前无损剪辑路径，改用 FFV1 保留画面像素；原封装不支持所需的无损编码与精确帧时序");
        assert_eq!(
            change_reason(&fixture("mov", "av1", false)).unwrap(),
            "MOV → MKV：原封装不支持 AV1 无损画面编码"
        );
    }
    #[test]
    fn bitdepth_chroma_and_preview_compatibility_are_explicit() {
        assert_eq!(
            pixel_format("h264", "yuv420p10le", false, false),
            "yuv420p10le"
        );
        assert_eq!(
            pixel_format("hevc", "yuv444p12le", false, false),
            "yuv444p12le"
        );
        assert_eq!(pixel_format("h264", "yuv422p", true, false), "yuv444p");
        assert_eq!(pixel_format("h264", "gbrp", false, false), "yuv444p");
        assert_eq!(pixel_format("h264", "yuv420p10le", false, true), "yuv420p");
        assert_eq!(pixel_format("vp8", "yuv420p10le", false, false), "yuv420p");
    }
    #[test]
    fn quick_preview_bounds_display_pixels_without_upscaling_normal_sources() {
        assert_eq!(preview_dimensions(1920,1080,(1,1)),(960,540));
        assert_eq!(preview_dimensions(1080,1920,(1,1)),(540,960));
        assert_eq!(preview_dimensions(320,180,(1,1)),(320,180));
        assert_eq!(preview_dimensions(720,576,(16,15)),(768,576));
        assert_eq!(preview_dimensions(641,359,(1,1)),(640,358));
        assert_eq!(preview_dimensions(839,1080,(4,3)),(960,926));
    }
}
