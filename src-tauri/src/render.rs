use crate::{media, model::*, process::*};
use std::{fs, path::Path};
pub fn sec(t: Us) -> String {
    format!("{:.6}", t as f64 / SECOND as f64)
}
pub fn container(p: &Project) -> String {
    crate::source_export::extension(p)
}
pub fn codec_for(container: &str, codec: &str) -> Result<&'static str, String> {
    match (container, codec) {
        ("webm", "auto" | "vp9") => Ok("libvpx-vp9"),
        ("webm" | "mkv", "av1") => Ok("libaom-av1"),
        ("mp4" | "mkv", "hevc") => Ok("libx265"),
        ("mp4" | "mkv" | "mov", "auto" | "h264") => Ok("libx264"),
        _ => Err("容器与编码组合不兼容".into()),
    }
}
fn retained_spans(plan: &Plan, range: (Us, Us)) -> Vec<(Us, Us, Us)> {
    let (start, end) = range;
    plan.keep
        .iter()
        .filter_map(|k| {
            let a = start.max(k.output_us);
            let b = end.min(k.output_us + k.end_us - k.start_us);
            if b > a {
                Some((
                    k.start_us + a - k.output_us,
                    k.start_us + b - k.output_us,
                    a - start,
                ))
            } else {
                None
            }
        })
        .collect()
}
// Share exact frame selection with short-clip quality calibration. Encoding an
// uncut source can hide a quality drop after the cut changes temporal context.
pub(crate) fn video_selection(p: &Project, plan: &Plan, range: (Us, Us)) -> Result<String, String> {
    let (start, end) = range;
    if end <= start || start < 0 || end > plan.duration_us {
        return Err("无有效可渲染区间".into());
    }
    let spans = retained_spans(plan, range);
    let selection = spans
        .iter()
        .enumerate()
        .map(|(i, (s, e, _))| {
            let from = if i == 0 {
                let keep = plan.keep.iter().find(|k| *s >= k.start_us && *s < k.end_us);
                let previous = p
                    .source
                    .frame_pts
                    .iter()
                    .copied()
                    .take_while(|t| *t <= *s)
                    .last()
                    .unwrap_or(*s);
                keep.map(|k| previous.max(k.start_us)).unwrap_or(*s)
            } else {
                *s
            };
            format!("gte(pts,{from})*lt(pts,{e})")
        })
        .collect::<Vec<_>>()
        .join("+");
    let subtraction = plan
        .removed
        .iter()
        .map(|c| format!("-gte(PTS,{})*({})", c.end_us, c.end_us - c.start_us))
        .collect::<Vec<_>>()
        .join("");
    // Mapping uses actual PTS and known cut durations. Video concat alone would
    // guess a VFR segment's last frame duration and could accumulate drift.
    Ok(format!(
        "settb=expr=1/1000000,setpts=PTS-{},select='{selection}',setpts='max(0,PTS{subtraction}-{})'",
        p.source.origin_us,
        start
    ))
}
// Both local proxies and exports consume this immutable plan. Output coordinates
// are cropped only after global tail/music positions have been resolved.
pub fn filter(
    p: &Project,
    plan: &Plan,
    range: (Us, Us),
    preview: bool,
) -> Result<(String, bool), String> {
    let (start, end) = range;
    let duration = end - start;
    let selection = video_selection(p, plan, range)?;
    let spans = retained_spans(plan, range);
    let mut graph = vec![format!("[0:{}]{selection}[vcat]", p.video_index)];
    let display = "scale=w='round(iw*sar)':h=ih,setsar=1";
    let scale = if preview {
        let edge = crate::source_export::QUICK_PREVIEW_EDGE;
        // Preserve displayed proportions, bound pixel work, and retain every PTS.
        format!("scale=w='max(2,trunc(iw*sar*min(1,{edge}/max(iw*sar,ih))/2+0.000001)*2)':h='max(2,trunc(ih*min(1,{edge}/max(iw*sar,ih))/2+0.000001)*2)':flags=fast_bilinear,setsar=1")
    } else if let (Some(w), Some(h)) = (p.export.width, p.export.height) {
        if w < 2 || h < 2 || w > 16384 || h > 16384 {
            return Err("输出尺寸无效".into());
        }
        format!("{display},scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2")
    } else {
        // Keep the source raster and sample aspect ratio for automatic encoding.
        "null".into()
    };
    let mut vf = scale;
    if let Some(fps) = p.export.fps.filter(|_| !preview) {
        if ![24, 25, 30, 50, 60].contains(&fps) {
            return Err("不支持的固定帧率".into());
        }
        vf.push_str(&format!(",fps={fps}"));
    }
    graph.push(format!("[vcat]{vf}[vout]"));
    let mut audio = vec![];
    let multichannel_layout = crate::source_export::multichannel_layout(p)?;
    if let Some(ai) = p.audio_index {
        let game_channels = p
            .source
            .streams
            .iter()
            .find(|s| s.index == ai)
            .map(|s| s.channels)
            .unwrap_or(2)
            .max(1);
        // Retain the decoded source layout for multichannel material. Mono and
        // stereo keep the established mixing path, while FFmpeg maps stereo
        // background music into the first (source-game) input's native layout.
        let channel_constraint = if let Some(layout) = &multichannel_layout {
            format!(":channel_layouts='{layout}'")
        } else {
            ":channel_layouts=stereo".to_string()
        };
        let count = spans.len()
            + plan
                .tails
                .iter()
                .filter(|x| x.output_us < end && x.output_us + x.duration_us > start)
                .count();
        let labels = (0..count)
            .map(|i| format!("[as{i}]"))
            .collect::<Vec<_>>()
            .join("");
        graph.push(format!("[0:{ai}]asetpts=PTS-({})/TB,aresample=48000:async=1:min_comp=0:min_hard_comp=0:first_pts=0,aformat=sample_fmts=fltp:sample_rates=48000{channel_constraint},asplit={count}{labels}",sec(p.source.origin_us)));
        let mut base = vec![];
        for (i, (s, e, _)) in spans.iter().enumerate() {
            let len = e - s;
            let fade = 5000.min(len / 2);
            let bridge = plan
                .tails
                .iter()
                .any(|t| t.mode == "natural" && t.source_us == *e);
            let fade_out = if bridge { 0 } else { fade };
            graph.push(format!("[as{i}]atrim=start={}:end={},asetpts=PTS-STARTPTS,apad,atrim=duration={},afade=t=in:d={},afade=t=out:st={}:d={}[a{i}]",sec(*s),sec(*e),sec(len),sec(fade),sec(len-fade_out),sec(fade_out)));
            base.push(format!("[a{i}]"));
        }
        graph.push(format!(
            "{}concat=n={}:v=0:a=1,volume={}[game]",
            base.join(""),
            spans.len(),
            p.mix.game_gain
        ));
        audio.push("[game]".to_string());
        for (j, t) in plan
            .tails
            .iter()
            .filter(|x| x.output_us < end && x.output_us + x.duration_us > start)
            .enumerate()
        {
            let offset = (start - t.output_us).max(0);
            let len = t.duration_us.min(end - t.output_us) - offset;
            let delay = (t.output_us - start).max(0);
            let fade = 80000.min(t.duration_us / 2);
            let label = format!("tail{j}");
            let envelope = if t.mode == "natural" && !t.envelope.is_empty() {
                let mut expr = t.envelope[0].gain.to_string();
                for pair in t.envelope.windows(2) {
                    let (a, b) = (&pair[0], &pair[1]);
                    expr.push_str(&format!(
                        "+({})*clip((n/s-{})/{},0,1)",
                        b.gain - a.gain,
                        sec(a.time_us),
                        sec((b.time_us - a.time_us).max(1))
                    ));
                }
                if game_channels > 2 {
                    format!(
                        "aeval='{}':channel_layout='{}'",
                        (0..game_channels)
                            .map(|ch| format!("val({ch})*({expr})"))
                            .collect::<Vec<_>>()
                            .join("|"),
                        multichannel_layout.as_deref().ok_or("源多声道布局缺失")?
                    )
                } else {
                    format!(
                        "aeval='val(0)*({expr})|val(1)*({expr})',aformat=channel_layouts=stereo"
                    )
                }
            } else {
                format!(
                    "afade=t=in:d={},afade=t=out:st={}:d={}",
                    sec(5000.min(t.duration_us / 2)),
                    sec(t.duration_us - fade),
                    sec(fade)
                )
            };
            graph.push(format!("[as{}]atrim=start={}:end={},asetpts=PTS-STARTPTS,{envelope},atrim=start={}:duration={},asetpts=PTS-STARTPTS,volume={},adelay={}S:all=1[{label}]",spans.len()+j,sec(t.source_us),sec(t.source_us+t.duration_us),sec(offset),sec(len),t.gain*p.mix.game_gain,delay*48/1000));
            audio.push(format!("[{label}]"));
        }
    }
    let placements: Vec<_> = plan
        .music
        .iter()
        .enumerate()
        .filter(|(_, m)| m.output_us < end && m.output_us + m.duration_us > start)
        .collect();
    let music_end = plan
        .music
        .last()
        .map(|m| m.output_us + m.duration_us)
        .unwrap_or(0)
        .min(plan.duration_us);
    let fade_end = (2 * SECOND).min(music_end);
    for track in 0..p.music.len() {
        let instances: Vec<_> = placements
            .iter()
            .filter(|(_, m)| m.track == track)
            .collect();
        if instances.is_empty() {
            continue;
        }
        let ai = p.music[track].media.audio_index.ok_or("音乐无音轨")?;
        let origin = p.music[track]
            .media
            .streams
            .iter()
            .find(|s| s.index == ai)
            .map(|s| s.start_us)
            .unwrap_or(0);
        graph.push(format!("[{}:{ai}]asetpts=PTS-({})/TB,aresample=48000,aformat=sample_fmts=fltp:channel_layouts=stereo,asplit={}{}",track+1,sec(origin),instances.len(),instances.iter().map(|(j,_)|format!("[ms{j}]")).collect::<Vec<_>>().join("")));
        for (j, m) in instances {
            let offset = (start - m.output_us).max(0);
            let len = m.duration_us.min(end - m.output_us) - offset;
            let delay = (m.output_us - start).max(0);
            let full_len = p.music[m.track].media.duration_us;
            let fi = m.fade_in_us.min(full_len / 2);
            let fo = m.fade_out_us.min(full_len / 2);
            let mut fades = String::new();
            if fi > 0 {
                fades.push_str(&format!(",afade=t=in:d={}:curve=qsin", sec(fi)))
            }
            if fo > 0 {
                fades.push_str(&format!(
                    ",afade=t=out:st={}:d={}:curve=qsin",
                    sec(full_len - fo),
                    sec(fo)
                ))
            }
            graph.push(format!("[ms{j}]atrim=duration={}{} ,atrim=start={}:duration={},asetpts=PTS-STARTPTS,volume={},adelay={}S:all=1[m{j}]",sec(full_len),fades,sec(offset),sec(len),p.mix.music_gain*plan.loudness_gain[track],delay*48/1000));
            audio.push(format!("[m{j}]"));
        }
    }
    // Music ending fade uses global time, preserving nonzero-preview alignment.
    if !placements.is_empty() {
        let music_labels: Vec<_> = audio
            .iter()
            .filter(|s| s.starts_with("[m"))
            .cloned()
            .collect();
        audio.retain(|s| !s.starts_with("[m"));
        let fade_start = (music_end - fade_end - start) as f64 / SECOND as f64;
        graph.push(format!("{}amix=inputs={}:normalize=0,aeval='val(0)*clip(({}-n/s)/{},0,1)|val(1)*clip(({}-n/s)/{},0,1)',aformat=channel_layouts=stereo[music]",music_labels.join(""),music_labels.len(),(music_end-start)as f64/SECOND as f64,(fade_end as f64/SECOND as f64).max(0.000001),(music_end-start)as f64/SECOND as f64,(fade_end as f64/SECOND as f64).max(0.000001)));
        let _ = fade_start;
        audio.push("[music]".into());
    }
    let has_audio = !audio.is_empty();
    if has_audio {
        let layout = multichannel_layout
            .as_ref()
            .map(|v| format!("aformat=channel_layouts='{v}',"))
            .unwrap_or_default();
        graph.push(format!("{}amix=inputs={}:normalize=0,{layout}alimiter=limit=0.891250938:level=0:latency=1,apad,atrim=duration={}[aout]",audio.join(""),audio.len(),sec(duration)));
    }
    Ok((graph.join(";\n"), has_audio))
}
pub fn render(
    r: &Runner,
    p: &Project,
    plan: &Plan,
    range: (Us, Us),
    preview: bool,
    target: &Path,
) -> Result<(), String> {
    let format = if preview {
        crate::source_export::read_preview(r, p)?
    } else {
        crate::source_export::read(r, p)?
    };
    #[cfg(feature = "smart-cut")]
    if !preview
        && range == (0, plan.duration_us)
        && crate::smart::try_render(r, p, plan, target, &format)?
    {
        return Ok(());
    }
    // Full native-size renders retain more state than short calibration clips.
    // The 2400x1080 real project measured about 1.12 GB at four threads; the
    // old model underestimated its fixed cost at low thread counts. Budget
    // fixed surfaces separately from worker surfaces before calibrating, so a
    // lower budget changes worker counts without changing quality analysis.
    let bytes_per_pixel =
        if format.pixel_format.contains("10") || format.pixel_format.contains("12") {
            6
        } else {
            3
        };
    let output_frame_bytes = (format.width as u64)
        .saturating_mul(format.height as u64)
        .saturating_mul(bytes_per_pixel);
    // Resizing reduces encoder/output buffers, but decoding still uses source surfaces.
    let frame_bytes = if preview {
        p.source.streams.iter().find(|s| s.index == p.video_index)
            .map(|s| s.width as u64 * s.height as u64 * bytes_per_pixel)
            .unwrap_or(0).max(output_frame_bytes)
    } else { output_frame_bytes };
    r.resource().prepare_stage_limited(
        if preview {
            "previewRender"
        } else {
            "exportRender"
        },
        if preview && format.video_codec == "h264" && format.pixel_format == "yuv420p" {
            // Calibrated against 2400x1080 native previews: ~795 MB total
            // private commit at four threads, plus headroom. Other formats
            // retain the conservative model until separately measured.
            256 * 1024 * 1024 + frame_bytes * 56
        } else {
            384 * 1024 * 1024 + frame_bytes * 80
        },
        frame_bytes * 4 + 8 * 1024 * 1024,
        if preview {
            4
        } else {
            crate::resource::MAX_MEDIA_THREADS
        },
    )?;
    if preview {
        (r.progress)(
            &format!(
                "PREVIEW_BUDGET: {}",
                serde_json::json!({"width":format.width,"height":format.height,"pixelFormat":format.pixel_format,"sourceCodec":format.video_codec,"bytesPerFrameBudget":frame_bytes,"threads":r.threads(),"queueLimit":128,"model":"preview-native-surfaces-v2-calibrated-h264"})
            ),
            None,
        );
        if r.threads() <= 2 {
            (r.progress)("正在以较低内存占用生成预览，处理时间可能增加", None);
        }
    }
    if !preview {
        let codec = crate::source_export::video_encoder(&container(p), &format.video_codec)?;
        let mut quality = crate::encoder::initial(p, &format, codec);
        quality.quantizer = 0;
        let raw_bitrate = ((format.width as f64 * format.height as f64 * 6. * 8.)
            * p.source.frame_pts.len() as f64
            * SECOND as f64
            / p.source.duration_us.max(1) as f64) as u64;
        // An estimate, not a reservation for every uncompressed frame. The
        // continuous disk monitor still stops writes before the safety margin.
        // Twelve source bitrates covers the measured lossless recording; use a
        // 64 Mbit floor for highly compressed inputs, capped at raw storage.
        quality.estimated_bitrate = format
            .source_video_bitrate
            .saturating_mul(12)
            .max(64_000_000)
            .min(raw_bitrate);
        (r.progress)(
            "WARN: 此素材暂不支持局部复制，将使用无损画面编码；处理较慢、文件可能较大。",
            None,
        );
        return render_inner(
            r,
            p,
            plan,
            range,
            false,
            target,
            None,
            Some(&format),
            &quality,
        );
    }
    let ct = if preview { "mp4".into() } else { container(p) };
    let hardware_pixels = match format.video_codec.as_str() {
        "h264" => ["yuv420p", "yuv444p"].contains(&format.pixel_format.as_str()),
        "hevc" | "av1" => {
            ["yuv420p", "yuv420p10le", "yuv444p"].contains(&format.pixel_format.as_str())
        }
        _ => false,
    };
    let hw = if hardware_pixels {
        crate::encoder::preferred(r, &format.video_codec, &ct, false)
    } else {
        None
    };
    r.check()?;
    if let Some(codec) = hw {
        let result = (|| {
            let quality = if preview {
                crate::encoder::preview(p, &format, &codec)
            } else {
                crate::encoder::calibrated(r, p, plan, &format, &codec)?
            };
            render_inner(
                r,
                p,
                plan,
                range,
                preview,
                target,
                Some(&codec),
                Some(&format),
                &quality,
            )
        })();
        if result.is_ok() {
            return result;
        }
        r.check()?;
        // A pressure, disk, input, validation or mid-encode failure must stop.
        // Only unsupported encoder initialization can select software instead.
        if !result
            .as_ref()
            .err()
            .is_some_and(|e| crate::encoder::initialization_failure(e))
        {
            return result;
        }
        let _ = fs::remove_file(target);
        (r.progress)(
            &format!(
                "WARN: 硬件编码 {codec} 初始化不兼容，使用软件编码：{}",
                result.unwrap_err()
            ),
            None,
        );
    }
    let codec = crate::source_export::video_encoder(&ct, &format.video_codec)?;
    let quality = if preview {
        crate::encoder::preview(p, &format, codec)
    } else {
        crate::encoder::calibrated(r, p, plan, &format, codec)?
    };
    render_inner(
        r,
        p,
        plan,
        range,
        preview,
        target,
        None,
        Some(&format),
        &quality,
    )
}
fn render_inner(
    r: &Runner,
    p: &Project,
    plan: &Plan,
    range: (Us, Us),
    preview: bool,
    target: &Path,
    hardware: Option<&str>,
    source_format: Option<&crate::source_export::Format>,
    quality: &crate::encoder::Quality,
) -> Result<(), String> {
    media::verify_media(&p.source)?;
    for m in &p.music {
        media::verify_media(&m.media)?;
    }
    if plan.duration_us == 0 {
        return Err("全部内容已删除，请恢复至少一段".into());
    }
    let audio_rate = source_format.map(|f| f.audio_bitrate).unwrap_or(320000);
    let estimated = ((quality.estimated_bitrate.max(quality.max_bitrate) + audio_rate) as f64
        * (range.1 - range.0) as f64
        / SECOND as f64
        / 8.
        * 1.5) as u64
        + 4 * 1024 * 1024;
    let _disk = r.reserve_disk(
        target,
        estimated,
        if preview {
            "快速预览"
        } else {
            "高质量导出"
        },
    )?;
    let _active = crate::storage::active_file(target)?;
    if !preview {
        if let Some(f) = source_format {
            let reference = f.reference_bytes as f64 * (range.1 - range.0) as f64
                / p.source.duration_us.max(1) as f64;
            (r.progress)(&format!("SIZE_REFERENCE: sourceBytes={}; referenceBytes={reference:.0}; estimated={}; sourceVideoBitrate={}; audioBitrate={audio_rate}; targetRatio=1.3",f.reference_bytes,f.reference_is_estimate,f.source_video_bitrate),None);
        }
    }
    let (g, a) = filter(p, plan, range, preview)?;
    let script =
        crate::storage::temp_file(target.parent().ok_or("无效输出目录")?, "render-filter")?;
    let audio_script = if preview && a {
        let (video_graph, audio_graph) = separate_preview_graphs(&g)?;
        let audio_script = crate::storage::temp_file(target.parent().ok_or("无效输出目录")?, "preview-audio-filter")?;
        fs::write(script.path(), video_graph).map_err(|e|e.to_string())?;
        fs::write(audio_script.path(), audio_graph).map_err(|e|e.to_string())?;
        Some(audio_script)
    } else {
        fs::write(script.path(), g).map_err(|e| e.to_string())?;
        None
    };
    let run = (|| {
        let source_start = plan
            .keep
            .iter()
            .filter_map(|s| {
                let a = range.0.max(s.output_us);
                let b = range.1.min(s.output_us + s.end_us - s.start_us);
                (b > a).then_some(s.start_us + a - s.output_us)
            })
            .min()
            .unwrap_or(0);
        let tail_start = plan
            .tails
            .iter()
            .filter(|t| t.output_us < range.1 && t.output_us + t.duration_us > range.0)
            .map(|t| t.source_us + (range.0 - t.output_us).max(0))
            .min()
            .unwrap_or(source_start);
        let first = p
            .source
            .frame_pts
            .iter()
            .copied()
            .take_while(|t| *t <= source_start.min(tail_start))
            .last()
            .unwrap_or(0);
        let seek = if first >= 200000 {
            first + p.source.origin_us - 200000
        } else {
            0
        };
        // FFmpeg 9 counts queued frames across filter links and returns ENOMEM
        // instead of allowing an unbounded graph queue. Native memory sampling
        // separately covers decoder/encoder buffers and private allocations.
        let mut cmd = args(&[
            "-hide_banner",
            "-v",
            "warning",
            "-y",
            "-copyts",
            "-filter_buffered_frames",
            "128",
        ]);
        if seek > 0 {
            cmd.extend(args(&["-seek_timestamp", "1", "-ss", &sec(seek)]));
        }
        let source_end = plan
            .keep
            .iter()
            .filter_map(|k| {
                let a = range.0.max(k.output_us);
                let b = range.1.min(k.output_us + k.end_us - k.start_us);
                (b > a).then_some(k.start_us + b - k.output_us)
            })
            .max()
            .unwrap_or(p.source.duration_us);
        cmd.extend(args(&[
            "-threads",
            &r.threads().to_string(),
            "-t",
            &sec((source_end + p.source.origin_us - seek + 100000).max(100000)),
            "-i",
            &p.source.fingerprint.path,
        ]));
        for (track, m) in p.music.iter().enumerate() {
            let origin = m
                .media
                .audio_index
                .and_then(|ai| m.media.streams.iter().find(|s| s.index == ai))
                .map(|s| s.start_us)
                .unwrap_or(0);
            let offset = plan
                .music
                .iter()
                .filter(|x| {
                    x.track == track
                        && x.output_us < range.1
                        && x.output_us + x.duration_us > range.0
                })
                .map(|x| (range.0 - x.output_us).max(0))
                .min()
                .unwrap_or(0);
            let music_seek = if offset >= 200000 {
                offset + origin - 200000
            } else {
                0
            };
            if music_seek > 0 {
                cmd.extend(args(&["-seek_timestamp", "1", "-ss", &sec(music_seek)]));
            }
            let until = plan
                .music
                .iter()
                .filter(|x| {
                    x.track == track
                        && x.output_us < range.1
                        && x.output_us + x.duration_us > range.0
                })
                .map(|x| (range.1 - x.output_us).min(x.duration_us))
                .max()
                .unwrap_or(m.media.duration_us);
            cmd.extend(args(&[
                "-threads",
                &r.threads().min(2).to_string(),
                "-t",
                &sec((until + origin - music_seek + 100000).max(100000)),
                "-i",
                &m.media.fingerprint.path,
            ]));
        }
        cmd.extend(args(&[
            "-filter_complex_threads",
            &r.threads().min(2).to_string(),
            "-/filter_complex",
            &script.path().to_string_lossy(),
        ]));
        if let Some(audio_script) = &audio_script {
            cmd.extend(args(&["-/filter_complex", &audio_script.path().to_string_lossy()]));
        }
        cmd.extend(args(&["-map", "[vout]"]));
        let ct = if preview { "mp4".into() } else { container(p) };
        let codec = if let Some(h) = hardware {
            h
        } else if preview {
            "libx264"
        } else {
            crate::source_export::video_encoder(
                &ct,
                &source_format.ok_or("源编码信息缺失")?.video_codec,
            )?
        };
        cmd.extend(args(&["-c:v", codec]));
        let encoding_options = if preview {
            crate::encoder::preview_options(r, codec, quality)
        } else {
            crate::encoder::lossless_options(r.threads(), codec)?
        };
        cmd.extend(encoding_options.clone());
        if let Some(f) = source_format {
            // Pixel/color compatibility decisions are explicit in Format and
            // verified again before committing the compressed output.
            cmd.extend(args(&["-pix_fmt", &f.pixel_format]));
            for (flag, value) in &f.colors {
                cmd.extend([flag.clone(), value.clone()]);
            }
        }
        // Encoder defaults quantize VFR to the nominal frame-rate time base and
        // may drop distinct, closely spaced source frames. Keep the filter's
        // integer-microsecond timestamps for codecs supporting that precision.
        if !["avi", "wmv"].contains(&ct.as_str())
            && (hardware.is_some()
                || [
                    "libx264",
                    "libx265",
                    "libvpx",
                    "libvpx-vp9",
                    "libaom-av1",
                    "ffv1",
                    "huffyuv",
                ]
                .contains(&codec))
        {
            cmd.extend(args(&["-enc_time_base:v", "1:1000000"]));
        }
        if preview {
            // A VFR frame can begin inside this preview but extend beyond its
            // right edge. Shorten only that packet's display duration; retain
            // every encoded frame and its original mapped PTS/DTS.
            cmd.extend(args(&[
                "-bsf:v",
                &format!(
                    "setts=pts=PTS:dts=DTS:duration=min(DURATION\\,max(0\\,{}-PTS))",
                    range.1 - range.0
                ),
            ]));
        }
        (r.progress)(
            &format!(
                "ENCODER: {codec}; losslessVideo={}; q={}; maxrate={}; container={ct}; nativePTS=true; sampleSSIM={:.6}",
                !preview,quality.quantizer,quality.max_bitrate,quality.measured_ssim
            ),
            None,
        );
        (r.progress)(
            &format!(
                "ENCODER_POLICY: {}",
                serde_json::json!({"codec":codec,"videoOptions":encoding_options,"pixelFormat":source_format.map(|f|&f.pixel_format),"colors":source_format.map(|f|&f.colors),"audio":source_format.filter(|_|a).map(|f|serde_json::json!({"codec":f.audio_encoder,"bitrate":audio_rate,"sampleRate":f.sample_rate,"channels":f.channels,"layout":f.audio_layout})),"quality":quality})
            ),
            None,
        );
        if a {
            cmd.extend(args(&[
                "-map",
                "[aout]",
                "-c:a",
                source_format
                    .map(|f| f.audio_encoder.as_str())
                    .unwrap_or("aac"),
                "-b:a",
                &audio_rate.to_string(),
                "-ar",
                &source_format
                    .map(|f| f.sample_rate)
                    .unwrap_or(48000)
                    .to_string(),
            ]));
            if let Some(f) = source_format {
                cmd.extend(args(&["-ac", &f.channels.to_string()]));
                if let Some(layout) = &f.audio_layout {
                    cmd.extend(args(&["-channel_layout", layout]));
                }
                if f.audio_encoder == "libopus" {
                    cmd.extend(args(&["-application", "audio", "-vbr", "on"]));
                    if f.channels > 2 {
                        cmd.extend(args(&["-mapping_family", "1"]));
                    }
                }
            }
        } else {
            cmd.push("-an".into())
        }
        if preview {
            cmd.extend(args(&[
                "-fs",
                &crate::storage::settings().0.cache_limit.to_string(),
            ]));
        }
        cmd.extend(args(&[
            "-fps_mode",
            if ["avi", "wmv"].contains(&ct.as_str()) && source_format.is_some_and(|f| f.cfr) {
                "cfr"
            } else {
                "vfr"
            },
            "-t",
            &sec(range.1 - range.0),
            "-map_metadata",
            "-1",
            "-progress",
            "pipe:2",
            "-nostats",
        ]));
        if ct == "mp4" || ct == "mov" {
            cmd.extend(args(&["-movflags", "+faststart"]))
        }
        if ct == "avi" || ct == "wmv" {
            if let Some(f) = source_format.filter(|f| f.cfr) {
                cmd.extend(args(&["-r", &f.rate]));
            }
        }
        if ["ts", "mts", "m2ts"].contains(&ct.as_str()) {
            // Keep AAC priming at its negative PTS rather than shifting every
            // retained video frame forward by one audio codec frame.
            cmd.extend(args(&[
                "-muxdelay",
                "0",
                "-muxpreload",
                "0",
                "-avoid_negative_ts",
                "disabled",
            ]));
        }
        // Keep WMA encoder preroll before zero; ASF otherwise shifts every video
        // timestamp forward and counts the audio latency twice in its duration.
        if ct == "wmv" {
            cmd.extend(args(&["-avoid_negative_ts", "disabled"]));
        }
        cmd.extend(args(&[
            "-f",
            crate::source_export::muxer(&ct),
            &target.to_string_lossy(),
        ]));
        r.measure(
            if preview {
                "previewRender"
            } else {
                "exportRender"
            },
            0,
            Some(range.1 - range.0),
            "us",
            "编码媒体片段",
        );
        r.ff(cmd)?;
        // Probe duration and decode all output streams before any atomic commit.
        // Encoder buffers have now been freed. Refresh the budget at this safe
        // boundary instead of charging them again against the last peak sample.
        validate_output(r, p, range, preview, target, source_format, a, false)
    })();
    if run.is_err() {
        let _ = fs::remove_file(target);
    }
    run
}

