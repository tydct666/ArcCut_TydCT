//! Explicit acceptance test; requires ARKCUT_R12_AUDIT_PROJECT and ARKCUT_R12_AUDIT_OUTPUT.
use arkcut_core::{detector, model::Project, process::Runner, timeline};
use serde_json::json;
use std::{fs, path::PathBuf, sync::Arc};

#[test]
#[ignore]
fn continuous_audit_equals_window_audit_and_rejects_damaged_tail() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let output =
        PathBuf::from(std::env::var("ARKCUT_R12_AUDIT_OUTPUT").expect("explicit verified output"));
    let dir = root.join("tests/results/revision12/audit-equivalence");
    fs::create_dir_all(&dir).unwrap();
    let project = PathBuf::from(
        std::env::var_os("ARKCUT_R12_AUDIT_PROJECT").expect("Set ARKCUT_R12_AUDIT_PROJECT to the project matching the output"),
    );
    let p: Project = serde_json::from_slice(&fs::read(project).unwrap())
    .unwrap();
    let plan = timeline::build(&p).unwrap();
    let mut results = vec![];
    for overlap in [false, true] {
        let mut probe = plan.clone();
        if overlap {
            probe.removed[1].output_us = probe.removed[0].output_us + 100000;
        }
        let r = Runner::discover(Arc::new(|_, _| {})).unwrap();
        r.prepare_stage("audit", 128 * 1024 * 1024, 8 * 1024 * 1024)
            .unwrap();
        let old = detector::check_output(&r, &p, &probe, &output, (0, plan.duration_us)).unwrap();
        let new =
            detector::check_full_output(&r, &p, &probe, &output, (0, plan.duration_us)).unwrap();
        assert_eq!(
            old, new,
            "same frame decisions and real timestamps, including overlaps"
        );
        results.push(json!({"overlap":overlap,"equal":true,"checks":new["checks"].as_array().unwrap().len()}));
    }
    let damaged = dir.join("damaged-tail.mp4");
    fs::copy(&output, &damaged).unwrap();
    let file = fs::OpenOptions::new().write(true).open(&damaged).unwrap();
    file.set_len(file.metadata().unwrap().len() - 2 * 1024 * 1024)
        .unwrap();
    drop(file);
    let r = Runner::discover(Arc::new(|_, _| {})).unwrap();
    let error =
        detector::check_full_output(&r, &p, &plan, &damaged, (0, plan.duration_us)).unwrap_err();
    fs::remove_file(damaged).unwrap();
    fs::write(
        dir.join("evidence.json"),
        serde_json::to_vec_pretty(
            &json!({"status":"passed","cases":results,"damagedTailRejected":error}),
        )
        .unwrap(),
    )
    .unwrap();
}
