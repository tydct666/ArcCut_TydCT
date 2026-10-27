use crate::model::*;
pub fn snap(pts: &[Us], t: Us, duration: Us) -> Us {
    if t >= duration {
        return duration;
    }
    if pts.is_empty() {
        return t;
    }
    let i = pts.partition_point(|x| *x < t);
    if i == 0 {
        return pts[0];
    }
    if i == pts.len() {
        return *pts.last().unwrap();
    }
    if t - pts[i - 1] <= pts[i] - t {
        pts[i - 1]
    } else {
        pts[i]
    }
}
pub fn build(p: &Project) -> Result<Plan, String> {
    if p.schema_version != 1 {
        return Err("不支持的工程版本".into());
    }
    let d = p.source.duration_us;
    if d <= 0 {
        return Err("无效媒体时长".into());
    }
    if !p
        .source
        .streams
        .iter()
        .any(|s| s.index == p.video_index && s.kind == "video")
    {
        return Err("视频流索引无效".into());
    }
    if let Some(i) = p.audio_index {
        if !p
            .source
            .streams
            .iter()
            .any(|s| s.index == i && s.kind == "audio")
        {
            return Err("游戏音轨索引无效".into());
        }
    }
    if !(0. ..=2.).contains(&p.mix.game_gain) || !(0. ..=1.).contains(&p.mix.music_gain) {
        return Err("音量超出范围".into());
    }
    let mut cuts = Vec::<(Us, Us)>::new();
    for x in &p.pauses {
        if x.start_us < 0 || x.end_us > d || x.start_us >= x.end_us {
            return Err(format!("区间 {} 非法：需要 0 ≤ 起点 < 终点 ≤ 时长", x.id));
        }
        if x.enabled && x.state == "confirmed" {
            let s = snap(&p.source.frame_pts, x.start_us, d);
            let e = snap(&p.source.frame_pts, x.end_us, d);
            if s >= e {
                return Err("吸附帧边界后区间为空".into());
            }
            cuts.push((s, e));
        }
    }
    cuts.sort();
    let mut merged = Vec::<(Us, Us)>::new();
    for (s, e) in cuts {
        if let Some(last) = merged.last_mut() {
            if s <= last.1 {
                last.1 = last.1.max(e);
                continue;
            }
        }
        merged.push((s, e));
    }
    let (mut cursor, mut out) = (0, 0);
    let (mut keep, mut removed) = (vec![], vec![]);
    for (s, e) in merged {
        if s > cursor {
            keep.push(Span {
                start_us: cursor,
                end_us: s,
                output_us: out,
            });
            out += s - cursor;
        }
        removed.push(Span {
            start_us: s,
            end_us: e,
            output_us: out,
        });
        cursor = e;
    }
    if cursor < d {
        keep.push(Span {
            start_us: cursor,
            end_us: d,
            output_us: out,
        });
        out += d - cursor;
    }
    let music = playlist(p, out)?;
    Ok(Plan {
        version: PLAN_VERSION.into(),
        project_id: p.project_id.clone(),
        revision: p.revision,
        keep,
        removed,
        duration_us: out,
        tails: vec![],
        music,
        loudness_gain: vec![1.; p.music.len()],
        warnings: vec![],
    })
}
pub fn source_to_output(plan: &Plan, t: Us) -> Option<Us> {
    plan.keep
        .iter()
        .find(|s| t >= s.start_us && t < s.end_us)
        .map(|s| s.output_us + t - s.start_us)
}
pub fn output_to_source(plan: &Plan, t: Us) -> Option<Us> {
    plan.keep
        .iter()
        .find(|s| t >= s.output_us && t < s.output_us + s.end_us - s.start_us)
        .map(|s| s.start_us + t - s.output_us)
}
pub fn preview_range(plan: &Plan, position: Option<Us>) -> (Us, Us) {
    let at = position.unwrap_or_else(|| plan.removed.first().map(|x| x.output_us).unwrap_or(0));
    (
        (at - 3 * SECOND).max(0),
        (at + 3 * SECOND).min(plan.duration_us),
    )
}
pub fn playlist(p: &Project, d: Us) -> Result<Vec<Placement>, String> {
    if p.music.is_empty() || d == 0 {
        return Ok(vec![]);
    }
    if !["single", "loop", "once", "shuffle"].contains(&p.mix.mode.as_str()) {
        return Err("未知音乐模式".into());
    }
    if p.music
        .iter()
        .any(|m| m.media.duration_us <= 0 || m.media.audio_index.is_none())
    {
        return Err("音乐没有有效音轨或时长".into());
    }
    let current = p
        .mix
        .current_track
        .as_ref()
        .and_then(|id| p.music.iter().position(|m| &m.id == id))
        .unwrap_or(0);
    let mut result: Vec<Placement> = vec![];
    let mut index = if p.mix.mode == "single" {
        current
    } else if p.mix.mode == "shuffle" {
        use sha2::{Digest, Sha256};
        let seed = Sha256::digest(format!("{}:music-start", p.project_id));
        u32::from_le_bytes(seed[..4].try_into().unwrap()) as usize % p.music.len()
    } else {
        0
    };
    let mut start = 0;
    let mut fade_in = 0;
    while start < d {
        if result.len() >= 4096 {
            return Err("音乐排布超过 4096 段，请使用较长曲目".into());
        }
        let length = p.music[index].media.duration_us;
        let next = if p.mix.mode == "shuffle" && p.music.len() > 1 {
            use sha2::{Digest, Sha256};
            let hash = Sha256::digest(format!(
                "{}:{}:{}",
                p.project_id,
                p.music[index].id,
                result.len()
            ));
            (index
                + 1
                + (u32::from_le_bytes(hash[..4].try_into().unwrap()) as usize
                    % (p.music.len() - 1)))
                % p.music.len()
        } else if p.mix.mode == "single" {
            index
        } else {
            (index + 1) % p.music.len()
        };
        let last = (p.mix.mode == "once" && index == p.music.len() - 1) || start + length >= d;
        let overlap = if last {
            0
        } else {
            (if next == index { 200000 } else { SECOND })
                .min(length / 2)
                .min(p.music[next].media.duration_us / 2)
        };
        result.push(Placement {
            track: index,
            output_us: start,
            duration_us: length.min(d - start),
            fade_in_us: fade_in,
            fade_out_us: overlap,
        });
        if last {
            break;
        }
        start += length - overlap;
        index = next;
        fade_in = overlap;
    }
    Ok(result)
}
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub fn project() -> Project {
        Project::new(Media {
            title: None,
            fingerprint: Fingerprint {
                path: "fixture".into(),
                size: 1,
                modified_ms: 0,
                sample_hash: "a".into(),
            },
            duration_us: 10 * SECOND,
            format: "mp4".into(),
            streams: vec![Stream {
                index: 0,
                kind: "video".into(),
                codec: "h264".into(),
                language: "und".into(),
                channels: 0,
                channel_layout: String::new(),
                width: 640,
                height: 360,
                rate: "30/1".into(),
                time_base: "1/90000".into(),
                start_us: 0,
                default: true,
            }],
            video_index: Some(0),
            audio_index: None,
            origin_us: 0,
            frame_pts: vec![],
            hdr: false,
            warnings: vec![],
        })
        .unwrap()
    }
    fn cut(s: Us, e: Us) -> Pause {
        Pause {
            id: format!("{s}"),
            start_us: s,
            end_us: e,
            enabled: true,
            source: "manual".into(),
            state: "confirmed".into(),
            score: 0.,
            audio_mode: None,
            detected_bounds: None,
            transition: None,
        }
    }
    #[test]
    fn exact_example() {
        let mut p = project();
        p.pauses = vec![cut(2 * SECOND, 4 * SECOND), cut(6 * SECOND, 7 * SECOND)];
        let plan = build(&p).unwrap();
        assert_eq!(plan.duration_us, 7 * SECOND);
        assert_eq!(source_to_output(&plan, 5 * SECOND), Some(3 * SECOND));
        assert_eq!(output_to_source(&plan, 3 * SECOND), Some(5 * SECOND));
        assert_eq!(source_to_output(&plan, 2 * SECOND), None);
    }
    #[test]
    fn boundaries_and_validation() {
        let mut p = project();
        p.pauses = vec![
            cut(0, SECOND),
            cut(SECOND, 2 * SECOND),
            cut(SECOND, 3 * SECOND),
            cut(9 * SECOND, 10 * SECOND),
        ];
        let a = build(&p).unwrap();
        assert_eq!(a.duration_us, 6 * SECOND);
        assert_eq!(a.removed.len(), 2);
        p.pauses = vec![cut(0, 10 * SECOND)];
        assert_eq!(build(&p).unwrap().duration_us, 0);
        p.pauses[0].end_us += 1;
        assert!(build(&p).is_err());
    }
    #[test]
    fn vfr_snap_and_no_gap_merge() {
        let mut p = project();
        p.source.frame_pts = vec![0, 40000, 100000, 130000, 200000, SECOND];
        p.pauses = vec![cut(42000, 95000), cut(132000, 190000)];
        let a = build(&p).unwrap();
        assert_eq!(a.removed.len(), 2);
        assert_eq!(a.removed[0].start_us, 40000);
        assert_eq!(a.removed[1].start_us, 130000);
    }
    #[test]
    fn short_playlist_makes_progress() {
        let mut p = project();
        let mut m = p.source.clone();
        m.audio_index = Some(1);
        m.duration_us = 10000;
        p.music.push(Music {
            id: "one".into(),
            media: m,
        });
        let x = playlist(&p, SECOND).unwrap();
        assert_eq!(x.len(), 199);
        assert_eq!(x[1].output_us, 5000);
        p.mix.mode = "once".into();
        assert_eq!(playlist(&p, SECOND).unwrap().len(), 1);
    }
    #[test]
    fn preview_window() {
        let p = build(&project()).unwrap();
        assert_eq!(
            preview_range(&p, Some(9 * SECOND)),
            (6 * SECOND, 10 * SECOND)
        );
    }
    #[test]
    fn shuffle_is_stable_across_revisions_and_never_repeats_adjacent_tracks() {
        let mut p = project();
        for i in 0..3 {
            let mut m = p.source.clone();
            m.audio_index = Some(0);
            m.duration_us = 2 * SECOND;
            p.music.push(Music {
                id: i.to_string(),
                media: m,
            })
        }
        p.mix.mode = "shuffle".into();
        let first = playlist(&p, 60 * SECOND).unwrap();
        p.revision += 1;
        p.mix.music_gain = 0.6;
        let second = playlist(&p, 60 * SECOND).unwrap();
        assert_eq!(
            serde_json::to_value(&first).unwrap(),
            serde_json::to_value(&second).unwrap()
        );
        assert!(first.windows(2).all(|x| x[0].track != x[1].track));
        assert!(first.iter().all(|x| x.track < 3 && x.duration_us > 0));
    }
}
