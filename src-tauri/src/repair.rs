//! Bounded repair reports. Evidence never substitutes for final-file validation.
use crate::{model::*, process::Runner, storage};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
};
pub const RULE: &str = "s04-full-decode-v3-ui8-transitions1";

pub fn identity(r: &Runner, path: &str) -> Result<String, String> {
    let fingerprint = crate::media::fingerprint(path)?;
    let key = storage::hash(&fingerprint)?;
    if let Some(value) = r.content_ids.lock().unwrap().get(&key) {
        return Ok(value.clone());
    }
    let (_, sha) = crate::export_record::digest_checked(Path::new(path), || r.check())?;
    r.content_ids.lock().unwrap().insert(key, sha.clone());
    Ok(sha)
}
fn path(p: &Project) -> Result<PathBuf, String> {
    let dir = storage::root().join("recovery");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join(format!("{}.json", storage::hash(&p.project_id)?)))
}
pub fn read(p: &Project) -> Result<Value, String> {
    let file = match fs::File::open(path(p)?) {
        Ok(f) => f,
        Err(_) => return Ok(Value::Null),
    };
    if file.metadata().map_err(|e| e.to_string())?.len() > 16 * 1024 * 1024 {
        return Ok(Value::Null);
    }
    let value: Value = match serde_json::from_reader(file.take(16 * 1024 * 1024 + 1)) {
        Ok(v) => v,
        Err(_) => return Ok(Value::Null),
    };
    let signature = crate::export_record::signature(p)?;
    if !matches_snapshot(p, &value, &signature) {
        return Ok(Value::Null);
    }
    Ok(value)
}
fn matches_snapshot(p: &Project, value: &Value, signature: &str) -> bool {
    value["rule"] == RULE
        && (value["signature"] == signature
            || value["beforeSignature"] == signature
                && value["beforeProject"]["revision"]
                    .as_u64()
                    .is_some_and(|r| p.revision <= r))
}
pub fn save(p: &Project, report: &Value) -> Result<(), String> {
    let target = path(p)?;
    // Keep one bounded report per project; older reports are optional diagnostics.
    let mut files: Vec<_> = fs::read_dir(target.parent().unwrap())
        .map_err(|e| e.to_string())?
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| {
            e.metadata()
                .ok()
                .map(|m| (m.modified().ok(), e.path(), m.len()))
        })
        .collect();
    files.sort_by_key(|e| e.0);
    let mut bytes: u64 = files.iter().map(|e| e.2).sum();
    let mut count = files.len();
    for (_, file, len) in files {
        if count < 32 && bytes < 32 * 1024 * 1024 {
            break;
        }
        if file != target {
            let _ = fs::remove_file(file);
            count -= 1;
            bytes = bytes.saturating_sub(len);
        }
    }
    storage::atomic_json_limited(&target, report, 16 * 1024 * 1024)
}
pub fn issue(
    kind: &str,
    message: &str,
    source: Option<Us>,
    output: Option<Us>,
    ids: Vec<String>,
    evidence: Value,
) -> Value {
    json!({"kind":kind,"message":message,"sourceUs":source,"outputUs":output,"pauseIds":ids,"scope":if source.is_some(){"cut"}else{"file"},"status":"needsReview","evidence":evidence})
}
pub fn audit_issues(p: &Project, plan: &Plan, audit: &Value) -> Vec<Value> {
    let mut issues:Vec<_> = audit["checks"].as_array().into_iter().flatten().filter_map(|c| {
        let frames=c["frames"].as_array()?;
        let residual=frames.iter().any(|s|s["state"]=="confirmed");
        let sample=frames.iter().find(|s|s["state"]==if residual{"confirmed"}else{"candidate"})?;
        let start=c["sourceStartUs"].as_i64()?;let end=c["sourceEndUs"].as_i64()?;
        let at=sample["timeUs"].as_i64()?;
        let source=plan.keep.iter().find(|k|at>=k.output_us&&at<k.output_us+k.end_us-k.start_us).map(|k|k.start_us+at-k.output_us);
        let ids=p.pauses.iter().filter(|v|v.enabled&&v.start_us<=end&&v.end_us>=start).map(|v|v.id.clone()).collect();
        Some(issue(if residual{"pauseResidual"}else{"suspectedFrame"}, if residual{"剪辑点仍有暂停画面，请查看并调整边界"}else{"剪辑点画面待确认，请查看后调整或确认保留"}, source,Some(at),ids,json!({"check":c,"sourceRange":[start,end],"outputRange":[(c["outputUs"].as_i64().unwrap_or(at)-500000).max(0),c["outputUs"].as_i64().unwrap_or(at)+500000]})))
    }).collect();
    for check in audit["checks"].as_array().into_iter().flatten() {
        let Some(at)=check["transition"]["proposedStartUs"].as_i64() else {continue};
        let (Some(start),Some(end))=(check["sourceStartUs"].as_i64(),check["sourceEndUs"].as_i64()) else {continue};
        let source=plan.keep.iter().find(|k|at>=k.output_us && at<k.output_us+k.end_us-k.start_us).map(|k|k.start_us+at-k.output_us);
        let ids=p.pauses.iter().filter(|v|v.enabled&&v.start_us<end&&v.end_us>start).map(|v|v.id.clone()).collect();
        let automatic=check["transitionAutomatic"].as_bool()==Some(true);
        issues.push(issue(if automatic{"transitionResidual"}else{"suspectedFrame"},if automatic{"剪辑点仍有短暂选中偏移，正在复核源片"}else{"人工剪辑点有短暂选中偏移，请查看后调整或确认保留"},source,Some(at),ids,json!({"check":check,"sourceRange":[start,end],"outputRange":[at,check["outputUs"]]})));
    }
    coalesce_audit_issues(issues)
}
fn coalesce_audit_issues(issues: Vec<Value>) -> Vec<Value> {
    let mut by_source: HashMap<(String, Us), usize> = HashMap::new();
    let mut unique: Vec<Value> = Vec::new();
    for item in issues {
        // Overlapping seam windows can observe the same retained source frame.
        // Never combine unmapped samples: their source identity is unknown.
        let key = item["kind"]
            .as_str()
            .zip(item["sourceUs"].as_i64())
            .map(|(kind, source)| (kind.to_owned(), source));
        let existing = key.as_ref().and_then(|key| by_source.get(key)).copied();
        if let Some(index) = existing {
            let previous = &mut unique[index];
            if let Some(ids) = previous["pauseIds"].as_array_mut() {
                for id in item["pauseIds"].as_array().into_iter().flatten() {
                    if !ids.contains(id) {
                        ids.push(id.clone());
                    }
                }
            }
            // Preserve every implicated seam for diagnostics and invalidate the
            // full dependency range when smart rendering considers cache reuse.
            for field in ["sourceRange", "outputRange"] {
                if let (Some(a), Some(b), Some(c), Some(d)) = (
                    previous["evidence"][field][0].as_i64(),
                    previous["evidence"][field][1].as_i64(),
                    item["evidence"][field][0].as_i64(),
                    item["evidence"][field][1].as_i64(),
                ) {
                    previous["evidence"][field] = json!([a.min(c), b.max(d)]);
                }
            }
            if previous["evidence"]["checks"].is_null() {
                previous["evidence"]["checks"] =
                    json!([previous["evidence"]["check"].clone()]);
            }
            let check = &item["evidence"]["check"];
            let checks = previous["evidence"]["checks"].as_array_mut().unwrap();
            if !checks.contains(check) {
                checks.push(check.clone());
            }
        } else {
            if let Some(key) = key {
                by_source.insert(key, unique.len());
            }
            unique.push(item);
        }
    }
    unique
}
pub fn failure(error: &str) -> Value {
    let (kind, message) = if error.contains("RESOURCE_MEMORY") {
        ("resource", "内存不足，请释放内存后重试修复")
    } else if error.contains("RESOURCE_DISK") {
        ("resource", "磁盘空间不足，请清理空间后重试修复")
    } else if error.contains("AV_LENGTH") {
        (
            "audio",
            "音视频长度异常，请重试修复；若再次失败，请导出日志",
        )
    } else if error.contains("TIMING")
        || error.contains("PTS")
        || error.contains("OUTPUT_DURATION")
        || error.contains("OUTPUT_FRAMES")
    {
        ("timing", "成片时序异常，请重试修复；若再次失败，请导出日志")
    } else if error.contains("COMMIT")
        || error.contains("OUTPUT_DIRECTORY")
        || error.contains("OUTPUT_PATH")
        || error.contains("OUTPUT_EXISTS")
        || error.contains("无效输出")
        || error.contains("os error 5")
        || error.contains("os error 3")
    {
        ("path", "输出位置不可用，请更换保存位置后重试修复")
    } else if error.contains("SOURCE") || error.contains("素材") {
        ("source", "源素材发生变化或无法读取，请检查或重新导入素材")
    } else if error.contains("OUTPUT_FORMAT")
        || error.contains("SMART_MP4")
        || error.contains("MUX")
    {
        (
            "mux",
            "成片封装检查未通过，请重试修复；若仍失败，请导出日志",
        )
    } else if error.to_ascii_lowercase().contains("decoding")
        || error.to_ascii_lowercase().contains("invalid data")
    {
        (
            "decode",
            "媒体解码异常，尚无法可靠定位；请检查源文件或重试修复",
        )
    } else {
        (
            "file",
            "媒体检查未通过，暂无法精确定位；请重试或导出日志排查",
        )
    };
    issue(
        kind,
        message,
        None,
        None,
        vec![],
        json!({"diagnostic":error,"locationLimit":"未获得可靠帧位置，文件级检查失败"}),
    )
}
pub fn report(
    before: &Project,
    p: &Project,
    source: &str,
    issues: &[Value],
    changes: &[Value],
    state: &str,
) -> Result<Value, String> {
    Ok(
        json!({"rule":RULE,"signature":crate::export_record::signature(p)?,"beforeSignature":crate::export_record::signature(before)?,"sourceSha256":source,"issues":issues,"changes":changes,"state":state,"beforeProject":if before.revision!=p.revision{Some(before)}else{None},"effectiveProject":if before.revision!=p.revision{Some(p)}else{None},"validation":{"requiredCoverage":"full decode + stream/timing/length checks; pause seams only with applicable recognition","detectionStatus":p.detection.status,"pauseAuditPassed":if crate::detector::can_check(p)&&p.pauses.iter().any(|c|c.enabled){Some(state=="passed")}else{None},"fileValidationReused":false,"passed":state=="passed"}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dense_seam_windows_report_one_source_frame_and_keep_all_dependencies() {
        let mut p = crate::timeline::tests::project();
        p.pauses = [("a", SECOND, 2 * SECOND), ("b", 2_100_000, 3 * SECOND), ("c", 3_100_000, 4 * SECOND)]
            .into_iter()
            .map(|(id, start_us, end_us)| Pause {
                id: id.into(), start_us, end_us, enabled: true, source: "auto".into(),
                state: "confirmed".into(), score: 1., audio_mode: None,
                detected_bounds: Some([start_us, end_us]), transition: None,
            })
            .collect();
        let plan = crate::timeline::build(&p).unwrap();
        let mut checks: Vec<_> = plan.removed.iter().map(|cut| json!({
            "outputUs":cut.output_us,"sourceStartUs":cut.start_us,"sourceEndUs":cut.end_us,
            "frames":[{"timeUs":1_020_000,"state":"confirmed"}]
        })).collect();
        checks.push(checks[0].clone());
        let issues = audit_issues(&p, &plan, &json!({"checks":checks}));
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0]["kind"], "pauseResidual");
        assert_eq!(issues[0]["sourceUs"], 2_020_000);
        assert_eq!(issues[0]["outputUs"], 1_020_000);
        assert_eq!(issues[0]["pauseIds"], json!(["a", "b", "c"]));
        assert_eq!(issues[0]["evidence"]["sourceRange"], json!([SECOND, 4 * SECOND]));
        assert_eq!(issues[0]["evidence"]["outputRange"], json!([500_000, 1_700_000]));
        assert_eq!(issues[0]["evidence"]["checks"].as_array().unwrap().len(), 3);
    }
    #[test]
    fn audit_coalescing_keeps_distinct_kinds_sources_and_unmapped_samples() {
        let items = [
            ("pauseResidual", Some(1)), ("pauseResidual", Some(2)),
            ("transitionResidual", Some(1)), ("suspectedFrame", Some(1)),
            ("pauseResidual", None), ("pauseResidual", None),
        ].into_iter().map(|(kind, source)| issue(kind, "review", source, Some(1), vec![], json!({}))).collect();
        assert_eq!(coalesce_audit_issues(items).len(), 6);
    }
    #[test]
    fn transition_audit_keeps_manual_boundary_reviewable() {
        let mut p=crate::timeline::tests::project();
        p.pauses=vec![Pause{id:"p".into(),start_us:SECOND,end_us:2*SECOND,enabled:true,source:"auto".into(),state:"confirmed".into(),score:1.,audio_mode:None,detected_bounds:Some([SECOND,2*SECOND]),transition:None}];
        let plan=crate::timeline::build(&p).unwrap();
        let mut audit=json!({"checks":[{"outputUs":SECOND,"sourceStartUs":SECOND,"sourceEndUs":2*SECOND,"frames":[],"transitionAutomatic":true,"transition":{"proposedStartUs":SECOND-20_000,"removedFrames":2}}]});
        let automatic=audit_issues(&p,&plan,&audit);
        assert_eq!(automatic.len(),1);
        assert_eq!(automatic[0]["kind"],"transitionResidual");
        assert_eq!(automatic[0]["sourceUs"],SECOND-20_000);
        audit["checks"][0]["transitionAutomatic"]=json!(false);
        let manual=audit_issues(&p,&plan,&audit);
        assert_eq!(manual[0]["kind"],"suspectedFrame");
    }
    #[test]
    fn undo_is_not_replaced_by_persisted_automatic_correction() {
        let p = crate::timeline::tests::project();
        let mut fixed = p.clone();
        fixed.mix.game_gain = 0.5;
        fixed.revision += 1;
        let value = report(&p, &fixed, "sha", &[], &[], "passed").unwrap();
        let signature = crate::export_record::signature(&p).unwrap();
        assert!(matches_snapshot(&p, &value, &signature));
        let mut undone = p.clone();
        undone.revision = fixed.revision + 1;
        assert!(!matches_snapshot(&undone, &value, &signature));
        assert!(matches_snapshot(
            &fixed,
            &value,
            &crate::export_record::signature(&fixed).unwrap()
        ));
    }
    #[test]
    fn issues_map_actual_output_sample_through_merged_timeline() {
        let mut p = crate::timeline::tests::project();
        p.pauses = vec![
            Pause {
                id: "a".into(),
                start_us: SECOND,
                end_us: 2 * SECOND,
                enabled: true,
                source: "auto".into(),
                state: "confirmed".into(),
                score: 1.,
                audio_mode: None,
                detected_bounds: Some([SECOND, 2 * SECOND]),
                transition: None,
            },
            Pause {
                id: "b".into(),
                start_us: 2 * SECOND,
                end_us: 3 * SECOND,
                enabled: true,
                source: "manual".into(),
                state: "confirmed".into(),
                score: 1.,
                audio_mode: None,
                detected_bounds: None,
                transition: None,
            },
        ];
        let plan = crate::timeline::build(&p).unwrap();
        let audit = json!({"checks":[{"outputUs":SECOND,"sourceStartUs":SECOND,"sourceEndUs":3*SECOND,"frames":[{"timeUs":SECOND+1000,"state":"confirmed"}]}]});
        let issues = audit_issues(&p, &plan, &audit);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0]["sourceUs"], 3 * SECOND + 1000);
        assert_eq!(issues[0]["pauseIds"], json!(["a", "b"]));
        let mut uncertain = audit.clone();
        uncertain["checks"][0]["frames"][0]["state"] = json!("candidate");
        assert_eq!(
            audit_issues(&p, &plan, &uncertain)[0]["kind"],
            "suspectedFrame"
        );
    }
    #[test]
    fn missing_positions_remain_file_level() {
        let v = failure("unknown decode error");
        assert!(v["sourceUs"].is_null());
        assert_eq!(v["scope"], "file");
    }
    #[test]
    fn resource_failure_is_not_a_media_repair() {
        assert_eq!(failure("RESOURCE_MEMORY")["kind"], "resource");
        assert_eq!(failure("AV_LENGTH")["kind"], "audio");
        assert_eq!(failure("OUTPUT_DIRECTORY: os error 2")["kind"], "path");
    }
    #[test]
    fn confirmation_is_bound_and_cannot_bypass_confirmed_residuals() {
        let mut p = crate::timeline::tests::project();
        p.project_id = uuid::Uuid::new_v4().to_string();
        let candidate = issue(
            "suspectedFrame",
            "review",
            Some(1),
            Some(1),
            vec![],
            json!({}),
        );
        let value = report(&p, &p, "sha", &[candidate], &[], "needsReview").unwrap();
        save(&p, &value).unwrap();
        let engine = crate::Engine::default();
        let confirmed = engine
            .request("confirmReview", json!({"project":p}))
            .unwrap();
        assert!(confirmed["confirmedIssues"].is_string());
        let mut changed = p.clone();
        changed.mix.game_gain = 0.6;
        assert!(engine
            .request("confirmReview", json!({"project":changed}))
            .is_err());
        let mut mixed = value.clone();
        mixed["issues"][0]["kind"] = json!("pauseResidual");
        save(&p, &mixed).unwrap();
        assert!(engine
            .request("confirmReview", json!({"project":p}))
            .is_err());
        fs::remove_file(path(&p).unwrap()).unwrap();
    }
}
