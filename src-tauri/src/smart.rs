//! Conservative smart rendering: MP4 H.264 with one progressive picture per
//! packet and no reordering. Other streams retain the separately validated path.
use crate::{model::*, process::*, source_export::Format, storage};
use serde_json::{json, Value};
use std::{
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::Path,
};

#[derive(Debug)]
struct Packet {
    pts: Us,
    pos: u64,
    size: u64,
    idr: bool,
}
fn io(e: std::io::Error) -> String {
    e.to_string()
}
fn packet_clock(values: &[Us]) -> String {
    fn append(out: &mut String, values: &[Us], offset: usize) {
        use std::fmt::Write;
        match values.len() {
            0 => out.push('0'),
            1 => write!(out, "{}*eq(N,{offset})", values[0]).unwrap(),
            n => {
                let half = n / 2;
                out.push('(');
                append(out, &values[..half], offset);
                out.push('+');
                append(out, &values[half..], offset + half);
                out.push(')');
            }
        }
    }
    // FFmpeg rejects expression trees deeper than 100 nodes as ENOMEM.
    // A balanced sum preserves the same per-packet values with logarithmic depth.
    let mut out = String::new();
    append(&mut out, values, 0);
    out
}
struct CheckedFile<'a> {
    file: File,
    runner: &'a Runner,
}
impl Write for CheckedFile<'_> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.runner.check().map_err(std::io::Error::other)?;
        self.file.write(&data[..data.len().min(1024 * 1024)])
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.runner.check().map_err(std::io::Error::other)?;
        self.file.flush()
    }
}
fn read_cache(r: &Runner, video: &Path, meta: &Path, key: &str) -> Result<Option<Vec<u8>>, String> {
    if fs::metadata(meta).map_err(io)?.len() > 65536 {
        return Err("缓存元数据过大".into());
    }
    let record: Value =
        serde_json::from_reader(BufReader::new(File::open(meta).map_err(io)?).take(65537))
            .map_err(|e| e.to_string())?;
    let (bytes, sha) = crate::export_record::digest_checked(video, || r.check())?;
    let mut proof = record.clone();
    proof
        .as_object_mut()
        .ok_or("缓存记录无效")?
        .remove("integrity");
    let metadata_hash = storage::hash(&proof)?;
    if record["key"] != key
        || record["bytes"] != bytes
        || record["sha256"] != sha
        || record["integrity"] != metadata_hash
        || record["validation"]["rule"] != crate::repair::RULE
        || record["validation"]["passed"] != true
    {
        return Err("视频缓存完整性不匹配".into());
    }
    let config: Option<Vec<u8>> =
        serde_json::from_value(record["config"].clone()).map_err(|e| e.to_string())?;
    Ok(config)
}
fn append_piece(list: &mut String, path: &Path, duration: Us) {
    let name = path
        .to_string_lossy()
        .replace('\\', "/")
        .replace('\'', "'\\''");
    list.push_str(&format!(
        "file '{name}'\nduration {}\n",
        crate::render::sec(duration)
    ));
}
fn number(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str()?.parse().ok())
}
fn display_transform(v: &Value) -> Option<(i32, [i32; 9])> {
    let matrices: Vec<_> = v["side_data_list"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|x| x["side_data_type"] == "Display Matrix")
        .collect();
    if matrices.is_empty() {
        return (v["tags"]["rotate"].is_null())
            .then_some((0, [65536, 0, 0, 0, 65536, 0, 0, 0, 1073741824]));
    }
    if matrices.len() != 1 {
        return None;
    }
    let mut values = Vec::new();
    for line in matrices[0]["displaymatrix"]
        .as_str()?
        .lines()
        .filter(|x| !x.trim().is_empty())
    {
        let (_, row) = line.split_once(':')?;
        for value in row.split_whitespace() {
            values.push(value.parse::<i32>().ok()?);
        }
    }
    let matrix: [i32; 9] = values.try_into().ok()?;
    let rotation = match matrix {
        [65536, 0, 0, 0, 65536, 0, 0, 0, 1073741824] => 0,
        [0, -65536, 0, 65536, 0, 0, 0, 0, 1073741824] => 90,
        [0, 65536, 0, -65536, 0, 0, 0, 0, 1073741824] => -90,
        [-65536, 0, 0, 0, -65536, 0, 0, 0, 1073741824] => 180,
        _ => return None,
    };
    Some((rotation, matrix))
}
fn packet_shape(
    file: &mut File,
    packet: &Packet,
    settings: &crate::edge_pcm::Settings,
) -> Result<Option<bool>, String> {
    let end = packet
        .pos
        .checked_add(packet.size)
        .ok_or("SMART_INDEX: packet overflow")?;
    if end > file.metadata().map_err(io)?.len() {
        return Err("SMART_INDEX: packet outside source".into());
    }
    let mut at = packet.pos;
    let mut found = false;
    let mut ordinary = false;
    while at + 5 <= end {
        file.seek(SeekFrom::Start(at)).map_err(io)?;
        let mut h = [0; 5];
        file.read_exact(&mut h).map_err(io)?;
        let n = u32::from_be_bytes(h[..4].try_into().unwrap()) as u64;
        if n == 0 || at + 4 + n > end {
            return Ok(None);
        }
        let kind = h[4] & 31;
        if ![1, 5, 6, 7, 8, 9, 10, 11, 12].contains(&kind) {
            return Ok(None);
        }
        if kind == 7 || kind == 8 {
            let expected = if kind == 7 {
                &settings.source_sps
            } else {
                &settings.source_pps
            };
            if n as usize != expected.len() {
                return Ok(None);
            }
            let mut actual = vec![0u8; n as usize];
            actual[0] = h[4];
            file.read_exact(&mut actual[1..]).map_err(io)?;
            if &actual != expected {
                return Ok(None);
            }
        }
        ordinary |= kind == 1;
        found |= kind == 5;
        at += 4 + n
    }
    Ok((at == end && found != ordinary).then_some(found))
}
fn packets(
    r: &Runner,
    p: &Project,
    dir: &Path,
    settings: &crate::edge_pcm::Settings,
) -> Result<Option<Vec<Packet>>, String> {
    let _memory = r.reserve_memory(
        (p.source.frame_pts.len() as u64).saturating_mul(40),
        "读取剪辑包索引",
    )?;
    let spool = storage::temp_file(dir, "smart-index")?;
    let limit = (p.source.frame_pts.len() as u64)
        .saturating_mul(160)
        .min(256 * 1024 * 1024);
    let _disk = r.reserve_disk(spool.path(), limit, "剪辑包索引")?;
    r.run_to_file(
        true,
        &args(&[
            "-v",
            "error",
            "-select_streams",
            &p.video_index.to_string(),
            "-show_packets",
            "-show_entries",
            "packet=pts_time,dts_time,pos,size,flags",
            "-of",
            "compact=p=0:nk=0",
            &p.source.fingerprint.path,
        ]),
        spool.path(),
        limit,
    )?;
    let mut file = File::open(&p.source.fingerprint.path).map_err(io)?;
    let mut out = Vec::new();
    out.try_reserve_exact(p.source.frame_pts.len())
        .map_err(|_| "RESOURCE_MEMORY: 无法分配剪辑索引")?;
    let mut reader = BufReader::new(File::open(spool.path()).map_err(io)?);
    let mut line = String::new();
    loop {
        r.check()?;
        line.clear();
        let n = reader
            .by_ref()
            .take(4097)
            .read_line(&mut line)
            .map_err(io)?;
        if n == 0 {
            break;
        }
        if n > 4096 {
            return Err("SMART_INDEX: oversized packet row".into());
        }
        let values: std::collections::HashMap<_, _> = line
            .trim()
            .split('|')
            .filter_map(|x| x.split_once('='))
            .collect();
        let time = |key| {
            values
                .get(key)
                .and_then(|x| x.parse::<f64>().ok())
                .filter(|x| x.is_finite())
                .map(|x| (x * SECOND as f64).round() as Us)
        };
        let (Some(pts), Some(dts), Some(pos), Some(size)) = (
            time("pts_time"),
            time("dts_time"),
            values.get("pos").and_then(|x| x.parse().ok()),
            values.get("size").and_then(|x| x.parse().ok()),
        ) else {
            return Ok(None);
        };
        if pts != dts
            || out.len() >= p.source.frame_pts.len()
            || (pts - p.source.frame_pts[out.len()]).abs() > 1
        {
            return Ok(None);
        }
        let mut packet = Packet {
            pts,
            pos,
            size,
            idr: false,
        };
        let Some(is_idr) = packet_shape(&mut file, &packet, settings)? else {
            return Ok(None);
        };
        if is_idr != values.get("flags").is_some_and(|x| x.contains('K')) {
            return Ok(None);
        }
        packet.idr = is_idr;
        out.push(packet)
    }
    if out.len() != p.source.frame_pts.len() || !out.first().is_some_and(|x| x.idr) {
        return Ok(None);
    }
    Ok(Some(out))
}
pub fn try_render(
    r: &Runner,
    p: &Project,
    plan: &Plan,
    target: &Path,
    format: &Format,
) -> Result<bool, String> {
    crate::media::verify_media(&p.source)?;
    for m in &p.music {
        crate::media::verify_media(&m.media)?;
    }
    if p.source.origin_us != 0
        || p.export.codec != "h264"
        || crate::render::container(p) != "mp4"
        || !p
            .source
            .fingerprint
            .path
            .to_ascii_lowercase()
            .ends_with(".mp4")
    {
        return Ok(false);
    }
    let (data, _) = r.run(
        true,
        &args(&[
            "-v",
            "error",
            "-show_streams",
            "-show_data",
            "-of",
            "json",
            &p.source.fingerprint.path,
        ]),
        4 * 1024 * 1024,
    )?;
    let source: Value = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
    let video = source["streams"]
        .as_array()
        .and_then(|a| {
            a.iter()
                .find(|v| v["index"].as_u64() == Some(p.video_index as u64))
        })
        .ok_or("SMART_SOURCE: missing video")?;
    if video["codec_name"] != "h264"
        || number(&video["has_b_frames"]) != Some(0)
        || number(&video["nal_length_size"]) != Some(4)
        || video["pix_fmt"] != "yuv420p"
        || !matches!(
            video["field_order"].as_str(),
            Some("progressive" | "unknown") | None
        )
    {
        return Ok(false);
    }
    let width = number(&video["width"]).ok_or("SMART_SOURCE: missing width")?;
    let height = number(&video["height"]).ok_or("SMART_SOURCE: missing height")?;
    if width % 2 != 0 || height % 2 != 0 || width > 8192 || height > 8176 {
        return Ok(false);
    }
    let Some((source_rotation, source_matrix)) = display_transform(video) else {
        return Ok(false);
    };
    let Some(avcc) = video["extradata"]
        .as_str()
        .and_then(crate::edge_pcm::hex_dump)
    else {
        return Ok(false);
    };
    let Some(edge_settings) =
        crate::edge_pcm::Settings::from_avcc(&avcc, width as usize, height as usize)
    else {
        return Ok(false);
    };
    let cache_dir = storage::project_dir(&p.project_id)?;
    let source_sha = crate::repair::identity(r, &p.source.fingerprint.path)?;
    let cache_key = storage::hash(
        &json!({"version":"smart-video-v2-compatible-pcm","source":p.source.fingerprint,"stream":p.video_index,"video":video,"framePts":p.source.frame_pts,"keep":plan.keep,"duration":plan.duration_us}),
    )?;
    let cache_key = storage::hash(&json!([cache_key, &source_sha, crate::repair::RULE]))?;
    let cache_video = cache_dir.join(format!("smart-video-{cache_key}.mp4"));
    let cache_meta = cache_dir.join(format!("smart-video-{cache_key}.json"));
    let _video_lease = storage::active_file(&cache_video)?;
    let _meta_lease = storage::active_file(&cache_meta)?;
    let cached = read_cache(r, &cache_video, &cache_meta, &cache_key);
    if let Ok(config) = cached {
        let _timing_memory = r.reserve_memory(
            (p.source.frame_pts.len() as u64).saturating_mul(8),
            "缓存视频帧时序",
        )?;
        let mut pts = Vec::new();
        pts.try_reserve_exact(p.source.frame_pts.len())
            .map_err(|_| "RESOURCE_MEMORY: 无法分配缓存时间戳")?;
        for keep in &plan.keep {
            let first = p.source.frame_pts.partition_point(|t| *t < keep.start_us);
            let last = p.source.frame_pts.partition_point(|t| *t < keep.end_us);
            pts.extend(
                p.source.frame_pts[first..last]
                    .iter()
                    .map(|t| (*t - keep.start_us + keep.output_us).max(0)),
            );
        }
        let _disk = r.reserve_disk(
            target,
            fs::metadata(&cache_video)
                .map_err(io)?
                .len()
                .saturating_mul(2),
            "缓存视频混音与完整校验",
        )?;
        (r.progress)(
            "CACHE_HIT: 复用已验证的原画质视频，重新混音并完整校验",
            None,
        );
        finish_video(
            r,
            p,
            plan,
            &cache_video,
            target,
            format,
            config.as_deref(),
            &pts,
            source_rotation,
            source_matrix,
            width,
            height,
        )?;
        return Ok(true);
    }
    r.check()?;
    // Only remove the two owned entries for this exact content key.
    let _ = fs::remove_file(&cache_video);
    let _ = fs::remove_file(&cache_meta);
    let dir = target.parent().ok_or("SMART_OUTPUT: missing parent")?;
    r.prepare_stage("smartIndex", 64 * 1024 * 1024, 1024 * 1024)?;
    r.measure("smartIndex", 0, None, "packet", "读取剪辑索引");
    let Some(index) = packets(r, p, dir, &edge_settings)? else {
        return Ok(false);
    };
    let _index_memory = r.reserve_memory(index.len() as u64 * 56, "保留剪辑索引")?;
    let keys: Vec<_> = index
        .iter()
        .enumerate()
        .filter_map(|(i, p)| p.idr.then_some(i))
        .collect();
    let mut pieces = Vec::new();
    let mut pending_piece_cache = Vec::new();
    let mut piece_leases = Vec::new();
    let mut reused_frames = 0usize;
    let mut list = String::new();
    let mut config = None;
    let mut copied = 0usize;
    let mut encoded = 0usize;
    let mut boundary_policies = Vec::new();
    let mut output_pts = Vec::new();
    output_pts
        .try_reserve_exact(index.len())
        .map_err(|_| "RESOURCE_MEMORY: 无法分配输出时间戳")?;
    let mut estimates = 0u64;
    let mut keep_parts = Vec::new();
    for keep in &plan.keep {
        let first = p.source.frame_pts.partition_point(|t| *t < keep.start_us);
        let last = p.source.frame_pts.partition_point(|t| *t < keep.end_us);
        if first == last {
            return Ok(false);
        }
        let next = keys
            .iter()
            .copied()
            .find(|i| *i >= first)
            .unwrap_or(last)
            .min(last);
        for (t, a, b, start, end) in [
            (
                true,
                first,
                next,
                keep.start_us,
                if next < last {
                    index[next].pts
                } else {
                    keep.end_us
                },
            ),
            (
                false,
                next,
                last,
                if next < last {
                    index[next].pts
                } else {
                    keep.end_us
                },
                keep.end_us,
            ),
        ] {
            if a >= b {
                continue;
            }
            let amount = if t {
                (b - a) as u64 * (width.div_ceil(16) * 16) * (height.div_ceil(16) * 16) * 3
                    + 4 * 1024 * 1024
            } else {
                index[a..b].iter().map(|x| x.size).sum::<u64>() + 4 * 1024 * 1024
            };
            estimates = estimates.saturating_add(amount);
            keep_parts.push((t, a, b, start, end))
        }
        output_pts.extend(
            p.source.frame_pts[first..last]
                .iter()
                .map(|t| (*t - keep.start_us + keep.output_us).max(0)),
        );
    }
    if keep_parts
        .iter()
        .any(|(head, a, b, _, _)| *head && b - a > 240)
    {
        return Ok(false);
    }
    if output_pts.first() != Some(&0) {
        return Ok(false);
    }
    // Intermediates and final output coexist until validation. Reserve them once
    // on this output volume; no hidden cache volume or unbounded RAM video buffer.
    let audio_bytes =
        (plan.duration_us as u64 / SECOND as u64 + 1).saturating_mul(format.audio_bitrate / 8);
    let _disk = r.reserve_disk(
        target,
        estimates.saturating_mul(4).saturating_add(audio_bytes * 2),
        "保留原画质的局部剪辑",
    )?;
    let _active = storage::active_file(target)?;
    for (n, (head, first, last, start, end)) in keep_parts.iter().copied().enumerate() {
        r.check()?;
        let previous_idr = keys
            .iter()
            .copied()
            .take_while(|i| *i <= first)
            .last()
            .ok_or("SMART_INDEX: no preceding IDR")?;
        let piece_key = storage::hash(
            &json!({"version":"smart-piece-v2-compatible-pcm","source":p.source.fingerprint,"stream":p.video_index,"video":video,"pixelFormat":format.pixel_format,"colors":format.colors,"head":head,"first":first,"last":last,"start":start,"end":end,"previousIDR":index[previous_idr].pts,"framePts":&p.source.frame_pts[first..last]}),
        )?;
        let piece_key = storage::hash(&json!([piece_key, &source_sha, crate::repair::RULE]))?;
        let piece_video = cache_dir.join(format!("smart-piece-{piece_key}.mp4"));
        let piece_meta = piece_video.with_extension("json");
        piece_leases.push(storage::active_file(&piece_video)?);
        piece_leases.push(storage::active_file(&piece_meta)?);
        if let Ok(piece_config) = read_cache(r, &piece_video, &piece_meta, &piece_key) {
            if head && config.is_none() {
                config = piece_config;
            }
            if head {
                encoded += last - first;
            } else {
                copied += last - first;
            }
            reused_frames += last - first;
            append_piece(&mut list, &piece_video, end - start);
            (r.progress)(
                &format!(
                    "CACHE_PIECE_HIT: {}",
                    json!({"part":n,"frames":last-first,"head":head,"sourceStartUs":start,"sourceEndUs":end,"bytes":fs::metadata(&piece_video).map_err(io)?.len()})
                ),
                None,
            );
            continue;
        }
        r.check()?;
        let _ = fs::remove_file(&piece_video);
        let _ = fs::remove_file(&piece_meta);
        let piece_started = std::time::Instant::now();
        let temp = storage::temp_file(dir, "smart-piece")?;
        let mut cmd = args(&["-hide_banner", "-v", "warning", "-y"]);
        if head {
            let threads = r.resource().prepare_stage_limited(
                "smartBoundary",
                128 * 1024 * 1024 + width * height * 16,
                width * height * 4,
                2,
            )?;
            r.measure(
                "smartBoundary",
                n as i64,
                Some(keep_parts.len() as i64),
                "part",
                "无损处理剪辑边缘",
            );
            let raw = storage::temp_file(dir, "smart-pcm")?;
            let _edge_memory = r.reserve_memory(
                width.div_ceil(16) * height.div_ceil(16) * 256 * 5,
                "无损边缘参考帧与编码缓冲",
            )?;
            let mut encoder = crate::edge_pcm::Encoder::new(
                BufWriter::with_capacity(
                    64 * 1024,
                    CheckedFile {
                        file: File::create(raw.path()).map_err(io)?,
                        runner: r,
                    },
                ),
                edge_settings.clone(),
            )?;
            let decode=args(&["-v","info","-nostats","-copyts","-ss",&crate::render::sec(index[previous_idr].pts),"-threads",&threads.to_string(),"-noautorotate","-i",&p.source.fingerprint.path,"-map",&format!("0:{}",p.video_index),"-an","-vf",&format!("settb=1/1000000,trim=end_pts={end},select='gte(pts,{start})',format=yuv420p,showinfo@arkcut_pts=checksum=0"),"-frames:v",&(last-first).to_string(),"-fps_mode","passthrough","-f","rawvideo","-pix_fmt","yuv420p","pipe:1"]);
            let mut frames = 0;
            r.frames_with_timestamps(
                &decode,
                (width * height * 3 / 2) as usize,
                |i, pts, bytes| {
                    if i >= last - first || pts != p.source.frame_pts[first + i] {
                        return Err("SMART_PCM: 边缘帧与原片时间戳不一致".into());
                    }
                    r.check()?;
                    encoder.frame(bytes, || r.check())?;
                    frames += 1;
                    Ok(())
                },
            )?;
            if frames != last - first {
                return Err("SMART_PCM: 边缘帧数不完整".into());
            }
            let stats = json!({"part":n,"threads":threads,"encoder":"h264-ipcm-pskip","pcmBlocks":encoder.pcm_blocks,"skippedBlocks":encoder.skipped_blocks});
            encoder.finish()?;
            boundary_policies.push(stats);
            // A bounded packet timestamp expression retains every VFR interval.
            // File-valued option avoids Windows command-line length limits.
            let clock = storage::temp_file(dir, "smart-pcm-clock")?;
            let ts = packet_clock(
                &(first..last).map(|i| p.source.frame_pts[i] - start).collect::<Vec<_>>(),
            );
            let duration = packet_clock(
                &(first..last).map(|i| {
                        p.source
                            .frame_pts
                            .get(i + 1)
                            .copied()
                            .unwrap_or(end)
                            .min(end)
                            - p.source.frame_pts[i]
                    }).collect::<Vec<_>>(),
            );
            fs::write(
                clock.path(),
                format!("setts=time_base=1/1000000:ts='{ts}':duration='{duration}'"),
            )
            .map_err(io)?;
            cmd.extend(args(&[
                "-r",
                // Raw demuxing uses microsecond ticks, then setts supplies the
                // real per-packet clock; this is not an output frame rate.
                "1000000",
                "-i",
                &raw.path().to_string_lossy(),
                "-map",
                "0:v:0",
                "-an",
                "-c:v",
                "copy",
                "-/bsf:v",
                &clock.path().to_string_lossy(),
                "-video_track_timescale",
                "1000000",
                "-f",
                "mp4",
                &temp.path().to_string_lossy(),
            ]));
            r.ff(cmd.clone())?;
            encoded += last - first;
        } else {
            r.prepare_stage("smartCopy", 32 * 1024 * 1024, 1024 * 1024)?;
            cmd.extend(args(&[
                "-ss",
                &crate::render::sec(index[first].pts),
                "-noautorotate",
                "-i",
                &p.source.fingerprint.path,
                "-map",
                &format!("0:{}", p.video_index),
                "-an",
                "-frames:v",
                &(last - first).to_string(),
                "-c:v",
                "copy",
                "-avoid_negative_ts",
                "disabled",
            ]));
            copied += last - first;
        }
        cmd.extend(args(&[
            "-video_track_timescale",
            "1000000",
            "-f",
            "mp4",
            &temp.path().to_string_lossy(),
        ]));
        r.measure(
            if head { "smartBoundary" } else { "smartCopy" },
            n as i64,
            Some(keep_parts.len() as i64),
            "part",
            if head {
                "无损处理剪辑边缘"
            } else {
                "直接复制保留画面"
            },
        );
        if !head {
            r.ff(cmd)?;
        }
        let piece_config: Option<Vec<u8>> = None;
        if head && config.is_none() {
            config = piece_config.clone();
        }
        (r.progress)(
            &format!(
                "SMART_IO: {}",
                json!({"phase":if head{"smartBoundary"}else{"smartCopy"},"part":n,"writeBytes":fs::metadata(temp.path()).map_err(io)?.len(),"elapsedUs":piece_started.elapsed().as_micros()})
            ),
            None,
        );
        append_piece(&mut list, temp.path(), end - start);
        pending_piece_cache.push((
            pieces.len(),
            piece_key,
            piece_video,
            piece_meta,
            piece_config,
            start,
            end,
        ));
        pieces.push(temp);
    }
    let script = storage::temp_file(dir, "smart-concat")?;
    fs::write(script.path(), list).map_err(io)?;
    let joined = storage::temp_file(dir, "smart-video")?;
    r.prepare_stage("smartMux", 128 * 1024 * 1024, 2 * 1024 * 1024)?;
    r.measure("smartMux", 0, None, "file", "拼接保留画面");
    let mux_started = std::time::Instant::now();
    r.ff(args(&[
        "-v",
        "warning",
        "-y",
        "-f",
        "concat",
        "-safe",
        "0",
        "-i",
        &script.path().to_string_lossy(),
        "-map",
        "0:v:0",
        "-c:v",
        "copy",
        "-tag:v",
        "avc3",
        "-video_track_timescale",
        "1000000",
        "-metadata:s:v:0",
        &format!("rotate={source_rotation}"),
        "-f",
        "mp4",
        &joined.path().to_string_lossy(),
    ]))?;
    (r.progress)(
        &format!(
            "SMART_IO: {}",
            json!({"phase":"smartMux","writeBytes":fs::metadata(joined.path()).map_err(io)?.len(),"elapsedUs":mux_started.elapsed().as_micros()})
        ),
        None,
    );
    finish_video(
        r,
        p,
        plan,
        joined.path(),
        target,
        format,
        config.as_deref(),
        &output_pts,
        source_rotation,
        source_matrix,
        width,
        height,
    )?;
    // Cache only a video intermediate that participated in a complete validation.
    let store_cache = (|| -> Result<(), String> {
        let issues = r
            .validated_audit
            .lock()
            .unwrap()
            .as_ref()
            .map(|(_, audit)| crate::repair::audit_issues(p, plan, audit))
            .unwrap_or_default();
        let count = pending_piece_cache.len() as i64;
        for (n, (index, key, path, meta, piece_config, start, end)) in
            pending_piece_cache.into_iter().enumerate()
        {
            r.check()?;
            r.measure("smartPieceCache", n as i64, Some(count), "pieces", "整理已检查的视频片段");
            // Full-file decoding succeeded, but an implicated cut is not passed.
            // Keep only pieces outside every failed/uncertain seam dependency.
            if issues.iter().any(|v| {
                let a = v["evidence"]["sourceRange"][0].as_i64().unwrap_or(0) - SECOND;
                let b = v["evidence"]["sourceRange"][1]
                    .as_i64()
                    .unwrap_or(p.source.duration_us)
                    + SECOND;
                start < b && end > a
            }) {
                continue;
            }
            let store = (|| -> Result<(), String> {
                let piece = &pieces[index];
                let bytes = fs::metadata(piece.path()).map_err(io)?.len();
                if storage::available(&cache_dir).is_none_or(|free| {
                    free < bytes
                        .saturating_mul(2)
                        .saturating_add(5 * 1024 * 1024 * 1024)
                }) {
                    return Err("分段缓存空间不足".into());
                }
                storage::quota(
                    &cache_dir,
                    bytes.saturating_add(65536),
                    storage::settings().0.cache_limit,
                    &[],
                )?;
                let _space = r.reserve_disk(&path, bytes.saturating_mul(2), "可复用视频段")?;
                let began = std::time::Instant::now();
                let (_, sha, moved) = storage::transfer_cache_checked(piece, &path, || r.check())?;
                let mut record =
                    json!({"key":key,"bytes":bytes,"sha256":sha,"config":piece_config});
                record["validation"] = json!({"rule":crate::repair::RULE,"passed":true,"sourceSha256":source_sha,"sourceRange":[start,end],"coverage":"all video frames decoded in final composition; failed seam dependencies excluded"});
                record["integrity"] = json!(storage::hash(&record)?);
                storage::atomic_json(&meta, &record)?;
                (r.progress)(
                    &format!(
                        "CACHE_TRANSFER: {}",
                        json!({"scope":"piece","bytes":bytes,"readBytes":bytes,"writeBytes":if moved{0}else{bytes},"method":if moved{"owned-same-volume-move"}else{"bounded-copy-with-hash"},"elapsedUs":began.elapsed().as_micros()})
                    ),
                    None,
                );
                Ok(())
            })();
            if let Err(e) = store {
                r.check()?;
                (r.progress)(&format!("WARN: 分段缓存未保留：{e}"), None);
            }
        }
        if count > 0 {
            r.measure("smartPieceCache", count, Some(count), "pieces", "整理已检查的视频片段");
        }
        if !issues.is_empty() {
            return Ok(());
        }
        r.measure("smartVideoCache", 0, None, "file", "保存下次导出可复用的视频");
        let bytes = fs::metadata(joined.path()).map_err(io)?.len();
        // Optional reuse must not turn an already validated export into a
        // failure just because there is no room for an additional cache copy.
        if storage::available(&cache_dir).is_none_or(|free| {
            free < bytes
                .saturating_mul(2)
                .saturating_add(5 * 1024 * 1024 * 1024)
        }) {
            return Err("可用空间不足以安全保留额外缓存".into());
        }
        storage::quota(
            &cache_dir,
            bytes.saturating_add(65536),
            storage::settings().0.cache_limit,
            &[],
        )?;
        let _space = r.reserve_disk(&cache_video, bytes.saturating_mul(2), "可复用视频缓存")?;
        let began = std::time::Instant::now();
        let (_, sha, moved) = storage::transfer_cache_checked(&joined, &cache_video, || r.check())?;
        (r.progress)(
            &format!(
                "CACHE_TRANSFER: {}",
                json!({"bytes":bytes,"readBytes":bytes,"writeBytes":if moved{0}else{bytes},"method":if moved{"owned-same-volume-move"}else{"bounded-copy-with-hash"},"elapsedUs":began.elapsed().as_micros()})
            ),
            None,
        );
        let mut record = json!({"key":cache_key,"bytes":bytes,"sha256":sha,"config":config});
        record["validation"] = json!({"rule":crate::repair::RULE,"passed":true,"sourceSha256":source_sha,"keep":plan.keep,"coverage":"full video decode and all seams; new audio/mux always rechecked"});
        record["integrity"] = json!(storage::hash(&record)?);
        storage::atomic_json(&cache_meta, &record)?;
        Ok(())
    })();
    if let Err(error) = store_cache {
        r.check()?;
        (r.progress)(
            &format!("WARN: 视频缓存未保留，下次将重新处理：{error}"),
            None,
        );
    }
    (r.progress)(
        &format!(
            "SMART_RENDER: {}",
            json!({"copiedFrames":copied,"losslessBoundaryFrames":encoded,"cachedFrames":reused_frames,"totalFrames":output_pts.len(),"videoPolicy":"source-pixels-identical","boundaryEncoder":"h264-ipcm-pskip","boundaryOptions":["unchanged-source-sps-fields","no-transform-switch","source-packets-unchanged"],"rotation":source_rotation,"boundaries":boundary_policies,"timestampPolicy":"exact-source-microseconds"})
        ),
        None,
    );
    Ok(true)
}

