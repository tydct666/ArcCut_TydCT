use crate::{model::*, process::*};
pub struct AudioSamples {
    data: Vec<f32>,
    _memory: crate::resource::MemoryReservation,
}
impl std::ops::Deref for AudioSamples {
    type Target = [f32];
    fn deref(&self) -> &[f32] {
        &self.data
    }
}
pub fn activity(samples: &[f32], threshold: f64) -> bool {
    if samples.is_empty() {
        return false;
    }
    let power = samples.iter().map(|x| (*x as f64).powi(2)).sum::<f64>() / samples.len() as f64;
    power > 10f64.powf(threshold / 10.)
}
pub fn tail_length(samples: &[f32], max_us: Us, threshold: f64) -> Us {
    let frame = 960 * 2; // 20 ms, stereo, 48 kHz
    if samples.len() < frame * 10
        || !samples[..frame * 5]
            .chunks(frame)
            .any(|w| activity(w, threshold))
        || !samples[frame * 5..frame * 10]
            .chunks(frame)
            .any(|w| activity(w, threshold))
    {
        return 0;
    }
    let mut quiet = 0;
    let mut end = 0;
    for (i, window) in samples[frame * 5..].chunks(frame).enumerate() {
        end = ((i + 1) as Us * 20000).min(max_us);
        if activity(window, threshold) {
            quiet = 0
        } else {
            quiet += 1
        }
        if quiet >= 6 {
            end = (end - 120000).max(0);
            break;
        }
        if end >= max_us {
            break;
        }
    }
    end
}
pub fn enrich(r: &Runner, p: &Project, plan: &mut Plan) -> Result<(), String> {
    if p.mix.tails {
        if p.audio_index.is_some() {
            for (i, c) in plan.removed.iter().enumerate() {
                r.check()?;
                r.measure(
                    "audioBridge",
                    i as i64,
                    Some(plan.removed.len() as i64),
                    "cuts",
                    "处理游戏声音衔接",
                );
                let mode = p
                    .pauses
                    .iter()
                    .find(|x| x.enabled && x.start_us == c.start_us)
                    .and_then(|x| x.audio_mode.as_deref())
                    .unwrap_or(&p.mix.join_mode);
                if mode == "basic" {
                    continue;
                }
                if !["legacy", "natural"].contains(&mode) {
                    return Err("未知语音衔接模式".into());
                }
                if c.start_us < 100000 || c.output_us >= plan.duration_us {
                    continue;
                }
                let next = plan
                    .removed
                    .get(i + 1)
                    .map(|x| x.output_us)
                    .unwrap_or(plan.duration_us);
                let max = (c.end_us - c.start_us)
                    .min(1500000)
                    .min(next - c.output_us)
                    .min(plan.duration_us - c.output_us);
                if max <= 0 {
                    continue;
                }
                r.prepare_stage("audioBridge", 8 * 1024 * 1024, 2 * 1024 * 1024)?;
                let samples = audio_slice(r, p, c.start_us - 100000, c.start_us + max.max(100000))?;
                let mut length = tail_length(&samples, max, p.detection.activity_db);
                if length > 0 {
                    let (mut envelope, mut notes) = (vec![], vec![]);
                    if mode == "natural" {
                        length = low_energy_end(&samples[9600..], length);
                        let restored = audio_slice(
                            r,
                            p,
                            c.end_us,
                            (c.end_us + length).min(p.source.duration_us),
                        )?;
                        (envelope, notes) = natural_envelope(&samples[9600..], &restored, length);
                        r.check()?;
                    }
                    plan.tails.push(Tail {
                        source_us: c.start_us,
                        output_us: c.output_us,
                        duration_us: length,
                        gain: if mode == "natural" {
                            1.
                        } else {
                            10f64.powf(-6. / 20.)
                        },
                        mode: mode.into(),
                        envelope,
                        notes,
                    });
                }
            }
        }
    }
    if p.mix.normalize {
        for (i, m) in p.music.iter().enumerate() {
            r.check()?;
            (r.progress)("loudness", None);
            r.measure(
                "loudness",
                0,
                Some(m.media.duration_us),
                "us",
                &format!("分析音乐响度 · 第 {} / {} 首", i + 1, p.music.len()),
            );
            let ai = m.media.audio_index.ok_or("音乐无音轨")?;
            r.prepare_stage("loudness", 8 * 1024 * 1024, 2 * 1024 * 1024)?;
            let result = r.ff(args(&[
                "-hide_banner",
                "-i",
                &m.media.fingerprint.path,
                "-map",
                &format!("0:{ai}"),
                "-af",
                "ebur128=peak=none:framelog=verbose",
                "-f",
                "null",
                "-progress",
                "pipe:2",
                "-nostats",
                "-",
            ]));
            match result {
                Ok(log) => {
                    let measured = log.lines().rev().find_map(|line| {
                        line.trim()
                            .strip_prefix("I:")?
                            .split_whitespace()
                            .next()?
                            .parse::<f64>()
                            .ok()
                    });
                    if let Some(lufs) = measured.filter(|x| x.is_finite() && *x > -69.9) {
                        plan.loudness_gain[i] = 10f64.powf((-18. - lufs).clamp(-30., 18.) / 20.);
                    } else {
                        plan.warnings
                            .push(format!("曲目 {} 无法可靠测量响度，使用原增益", i + 1));
                    }
                }
                Err(e) => {
                    r.check()?;
                    if e.starts_with("RESOURCE_") {
                        return Err(e);
                    }
                    plan.warnings
                        .push(format!("曲目 {} 响度测量失败，使用原增益：{e}", i + 1));
                }
            }
        }
    }
    Ok(())
}
pub fn audio_slice(r: &Runner, p: &Project, start: Us, end: Us) -> Result<AudioSamples, String> {
    r.check()?;
    if start < 0 || end < start || end - start > 2 * SECOND {
        return Err("AUDIO_SLICE: 语音衔接仅允许有界的两秒内音频切片".into());
    }
    // Tail analysis needs at most 1.6 seconds, regardless of recording length.
    // Budget bytes and f32 conversion together; keep the reservation with samples.
    let limit = (((end - start) as u128 * 48000 * 2 * 4 / SECOND as u128) as usize)
        .saturating_add(48000 * 2 * 4 / 10);
    let memory = r.reserve_memory(limit as u64 * 2, "语音衔接音频缓冲")?;
    let Some(ai) = p.audio_index else {
        return Ok(AudioSamples {
            data: vec![],
            _memory: memory,
        });
    };
    let seek = if start >= 200000 {
        start + p.source.origin_us - 200000
    } else {
        0
    };
    let filter=format!("asetpts=PTS-({})/TB,atrim=start={}:end={},asetpts=PTS-({})/TB,aresample=48000:async=1:min_comp=0:min_hard_comp=0:first_pts=0,aformat=channel_layouts=stereo",crate::render::sec(p.source.origin_us),crate::render::sec(start),crate::render::sec(end),crate::render::sec(start));
    let (bytes, _) = r.run(
        false,
        &args(&[
            "-v",
            "error",
            "-copyts",
            "-seek_timestamp",
            "1",
            "-ss",
            &crate::render::sec(seek),
            "-t",
            &crate::render::sec(end + p.source.origin_us - seek + 100000),
            "-i",
            &p.source.fingerprint.path,
            "-map",
            &format!("0:{ai}"),
            "-vn",
            "-af",
            &filter,
            "-f",
            "f32le",
            "pipe:1",
        ]),
        limit,
    )?;
    if bytes.len() % 4 != 0 {
        return Err("AUDIO_DECODE: 音频样本数据截断".into());
    }
    let mut data = Vec::new();
    data.try_reserve_exact(bytes.len() / 4)
        .map_err(|_| "RESOURCE_MEMORY: 无法分配音频缓冲，请关闭部分应用后重试")?;
    for chunk in bytes.chunks(16384) {
        r.check()?;
        data.extend(
            chunk
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes(b.try_into().unwrap())),
        )
    }
    Ok(AudioSamples {
        data,
        _memory: memory,
    })
}
fn rms(x: &[f32]) -> f64 {
    if x.is_empty() {
        0.
    } else {
        (x.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
    }
}
fn low_energy_end(samples: &[f32], limit: Us) -> Us {
    let sample_limit = ((limit as i128 * 48000 / SECOND as i128) as usize).min(samples.len() / 2);
    if sample_limit < 4800 {
        return limit;
    }
    let start = sample_limit.saturating_sub(7680);
    let mut best = sample_limit;
    let mut power = f64::INFINITY;
    for at in (start..sample_limit).step_by(960) {
        let b = (at + 960).min(sample_limit);
        let level = rms(&samples[at * 2..b * 2]);
        if level < power {
            power = level;
            best = b;
        }
    }
    let a = best.saturating_sub(120);
    let b = (best + 120).min(sample_limit);
    let quiet = (a..b)
        .min_by(|&x, &y| {
            let xx = (samples[x * 2] as f64).abs() + (samples[x * 2 + 1] as f64).abs();
            let yy = (samples[y * 2] as f64).abs() + (samples[y * 2 + 1] as f64).abs();
            xx.total_cmp(&yy)
        })
        .unwrap_or(best);
    ((quiet as i128 * SECOND as i128 / 48000) as Us).min(limit)
}
pub fn natural_envelope(
    tail: &[f32],
    resumed: &[f32],
    length: Us,
) -> (Vec<GainPoint>, Vec<String>) {
    let mut points = vec![GainPoint {
        time_us: 0,
        gain: 1.,
    }];
    let mut duck: f64 = 1.;
    let mut notes = vec![];
    let window = 1920;
    let count = ((length + 19999) / 20000) as usize;
    let mut strongest: f64 = 0.;
    for i in 1..=count {
        let a = (i - 1) * window;
        let b = (a + window).min(tail.len());
        let t = (i as Us * 20000).min(length);
        let ta = rms(tail.get(a..b).unwrap_or(&[]));
        let ra = rms(resumed
            .get(a.min(resumed.len())..(a + window).min(resumed.len()))
            .unwrap_or(&[]));
        let db = if ra > 0.01 && ra > ta * 1.25 {
            3. + 6. * ((ra / ta.max(0.0001)).log10() / 0.9).clamp(0., 1.)
        } else {
            0.
        };
        strongest = strongest.max(db);
        let target = 10f64.powf(-db / 20.);
        let tau = if target < duck { 0.04 } else { 0.15 };
        duck += (target - duck) * (1. - (-0.02f64 / tau).exp());
        let fade = (120000.min(length / 2)) as f64;
        let envelope = if fade > 0. {
            (((length - t) as f64 / fade).clamp(0., 1.) * std::f64::consts::FRAC_PI_2).sin()
        } else {
            0.
        };
        points.push(GainPoint {
            time_us: t,
            gain: duck * envelope,
        });
    }
    if strongest >= 3. {
        notes.push(format!(
            "恢复音频活动较强，尾音平滑衰减最大目标 {strongest:.1} dB；基础事件时序保持"
        ));
    }
    let n = tail.len().min(resumed.len()).min(48000);
    if n >= 1920 {
        let (mut dot, mut aa, mut bb) = (0., 0., 0.);
        for i in (0..n).step_by(8) {
            let a = tail[i] as f64;
            let b = resumed[i] as f64;
            dot += a * b;
            aa += a * a;
            bb += b * b
        }
        let corr = dot / (aa * bb).sqrt().max(1e-12);
        if corr.abs() > 0.75 {
            notes.push(format!(
                "局部相关性 {:.2}，疑似重复／相关声音，建议 A/B 试听",
                corr
            ));
        }
    }
    // Remove near-linear points without changing attack/release or endpoint levels.
    let mut compact = vec![points[0].clone()];
    for i in 1..points.len().saturating_sub(1) {
        let a = compact.last().unwrap();
        let b = &points[i + 1];
        let at = &points[i];
        let linear = a.gain
            + (b.gain - a.gain) * (at.time_us - a.time_us) as f64
                / (b.time_us - a.time_us).max(1) as f64;
        if (linear - at.gain).abs() > 0.003 {
            compact.push(at.clone());
        }
    }
    if let Some(end) = points.last() {
        compact.push(end.clone())
    }
    (compact, notes)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn natural_envelope_starts_continuously_and_finishes_at_zero() {
        let tail = vec![0.03; 96000];
        let restored = vec![0.25; 96000];
        let (points, notes) = natural_envelope(&tail, &restored, 1000000);
        assert_eq!(points[0].gain, 1.);
        assert_eq!(points.last().unwrap().gain, 0.);
        assert_eq!(points.last().unwrap().time_us, 1000000);
        assert!(points.iter().all(|p| p.gain >= 0. && p.gain <= 1.));
        assert!(points.windows(2).all(|w| w[1].time_us > w[0].time_us));
        assert!(!notes.is_empty());
    }
    #[test]
    fn tails_activity_and_silence() {
        let active = vec![0.1; 48000 * 2 * 2];
        assert_eq!(tail_length(&active, 1500000, -40.), 1500000);
        assert_eq!(tail_length(&vec![0.; 48000 * 2], 800000, -40.), 0);
        let mut mixed = active.clone();
        mixed[19200..].fill(0.);
        assert_eq!(tail_length(&mixed, 1500000, -40.), 100000);
    }
}