// Keep identical video selection and audio mixing expressions, but schedule
// their graphs independently. A late audio trim must not retain raw 120 fps
// video in the shared 128-frame filter queue before either encoder can start.
fn separate_preview_graphs(graph: &str) -> Result<(String, String), String> {
    let mut links=graph.splitn(3,";\n");
    let first=links.next().ok_or("PREVIEW_GRAPH: missing video input")?;
    let second=links.next().ok_or("PREVIEW_GRAPH: missing video output")?;
    let audio=links.next().ok_or("PREVIEW_GRAPH: missing audio graph")?;
    if !first.ends_with("[vcat]") || !second.ends_with("[vout]") || !audio.ends_with("[aout]") {
        return Err("PREVIEW_GRAPH: unexpected filter layout".into());
    }
    Ok((format!("{first};\n{second}"),audio.into()))
}

#[cfg(test)]
mod preview_graph_tests {
    use super::*;
    #[test]
    fn separate_graphs_preserve_every_filter_expression() {
        let original="[0:0]select='gte(pts,1)'[vcat];\n[vcat]null[vout];\n[0:1]atrim=start=50,asetpts=PTS-STARTPTS[a];\n[a]alimiter=level=0[aout]";
        let (video,audio)=separate_preview_graphs(original).unwrap();
        assert_eq!(format!("{video};\n{audio}"),original);
        assert!(separate_preview_graphs("[0:0]null[vout]").is_err());
    }
}