fn finish_video(
    r: &Runner,
    p: &Project,
    plan: &Plan,
    video_path: &Path,
    target: &Path,
    format: &Format,
    config: Option<&[u8]>,
    output_pts: &[Us],
    source_rotation: i32,
    source_matrix: [i32; 9],
    width: u64,
    height: u64,
) -> Result<(), String> {
    let audio_started = std::time::Instant::now();
    let has_audio = crate::render::mux_smart_audio(r, p, plan, video_path, target, format)?;
    (r.progress)(
        &format!(
            "SMART_IO: {}",
            json!({"phase":"smartAudio","writeBytes":fs::metadata(target).map_err(io)?.len(),"elapsedUs":audio_started.elapsed().as_micros()})
        ),
        None,
    );
    r.prepare_stage("smartFinalize", 64 * 1024 * 1024, 2 * 1024 * 1024)?;
    r.measure("smartFinalize", 0, None, "file", "校准输出时间轴");
    // Preserve source decoding configuration and exact VFR timing. PCM edges
    // neither change coded dimensions nor switch lossless transform tables.
    // A PCM first segment has no display matrix, and FFmpeg's output rotate
    // metadata does not create one. Restore the verified source matrix after
    // the last mux, alongside timing, without changing any media bytes.
    crate::smart_mp4::finalize(r, target, config, output_pts, plan.duration_us, &source_matrix)?;
    let (metadata, _) = r.run(
        true,
        &args(&[
            "-v",
            "error",
            "-show_streams",
            "-of",
            "json",
            &target.to_string_lossy(),
        ]),
        4 * 1024 * 1024,
    )?;
    let metadata: Value = serde_json::from_slice(&metadata).map_err(|e| e.to_string())?;
    let result_video = metadata["streams"]
        .as_array()
        .and_then(|s| s.iter().find(|x| x["codec_type"] == "video"))
        .ok_or("OUTPUT_FORMAT: missing video")?;
    if display_transform(result_video) != Some((source_rotation, source_matrix)) {
        return Err("OUTPUT_FORMAT: 导出未保留源画面的旋转方向".into());
    }
    let mut coded = format.clone();
    coded.width = width as u32;
    coded.height = height as u32;
    if source_rotation.abs() % 180 == 90 {
        coded.sar = (format.sar.1, format.sar.0)
    }
    crate::render::validate_output(
        r,
        p,
        (0, plan.duration_us),
        false,
        target,
        Some(&coded),
        has_audio,
        true,
    )?;
    Ok(())
}

