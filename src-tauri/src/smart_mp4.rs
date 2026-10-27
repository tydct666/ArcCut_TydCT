//! Bounded edits to our own, non-faststart MP4 intermediates. Media bytes never
//! enter memory here. Reject unexpected layouts instead of touching chunk offsets.
use crate::process::Runner;
use std::{
    fs::OpenOptions,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

const LIMIT: u64 = 64 * 1024 * 1024;
fn be(data: &[u8], at: usize) -> Result<u32, String> {
    data.get(at..at + 4)
        .and_then(|b| b.try_into().ok())
        .map(u32::from_be_bytes)
        .ok_or("SMART_MP4: truncated integer".into())
}
fn children(kind: &[u8]) -> Option<usize> {
    match kind {
        b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" => Some(0),
        b"stsd" => Some(8),
        b"avc1" | b"avc3" => Some(78),
        _ => None,
    }
}
fn find<'a>(data: &'a [u8], wanted: &[u8]) -> Result<Option<&'a [u8]>, String> {
    let mut at = 0;
    while at < data.len() {
        let size = be(data, at)? as usize;
        if size < 8 || at + size > data.len() {
            return Err("SMART_MP4: invalid box".into());
        }
        let kind = &data[at + 4..at + 8];
        let item = &data[at..at + size];
        if kind == wanted {
            return Ok(Some(item));
        }
        if let Some(skip) = children(kind) {
            if 8 + skip > size {
                return Err("SMART_MP4: invalid container".into());
            }
            if let Some(found) = find(&item[8 + skip..], wanted)? {
                return Ok(Some(found));
            }
        }
        at += size
    }
    Ok(None)
}
fn moov(
    r: &Runner,
    path: &Path,
    extra: u64,
) -> Result<(u64, Vec<u8>, crate::resource::MemoryReservation), String> {
    let mut file = OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    let length = file.metadata().map_err(|e| e.to_string())?.len();
    let mut at = 0u64;
    let mut media = false;
    while at + 8 <= length {
        r.check()?;
        file.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
        let mut header = [0u8; 8];
        file.read_exact(&mut header).map_err(|e| e.to_string())?;
        let mut size = u32::from_be_bytes(header[..4].try_into().unwrap()) as u64;
        if size == 1 {
            let mut extended = [0; 8];
            file.read_exact(&mut extended).map_err(|e| e.to_string())?;
            size = u64::from_be_bytes(extended)
        }
        if size < 8 || at.checked_add(size).is_none_or(|n| n > length) {
            return Err("SMART_MP4: invalid file layout".into());
        }
        if &header[4..] == b"mdat" {
            media = true
        }
        if &header[4..] == b"moov" {
            if !media || at + size != length || size > LIMIT {
                return Err("SMART_MP4: metadata must be bounded and follow media".into());
            }
            // Account for the bounded container tree rebuild, not the size of
            // the movie or a pessimistic 64 MiB allocation for every short file.
            let memory = r.reserve_memory(
                size.saturating_add(extra).saturating_mul(12),
                "更新视频封装索引",
            )?;
            let mut data = Vec::new();
            data.try_reserve_exact(size as usize)
                .map_err(|_| "RESOURCE_MEMORY: 无法分配封装索引")?;
            data.resize(size as usize, 0);
            file.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
            file.read_exact(&mut data).map_err(|e| e.to_string())?;
            return Ok((at, data, memory));
        }
        at += size
    }
    Err("SMART_MP4: missing metadata".into())
}
fn rebuild(
    data: &[u8],
    config: Option<&[u8]>,
    timing: &[u8],
    duration: u64,
    video: bool,
    display_matrix: &[i32; 9],
) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    out.try_reserve_exact(data.len().saturating_add(timing.len()))
        .map_err(|_| "RESOURCE_MEMORY: 无法更新封装索引")?;
    let mut at = 0;
    while at < data.len() {
        let size = be(data, at)? as usize;
        if size < 8 || at + size > data.len() {
            return Err("SMART_MP4: invalid nested box".into());
        }
        let item = &data[at..at + size];
        let kind = &item[4..8];
        let video = if kind == b"trak" {
            find(&item[8..], b"avcC")?.is_some()
        } else {
            video
        };
        if video && kind == b"ctts" {
            return Err("SMART_MP4: unexpected reordered video timestamps".into());
        }
        if video && kind == b"stsz" {
            let expected: u64 = timing[16..]
                .chunks_exact(8)
                .map(|run| u32::from_be_bytes(run[..4].try_into().unwrap()) as u64)
                .sum();
            if be(item, 16)? as u64 != expected {
                return Err(format!(
                    "OUTPUT_FRAMES: 实际保留帧数 {} 与剪辑计划 {expected} 不符，未提交成片",
                    be(item, 16)?
                ));
            }
        }
        let replacement = if video && kind == b"avcC" && config.is_some() {
            config.unwrap().to_vec()
        } else if video && kind == b"stts" {
            timing.to_vec()
        } else if let Some(skip) = children(kind) {
            if 8 + skip > size {
                return Err("SMART_MP4: truncated box".into());
            }
            let nested = rebuild(&item[8 + skip..], config, timing, duration, video, display_matrix)?;
            let len = 8 + skip + nested.len();
            if len > LIMIT as usize {
                return Err("RESOURCE_MEMORY: 封装索引超过安全上限".into());
            }
            let mut value = Vec::new();
            value
                .try_reserve_exact(len)
                .map_err(|_| "RESOURCE_MEMORY: 无法分配封装索引")?;
            value.extend_from_slice(&(len as u32).to_be_bytes());
            value.extend_from_slice(&item[4..8 + skip]);
            value.extend(nested);
            value
        } else {
            let mut value = item.to_vec();
            if video && kind == b"tkhd" {
                // tkhd's matrix is after the version-dependent time/duration
                // fields. Write the verified source values verbatim; audio
                // track headers and the encoded video payload stay untouched.
                let offset = match value.get(8) {
                    Some(0) => 48,
                    Some(1) => 60,
                    _ => return Err("SMART_MP4: unsupported track header version".into()),
                };
                let destination = value.get_mut(offset..offset + 36)
                    .ok_or("SMART_MP4: truncated track display matrix")?;
                for (bytes, entry) in destination.chunks_exact_mut(4).zip(display_matrix) {
                    bytes.copy_from_slice(&entry.to_be_bytes());
                }
            }
            if video && kind == b"mdhd" {
                let version = *value.get(8).ok_or("SMART_MP4: missing header")?;
                let ts = if version == 1 { 28 } else { 20 };
                if be(&value, ts)? != 1_000_000 {
                    return Err("SMART_MP4: unexpected video timebase".into());
                }
                let off = ts + 4;
                if version == 1 {
                    value
                        .get_mut(off..off + 8)
                        .ok_or("SMART_MP4: missing duration")?
                        .copy_from_slice(&duration.to_be_bytes())
                } else {
                    let d = u32::try_from(duration)
                        .map_err(|_| "SMART_MP4: duration requires 64-bit header")?;
                    value
                        .get_mut(off..off + 4)
                        .ok_or("SMART_MP4: missing duration")?
                        .copy_from_slice(&d.to_be_bytes())
                }
            }
            value
        };
        out.extend(replacement);
        at += size
    }
    Ok(out)
}
fn timing(pts: &[i64], duration: i64) -> Result<Vec<u8>, String> {
    if pts.first() != Some(&0) || duration <= *pts.last().unwrap_or(&duration) {
        return Err("SMART_MP4: invalid output timing".into());
    }
    if pts.len() as u64 * 8 + 16 > LIMIT / 4 {
        return Err("RESOURCE_MEMORY: 时间戳索引超过安全上限".into());
    }
    let mut runs: Vec<(u32, u32)> = Vec::new();
    runs.try_reserve_exact(pts.len())
        .map_err(|_| "RESOURCE_MEMORY: 无法分配时间戳索引")?;
    for (i, start) in pts.iter().enumerate() {
        let end = pts.get(i + 1).copied().unwrap_or(duration);
        let delta = u32::try_from(end - start).map_err(|_| "SMART_MP4: invalid frame interval")?;
        if delta == 0 {
            return Err("SMART_MP4: duplicate frame timestamp".into());
        }
        if let Some(last) = runs.last_mut().filter(|last| last.1 == delta) {
            last.0 += 1
        } else {
            runs.push((1, delta))
        }
    }
    let len = 16 + runs.len() * 8;
    if len > LIMIT as usize / 4 {
        return Err("RESOURCE_MEMORY: 时间戳索引超过安全上限".into());
    }
    let mut out = Vec::new();
    out.try_reserve_exact(len)
        .map_err(|_| "RESOURCE_MEMORY: 无法分配时间戳索引")?;
    out.extend_from_slice(&(len as u32).to_be_bytes());
    out.extend_from_slice(b"stts");
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(runs.len() as u32).to_be_bytes());
    for (count, delta) in runs {
        out.extend_from_slice(&count.to_be_bytes());
        out.extend_from_slice(&delta.to_be_bytes())
    }
    Ok(out)
}
pub fn finalize(
    r: &Runner,
    path: &Path,
    config: Option<&[u8]>,
    pts: &[i64],
    duration: i64,
    display_matrix: &[i32; 9],
) -> Result<(), String> {
    let (at, data, _memory) = moov(
        r,
        path,
        (pts.len() as u64).saturating_mul(8).saturating_add(16),
    )?;
    let timing = timing(pts, duration)?;
    let updated = rebuild(&data, config, &timing, duration as u64, false, display_matrix)?;
    r.check()?;
    let mut file = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
    file.write_all(&updated).map_err(|e| e.to_string())?;
    file.set_len(at + updated.len() as u64)
        .map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    const IDENTITY: [i32; 9] = [65536, 0, 0, 0, 65536, 0, 0, 0, 1073741824];
    fn atom(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut result = Vec::new();
        result.extend_from_slice(&((body.len() + 8) as u32).to_be_bytes());
        result.extend_from_slice(kind);
        result.extend_from_slice(body);
        result
    }
    fn track_header(version: u8) -> Vec<u8> {
        let mut body = vec![0x23; if version == 1 {96} else {84}];
        body[0] = version;
        atom(b"tkhd", &body)
    }
    #[test]
    fn exact_source_matrix_only_changes_video_header_for_both_header_versions() {
        let matrices = [
            IDENTITY,
            [0, -65536, 0, 65536, 0, 0, 0, 0, 1073741824],
            [0, 65536, 0, -65536, 0, 0, 0, 0, 1073741824],
            [-65536, 0, 0, 0, -65536, 0, 0, 0, 1073741824],
        ];
        let clock = timing(&[0], 40).unwrap();
        for version in [0, 1] {
            for matrix in matrices {
                let header = track_header(version);
                let mut video = header.clone();
                video.extend(atom(b"avcC", &[1, 2, 3, 4]));
                let audio = atom(b"trak", &header);
                let mut movie = atom(b"trak", &video);
                movie.extend_from_slice(&audio);
                let updated = rebuild(&movie, None, &clock, 40, false, &matrix).unwrap();
                let matrix_offset = 8 + if version == 1 {60} else {48};
                let bytes: Vec<_> = matrix.iter().flat_map(|v| v.to_be_bytes()).collect();
                let mut expected = movie.clone();
                expected[matrix_offset..matrix_offset + 36].copy_from_slice(&bytes);
                assert_eq!(updated, expected, "version {version}, matrix {matrix:?}");
                assert_eq!(&updated[updated.len() - audio.len()..], &audio);
            }
        }
    }
    #[test]
    fn invalid_track_headers_cannot_be_patched() {
        let clock = timing(&[0], 40).unwrap();
        for header in [track_header(2), atom(b"tkhd", &[0; 50])] {
            assert!(rebuild(&header, None, &clock, 40, true, &IDENTITY).is_err());
        }
    }
    #[test]
    fn exact_variable_frame_clock() {
        let t = timing(&[0, 16667, 33333, 50000], 70000).unwrap();
        assert_eq!(be(&t, 12).unwrap(), 4);
        assert!(timing(&[0, 0], 1).is_err());
        assert!(timing(&[1, 2], 3).is_err())
    }
    #[test]
    fn corrupt_boxes_are_rejected() {
        assert!(find(&[0, 0, 0, 20, b'm', b'o', b'o', b'v'], b"avcC").is_err())
    }
    #[test]
    fn timestamps_never_hide_missing_video_samples() {
        let mut table = vec![
            0, 0, 0, 20, b's', b't', b's', b'z', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3,
        ];
        let clock = timing(&[0, 10, 20, 30], 40).unwrap();
        assert!(rebuild(&table, None, &clock, 40, true, &IDENTITY)
            .unwrap_err()
            .starts_with("OUTPUT_FRAMES:"));
        table[19] = 4;
        assert!(rebuild(&table, None, &clock, 40, true, &IDENTITY).is_ok());
    }
}