#[cfg(feature = "smart-cut")]
pub(crate) fn mux_smart_audio(
    r: &Runner,
    p: &Project,
    plan: &Plan,
    video: &Path,
    target: &Path,
    f: &crate::source_export::Format,
) -> Result<bool, String> {
    let (graph, audio) = filter(p, plan, (0, plan.duration_us), false)?;
    let script = crate::storage::temp_file(target.parent().ok_or("无效输出目录")?, "smart-audio")?;
    // The first two graph links exclusively select/map video. Audio keeps the
    // exact existing mix/tail plan, but no source video is decoded in this pass.
    let mut links = graph.split(";\n");
    if !links.next().is_some_and(|s| s.ends_with("[vcat]"))
        || !links.next().is_some_and(|s| s.ends_with("[vout]"))
    {
        return Err("SMART_AUDIO: unexpected graph layout".into());
    }
    fs::write(script.path(), links.collect::<Vec<_>>().join(";\n")).map_err(|e| e.to_string())?;
    let threads = r.resource().prepare_stage_limited(
        "smartAudio",
        128 * 1024 * 1024 + (p.music.len() as u64) * 8 * 1024 * 1024,
        16 * 1024 * 1024,
        2,
    )?;
    let mut cmd = args(&[
        "-hide_banner",
        "-v",
        "warning",
        "-y",
        "-copyts",
        "-filter_buffered_frames",
        "128",
    ]);
    let video_index = if audio {
        cmd.extend(args(&[
            "-threads",
            &threads.to_string(),
            "-i",
            &p.source.fingerprint.path,
        ]));
        for music in &p.music {
            cmd.extend(args(&[
                "-threads",
                &threads.to_string(),
                "-i",
                &music.media.fingerprint.path,
            ]));
        }
        p.music.len() + 1
    } else {
        0
    };
    cmd.extend(args(&[
        "-noautorotate",
        "-i",
        &video.to_string_lossy(),
        "-map",
        &format!("{video_index}:v:0"),
        "-c:v",
        "copy",
        "-tag:v",
        "avc3",
        "-video_track_timescale",
        "1000000",
    ]));
    if audio {
        cmd.extend(args(&[
            "-filter_complex_threads",
            &threads.to_string(),
            "-/filter_complex",
            &script.path().to_string_lossy(),
            "-map",
            "[aout]",
            "-c:a",
            &f.audio_encoder,
            "-b:a",
            &f.audio_bitrate.to_string(),
            "-ar",
            &f.sample_rate.to_string(),
            "-ac",
            &f.channels.to_string(),
        ]));
        if let Some(layout) = &f.audio_layout {
            cmd.extend(args(&["-channel_layout", layout]));
        }
    } else {
        cmd.push("-an".into());
    }
    cmd.extend(args(&[
        "-t",
        &sec(plan.duration_us),
        "-map_metadata",
        "-1",
        "-progress",
        "pipe:2",
        "-nostats",
        "-f",
        "mp4",
        &target.to_string_lossy(),
    ]));
    r.measure(
        "smartAudio",
        0,
        Some(plan.duration_us),
        "us",
        "处理音频并封装原画质视频",
    );
    r.ff(cmd)?;
    Ok(audio)
}

