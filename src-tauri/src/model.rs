use serde::{Deserialize, Serialize};
pub type Us = i64;
pub const SECOND: Us = 1_000_000;
pub const PLAN_VERSION: &str = "arkcut-plan-5-events-envelope-audio-clock";
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Fingerprint {
    pub path: String,
    pub size: u64,
    pub modified_ms: u128,
    pub sample_hash: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stream {
    pub index: u32,
    pub kind: String,
    pub codec: String,
    pub language: String,
    pub channels: u32,
    #[serde(default)]
    pub channel_layout: String,
    pub width: u32,
    pub height: u32,
    pub rate: String,
    pub time_base: String,
    pub start_us: Us,
    pub default: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Media {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub fingerprint: Fingerprint,
    pub duration_us: Us,
    pub format: String,
    pub streams: Vec<Stream>,
    pub video_index: Option<u32>,
    pub audio_index: Option<u32>,
    pub origin_us: Us,
    pub frame_pts: Vec<Us>,
    pub hdr: bool,
    pub warnings: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pause {
    pub id: String,
    pub start_us: Us,
    pub end_us: Us,
    pub enabled: bool,
    pub source: String,
    pub state: String,
    pub score: f64,
    #[serde(default)]
    pub audio_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detected_bounds: Option<[Us; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition: Option<TransitionReview>,
}
/// UI pause bounds and the separate, reversible transition decision. The
/// detected_bounds marker continues to describe the applied automatic bounds.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransitionReview {
    pub version: String,
    pub original_bounds: [Us; 2],
    pub applied_bounds: [Us; 2],
    pub decision: String,
    pub evidence: serde_json::Value,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Music {
    pub id: String,
    pub media: Media,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mix {
    pub game_gain: f64,
    pub music_gain: f64,
    pub tails: bool,
    pub mode: String,
    pub mode_manual: bool,
    pub current_track: Option<String>,
    pub normalize: bool,
    #[serde(default = "legacy_mode")]
    pub join_mode: String,
}
impl Default for Mix {
    fn default() -> Self {
        Self {
            game_gain: 1.,
            music_gain: 0.3,
            tails: true,
            mode: "single".into(),
            mode_manual: false,
            current_track: None,
            normalize: true,
            join_mode: "natural".into(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Export {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceleration: Option<String>,
    pub container: String,
    pub codec: String,
    #[serde(default = "default_quality")]
    pub quality: String,
    pub fps: Option<u32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    #[serde(default = "proxy_edge")]
    pub preview_edge: u32,
}
fn default_quality() -> String {
    "high".into()
}
impl Default for Export {
    fn default() -> Self {
        Self {
            acceleration: None,
            container: "source".into(),
            codec: "auto".into(),
            quality: default_quality(),
            fps: None,
            width: None,
            height: None,
            preview_edge: 1280,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct DetectionGeometry { pub width:usize,pub height:usize,pub scale:f64 }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Detection {
    #[serde(default)]
    pub geometry: Option<DetectionGeometry>,
    #[serde(default = "detection_unknown")]
    pub status: String,
    pub version: String,
    pub scan_hz: u32,
    pub min_pause_us: Us,
    pub activity_db: f64,
    pub region: Option<[f64; 4]>,
    pub profile: Option<String>,
}
fn detection_unknown() -> String { "unknown".into() }
impl Default for Detection {
    fn default() -> Self {
        Self {
            geometry: None,
              status: detection_unknown(),
              version: "ui-events-8-transitions-3".into(),
            scan_hz: 8,
            min_pause_us: 250000,
            activity_db: -40.,
            region: None,
            profile: None,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_export: Option<crate::export_record::ExportRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_path: Option<String>,
    pub schema_version: u32,
    pub project_id: String,
    pub revision: u64,
    pub source: Media,
    pub video_index: u32,
    pub audio_index: Option<u32>,
    pub detection: Detection,
    pub pauses: Vec<Pause>,
    pub music: Vec<Music>,
    pub mix: Mix,
    pub export: Export,
}
impl Project {
    pub fn new(source: Media) -> Result<Self, String> {
        let vi = source.video_index.ok_or("未找到有效视频流")?;
        Ok(Self {
            completed_export: None,
            name: None,
            output_path: None,
            schema_version: 1,
            project_id: uuid::Uuid::new_v4().to_string(),
            revision: 0,
            video_index: vi,
            audio_index: source.audio_index,
            source,
            detection: Detection::default(),
            pauses: vec![],
            music: vec![],
            mix: Mix::default(),
            export: Export::default(),
        })
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Span {
    pub start_us: Us,
    pub end_us: Us,
    pub output_us: Us,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tail {
    pub source_us: Us,
    pub output_us: Us,
    pub duration_us: Us,
    pub gain: f64,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub envelope: Vec<GainPoint>,
    #[serde(default)]
    pub notes: Vec<String>,
}
fn legacy_mode() -> String {
    "legacy".into()
}
fn proxy_edge() -> u32 {
    1280
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GainPoint {
    pub time_us: Us,
    pub gain: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Placement {
    pub track: usize,
    pub output_us: Us,
    pub duration_us: Us,
    pub fade_in_us: Us,
    pub fade_out_us: Us,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub version: String,
    pub project_id: String,
    pub revision: u64,
    pub keep: Vec<Span>,
    pub removed: Vec<Span>,
    pub duration_us: Us,
    pub tails: Vec<Tail>,
    pub music: Vec<Placement>,
    pub loudness_gain: Vec<f64>,
    pub warnings: Vec<String>,
}

#[cfg(test)]
mod defaults_tests {
    use super::*;
    #[test]
    fn export_missing_quality_defaults_high_and_explicit_choices_survive() {
        assert_eq!(Export::default().quality, "high");
        let mut data = serde_json::json!({"container":"source","codec":"auto"});
        assert_eq!(
            serde_json::from_value::<Export>(data.clone())
                .unwrap()
                .quality,
            "high"
        );
        for quality in ["standard", "high", "small"] {
            data["quality"] = serde_json::json!(quality);
            let restored: Export = serde_json::from_value(data.clone()).unwrap();
            assert_eq!(restored.quality, quality);
            assert_eq!(serde_json::to_value(restored).unwrap()["quality"], quality);
        }
    }
}
