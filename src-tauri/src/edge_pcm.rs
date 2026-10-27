//! Exact H.264 edge pictures without switching transform modes or coded size.
//! Changed macroblocks use I_PCM; identical blocks use zero-motion P_SKIP.
//! The original SPS is retained (only its ID changes). No source packet is edited.
use std::io::Write;

struct Bits {
    data: Vec<u8>,
    n: usize,
}
impl Bits {
    fn new() -> Self {
        Self {
            data: Vec::new(),
            n: 0,
        }
    }
    fn bit(&mut self, v: bool) {
        if self.n % 8 == 0 {
            self.data.push(0);
        }
        if v {
            *self.data.last_mut().unwrap() |= 1 << (7 - self.n % 8);
        }
        self.n += 1;
    }
    fn uint(&mut self, n: usize, v: u32) {
        for i in (0..n).rev() {
            self.bit((v >> i) & 1 != 0);
        }
    }
    fn ue(&mut self, v: u32) {
        let n = 32 - (v + 1).leading_zeros() as usize;
        self.uint(n - 1, 0);
        self.uint(n, v + 1);
    }
    fn se(&mut self, v: i32) {
        self.ue(if v <= 0 {
            (-v * 2) as u32
        } else {
            (v * 2 - 1) as u32
        });
    }
    fn align(&mut self) {
        while self.n % 8 != 0 {
            self.bit(false);
        }
    }
    fn finish(mut self) -> Vec<u8> {
        self.bit(true);
        self.align();
        self.data
    }
}
struct Reader<'a> {
    data: &'a [u8],
    n: usize,
}
impl Reader<'_> {
    fn uint(&mut self, n: usize) -> Option<u32> {
        if n > 32 || self.n + n > self.data.len() * 8 {
            return None;
        }
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | ((self.data[self.n / 8] >> (7 - self.n % 8)) & 1) as u32;
            self.n += 1;
        }
        Some(v)
    }
    fn ue(&mut self) -> Option<u32> {
        let mut n = 0;
        while self.uint(1)? == 0 {
            n += 1;
            if n > 24 {
                return None;
            }
        }
        Some((1 << n) - 1 + self.uint(n)?)
    }
}
fn rbsp(nal: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut zeros = 0;
    for (i, &b) in nal.iter().enumerate() {
        if zeros == 2 && b == 3 {
            if nal.get(i + 1).is_none_or(|v| *v > 3) {
                return None;
            }
            zeros = 0;
            continue;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    Some(out)
}
fn write_nal(w: &mut impl Write, header: u8, raw: &[u8]) -> Result<(), String> {
    w.write_all(&[0, 0, 0, 1, header])
        .map_err(|e| e.to_string())?;
    let mut begin = 0;
    let mut zeros = 0;
    for (i, &b) in raw.iter().enumerate() {
        if zeros == 2 && b <= 3 {
            w.write_all(&raw[begin..i]).map_err(|e| e.to_string())?;
            w.write_all(&[3]).map_err(|e| e.to_string())?;
            begin = i;
            zeros = 0;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    w.write_all(&raw[begin..]).map_err(|e| e.to_string())
}
pub fn hex_dump(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let (_, rest) = line.split_once(':')?;
        let hex = rest.trim_start().split("  ").next()?;
        let compact: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
        if compact.len() % 2 != 0 || out.len() + compact.len() / 2 > 65536 {
            return None;
        }
        for pair in compact.as_bytes().chunks_exact(2) {
            out.push(u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?);
        }
    }
    Some(out)
}
#[derive(Clone)]
pub struct Settings {
    pub source_sps: Vec<u8>,
    pub source_pps: Vec<u8>,
    sps: Vec<u8>,
    frame_bits: usize,
    width: usize,
    height: usize,
    mb_width: usize,
    mb_height: usize,
}
impl Settings {
    /// Deliberately narrow eligibility: a single SPS/PPS with IDs zero, 8-bit
    /// progressive 4:2:0, POC type 2, and right/bottom-only cropping.
    pub fn from_avcc(data: &[u8], width: usize, height: usize) -> Option<Self> {
        if data.len() < 8 || data[0] != 1 || data[4] & 3 != 3 || data[5] & 31 != 1 {
            return None;
        }
        let n = u16::from_be_bytes(data[6..8].try_into().ok()?) as usize;
        let sps = data.get(8..8 + n)?.to_vec();
        let at = 8 + n;
        if *data.get(at)? != 1 {
            return None;
        }
        let n = u16::from_be_bytes(data.get(at + 1..at + 3)?.try_into().ok()?) as usize;
        let pps = data.get(at + 3..at + 3 + n)?.to_vec();
        if sps.first().map(|b| b & 31) != Some(7) || pps.first().map(|b| b & 31) != Some(8) {
            return None;
        }
        let raw = rbsp(&sps)?;
        if raw.get(2)? & 0x10 != 0 {
            return None;
        }
        let pr = rbsp(&pps)?;
        let mut pp = Reader { data: &pr, n: 8 };
        if pp.ue()? != 0 || pp.ue()? != 0 {
            return None;
        }
        let mut r = Reader { data: &raw, n: 8 };
        let profile = r.uint(8)?;
        r.uint(16)?;
        if r.ue()? != 0 {
            return None;
        }
        let id_end = r.n;
        if [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135].contains(&profile) {
            if r.ue()? != 1 || r.ue()? != 0 || r.ue()? != 0 {
                return None;
            }
            r.uint(1)?;
            if r.uint(1)? != 0 {
                return None;
            }
        } else if ![66, 77, 88].contains(&profile) {
            return None;
        }
        let frame_bits = (r.ue()? + 4) as usize;
        if frame_bits > 16 || r.ue()? != 2 {
            return None;
        }
        let refs = r.ue()?;
        if refs == 0 || refs > 16 {
            return None;
        }
        r.uint(1)?;
        let mb_width = r.ue()? as usize + 1;
        let mb_height = r.ue()? as usize + 1;
        if r.uint(1)? != 1 {
            return None;
        }
        r.uint(1)?;
        let (left, right, top, bottom) = if r.uint(1)? != 0 {
            (
                r.ue()? as usize,
                r.ue()? as usize,
                r.ue()? as usize,
                r.ue()? as usize,
            )
        } else {
            (0, 0, 0, 0)
        };
        if left != 0
            || top != 0
            || width > 8192
            || height > 8192
            || mb_width != width.div_ceil(16)
            || mb_height != height.div_ceil(16)
            || mb_width.checked_mul(16)?.checked_sub(right * 2)? != width
            || mb_height.checked_mul(16)?.checked_sub(bottom * 2)? != height
            || width % 2 != 0
            || height % 2 != 0
        {
            return None;
        }
        // Reject HRD/SEI-dependent and per-picture byte-restricted streams.
        // The bounded PCM path does not synthesize buffering/timing SEI.
        if r.uint(1)? != 0 {
            if r.uint(1)? != 0 && r.uint(8)? == 255 {
                r.uint(16)?;
                r.uint(16)?;
            }
            if r.uint(1)? != 0 {
                r.uint(1)?;
            }
            if r.uint(1)? != 0 {
                r.uint(4)?;
                if r.uint(1)? != 0 {
                    r.uint(24)?;
                }
            }
            if r.uint(1)? != 0 {
                r.ue()?;
                r.ue()?;
            }
            if r.uint(1)? != 0 {
                r.uint(32)?;
                r.uint(32)?;
                r.uint(1)?;
            }
            if r.uint(1)? != 0 || r.uint(1)? != 0 || r.uint(1)? != 0 {
                return None;
            }
            if r.uint(1)? != 0 {
                r.uint(1)?;
                if r.ue()? != 0 || r.ue()? != 0 {
                    return None;
                }
                r.ue()?;
                r.ue()?;
                if r.ue()? != 0 || r.ue()? == 0 {
                    return None;
                }
            }
        }
        // Repack the original RBSP with SPS ID 1, preserving every other field.
        let last = raw.iter().rposition(|b| *b != 0)?;
        let meaningful = last * 8 + 8 - raw[last].trailing_zeros() as usize;
        if meaningful <= id_end {
            return None;
        }
        let mut b = Bits::new();
        for i in 8..32 {
            b.bit(raw[i / 8] >> (7 - i % 8) & 1 != 0);
        }
        b.ue(1);
        for i in id_end..meaningful {
            b.bit(raw[i / 8] >> (7 - i % 8) & 1 != 0);
        }
        b.align();
        Some(Self {
            source_sps: sps,
            source_pps: pps,
            sps: b.data,
            frame_bits,
            width,
            height,
            mb_width,
            mb_height,
        })
    }
}
pub struct Encoder<W: Write> {
    writer: W,
    settings: Settings,
    previous: Vec<u8>,
    index: usize,
    pub pcm_blocks: u64,
    pub skipped_blocks: u64,
}
impl<W: Write> Encoder<W> {
    pub fn new(mut writer: W, settings: Settings) -> Result<Self, String> {
        write_nal(&mut writer, 0x67, &settings.sps)?;
        let mut p = Bits::new();
        p.ue(1);
        p.ue(1);
        p.bit(false);
        p.bit(false);
        p.ue(0);
        p.ue(0);
        p.ue(0);
        p.bit(false);
        p.uint(2, 0);
        p.se(0);
        p.se(0);
        p.se(0);
        p.bit(true);
        p.bit(false);
        p.bit(false);
        write_nal(&mut writer, 0x68, &p.finish())?;
        let mut previous = Vec::new();
        previous
            .try_reserve_exact(settings.width * settings.height * 3 / 2)
            .map_err(|_| "RESOURCE_MEMORY: 无法分配无损边缘参考帧")?;
        Ok(Self {
            writer,
            settings,
            previous,
            index: 0,
            pcm_blocks: 0,
            skipped_blocks: 0,
        })
    }
    pub fn frame(
        &mut self,
        frame: &[u8],
        mut check: impl FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        let s = &self.settings;
        let size = s.width * s.height * 3 / 2;
        if frame.len() != size {
            return Err("SMART_PCM: 无损边缘帧尺寸不匹配".into());
        }
        let first = self.index == 0;
        let mut b = Bits::new();
        b.data
            .try_reserve_exact(s.mb_width * s.mb_height * 390 + 128)
            .map_err(|_| "RESOURCE_MEMORY: 无法分配无损边缘数据")?;
        b.ue(0);
        b.ue(if first { 2 } else { 0 });
        b.ue(1);
        b.uint(
            s.frame_bits,
            (self.index & ((1 << s.frame_bits) - 1)) as u32,
        );
        if first {
            b.ue(0);
            b.bit(false);
            b.bit(false);
        } else {
            b.bit(false);
            b.bit(false);
            b.bit(false);
        }
        b.se(0);
        b.ue(1);
        let mut skipped = 0;
        for my in 0..s.mb_height {
            check()?;
            for mx in 0..s.mb_width {
                let mut block = [0u8; 384];
                let mut at = 0;
                let mut offset = 0;
                let mut same = !first;
                for (width, height, n) in [
                    (s.width, s.height, 16),
                    (s.width / 2, s.height / 2, 8),
                    (s.width / 2, s.height / 2, 8),
                ] {
                    for y in 0..n {
                        let row = offset + (my * n + y).min(height - 1) * width;
                        for x in 0..n {
                            let pos = row + (mx * n + x).min(width - 1);
                            block[at] = frame[pos];
                            at += 1;
                            if same && self.previous[pos] != frame[pos] {
                                same = false;
                            }
                        }
                    }
                    offset += width * height;
                }
                if same {
                    skipped += 1;
                    self.skipped_blocks += 1;
                    continue;
                }
                if !first {
                    b.ue(skipped);
                    skipped = 0;
                }
                b.ue(if first { 25 } else { 30 });
                b.align();
                b.data.extend_from_slice(&block);
                b.n += 384 * 8;
                self.pcm_blocks += 1;
            }
        }
        if skipped > 0 {
            b.ue(skipped);
        }
        write_nal(
            &mut self.writer,
            if first { 0x65 } else { 0x41 },
            &b.finish(),
        )?;
        self.previous.clear();
        self.previous.extend_from_slice(frame);
        self.index += 1;
        Ok(())
    }
    pub fn finish(mut self) -> Result<(), String> {
        self.writer.flush().map_err(|e| e.to_string())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pcm_skip_roundtrip_crop_chroma_and_frame_number_wrap() {
        use crate::{
            process::{args, Runner},
            storage,
        };
        use std::{fs::File, sync::Arc};
        let r = Runner::discover(Arc::new(|_, _| {})).unwrap();
        let dir = storage::project_dir("44578c42-eaa6-47ee-860d-df4d52c0ecab").unwrap();
        let source = storage::temp_file(&dir, "source").unwrap();
        r.ff(args(&[
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=34x18:rate=30",
            "-frames:v",
            "1",
            "-c:v",
            "libx264",
            "-bf",
            "0",
            "-pix_fmt",
            "yuv420p",
            "-f",
            "mp4",
            &source.path().to_string_lossy(),
        ]))
        .unwrap();
        let (data, _) = r
            .run(
                true,
                &args(&[
                    "-v",
                    "error",
                    "-show_streams",
                    "-show_data",
                    "-of",
                    "json",
                    &source.path().to_string_lossy(),
                ]),
                65536,
            )
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&data).unwrap();
        let settings = Settings::from_avcc(
            &hex_dump(v["streams"][0]["extradata"].as_str().unwrap()).unwrap(),
            34,
            18,
        )
        .unwrap();
        assert_eq!(settings.frame_bits, 4);
        let movie = storage::temp_file(&dir, "edge").unwrap();
        let mut e = Encoder::new(File::create(movie.path()).unwrap(), settings.clone()).unwrap();
        let mut frames = Vec::new();
        let mut bytes = vec![0; 34 * 18 * 3 / 2];
        for i in 0..65 {
            if i % 4 == 1 {
                for (j, v) in bytes.iter_mut().enumerate() {
                    *v = ((j * 137 + i * 39) % 256) as u8;
                }
            }
            if i % 4 == 2 {
                let n = bytes.len();
                bytes[n - 1] ^= 13;
                bytes[0] ^= 77;
            }
            e.frame(&bytes, || Ok(())).unwrap();
            frames.push(bytes.clone());
        }
        assert!(e.skipped_blocks > 0 && e.pcm_blocks > 0);
        e.finish().unwrap();
        let mut count = 0;
        r.frames(
            &args(&[
                "-v",
                "error",
                "-xerror",
                "-f",
                "h264",
                "-i",
                &movie.path().to_string_lossy(),
                "-fps_mode",
                "passthrough",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuv420p",
                "pipe:1",
            ]),
            bytes.len(),
            |i, frame| {
                assert_eq!(frame, frames[i], "frame {i}");
                count += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(count, 65);
        let mut canceled = Encoder::new(Vec::new(), settings).unwrap();
        let mut rows = 0;
        assert_eq!(
            canceled
                .frame(&bytes, || {
                    rows += 1;
                    if rows == 2 {
                        Err("CANCELLED".into())
                    } else {
                        Ok(())
                    }
                })
                .unwrap_err(),
            "CANCELLED"
        );
        assert_eq!(canceled.index, 0);
    }
    #[test]
    fn bit_codes_and_escaping() {
        let mut b = Bits::new();
        b.ue(0);
        b.ue(1);
        b.ue(2);
        b.se(-1);
        let raw = b.finish();
        let mut r = Reader { data: &raw, n: 0 };
        assert_eq!(
            (r.ue(), r.ue(), r.ue(), r.ue()),
            (Some(0), Some(1), Some(2), Some(2))
        );
        let mut out = vec![];
        write_nal(&mut out, 0x67, &[0, 0, 0, 0, 0, 1, 0, 0, 3]).unwrap();
        assert_eq!(rbsp(&out[4..]).unwrap(), [0x67, 0, 0, 0, 0, 0, 1, 0, 0, 3]);
    }
    #[test]
    fn source_configuration_is_preserved_and_malformed_rejected() {
        let data = [
            1, 100, 0, 50, 255, 225, 0, 20, 103, 100, 0, 50, 172, 26, 212, 8, 128, 37, 188, 188,
            212, 4, 4, 4, 30, 17, 8, 212, 1, 0, 4, 104, 234, 67, 203,
        ];
        let s = Settings::from_avcc(&data, 1080, 2400).unwrap();
        assert_eq!(s.frame_bits, 16);
        let mut r = Reader {
            data: &s.sps,
            n: 24,
        };
        assert_eq!(r.ue(), Some(1));
        assert!(Settings::from_avcc(&data, 2400, 1080).is_none());
        for n in 0..35 {
            assert!(Settings::from_avcc(&data[..n], 1080, 2400).is_none());
        }
    }
}
