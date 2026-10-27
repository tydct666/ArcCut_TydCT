use arkcut_core::{model::Project, Engine};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
fn wait(e: &Engine) -> Value {
    let start = Instant::now();
    loop {
        let v = e.request("status", json!({"after":0})).unwrap();
        if !["running", "cancelling"].contains(&v["job"]["status"].as_str().unwrap_or("")) {
            return v;
        }
        assert!(start.elapsed() < Duration::from_secs(60));
        std::thread::sleep(Duration::from_millis(20));
    }
}
#[test]
#[ignore = "Media integration: run scripts/bootstrap-runtime.ps1, then python tests/fixtures/generate.py before cargo test --test jobs -- --ignored"]
fn real_jobs_cancel_busy_and_safe_commit() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let data = root.join("tests/results/job-tests");
    std::fs::create_dir_all(&data).unwrap();
    std::env::set_var("ARKCUT_DATA_DIR", &data);
    let e = Engine::default();
    let path = root.join("tests/fixtures/generated/中文 空格 & 信号.mp4");
    assert!(path.exists(), "Run tests/fixtures/generate.py first");
    e.request("probe", json!({"path":path})).unwrap();
    assert!(e
        .request("probe", json!({"path":path}))
        .unwrap_err()
        .starts_with("JOB_BUSY"));
    let result = wait(&e);
    assert_eq!(result["job"]["status"], "completed");
    let p: Project = serde_json::from_value(result["job"]["result"].clone()).unwrap();
    let out = data.join("cancelled.mp4");
    let start = Instant::now();
    e.request("export", json!({"project":p,"path":out}))
        .unwrap();
    std::thread::sleep(Duration::from_millis(40));
    e.request("cancel", json!({})).unwrap();
    let cancelled = wait(&e);
    assert_eq!(cancelled["job"]["status"], "cancelled");
    assert!(start.elapsed() < Duration::from_secs(5));
    assert!(!out.exists());
    assert!(std::fs::read_dir(&data).unwrap().all(|e| !e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".arkcut-")));
    // Existing output is never touched, even if a render is immediately cancelled.
    let sentinel = data.join("existing.mp4");
    std::fs::write(&sentinel, b"user-owned sentinel").unwrap();
    e.request("export", json!({"project":p,"path":sentinel}))
        .unwrap();
    e.request("cancel", json!({})).unwrap();
    wait(&e);
    assert_eq!(std::fs::read(&sentinel).unwrap(), b"user-owned sentinel");
    // Cancel after the first real group commits: keep it, cancel the remaining group,
    // then retry and reuse the already validated media.
    let mut q = p.clone();
    q.mix.tails = false;
    q.mix.normalize = false;
    q.pauses = serde_json::from_value(json!([
        {"id":"a","startUs":1000000,"endUs":1100000,"enabled":true,"source":"manual","state":"confirmed","score":1},
        {"id":"b","startUs":9000000,"endUs":9100000,"enabled":true,"source":"manual","state":"confirmed","score":1}
    ])).unwrap();
    e.request("previewBatch", json!({"project":q,"selectedId":"b"}))
        .unwrap();
    let started = Instant::now();
    let partial = loop {
        let s = e.request("status", json!({"after":0})).unwrap();
        if s["job"]["previews"]
            .as_array()
            .is_some_and(|v| v.iter().any(|x| x["status"] == "ready"))
        {
            break s;
        }
        assert!(
            matches!(s["job"]["status"].as_str(), Some("running" | "cancelling")),
            "Preview job ended before any media became ready: {s}"
        );
        assert!(started.elapsed() < Duration::from_secs(30));
        std::thread::sleep(Duration::from_millis(5));
    };
    let entries = partial["job"]["previews"].as_array().unwrap();
    assert_eq!(entries[1]["status"], "ready");
    assert_ne!(entries[0]["status"], "ready");
    let saved = entries[1]["path"].as_str().unwrap().to_string();
    e.request("cancel", json!({})).unwrap();
    let cancelled = wait(&e);
    assert_eq!(cancelled["job"]["status"], "cancelled");
    assert_eq!(cancelled["job"]["previews"][0]["status"], "cancelled");
    assert!(PathBuf::from(&saved).is_file());
    e.request("previewBatch", json!({"project":q,"selectedId":"b"}))
        .unwrap();
    let retry = wait(&e);
    assert_eq!(retry["job"]["result"]["ready"], 2);
    assert_eq!(retry["job"]["result"]["previews"][1]["cached"], true);
    q.pauses.clear();
    e.request("previewBatch", json!({"project":q})).unwrap();
    let opening = wait(&e);
    assert_eq!(opening["job"]["result"]["previews"][0]["endUs"], 6000000);
    q.pauses=serde_json::from_value(json!([
        {"id":"head","startUs":0,"endUs":1000000,"enabled":true,"source":"manual","state":"confirmed","score":1},
        {"id":"tail","startUs":9000000,"endUs":10000000,"enabled":true,"source":"manual","state":"confirmed","score":1}
    ])).unwrap();
    e.request("previewBatch", json!({"project":q})).unwrap();
    let edges = wait(&e);
    assert_eq!(edges["job"]["result"]["ready"], 2);
    assert_eq!(edges["job"]["result"]["previews"][0]["startUs"], 0);
    assert_eq!(edges["job"]["result"]["previews"][1]["endUs"], 8000000);
    let mut empty = q.clone();
    empty.pauses=serde_json::from_value(json!([
        {"id":"all","startUs":0,"endUs":empty.source.duration_us,"enabled":true,"source":"manual","state":"confirmed","score":1}
    ])).unwrap();
    e.request("previewBatch", json!({"project":empty})).unwrap();
    let failed = wait(&e);
    assert_eq!(failed["job"]["status"], "failed");
    assert!(!failed["job"]["previews"].as_array().unwrap().is_empty());
    assert!(failed["job"]["previews"]
        .as_array()
        .unwrap()
        .iter()
        .all(|x| x["status"] == "failed"));
    // A failed cleanup leaves a recoverable marker. A live process retains its
    // files; a subsequent process can recover them without a 60-second delay.
    let cache = arkcut_core::storage::project_dir(&q.project_id).unwrap();
    let sentinel = cache.join("readonly-test.bin");
    std::fs::write(&sentinel, b"generated cache").unwrap();
    use std::os::windows::fs::OpenOptionsExt;
    let locked = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&sentinel)
        .unwrap();
    assert!(arkcut_core::storage::clear(&q.project_id, &[]).is_err());
    arkcut_core::storage::defer_cleanup(&q.project_id).unwrap();
    arkcut_core::storage::recover().unwrap();
    assert!(sentinel.exists());
    drop(locked);
    std::fs::write(cache.join(".session.json"), b"{\"pid\":4294967295}").unwrap();
    arkcut_core::storage::recover().unwrap();
    assert!(!cache.exists());
    assert!(path.exists());
    // Corrupt settings get a recoverable backup, future project versions stay intact.
    std::fs::write(data.join("settings.json"), b"{bad").unwrap();
    let settings = e.request("settings", json!({})).unwrap();
    assert!(settings["warning"].as_str().unwrap().contains("配置损坏"));
    assert!(std::fs::read_dir(&data).unwrap().any(|x| x
        .unwrap()
        .file_name()
        .to_string_lossy()
        .contains("corrupt-")));
}