#[cfg(test)]
mod clock_tests {
    use super::*;
    #[test]
    fn display_transform_retains_exact_matrix_and_rejects_non_rotations() {
        let matrix = [0, -65536, 0, 65536, 0, 0, 0, 0, 1073741824];
        let metadata = |values: [i32; 9]| json!({"side_data_list":[{
            "side_data_type":"Display Matrix",
            "displaymatrix":format!("0: {} {} {}\n1: {} {} {}\n2: {} {} {}\n",
                values[0],values[1],values[2],values[3],values[4],values[5],values[6],values[7],values[8])
        }]});
        assert_eq!(display_transform(&metadata(matrix)), Some((90, matrix)));
        for (index, value) in [(1, -32768), (6, 123), (8, 65536)] {
            let mut invalid = matrix;
            invalid[index] = value;
            assert!(display_transform(&metadata(invalid)).is_none());
        }
    }
    #[test]
    #[ignore = "Real FFmpeg matrix regression; requires locked media components"]
    fn finalize_restores_matrix_without_changing_encoded_packets() {
        let r = Runner::discover(std::sync::Arc::new(|_, _| {})).unwrap();
        let movie = storage::temp_file(&std::env::temp_dir(), "display-matrix").unwrap();
        let path = movie.path().to_string_lossy();
        r.run(false, &args(&["-v", "error", "-f", "lavfi", "-i", "testsrc2=size=96x64:rate=10:duration=0.4",
            "-c:v", "libx264", "-bf", "0", "-threads", "1", "-video_track_timescale", "1000000", "-f", "mp4", &path]), 1024*1024).unwrap();
        let probe = || {
            let (bytes, _) = r.run(true, &args(&["-v", "error", "-show_streams", "-show_packets", "-show_data_hash", "sha256", "-of", "json", &path]), 1024*1024).unwrap();
            serde_json::from_slice::<Value>(&bytes).unwrap()
        };
        let before = probe();
        assert_eq!(display_transform(&before["streams"][0]).unwrap().0, 0);
        let source_matrix = [0, -65536, 0, 65536, 0, 0, 0, 0, 1073741824];
        crate::smart_mp4::finalize(&r, movie.path(), None, &[0, 100000, 200000, 300000], 400000, &source_matrix).unwrap();
        let after = probe();
        assert_eq!(display_transform(&after["streams"][0]), Some((90, source_matrix)));
        assert_eq!(before["packets"], after["packets"]);
        assert_eq!(after["streams"][0]["width"], 96);
        assert_eq!(after["streams"][0]["height"], 64);
        let (displayed, _) = r.run(false, &args(&["-v", "error", "-i", &path, "-map", "0:v:0", "-f", "framemd5", "pipe:1"]), 1024*1024).unwrap();
        assert!(String::from_utf8(displayed).unwrap().lines().any(|line| line == "#dimensions 0: 64x96"));
    }
    #[test]
    #[ignore = "Real FFmpeg timestamp regression; requires locked media components"]
    fn long_vfr_clock_keeps_every_packet_timestamp_and_duration() {
        let mut stamps = vec![];
        let durations: Vec<Us> = (0..240).map(|i| if i % 3 == 0 { 40_000 } else { 33_333 }).collect();
        let mut at = 0;
        for duration in &durations { stamps.push(at); at += duration; }
        let clock = storage::temp_file(&std::env::temp_dir(), "long-vfr-clock").unwrap();
        fs::write(clock.path(), format!("setts=time_base=1/1000000:ts='{}':duration='{}'", packet_clock(&stamps), packet_clock(&durations))).unwrap();
        let r = Runner::discover(std::sync::Arc::new(|_, _| {})).unwrap();
        let (bytes, _) = r.run(false, &args(&["-v", "error", "-f", "lavfi", "-i", "color=s=16x16:r=30", "-frames:v", "240", "-c:v", "rawvideo", "-threads", "1", "-enc_time_base", "1:1000000", "-/bsf:v", &clock.path().to_string_lossy(), "-f", "framecrc", "pipe:1"]), 1024*1024).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.lines().any(|line| line == "#tb 0: 1/1000000"));
        let packets: Vec<_> = text.lines().filter(|line| !line.starts_with('#') && !line.is_empty()).collect();
        assert_eq!(packets.len(), stamps.len());
        for (i, packet) in packets.iter().enumerate() {
            let fields: Vec<_> = packet.split(',').map(str::trim).collect();
            assert_eq!(fields[1].parse::<Us>().unwrap(), stamps[i]);
            assert_eq!(fields[2].parse::<Us>().unwrap(), stamps[i]);
            assert_eq!(fields[3].parse::<Us>().unwrap(), durations[i]);
        }
    }
}