pub(crate) fn validate_output(
    r: &Runner,
    p: &Project,
    range: (Us, Us),
    preview: bool,
    target: &Path,
    source_format: Option<&crate::source_export::Format>,
    a: bool,
    exact_table: bool,
) -> Result<(), String> {
    let validation_frame = source_format
        .map(|f| f.width as u64 * f.height as u64 * 6)
        .unwrap_or(16 * 1024 * 1024);
    r.prepare_stage(
        "outputValidate",
        64 * 1024 * 1024 + validation_frame * 12,
        validation_frame * 3,
    )?;
    r.measure("outputValidate", 0, None, "state", "校验媒体流与时长");
    let (data, _) = r.run(
        true,
        &args(&[
            "-v",
            "error",
            "-show_format",
            "-show_streams",
            "-of",
            "json",
            &target.to_string_lossy(),
        ]),
        4 * 1024 * 1024,
    )?;
    let v: serde_json::Value = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
    if let Some(f) = source_format {
        crate::source_export::verify(f, v["streams"].as_array().ok_or("输出媒体流缺失")?, a)?;
    }
    let actual = v["format"]["duration"]
        .as_str()
        .and_then(|x| x.parse::<f64>().ok())
        .ok_or("无法验证输出时长")?;
    let expected = (range.1 - range.0) as f64 / SECOND as f64;
    let rate = p
        .source
        .streams
        .iter()
        .find(|s| s.index == p.video_index)
        .map(|s| {
            s.rate
                .split('/')
                .filter_map(|x| x.parse::<f64>().ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let tolerance = if rate.len() == 2 && rate[0] > 0. {
        (rate[1] / rate[0]).max(0.05)
    } else {
        0.05
    };
    if (actual - expected).abs() > tolerance + 0.005 {
        return Err(format!(
            "OUTPUT_DURATION: 预期 {expected:.6}s，实际 {actual:.6}s"
        ));
    }
    if let Some(streams) = v["streams"].as_array() {
        let lens: Vec<f64> = streams
            .iter()
            .filter_map(|s| s["duration"].as_str().and_then(|x| x.parse().ok()))
            .collect();
        if lens.len() > 1
            && lens.iter().copied().fold(f64::NEG_INFINITY, f64::max)
                - lens.iter().copied().fold(f64::INFINITY, f64::min)
                > tolerance + 0.005
        {
            return Err(format!(
                "AV_LENGTH: 音视频时长差超过容差 {:?}, expected {:.6}",
                lens, expected
            ));
        }
    }
    // Smart MP4 already checks the sample count and writes the exact source
    // time table. Other containers/codecs must prove decoded frame coverage;
    // keep the conservative extra scan until an equivalent shortcut is tested.
    if !preview && !exact_table && p.export.fps.is_none() {
        let plan = crate::timeline::build(p)?;
        let _memory = r.reserve_memory(p.source.frame_pts.len() as u64 * 8, "输出时序对照")?;
        let expected: Vec<Us> = plan
            .keep
            .iter()
            .flat_map(|k| {
                let first = p.source.frame_pts.partition_point(|t| *t < k.start_us);
                let last = p.source.frame_pts.partition_point(|t| *t < k.end_us);
                p.source.frame_pts[first..last]
                    .iter()
                    .map(move |t| t - k.start_us + k.output_us)
            })
            .collect();
        let decoded = media::probe(r, &target.to_string_lossy(), true)?;
        let tick = v["streams"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|s| s["codec_type"] == "video")
            .and_then(|s| s["time_base"].as_str())
            .and_then(|s| s.split_once('/'))
            .and_then(|(a, b)| Some((a.parse::<f64>().ok()?, b.parse::<f64>().ok()?)))
            .filter(|(_, b)| *b > 0.)
            .map(|(a, b)| (a / b * SECOND as f64).ceil() as Us)
            .unwrap_or(1)
            .max(1);
        if decoded.frame_pts.len() != expected.len() {
            return Err(format!(
                "OUTPUT_FRAMES: 预期 {} 帧，实际 {} 帧",
                expected.len(),
                decoded.frame_pts.len()
            ));
        }
        for (i, (actual, expected)) in decoded.frame_pts.iter().zip(&expected).enumerate() {
            if (actual + decoded.origin_us - expected).abs() > tick + 1 {
                return Err(format!(
                    "OUTPUT_TIMING: 第 {} 帧预期 {}，实际 {} 微秒",
                    i + 1,
                    expected,
                    actual + decoded.origin_us
                ));
            }
        }
        r.measure(
            "outputTiming",
            expected.len() as i64,
            Some(expected.len() as i64),
            "frames",
            "帧数与真实时序检查完成",
        );
    }
    if p.pauses
        .iter()
        .any(|x| x.enabled) && crate::detector::can_check(p)
    {
        let plan = crate::timeline::build(p)?;
        let audit = crate::detector::check_full_output(r, p, &plan, target, range)?;
        *r.validated_audit.lock().unwrap() = Some((target.to_path_buf(), audit));
    } else {
        r.measure("outputValidate",0,Some(range.1-range.0),"us","完整解码检查输出");
        r.ff(args(&[
            "-v",
            "error",
            "-xerror",
            "-i",
            &target.to_string_lossy(),
            "-map",
            "0:v:0",
            "-map",
            "0:a?",
            "-f",
            "null",
            "-progress",
            "pipe:2",
            "-nostats",
            "-",
        ]))?;
    }
    if !preview {
        if let Some(f) = source_format {
            let bytes = fs::metadata(target).map_err(|e| e.to_string())?.len();
            let reference = f.reference_bytes as f64 * (range.1 - range.0) as f64
                / p.source.duration_us.max(1) as f64;
            (r.progress)(&format!("OUTPUT_SIZE: bytes={bytes}; referenceRatio={:.4}; fullSourceRatio={:.4}; referenceEstimated={}",bytes as f64/reference.max(1.),bytes as f64/p.source.fingerprint.size.max(1)as f64,f.reference_is_estimate),None);
        }
    }
    Ok(())
}
