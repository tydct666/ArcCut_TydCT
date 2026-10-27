#![cfg(all(windows, feature = "resource-test-hooks"))]
//! Reproduce the user's 16 GiB system with ~2 GiB free. Overrides only reduce
//! real counter values; this never allocates ballast or changes OS settings.
use arkcut_core::{
    resource::{ResourceController, GIB, MIB},
    Engine,
};
use serde_json::{json, Value};
use std::{
    fs,
    path::Path,
    sync::{atomic::AtomicBool, Arc},
    time::{Duration, Instant},
};

fn write(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}
fn hook(path: &Path, value: Value) {
    let next = path.with_extension("next");
    write(&next, &value);
    fs::rename(next, path).unwrap();
}

#[test]
#[ignore = "Real-media acceptance; requires ARKCUT_LOW_HEADROOM_VIDEO and ARKCUT_LOW_HEADROOM_OUT"]
fn sustained_soft_pressure_allows_complete_import_but_critical_memory_still_stops() {
    let out = std::path::PathBuf::from(std::env::var_os("ARKCUT_LOW_HEADROOM_OUT").unwrap());
    fs::create_dir(&out).unwrap();
    let data = out.join("data");
    fs::create_dir(&data).unwrap();
    std::env::set_var("ARKCUT_DATA_DIR", &data);
    let hooks = data.join(".arkcut-resource-test.json");
    std::env::set_var("ARKCUT_RESOURCE_TEST_FILE", &hooks);
    let low = json!({"totalBytes":16*GIB,"availableBytes":2*GIB,"commitAvailableBytes":7*GIB});
    hook(&hooks, low.clone());

    let controller = ResourceController::new(Arc::new(AtomicBool::new(false)));
    for _ in 0..5 {
        controller.refresh(true);
    }
    assert_eq!(controller.summary().pressure, "pressure");
    assert_eq!(controller.threads(), 1);
    // This exact dispatch failed before the fix, although the existing critical
    // physical/commit guards and the bounded allocation leases were satisfied.
    for _ in 0..7 {
        let process = controller
            .process()
            .expect("soft pressure must allow the next bounded process");
        drop(process);
    }
    write(&out.join("dispatch.json"), &json!(controller.summary()));

    let mut protection = vec![];
    for (name, counters) in [
        ("physical", json!({"availableBytes":256*MIB})),
        ("commit", json!({"commitAvailableBytes":511*MIB})),
    ] {
        hook(&hooks, counters);
        let c = ResourceController::new(Arc::new(AtomicBool::new(false)));
        let error = c
            .process()
            .err()
            .expect("critical memory must reject process dispatch");
        assert!(error.starts_with("RESOURCE_MEMORY:"));
        protection.push(json!({"case":name,"error":error,"resource":c.summary()}));
    }
    write(&out.join("critical-guards.json"), &json!(protection));
    hook(&hooks, low);

    let engine = Engine::default();
    let source = std::path::PathBuf::from(std::env::var_os("ARKCUT_LOW_HEADROOM_VIDEO").unwrap());
    engine.request("import", json!({"path":source})).unwrap();
    let started = Instant::now();
    let mut phases = std::collections::BTreeSet::new();
    let mut max_layout_samples = 0;
    let mut next_report = Duration::ZERO;
    let result = loop {
        let s = engine
            .request("status", json!({"includeResult":true}))
            .unwrap();
        let job = &s["job"];
        if let Some(phase) = job["phase"].as_str() {
            phases.insert(phase.to_owned());
        }
        if job["phase"] == "detectLayout" {
            max_layout_samples =
                max_layout_samples.max(job["completedUnits"].as_i64().unwrap_or(0));
        }
        if !matches!(job["status"].as_str(), Some("running" | "cancelling")) {
            break s;
        }
        if started.elapsed() >= next_report {
            write(&out.join("progress.json"), &s);
            eprintln!("{}s: {}", started.elapsed().as_secs(), job["phase"]);
            next_report = started.elapsed() + Duration::from_secs(15);
        }
        if started.elapsed() > Duration::from_secs(1200) {
            let _ = engine.request("cancel", json!({}));
            panic!("import timed out: {s}");
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    write(&out.join("import.json"), &result);
    let job = &result["job"];
    assert_eq!(job["status"], "completed", "{result}");
    assert_eq!(job["resource"]["simulation"], true);
    assert!(job["resource"]["failure"].is_null());
    assert!(job["resource"]["budgetDecisions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["advisoryFallback"] == true));
    assert_eq!(job["resource"]["threads"], 1);
    assert!(job["resource"]["minimumAvailable"].as_u64().unwrap() <= 2 * GIB);
    write(&out.join("project.json"), &job["result"]);
    write(
        &out.join("evidence.json"),
        &json!({
            "status":"passed", "simulation":true, "realLowRamHardware":false,
            "method":"Real Windows process counters and real full import; free physical/commit counter ceilings only, no memory exhaustion or CPU emulation",
            "softPressureDispatches":7,"criticalPhysicalAndCommitGuards":true,
            "source":source,"elapsedMs":started.elapsed().as_millis(),"phases":phases,
            "observedLayoutSamples":max_layout_samples,"pauseCount":job["result"]["pauses"].as_array().map(Vec::len),
            "resource":job["resource"]
        }),
    );
}
