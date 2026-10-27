use crate::{model::*, process::*};
use serde::{Deserialize, Serialize};
pub const ALGORITHM_VERSION: &str = "ui-events-8-transitions-1";
const TRANSITION_PRE_US: Us = 300_000;
const TRANSITION_POST_US: Us = 200_000;
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Profile {
    version: String,
    width: usize,
    height: usize,
    evidence: Vec<Evidence>,
    thresholds: Thresholds,
}
#[derive(Clone, Deserialize)]
struct Evidence {
    name: String,
    kind: String,
    #[serde(rename = "box")]
    rect: [usize; 4],
    positive: Vec<usize>,
    negative: Vec<usize>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Thresholds {
    overlay_contrast: f64,
    hud_contrast: f64,
    positive_floor: f64,
    coverage: f64,
    occluded_coverage: f64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sample {
    pub time_us: Us,
    pub state: String,
    pub score: f64,
    pub evidence: Vec<(String, f64, f64)>,
    pub pause_evidence: bool,
    pub normal_evidence: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Analysis {
    pub geometry: Option<DetectionGeometry>,
    pub status: String,
    pub region: Option<[f64;4]>,
    pub version: String,
    pub quality_verified: bool,
    pub pauses: Vec<Pause>,
    pub samples: Vec<Sample>,
    pub warnings: Vec<String>,
    #[serde(default)]
    pub boundary_frames: Vec<Sample>,
}
#[cfg(test)]
fn profile() -> Result<Profile, String> { load_profile(false) }
fn load_profile(pc:bool) -> Result<Profile,String> {
    let mut profile: Profile =
        serde_json::from_str(if pc { include_str!("../profiles/cn-pc-16x9-v5-events.json") } else { include_str!("../profiles/cn-mobile-20x9-v4-events.json") })
            .map_err(|e| e.to_string())?;
    if !pc {
        // The legacy mobile mask contains both PAUSE and a tiny Chinese line.
        // A selected operator's bright map textures can match that line alone.
        // Retain the calibrated PAUSE pixels, but require independent letters
        // just as the PC profile does. Never use the small caption as an event.
        let word = profile.evidence.iter().find(|e| e.name == "pause_word")
            .ok_or("手机暂停文字模板缺失")?.clone();
        let mut letters = Vec::new();
        for (index, (left, right)) in [(0, 23), (23, 48), (48, 74), (74, 98), (98, 120)].into_iter().enumerate() {
            let mut letter = word.clone();
            letter.name = format!("pause_letter_{index}");
            letter.positive.retain(|i| (left..right).contains(&(i % word.rect[2])));
            letter.negative.retain(|i| (left..right).contains(&(i % word.rect[2])));
            if letter.positive.is_empty() || letter.negative.is_empty() {
                return Err("手机暂停字母模板无效".into());
            }
            letters.push(letter);
        }
        letters.extend(profile.evidence.into_iter().filter(|e| e.kind != "overlay"));
        profile.evidence = letters;
        profile.version = "cn-mobile-20x9-v5-letters".into();
    }
    // Compile the template's local ROI coordinates once. Classifying every
    // frame then reads the same pixels without repeated integer division.
    for evidence in &mut profile.evidence {
        let [x, y, w, _] = evidence.rect;
        if w == 0 {
            return Err("检测模板区域尺寸无效".into());
        }
        for index in evidence
            .positive
            .iter_mut()
            .chain(evidence.negative.iter_mut())
        {
            *index = (y + *index / w) * profile.width + x + *index % w;
            if *index >= profile.width * profile.height {
                return Err("检测模板像素超出区域".into());
            }
        }
    }
    Ok(profile)
}
pub const UNSUPPORTED:&str="当前录屏的暂停画面尚未识别，请检查原片或手动标记";
pub fn can_check(p:&Project)->bool {
    if matches!(p.detection.status.as_str(),"unsupported"|"failed"|"cancelled"){return false}
    let Some(id)=p.detection.profile.as_ref()else{return false};
    if !id.starts_with("cn-mobile-20x9")&&!id.starts_with("cn-pc-16x9"){return false}
    if p.detection.geometry.is_some(){return true}
    // Legacy archives had no layout evidence and sometimes recorded a mobile
    // profile after skipping PC media. Do not turn that old skip into a pass.
    let Some(stream)=p.source.streams.iter().find(|s|s.index==p.video_index&&s.kind=="video")else{return false};
    let ratio=stream.width.max(stream.height)as f64/stream.width.min(stream.height).max(1)as f64;
    (ratio-if id.starts_with("cn-pc"){16./9.}else{20./9.}).abs()<0.025
}
fn project_profile(p:&Project)->Result<Profile,String>{
    if !can_check(p){return Err(UNSUPPORTED.into())}
    let profile=load_profile(p.detection.profile.as_ref().is_some_and(|s|s.starts_with("cn-pc-16x9")))?;
    if let Some(g)=&p.detection.geometry {transform_profile(profile,g).ok_or("暂停识别几何配置无效".into())}else{Ok(profile)}
}
fn crop_filter(p:&Project,profile:&Profile)->String {let region=p.detection.region.unwrap_or([0.,0.,1.,1.]);format!("crop=iw*{}:ih*{}:iw*{}:ih*{},scale={}:{}",region[2],region[3],region[0],region[1],profile.width,profile.height)}

// Match UI elements independently of the frame aspect. Overlay text is anchored
// at screen centre, battle HUD at top centre and playback controls at top right.
// Uniform scaling preserves glyph geometry; no whole-frame aspect stretching.
fn transform_profile(mut p:Profile,g:&DetectionGeometry)->Option<Profile>{
    if g.width!=640 || g.height<100 || g.height>1500 || !g.scale.is_finite() || g.scale<=0. {return None}
    for e in &mut p.evidence {
        let right=matches!(e.kind.as_str(),"resume"|"running");
        let center=e.kind=="overlay";
        let (ox,nx)=if right{(p.width as f64,g.width as f64)}else{(p.width as f64/2.,g.width as f64/2.)};
        let (oy,ny)=if center{(p.height as f64/2.,g.height as f64/2.)}else{(0.,0.)};
        for index in e.positive.iter_mut().chain(e.negative.iter_mut()){
            let x=(nx+(*index%p.width)as f64*g.scale-ox*g.scale).round()as i64;
            let y=(ny+(*index/p.width)as f64*g.scale-oy*g.scale).round()as i64;
            if x<0||y<0||x>=g.width as i64||y>=g.height as i64{return None}
            *index=y as usize*g.width+x as usize;
        }
    }
    p.width=g.width;p.height=g.height;Some(p)
}
fn discover_profile(r:&Runner,p:&Project,w:usize,h:usize,mut region:[f64;4])->Result<Option<(Profile,DetectionGeometry,[f64;4])>,String>{
    let fractions=[0.08,0.2,0.35,0.5,0.65,0.8,0.92];
    let mut frames=vec![];
    for (i,fraction) in fractions.iter().enumerate(){
        r.measure("detectLayout",i as i64,Some(fractions.len()as i64),"frames","定位暂停文字与战斗控件");
        let (gray,_)=r.run(false,&args(&["-v","error","-ss",&crate::render::sec((p.source.duration_us as f64*fraction)as Us),"-i",&p.source.fingerprint.path,"-map",&format!("0:{}",p.video_index),"-frames:v","1","-vf",&format!("scale={w}:{h},format=gray"),"-f","rawvideo","pipe:1"]),w*h+1024)?;
        if gray.len()!=w*h{return Err("有效画面采样不完整".into())}frames.push(gray);
    }
    if p.detection.region.is_none(){
        let mut content=vec![0u8;w*h];for f in &frames{for (a,b)in content.iter_mut().zip(f){*a=(*a).max(*b)}}
        let row=|y:usize|content[y*w..(y+1)*w].iter().filter(|v|**v>12).count()>w/100;
        let col=|x:usize|(0..h).filter(|y|content[y*w+x]>12).count()>h/100;
        if let (Some(t),Some(b),Some(l),Some(e))=((0..h).find(|&y|row(y)),(0..h).rfind(|&y|row(y)),(0..w).find(|&x|col(x)),(0..w).rfind(|&x|col(x))){
            // A full landscape game may occupy less than a third of a portrait
            // canvas. Require usable sample pixels, not a fraction of the
            // container; the independent UI evidence below validates the crop.
            if e-l+1>=128 && b-t+1>=100 {region=[l as f64/w as f64,t as f64/h as f64,(e-l+1)as f64/w as f64,(b-t+1)as f64/h as f64]}
        }
    }
    let mut regions=vec![p.detection.region.unwrap_or([0.,0.,1.,1.])];if region!=regions[0]{regions.push(region)}
    let mut best=None;let mut best_score=0.;
    for region in regions {
    let (cw,ch)=(w as f64*region[2],h as f64*region[3]);
    let height=((640.*ch/cw/2.).round()as usize)*2;
    if !(100..=1500).contains(&height){continue}
    // Bilinear resampling only for layout search. Full scan and boundary checks
    // use the same persisted crop and FFmpeg scaler at native frame timestamps.
    let normalized:Vec<Vec<u8>>=frames.iter().map(|f|{let mut out=vec![0;640*height];for y in 0..height{for x in 0..640{let sx=(region[0]*w as f64+(x as f64+0.5)*cw/640.-0.5).clamp(0.,(w-1)as f64);let sy=(region[1]*h as f64+(y as f64+0.5)*ch/height as f64-0.5).clamp(0.,(h-1)as f64);let(ix,iy)=(sx as usize,sy as usize);let(dx,dy)=(sx-ix as f64,sy-iy as f64);let a=f[iy*w+ix]as f64*(1.-dx)+f[iy*w+(ix+1).min(w-1)]as f64*dx;let b=f[(iy+1).min(h-1)*w+ix]as f64*(1.-dx)+f[(iy+1).min(h-1)*w+(ix+1).min(w-1)]as f64*dx;out[y*640+x]=(a*(1.-dy)+b*dy).round()as u8;}}out}).collect();
    for pc in [false,true]{for n in 28..=52{
        let base=load_profile(pc)?;let g=DetectionGeometry{width:640,height,scale:height as f64/base.height as f64*n as f64/40.};
        if let Some(profile)=transform_profile(base,&g){let samples:Vec<_>=normalized.iter().map(|f|classify(f,&profile,0)).collect();let known=samples.iter().filter(|s|s.state=="confirmed"||s.normal_evidence).count();if known<2{continue}
            let score=known as f64*100.+samples.iter().map(|s|if s.state=="confirmed"||s.normal_evidence{s.score}else{0.}).sum::<f64>();
            if score>best_score{best_score=score;best=Some((profile,g,region))}
        }
    }}
    }
    r.measure("detectLayout",fractions.len()as i64,Some(fractions.len()as i64),"frames",if best.is_some(){"暂停控件位置已确认"}else{"当前画面尚未识别"});Ok(best)
}
fn decode_args(parts: &[&str]) -> Vec<String> {
    let mut values = args(&["-filter_threads", "1"]);
    values.extend(args(parts));
    values
}
fn canonical_scan_rotation(stream: &serde_json::Value) -> Option<&'static str> {
    let matrices: Vec<_> = stream["side_data_list"]
        .as_array()?
        .iter()
        .filter(|s| s["side_data_type"] == "Display Matrix")
        .collect();
    if matrices.len() != 1 {
        return None;
    }
    let mut values = Vec::new();
    for row in matrices[0]["displaymatrix"].as_str()?.lines() {
        if row.trim().is_empty() {
            continue;
        }
        let (_, data) = row.split_once(':')?;
        for value in data.split_whitespace() {
            values.push(value.parse::<i64>().ok()?);
        }
    }
    // Exact display matrices only: never guess a transform from raster shape
    // or a rounded angle. Mirroring, translation, scaling and arbitrary angles
    // retain FFmpeg's established autorotation path.
    match values.as_slice() {
        [0, -65536, 0, 65536, 0, 0, 0, 0, 1073741824] => Some("transpose=cclock"),
        [0, 65536, 0, -65536, 0, 0, 0, 0, 1073741824] => Some("transpose=clock"),
        [-65536, 0, 0, 0, -65536, 0, 0, 0, 1073741824] => Some("hflip,vflip"),
        _ => None,
    }
}
fn scan_rotation(r: &Runner, p: &Project) -> Result<Option<&'static str>, String> {
    let result = r.run(
        true,
        &args(&[
            "-v",
            "error",
            "-select_streams",
            &p.video_index.to_string(),
            "-show_entries",
            "stream=index:stream_side_data=side_data_type,displaymatrix",
            "-of",
            "json",
            &p.source.fingerprint.path,
        ]),
        1024 * 1024,
    );
    r.check()?;
    // This optional optimization must not reject media accepted by the normal
    // decoder if metadata is absent or a probe cannot describe the matrix.
    Ok(result.ok().and_then(|(data, _)| {
        let metadata: serde_json::Value = serde_json::from_slice(&data).ok()?;
        let stream = metadata["streams"]
            .as_array()?
            .iter()
            .find(|s| s["index"].as_u64() == Some(p.video_index as u64))?;
        canonical_scan_rotation(stream)
    }))
}
fn classify(f: &[u8], p: &Profile, t: Us) -> Sample {
    let mut evidence = vec![];
    let (mut overlay, mut hud, mut weak_overlay, mut resume, mut running) = (0, 0, 0, 0, 0);
    let mut strength: f64 = 0.;
    for e in &p.evidence {
        let value = |i: &usize| f[*i] as f64;
        let pos = e.positive.iter().map(value).sum::<f64>() / e.positive.len().max(1) as f64;
        let neg = e.negative.iter().map(value).sum::<f64>() / e.negative.len().max(1) as f64;
        let contrast = pos - neg;
        let floor = p
            .thresholds
            .positive_floor
            .min(neg + contrast.max(0.) * 0.5);
        let coverage = e.positive.iter().filter(|i| value(i) >= floor).count() as f64
            / e.positive.len().max(1) as f64;
        let pass = contrast
            >= if e.kind == "overlay" {
                p.thresholds.overlay_contrast
            } else {
                p.thresholds.hud_contrast
            }
            && coverage
                >= if e.name == "pause_word" {
                    p.thresholds.occluded_coverage
                } else {
                    p.thresholds.coverage
                };
        if pass {
            if e.kind == "overlay" {
                overlay += 1
            } else if e.kind == "hud" {
                hud += 1
            } else if e.kind == "resume" {
                resume += 1
            } else if e.kind == "running" {
                running += 1
            }
        }
        if e.kind == "overlay" && contrast >= 16. && coverage >= 0.45 {
            weak_overlay += 1
        }
        strength += contrast.max(0.) / 255.;
        evidence.push((e.name.clone(), contrast, coverage));
    }
    let letters=p.evidence.iter().any(|e|e.name.starts_with("pause_letter_"));
    // A clear running control contradicts a hard pause, including during real
    // operator selection / bullet time. Ambiguous or missing controls do not
    // independently prove either state.
    let state = if running > 0 && resume == 0 {
        "nonPause"
    } else if (overlay >= if letters{3}else{1} && (hud >= 1 || resume >= 1)) || (letters && overlay>=2 && resume>=1 && running==0 && hud>=1) {
        "confirmed"
    } else if if letters {overlay>=2 && resume>=1 && hud>=1} else {overlay >= 1 || (weak_overlay >= 1 && resume >= 1)} {
        "candidate"
    } else {
        "nonPause"
    };
    Sample {
        time_us: t,
        state: state.into(),
        score: strength / p.evidence.len() as f64,
        evidence,
        pause_evidence: state != "nonPause",
        // The same positive running evidence must also split dense pause
        // events; otherwise a one-frame recovery could be bridged and deleted
        // merely because background texture looks like a weak letter.
        normal_evidence: running > 0 && resume == 0 && hud > 0 && state == "nonPause",
    }
}
pub fn analyze(r: &Runner, p: &Project) -> Result<Analysis, String> {
    r.check()?;
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Key<'a> {
        algorithm: &'a str,
        profile: &'a str,
        source: &'a Media,
        decoder: &'a Fingerprint,
        video_index: u32,
        scan_hz: u32,
        min_pause_us: Us,
        region: Option<[f64; 4]>,
    }
    let decoder = crate::media::fingerprint(&r.ffmpeg.to_string_lossy())?;
    let key = crate::storage::hash(&Key {
        algorithm: ALGORITHM_VERSION,
        profile: concat!(include_str!("../profiles/cn-mobile-20x9-v4-events.json"),include_str!("../profiles/cn-pc-16x9-v5-events.json")),
        source: &p.source,
        decoder: &decoder,
        video_index: p.video_index,
        scan_hz: p.detection.scan_hz,
        min_pause_us: p.detection.min_pause_us,
        region: p.detection.region,
    })?;
    let cache = crate::storage::project_dir(&p.project_id)
        .ok()
        .map(|dir| dir.join(format!("analysis-{key}.json")));
    if let Some(path) = &cache {
        #[derive(Deserialize)]
        struct Cached {
            key: String,
            checksum: String,
            result: Analysis,
        }
        if let Ok(file) = std::fs::File::open(path) {
            use std::io::Read;
            let bytes = file.metadata().map(|v| v.len()).unwrap_or(u64::MAX);
            if bytes <= 128 * 1024 * 1024 {
                let _memory = r.reserve_memory(bytes.saturating_mul(3), "识别缓存")?;
                if let Ok(cached) = serde_json::from_reader::<_, Cached>(std::io::BufReader::new(
                    file.take(128 * 1024 * 1024 + 1),
                )) {
                    if cached.key == key && crate::storage::hash(&cached.result)? == cached.checksum
                    {
                        crate::media::verify_media(&p.source)?;
                        r.check()?;
                        r.measure(
                            "pauseScan",
                            p.source.duration_us,
                            Some(p.source.duration_us),
                            "us",
                            "复用已验证识别结果",
                        );
                        return Ok(cached.result);
                    }
                }
            }
        }
    }
    let result = analyze_uncached(r, p)?;
    r.check()?;
    if let Some(path) = cache {
        let checksum = crate::storage::hash(&result)?;
        #[derive(Serialize)]
        struct Cached<'a> {
            key: &'a str,
            checksum: &'a str,
            result: &'a Analysis,
        }
        let estimate = (result.samples.len() + result.boundary_frames.len()) as u64 * 2048 + 65536;
        if crate::storage::quota(
            path.parent().ok_or("识别缓存路径无效")?,
            estimate,
            crate::storage::settings().0.cache_limit,
            &[],
        )
        .is_ok()
        {
            let _disk = r.reserve_disk(&path, estimate, "识别缓存")?;
            // JSON is written directly to a replaceable owned temporary file.
            // Ordinary optional cache write failures keep the computed result.
            let written = crate::storage::atomic_json(
                &path,
                &Cached {
                    key: &key,
                    checksum: &checksum,
                    result: &result,
                },
            );
            if let Err(error) = written {
                let text = error.to_ascii_lowercase();
                if text.contains("os error 112") || text.contains("os error 39") {
                    return Err(
                        "RESOURCE_DISK: 识别缓存写入空间不足，请清理可回收缓存后重试".into(),
                    );
                }
                if text.contains("resource_") {
                    return Err(error);
                }
                r.resource().media_error(&error);
            }
            r.check()?;
        }
    }
    Ok(result)
}
fn analyze_uncached(r: &Runner, p: &Project) -> Result<Analysis, String> {
    if p.detection.scan_hz == 0 || p.detection.scan_hz > 30 || p.detection.min_pause_us <= 0 {
        return Err("检测配置无效".into());
    }
    let mut region = p.detection.region.unwrap_or([0., 0., 1., 1.]);
    if region.iter().any(|v| !v.is_finite() || *v < 0. || *v > 1.)
        || region[2] <= 0.
        || region[3] <= 0.
        || region[0] + region[2] > 1.
        || region[1] + region[3] > 1.
    {
        return Err("游戏区域无效".into());
    }
    // Obtain displayed orientation rather than trusting stored raster dimensions.
    let (data, _) = r.run(
        false,
        &args(&[
            "-v",
            "error",
            "-i",
            &p.source.fingerprint.path,
            "-map",
            &format!("0:{}", p.video_index),
            "-frames:v",
            "1",
            "-vf",
            "scale=640:960:force_original_aspect_ratio=decrease:force_divisible_by=2",
            "-f",
            "image2pipe",
            "-vcodec",
            "bmp",
            "pipe:1",
        ]),
        4 * 1024 * 1024,
    )?;
    if data.len() < 26 {
        return Err("无法定位显示方向".into());
    }
    let width = i32::from_le_bytes(data[18..22].try_into().unwrap()).unsigned_abs() as f64;
    let height = i32::from_le_bytes(data[22..26].try_into().unwrap()).unsigned_abs() as f64;
    let Some((profile,geometry,found_region))=discover_profile(r,p,width as usize,height as usize,region)? else {
        return Ok(Analysis{geometry:None,status:"unsupported".into(),region:None,version:"ui-events-5-unavailable".into(),quality_verified:false,pauses:vec![],samples:vec![],boundary_frames:vec![],warnings:vec![UNSUPPORTED.into()]});
    };
    region=found_region;
    let crop = format!(
        "crop=iw*{}:ih*{}:iw*{}:ih*{},scale={}:{}",
        region[2], region[3], region[0], region[1], profile.width, profile.height
    );
    // One dense sequential pass covers sub-frame-rate pauses and exact event
    // transitions. The former sampled pass then decoded every long pause again.
    // Evidence has an explicit resource lease; pixels remain a bounded stream.
    let _evidence_memory=r.reserve_memory((p.source.frame_pts.len()as u64).saturating_mul(2048),"逐帧暂停证据")?;
    let chosen=&p.source.frame_pts;
    let rotation=scan_rotation(r,p)?;
    let rotate=rotation.map(|s|format!("{s},")).unwrap_or_default();
    let vf=format!("setpts=PTS-({})/TB,{rotate}{crop},format=gray",crate::render::sec(p.source.origin_us));
    let mut scan_args = args(&["-v", "error", "-copyts"]);
    if rotation.is_some() {
        scan_args.push("-noautorotate".into());
    }
    scan_args.extend(args(&[
        "-i",
        &p.source.fingerprint.path,
        "-map",
        &format!("0:{}", p.video_index),
        "-vf",
        &vf,
        "-fps_mode",
        "passthrough",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "gray",
        "pipe:1",
    ]));
    let mut samples = vec![];
    samples
        .try_reserve(chosen.len())
        .map_err(|_| "RESOURCE_MEMORY: 识别证据内存不足，请关闭部分应用后重试")?;
    r.frames(&scan_args, profile.width * profile.height, |i, frame| {
        let t = *chosen.get(i).ok_or("扫描采样 PTS 不一致")?;
        (r.progress)("scan", Some(t));
        r.measure(
            "pauseScan",
            t,
            Some(p.source.duration_us),
            "us",
            "扫描暂停 UI 证据",
        );
        samples.push(classify(frame, &profile, t));
        Ok(())
    })?;
    if samples.len() != chosen.len() {
        return Err(format!(
            "扫描帧数与真实 PTS 不一致：{} / {}",
            samples.len(),
            chosen.len()
        ));
    }
    r.measure("boundaryRefine",0,Some(samples.len()as i64),"frames","汇总逐帧暂停边界");
    let mut pauses=vec![];let mut boundary_frames=vec![];
    for event in event_spans(&samples,p.source.duration_us){
        r.check()?;
        if event.end_us<=event.start_us{continue}
        let state=if event.strong && (event.end_us-event.start_us>=p.detection.min_pause_us || event.confirmed_frames>=2){"confirmed"}else{"candidate"};
        let start=samples.partition_point(|f|f.time_us<event.start_us-50000);
        let end=samples.partition_point(|f|f.time_us<=event.end_us+50000);
        boundary_frames.extend(samples[start..end].iter().filter(|f|(f.time_us-event.start_us).abs()<=50000||(f.time_us-event.end_us).abs()<=50000).cloned());
        pauses.push(Pause{id:format!("auto-{}-{}",&p.source.fingerprint.sample_hash[..8],event.start_us),start_us:event.start_us,end_us:event.end_us,enabled:state=="confirmed",source:"auto".into(),state:state.into(),score:event.score,audio_mode:None,detected_bounds:Some([event.start_us,event.end_us]),transition:None});
    }
    r.measure("boundaryRefine",samples.len()as i64,Some(samples.len()as i64),"frames","事件边界复核完成");
    pauses.sort_by_key(|p| p.start_us);
    let ui_bounds: Vec<_> = pauses.iter().map(|c| [c.start_us, c.end_us]).collect();
    for (i, pause) in pauses.iter_mut().enumerate() {
        if !automatic_boundary(pause) { continue; }
        r.measure("transitionReview", i as i64, Some(ui_bounds.len() as i64), "cuts", "检查暂停关联的短暂选中与战场偏移");
        let limits = [i.checked_sub(1).map(|j| ui_bounds[j][1]).unwrap_or(0), ui_bounds.get(i+1).map(|b| b[0]).unwrap_or(p.source.duration_us)];
        review_source_transition(r, p, &profile, geometry.scale, &crop, pause, limits)?;
    }
    r.measure("transitionReview", ui_bounds.len() as i64, Some(ui_bounds.len() as i64), "cuts", "暂停过渡检查完成，持续选中保持原样");
    let supported = samples.iter().any(|s|s.state=="confirmed" || s.normal_evidence);
    let status=if !supported {"unsupported"} else if pauses.is_empty(){"empty"} else {"completed"};
    Ok(Analysis {
        geometry:Some(geometry),status:status.into(),region:Some(region),
        version: profile.version,
        quality_verified: false,
        pauses,
        samples,
        boundary_frames,
        warnings: if supported {vec![]} else {vec![UNSUPPORTED.into()]},
    })
}
fn transition_frame(profile: &Profile, scale: f64, time_us: Us, pixels: &[u8]) -> Result<crate::transition::GrayFrame, String> {
    let selection = match crate::selection::classify(pixels, profile.width, profile.height, scale, profile.version.starts_with("cn-pc")) {
        crate::selection::SelectionState::Normal => Some(false),
        crate::selection::SelectionState::Selected => Some(true),
        crate::selection::SelectionState::Unknown => None,
    };
    let (pixels, _, _) = crate::transition::downsample_gray(pixels, profile.width, profile.height).ok_or("过渡检查画面尺寸无效")?;
    Ok(crate::transition::GrayFrame { time_us, pixels, selection })
}
fn transition_dimensions(profile: &Profile) -> (usize, usize) {
    let w = profile.width.min(192);
    (w, (profile.height * w / profile.width).max(1))
}
fn read_transition_window(r: &Runner, p: &Project, profile: &Profile, scale: f64, crop: &str, a: Us, b: Us) -> Result<Vec<crate::transition::GrayFrame>, String> {
    if b <= a { return Ok(vec![]); }
    let first = p.source.frame_pts.partition_point(|t| *t < a);
    let end = p.source.frame_pts.partition_point(|t| *t < b);
    let times = &p.source.frame_pts[first..end];
    if times.len() > 384 { return Err("RESOURCE_MEMORY: 过渡窗口帧数超出安全容量".into()); }
    let vf = format!("settb=expr=1/1000000,setpts=PTS-{},trim=start_pts={a}:end_pts={b},{crop},format=gray", p.source.origin_us);
    let seek = if a >= 2000 { a + p.source.origin_us - 2000 } else { 0 };
    let mut result = Vec::with_capacity(times.len());
    r.frames(&decode_args(&["-v","error","-copyts","-seek_timestamp","1","-ss",&crate::render::sec(seek),"-t",&crate::render::sec(b+p.source.origin_us-seek+100000),"-i",&p.source.fingerprint.path,"-map",&format!("0:{}",p.video_index),"-vf",&vf,"-fps_mode","passthrough","-f","rawvideo","-pix_fmt","gray","pipe:1"]), profile.width*profile.height, |i, pixels| {
        let t = *times.get(i).ok_or("过渡检查 PTS 不一致")?;
        result.push(transition_frame(profile, scale, t, pixels)?);
        Ok(())
    })?;
    if result.len() != times.len() { return Err("过渡检查实际帧数与 PTS 索引不一致".into()); }
    Ok(result)
}
fn review_source_transition(r: &Runner, p: &Project, profile: &Profile, scale: f64, crop: &str, pause: &mut Pause, limits: [Us; 2]) -> Result<(), String> {
    if !automatic_boundary(pause) { return Ok(()); }
    // Re-evaluate only the current applied cut. Never rewind to older evidence
    // after an undo/manual edit, or repeatedly consume additional source frames.
    let bounds = [pause.start_us, pause.end_us];
    let (w,h) = transition_dimensions(profile);
    let _memory = r.reserve_memory((w*h*768) as u64, "暂停过渡窗口")?;
    let pre = read_transition_window(r,p,profile,scale,crop,(bounds[0]-TRANSITION_PRE_US).max(limits[0]),bounds[0])?;
    let post = read_transition_window(r,p,profile,scale,crop,bounds[1],(bounds[1]+TRANSITION_POST_US).min(limits[1]))?;
    r.check()?;
    let assessment = crate::transition::assess(&pre,&post,w,h,bounds[0],bounds[1]);
    if assessment.proposed_start_us.is_none() && pause.transition.as_ref().is_some_and(|t|t.version==ALGORITHM_VERSION && t.applied_bounds==bounds && t.original_bounds[0]>bounds[0]) {
        return Ok(());
    }
    let decision = assessment.reason.to_string();
    if let Some(start) = assessment.proposed_start_us {
        if start < bounds[0] && start >= limits[0] && p.source.frame_pts.binary_search(&start).is_ok() {
            pause.start_us = start;
            pause.detected_bounds = Some([start, bounds[1]]);
        }
    }
    pause.transition = Some(TransitionReview {
        version: ALGORITHM_VERSION.into(), original_bounds: bounds,
        applied_bounds: [pause.start_us,pause.end_us], decision,
        evidence: serde_json::to_value(assessment).map_err(|e|e.to_string())?,
    });
    Ok(())
}
/// Only inspect bounded windows around the implicated boundaries. Never rescan
/// unrelated events or overwrite a boundary the user has edited.
pub fn repair_boundaries(
    r: &Runner,
    p: &Project,
    issues: &[serde_json::Value],
) -> Result<Project, String> {
    let profile = project_profile(p)?;
    let region = p.detection.region.unwrap_or([0., 0., 1., 1.]);
    let crop = format!(
        "crop=iw*{}:ih*{}:iw*{}:ih*{},scale={}:{}",
        region[2], region[3], region[0], region[1], profile.width, profile.height
    );
    let mut next = p.clone();
    let ids: std::collections::HashSet<&str> = issues
        .iter()
        .filter(|v| v["kind"] == "pauseResidual" || v["kind"] == "transitionResidual")
        .flat_map(|v| {
            v["pauseIds"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str())
        })
        .collect();
    for (i, pause) in next.pauses.iter_mut().enumerate() {
        if !ids.contains(pause.id.as_str()) || !automatic_boundary(pause) {
            continue;
        }
        r.measure(
            "sourceBoundaryReview",
            i as i64,
            None,
            "cuts",
            &format!("发现第 {} 处残帧，正在修正", i + 1),
        );
        let mut bounds = [pause.start_us, pause.end_us];
        for side in 0..2 {
            let at = bounds[side];
            let a = (at - SECOND).max(0);
            let b = (at + SECOND).min(p.source.duration_us);
            let count = p.source.frame_pts.partition_point(|t| *t < b)
                - p.source.frame_pts.partition_point(|t| *t < a);
            let _memory = r.reserve_memory(count as u64 * 2048, "局部边界复核")?;
            let frames = read_window(r, p, &profile, &crop, a, b)?;
            let matches: Vec<_> = event_spans(&frames, b)
                .into_iter()
                .filter(|e| e.strong && e.start_us <= pause.end_us && e.end_us >= pause.start_us)
                .collect();
            if matches.len() != 1 {
                continue;
            }
            let e = &matches[0];
            // Require an observed transition, not the edge of our search window.
            if side == 0 && (e.start_us > a || a == 0) && e.start_us <= pause.start_us {
                bounds[0] = e.start_us;
            }
            if side == 1 && (e.end_us < b || b == p.source.duration_us) && e.end_us >= pause.end_us
            {
                bounds[1] = e.end_us;
            }
        }
        if bounds != [pause.start_us, pause.end_us] {
            pause.start_us = bounds[0];
            pause.end_us = bounds[1];
            pause.detected_bounds = Some(bounds);
            if let Some(review)=&mut pause.transition {
                if review.original_bounds[0]>bounds[0] && review.applied_bounds[0]==bounds[0] {
                    review.applied_bounds=bounds;
                    review.evidence["pauseBoundaryRepair"]=serde_json::json!({"appliedBounds":bounds});
                }
            }
        }
        let limits = [p.pauses.iter().filter(|c|c.id!=pause.id && c.end_us<=pause.start_us).map(|c|c.end_us).max().unwrap_or(0), p.pauses.iter().filter(|c|c.id!=pause.id && c.start_us>=pause.end_us).map(|c|c.start_us).min().unwrap_or(p.source.duration_us)];
        review_source_transition(r,p,&profile,p.detection.geometry.as_ref().map(|g|g.scale).unwrap_or(1.),&crop,pause,limits)?;
    }
    if next.pauses.iter().zip(&p.pauses).any(|(a,b)|a.start_us!=b.start_us || a.end_us!=b.end_us) {
        next.revision += 1;
    }
    Ok(next)
}
/// Source windows and output seams share one decision rule. The output scan
/// stores only a half-second ring of reduced frames, regardless of video length.
struct TransitionAudit {
    ring: std::collections::VecDeque<crate::transition::GrayFrame>,
    cuts: Vec<Us>,
    next: usize,
    results: Vec<serde_json::Value>,
    width: usize,
    height: usize,
}
impl TransitionAudit {
    fn new(cuts: Vec<Us>, profile: &Profile) -> Self {
        let (width,height)=transition_dimensions(profile);
        Self{results:vec![serde_json::Value::Null;cuts.len()],cuts,next:0,ring:std::collections::VecDeque::new(),width,height}
    }
    fn finish_one(&mut self) -> Result<(), String> {
        let at=self.cuts[self.next];
        let pre:Vec<_>=self.ring.iter().filter(|f|f.time_us>=at-TRANSITION_PRE_US && f.time_us<at).cloned().collect();
        let post:Vec<_>=self.ring.iter().filter(|f|f.time_us>=at && f.time_us<at+TRANSITION_POST_US).cloned().collect();
        self.results[self.next]=serde_json::to_value(crate::transition::assess(&pre,&post,self.width,self.height,at,at)).map_err(|e|e.to_string())?;
        self.next+=1;
        Ok(())
    }
    fn push(&mut self, frame: crate::transition::GrayFrame) -> Result<(),String> {
        let t=frame.time_us;
        while self.next<self.cuts.len() && t>=self.cuts[self.next]+TRANSITION_POST_US {self.finish_one()?;}
        while self.ring.front().is_some_and(|f|f.time_us<t-TRANSITION_PRE_US-TRANSITION_POST_US-50_000) {self.ring.pop_front();}
        if self.ring.len()>=384 {return Err("RESOURCE_MEMORY: 输出过渡检查窗口帧数超出安全容量".into());}
        self.ring.push_back(frame);
        Ok(())
    }
    fn finish(&mut self)->Result<(),String>{while self.next<self.cuts.len(){self.finish_one()?;}Ok(())}
}
fn automatic_cut(p:&Project,start:Us,end:Us)->bool {
    let involved:Vec<_>=p.pauses.iter().filter(|c|c.enabled && c.start_us<end && c.end_us>start).collect();
    !involved.is_empty() && involved.iter().all(|c|automatic_boundary(c))
}
fn source_protection(p:&Project,start:Us,end:Us)->Option<serde_json::Value> {
    p.pauses.iter().filter(|c|c.enabled && c.start_us<end && c.end_us>start && automatic_boundary(c)).find_map(|c| {
        let t=c.transition.as_ref()?;
        (t.version==ALGORITHM_VERSION && t.applied_bounds==[c.start_us,c.end_us] && t.decision.starts_with("protected_")).then(||serde_json::json!({"pauseId":c.id,"decision":t.decision,"evidence":t.evidence}))
    })
}
fn preserve_output_selection(mut assessment:serde_json::Value,evidence:serde_json::Value)->serde_json::Value {
    assessment["outputProposal"]=serde_json::json!({"startUs":assessment["proposedStartUs"],"removedFrames":assessment["removedFrames"]});
    assessment["proposedStartUs"]=serde_json::Value::Null;
    assessment["removedFrames"]=serde_json::json!(0);
    assessment["reason"]=serde_json::json!("protected_source_context");
    assessment["sourceProtection"]=evidence;
    assessment
}
fn confirm_output_transition(r:&Runner,p:&Project,profile:&Profile,start:Us,end:Us,assessment:serde_json::Value)->Result<serde_json::Value,String> {
    if assessment["proposedStartUs"].as_i64().is_none() || !automatic_cut(p,start,end) {return Ok(assessment);}
    if let Some(evidence)=source_protection(p,start,end) {return Ok(preserve_output_selection(assessment,evidence));}
    // Cutting several pauses can compress a sustained selection into a short
    // output tail. Confirm every new automatic proposal in the original clock,
    // including old projects without persisted transition evidence.
    let crop=crop_filter(p,profile);
    let scale=p.detection.geometry.as_ref().map(|g|g.scale).unwrap_or(1.);
    let mut evidence=vec![];
    let mut accepted=false;
    for original in p.pauses.iter().filter(|c|c.enabled && c.start_us<end && c.end_us>start) {
        let mut checked=original.clone();
        let limits=[p.pauses.iter().filter(|c|c.id!=original.id && c.end_us<=original.start_us).map(|c|c.end_us).max().unwrap_or(0),p.pauses.iter().filter(|c|c.id!=original.id && c.start_us>=original.end_us).map(|c|c.start_us).min().unwrap_or(p.source.duration_us)];
        review_source_transition(r,p,profile,scale,&crop,&mut checked,limits)?;
        accepted|=checked.start_us<original.start_us;
        evidence.push(serde_json::json!({"pauseId":original.id,"sourceReview":checked.transition}));
    }
    if accepted {Ok(assessment)} else {Ok(preserve_output_selection(assessment,serde_json::json!(evidence)))}
}
pub(crate) fn automatic_boundary(p: &Pause) -> bool {
    p.source == "auto"
        && p.state == "confirmed"
        && p.enabled
        && p.detected_bounds == Some([p.start_us, p.end_us])
}
struct EventSpan {
    start_us: Us,
    end_us: Us,
    strong: bool,
    confirmed_frames: usize,
    score: f64,
}
fn event_spans(frames: &[Sample], end: Us) -> Vec<EventSpan> {
    let mut result = vec![];
    let (mut start, mut strong, mut sum, mut count) = (None, false, 0., 0);
    let (mut normal_start, mut normal_count) = (None, 0);
    let mut confirmed_frames=0;
    for frame in frames {
        if frame.pause_evidence {
            if start.is_none() {
                start = Some(frame.time_us);
                strong = false;
                confirmed_frames=0;
                sum = 0.;
                count = 0
            }
            strong |= frame.state == "confirmed";
            confirmed_frames+=usize::from(frame.state=="confirmed");
            sum += frame.score;
            count += 1;
            normal_start = None;
            normal_count = 0;
        } else if let Some(s) = start {
            normal_start.get_or_insert(frame.time_us);
            normal_count += 1;
            if frame.normal_evidence || normal_count >= 3 {
                result.push(EventSpan {
                    start_us: s,
                    end_us: normal_start.unwrap(),
                    strong,
                    confirmed_frames,
                    score: sum / count.max(1) as f64,
                });
                start = None;
                normal_start = None;
                normal_count = 0;
            }
        }
    }
    if let Some(s) = start {
        result.push(EventSpan {
            start_us: s,
            end_us: end,
            strong,
            confirmed_frames,
            score: sum / count.max(1) as f64,
        });
    }
    result
}
fn read_window(
    r: &Runner,
    p: &Project,
    profile: &Profile,
    crop: &str,
    a: Us,
    b: Us,
) -> Result<Vec<Sample>, String> {
    let start = p.source.frame_pts.partition_point(|t| *t < a);
    let end = p.source.frame_pts.partition_point(|t| *t < b);
    let times = &p.source.frame_pts[start..end];
    let vf = format!(
        "settb=expr=1/1000000,setpts=PTS-{},trim=start_pts={a}:end_pts={b},{crop},format=gray",
        p.source.origin_us
    );
    let mut frames = vec![];
    frames
        .try_reserve(times.len())
        .map_err(|_| "RESOURCE_MEMORY: 边界复核内存不足，请关闭部分应用后重试")?;
    let seek = if a >= 2000 {
        a + p.source.origin_us - 2000
    } else {
        0
    };
    r.frames(
        &args(&[
            "-v",
            "error",
            "-copyts",
            "-seek_timestamp",
            "1",
            "-ss",
            &crate::render::sec(seek),
            "-t",
            &crate::render::sec(b + p.source.origin_us - seek + 100000),
            "-i",
            &p.source.fingerprint.path,
            "-map",
            &format!("0:{}", p.video_index),
            "-vf",
            &vf,
            "-fps_mode",
            "passthrough",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "gray",
            "pipe:1",
        ]),
        profile.width * profile.height,
        |i, f| {
            let t = *times.get(i).ok_or("精查实际帧数多于 PTS 索引")?;
            frames.push(classify(f, profile, t));
            Ok(())
        },
    )?;
    if frames.len() != times.len() {
        return Err(format!(
            "精查 PTS 不一致：{} / {} at {a}..{b}",
            frames.len(),
            times.len()
        ));
    }
    Ok(frames)
}
/// Decode every video/audio frame once, classify only the union of cut windows.
/// The null branch must run to EOF; selection never shortens the full check.
pub fn check_full_output(
    r: &Runner,
    p: &Project,
    plan: &Plan,
    path: &std::path::Path,
    range: (Us, Us),
) -> Result<serde_json::Value, String> {
    let profile = project_profile(p)?;
    let cuts: Vec<_> = plan
        .removed
        .iter()
        .filter(|c| c.output_us >= range.0 && c.output_us < range.1)
        .collect();
    let windows: Vec<_> = cuts
        .iter()
        .map(|c| {
            (
                (c.output_us - range.0 - 500000).max(0),
                (c.output_us - range.0 + 500000).min(range.1 - range.0),
            )
        })
        .collect();
    let _memory = r.reserve_memory(
        (cuts.len() as u64).saturating_mul(1024 * 1024),
        "full-cut-audit",
    )?;
    let mut observed: Vec<Vec<Sample>> = vec![vec![]; cuts.len()];
    let mut transitions = TransitionAudit::new(cuts.iter().map(|c|c.output_us).collect(), &profile);
    let (tw,th)=transition_dimensions(&profile);
    let _transition_memory=r.reserve_memory((tw*th*1152) as u64,"输出过渡检查")?;
    let scale=p.detection.geometry.as_ref().map(|g|g.scale).unwrap_or(1.);
    let select = if windows.is_empty() {
        "0".into()
    } else {
        windows
            .iter()
            .map(|(a, b)| format!("gte(pts,{a})*lt(pts,{b})"))
            .collect::<Vec<_>>()
            .join("+")
    };
    let filter=format!("[0:v:0]split=2[complete][check];[check]settb=expr=1/1000000,select='{select}',{},format=gray,showinfo@arkcut_pts=checksum=0[audit]",crop_filter(p,&profile));
    r.measure(
        "outputValidate",
        0,
        Some(range.1 - range.0),
        "us",
        "完整解码并检查剪辑点",
    );
    let mut last = None;
    r.frames_with_timestamps(
        &args(&[
            "-v",
            "info",
            "-nostats",
            "-xerror",
            "-copyts",
            "-progress","pipe:2",
            "-filter_buffered_frames",
            "128",
            "-i",
            &path.to_string_lossy(),
            "-filter_complex",
            &filter,
            "-map",
            "[complete]",
            "-map",
            "0:a?",
            "-fps_mode",
            "passthrough",
            "-f",
            "null",
            "-",
            "-map",
            "[audit]",
            "-fps_mode",
            "passthrough",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "gray",
            "pipe:1",
        ]),
        profile.width * profile.height,
        |_, t, frame| {
            if last.is_some_and(|v| t < v) {
                return Err("输出验证实际 PTS 倒退".into());
            }
            last = Some(t);
            transitions.push(transition_frame(&profile,scale,t+range.0,frame)?)?;
            let sample = classify(frame, &profile, t + range.0);
            let first = windows.partition_point(|(_, b)| *b <= t);
            for i in first..windows.len() {
                if windows[i].0 > t {
                    break;
                }
                if t < windows[i].1 {
                    if observed[i].len() >= 1024 {
                        return Err(
                            "RESOURCE_MEMORY: 单个切点帧数超出安全检查容量，已停止导出".into()
                        );
                    }
                    observed[i].push(sample.clone());
                }
            }
            Ok(())
        },
    )?;
    transitions.finish()?;
    let mut uncertain = 0;
    let mut transition_residuals = 0;
    let mut checks = vec![];
    for (i, cut) in cuts.iter().enumerate() {
        if observed[i].is_empty() && windows[i].1 > windows[i].0 {
            return Err("输出验证窗口没有可解码帧".into());
        }
        let before = p.source.frame_pts.partition_point(|t| *t < cut.start_us);
        let after = p.source.frame_pts.partition_point(|t| *t < cut.end_us);
        transitions.results[i]=confirm_output_transition(r,p,&profile,cut.start_us,cut.end_us,transitions.results[i].take())?;
        let automatic=automatic_cut(p,cut.start_us,cut.end_us);
        let removed=transitions.results[i]["removedFrames"].as_u64().unwrap_or(0) as usize;
        if automatic {transition_residuals+=removed;} else {uncertain+=removed;}
        checks.push(serde_json::json!({"outputUs":cut.output_us,"sourceStartUs":cut.start_us,"sourceEndUs":cut.end_us,"lastKeptSourceUs":before.checked_sub(1).and_then(|n|p.source.frame_pts.get(n)),"firstKeptSourceUs":p.source.frame_pts.get(after),"frames":observed[i],"transition":transitions.results[i],"transitionAutomatic":automatic}));
    }
    r.measure(
        "cutFrameValidate",
        cuts.len() as i64,
        Some(cuts.len() as i64),
        "cuts",
        "完整解码与切点检查完成",
    );
    let (residuals, pause_candidates) = unique_pause_counts(&checks);
    uncertain += pause_candidates;
    Ok(
        serde_json::json!({"profile":profile.version,"transitionVersion":ALGORITHM_VERSION,"residualFrames":residuals,"transitionResidualFrames":transition_residuals,"candidateFrames":uncertain,"checks":checks,"needsReview":residuals>0||transition_residuals>0||uncertain>0}),
    )
}
pub fn check_output(
    r: &Runner,
    p: &Project,
    plan: &Plan,
    path: &std::path::Path,
    range: (Us, Us),
) -> Result<serde_json::Value, String> {
    let profile = project_profile(p)?;
    let cuts: Vec<_> = plan
        .removed
        .iter()
        .filter(|c| c.output_us >= range.0 && c.output_us < range.1)
        .collect();
    let mut checks = vec![];
    let mut uncertain = 0;
    let mut transition_residuals = 0;
    let (tw,th)=transition_dimensions(&profile);
    let _transition_memory=r.reserve_memory((tw*th*1152) as u64,"输出过渡检查")?;
    let scale=p.detection.geometry.as_ref().map(|g|g.scale).unwrap_or(1.);
    for (i, cut) in cuts.iter().enumerate() {
        r.measure(
            "cutFrameValidate",
            i as i64,
            Some(cuts.len() as i64),
            "cuts",
            "逐帧检查切点前后 0.5 秒",
        );
        let at = cut.output_us - range.0;
        let a = (at - 500000).max(0);
        let b = (at + 500000).min(range.1 - range.0);
        let mut observed = vec![];
        let mut transitions=TransitionAudit::new(vec![cut.output_us],&profile);
        // Read actual decoded PTS alongside the pixels. A separate ffprobe
        // frame scan used to decode the same window a second time.
        let vf=format!("settb=expr=1/1000000,trim=start_pts={a}:end_pts={b},{},format=gray,showinfo@arkcut_pts=checksum=0",crop_filter(p,&profile));
        let seek = (a - 2000).max(0);
        let mut previous = None;
        r.frames_with_timestamps(
            &decode_args(&[
                "-v",
                "info",
                "-nostats",
                "-copyts",
                "-ss",
                &crate::render::sec(seek),
                "-t",
                &crate::render::sec(b - seek + 100000),
                "-i",
                &path.to_string_lossy(),
                "-vf",
                &vf,
                "-fps_mode",
                "passthrough",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "gray",
                "pipe:1",
            ]),
            profile.width * profile.height,
            |_, t, f| {
                if t < a || t >= b || previous.is_some_and(|last| t < last) {
                    return Err("输出验证实际 PTS 超出窗口或倒退".into());
                }
                previous = Some(t);
                transitions.push(transition_frame(&profile,scale,t+range.0,f)?)?;
                let sample = classify(f, &profile, t + range.0);
                observed.push(sample);
                Ok(())
            },
        )?;
        transitions.finish()?;
        if observed.is_empty() && b > a {
            return Err("输出验证窗口没有可解码帧".into());
        }
        let source_before = p
            .source
            .frame_pts
            .iter()
            .copied()
            .take_while(|t| *t < cut.start_us)
            .last();
        let source_after = p
            .source
            .frame_pts
            .iter()
            .copied()
            .find(|t| *t >= cut.end_us);
        transitions.results[0]=confirm_output_transition(r,p,&profile,cut.start_us,cut.end_us,transitions.results[0].take())?;
        let automatic=automatic_cut(p,cut.start_us,cut.end_us);
        let removed=transitions.results[0]["removedFrames"].as_u64().unwrap_or(0) as usize;
        if automatic {transition_residuals+=removed;} else {uncertain+=removed;}
        checks.push(serde_json::json!({"outputUs":cut.output_us,"sourceStartUs":cut.start_us,"sourceEndUs":cut.end_us,"lastKeptSourceUs":source_before,"firstKeptSourceUs":source_after,"frames":observed,"transition":transitions.results[0],"transitionAutomatic":automatic}));
    }
    r.measure(
        "cutFrameValidate",
        cuts.len() as i64,
        Some(cuts.len() as i64),
        "cuts",
        "切点逐帧检查完成",
    );
    let (residuals, pause_candidates) = unique_pause_counts(&checks);
    uncertain += pause_candidates;
    Ok(
        serde_json::json!({"profile":profile.version,"transitionVersion":ALGORITHM_VERSION,"residualFrames":residuals,"transitionResidualFrames":transition_residuals,"candidateFrames":uncertain,"checks":checks,"needsReview":residuals>0||transition_residuals>0||uncertain>0}),
    )
}
// Dense cut windows overlap. Report physical frames once, without skipping any
// window's pixels or weakening the check that blocks confirmed residuals.
fn unique_pause_counts(checks: &[serde_json::Value]) -> (usize, usize) {
    let mut confirmed = std::collections::BTreeSet::new();
    let mut candidate = std::collections::BTreeSet::new();
    for frame in checks.iter().flat_map(|c| c["frames"].as_array().into_iter().flatten()) {
        let Some(time) = frame["timeUs"].as_i64() else { continue };
        match frame["state"].as_str() {
            Some("confirmed") => { confirmed.insert(time); }
            Some("candidate") => { candidate.insert(time); }
            _ => {}
        }
    }
    (confirmed.len(), candidate.difference(&confirmed).count())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn paint_evidence(pixels: &mut [u8], p: &Profile, names: &[&str]) {
        for e in p.evidence.iter().filter(|e| names.contains(&e.name.as_str())) {
            for &i in &e.positive { pixels[i] = 220; }
        }
    }
    #[test]
    fn mobile_caption_and_running_controls_cannot_delete_bullet_time() {
        let p = load_profile(false).unwrap();
        let mut pixels = vec![0; p.width * p.height];
        let legacy: Profile = serde_json::from_str(include_str!("../profiles/cn-mobile-20x9-v4-events.json")).unwrap();
        let caption = legacy.evidence.iter().find(|e| e.name == "pause_chinese").unwrap();
        for &i in &caption.positive {
            pixels[(caption.rect[1] + i / caption.rect[2]) * p.width + caption.rect[0] + i % caption.rect[2]] = 220;
        }
        paint_evidence(&mut pixels, &p, &["enemy_hud", "castle_hud", "pause_bars"]);
        assert_eq!(classify(&pixels, &p, 0).state, "nonPause");
        // Even stronger background coincidences must not override a clear
        // running button when there is no resume button.
        paint_evidence(&mut pixels, &p, &["pause_letter_0", "pause_letter_1", "pause_letter_2"]);
        let running = classify(&pixels, &p, 10);
        assert_eq!(running.state, "nonPause");
        assert!(running.normal_evidence);
        let events = event_spans(&[frame(0, "confirmed", false), running, frame(20, "confirmed", false)], 30);
        assert_eq!(events.len(), 2);
        assert_eq!((events[0].end_us, events[1].start_us), (10, 20));
    }
    #[test]
    fn mobile_pause_letters_allow_supported_occlusion_but_not_one_bright_patch() {
        let p = load_profile(false).unwrap();
        let mut pixels = vec![0; p.width * p.height];
        paint_evidence(&mut pixels, &p, &["enemy_hud", "resume_button", "pause_letter_0"]);
        assert_eq!(classify(&pixels, &p, 0).state, "nonPause");
        paint_evidence(&mut pixels, &p, &["pause_letter_3"]);
        assert_eq!(classify(&pixels, &p, 0).state, "confirmed");
    }
    #[test]
    fn overlapping_audits_count_each_residual_frame_once() {
        let checks = vec![
            serde_json::json!({"frames":[{"timeUs":100,"state":"confirmed"},{"timeUs":110,"state":"candidate"}]}),
            serde_json::json!({"frames":[{"timeUs":100,"state":"confirmed"},{"timeUs":110,"state":"candidate"},{"timeUs":120,"state":"confirmed"}]})
        ];
        assert_eq!(unique_pause_counts(&checks), (2, 1));
    }
    #[test]
    fn transition_evidence_preserves_automatic_identity_and_manual_edit_guard() {
        let mut p=Pause{id:"stable-pause-id".into(),start_us:980_000,end_us:2_000_000,enabled:true,source:"auto".into(),state:"confirmed".into(),score:1.,audio_mode:None,detected_bounds:Some([980_000,2_000_000]),transition:Some(TransitionReview{version:ALGORITHM_VERSION.into(),original_bounds:[1_000_000,2_000_000],applied_bounds:[980_000,2_000_000],decision:"accepted".into(),evidence:serde_json::json!({"removedFrames":2})})};
        assert!(automatic_boundary(&p));
        let restored:Pause=serde_json::from_value(serde_json::to_value(&p).unwrap()).unwrap();
        assert_eq!(restored.transition.unwrap().original_bounds,[1_000_000,2_000_000]);
        p.start_us=970_000;
        assert!(!automatic_boundary(&p));
        let mut old=serde_json::to_value(&p).unwrap();
        old.as_object_mut().unwrap().remove("transition");
        assert!(serde_json::from_value::<Pause>(old).unwrap().transition.is_none());
    }
    #[test]
    fn compressed_output_tail_cannot_override_source_bullet_time_protection() {
        let mut p=crate::timeline::tests::project();
        p.pauses=vec![Pause{id:"continued-selection".into(),start_us:SECOND,end_us:2*SECOND,enabled:true,source:"auto".into(),state:"confirmed".into(),score:1.,audio_mode:None,detected_bounds:Some([SECOND,2*SECOND]),transition:Some(TransitionReview{version:ALGORITHM_VERSION.into(),original_bounds:[SECOND,2*SECOND],applied_bounds:[SECOND,2*SECOND],decision:"protected_continued_selection".into(),evidence:serde_json::json!({"candidateFrames":1})})}];
        let protected=source_protection(&p,SECOND,2*SECOND).unwrap();
        let result=preserve_output_selection(serde_json::json!({"proposedStartUs":SECOND-50_000,"removedFrames":6}),protected);
        assert!(result["proposedStartUs"].is_null());
        assert_eq!(result["removedFrames"],0);
        assert_eq!(result["outputProposal"]["removedFrames"],6);
        p.pauses[0].start_us-=10_000;
        assert!(source_protection(&p,SECOND-10_000,2*SECOND).is_none());
    }
    #[test]
    fn ui_anchors_do_not_gate_on_aspect_and_survive_right_occlusion() {
        for height in [240,288,360,400,480] {
            let p=transform_profile(load_profile(true).unwrap(),&DetectionGeometry{width:640,height,scale:1.}).unwrap();
            let mut pixels=vec![0u8;p.width*p.height];
            for e in p.evidence.iter().filter(|e|e.kind=="overlay"||e.kind=="hud") {for &i in &e.positive{pixels[i]=220;}}
            assert_eq!(classify(&pixels,&p,0).state,"confirmed","height={height}");
            for e in p.evidence.iter().filter(|e|e.kind=="overlay") {for &i in &e.positive{pixels[i]=0;}}
            for e in p.evidence.iter().filter(|e|e.kind=="resume") {for &i in &e.positive{pixels[i]=220;}}
            assert_ne!(classify(&pixels,&p,0).state,"confirmed","right icon alone must not delete end-of-battle frames");
        }
    }
    #[test]
    fn automatic_repair_respects_user_bounds_and_uncertainty() {
        let mut p = Pause {
            id: "a".into(),
            start_us: 100,
            end_us: 200,
            enabled: true,
            source: "auto".into(),
            state: "confirmed".into(),
            score: 1.,
            audio_mode: None,
            detected_bounds: Some([100, 200]),
            transition: None,
        };
        assert!(automatic_boundary(&p));
        p.start_us = 101;
        assert!(!automatic_boundary(&p));
        p.start_us = 100;
        p.source = "manual".into();
        assert!(!automatic_boundary(&p));
        p.source = "auto".into();
        p.state = "candidate".into();
        assert!(!automatic_boundary(&p));
        p.state = "confirmed".into();
        p.detected_bounds = None;
        assert!(!automatic_boundary(&p));
    }
    fn frame(time_us: Us, state: &str, normal: bool) -> Sample {
        Sample {
            time_us,
            state: state.into(),
            score: 0.9,
            evidence: vec![],
            pause_evidence: state != "nonPause",
            normal_evidence: normal,
        }
    }
    #[test]
    fn scan_rotation_requires_a_canonical_unmirrored_matrix() {
        let stream = |values: [i64; 9]| {
            serde_json::json!({"side_data_list":[{
                "side_data_type":"Display Matrix",
                "displaymatrix":format!("\n00000000: {} {} {}\n00000001: {} {} {}\n00000002: {} {} {}\n",values[0],values[1],values[2],values[3],values[4],values[5],values[6],values[7],values[8])
            }]})
        };
        assert_eq!(
            canonical_scan_rotation(&stream([0, -65536, 0, 65536, 0, 0, 0, 0, 1073741824])),
            Some("transpose=cclock")
        );
        assert_eq!(
            canonical_scan_rotation(&stream([0, 65536, 0, -65536, 0, 0, 0, 0, 1073741824])),
            Some("transpose=clock")
        );
        assert_eq!(
            canonical_scan_rotation(&stream([-65536, 0, 0, 0, -65536, 0, 0, 0, 1073741824])),
            Some("hflip,vflip")
        );
        for values in [
            [65536, 0, 0, 0, 65536, 0, 0, 0, 1073741824], // no rotation
            [0, 65536, 0, 65536, 0, 0, 0, 0, 1073741824], // mirror
            [0, -65536, 0, 65536, 0, 0, 1, 0, 1073741824], // translation
            [0, -65535, 0, 65535, 0, 0, 0, 0, 1073741824], // scale
        ] {
            assert_eq!(canonical_scan_rotation(&stream(values)), None);
        }
        assert_eq!(
            canonical_scan_rotation(&serde_json::json!({"tags":{"rotate":"90"}})),
            None
        );
    }
    #[test]
    fn event_continuity_and_true_short_recovery() {
        let frames = vec![
            frame(0, "candidate", false),
            frame(10, "confirmed", false),
            frame(20, "nonPause", false),
            frame(30, "candidate", false),
            frame(40, "nonPause", false),
            frame(50, "nonPause", false),
            frame(60, "nonPause", false),
        ];
        let spans = event_spans(&frames, 70);
        assert_eq!(spans.len(), 1);
        assert_eq!(
            (spans[0].start_us, spans[0].end_us, spans[0].strong),
            (0, 40, true)
        );
        let frames = vec![
            frame(0, "confirmed", false),
            frame(10, "nonPause", true),
            frame(20, "confirmed", false),
        ];
        let spans = event_spans(&frames, 30);
        assert_eq!(spans.len(), 2);
        assert_eq!(
            (spans[0].end_us, spans[1].start_us, spans[1].end_us),
            (10, 20, 30)
        );
    }
    #[test]
    fn blank_and_static_not_pause() {
        let p = profile().unwrap();
        for value in [0u8, 100, 255] {
            let s = classify(&vec![value; p.width * p.height], &p, 0);
            assert_eq!(s.state, "nonPause");
        }
    }
}
