#![cfg(all(windows, feature = "resource-test-hooks"))]
//! Explicit acceptance harness, not part of ordinary tests. Uses real Engine
//! cancellation and OS process handles; terminating this harness is never a pass.
use arkcut_core::{
    model::{Music, Project},
    process::{args, Runner},
    resource::{Summary, GIB, MIB},
    Engine,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

fn state(engine: &Engine) -> Value {
    engine.request("status", json!({"after":0})).unwrap()
}
fn busy(value: &Value) -> bool {
    matches!(
        value["job"]["status"].as_str(),
        Some("running" | "cancelling")
    )
}
fn wait(engine: &Engine, seconds: u64) -> Value {
    let start = Instant::now();
    loop {
        let s = state(engine);
        if !busy(&s) {
            return s;
        }
        if start.elapsed() > Duration::from_secs(seconds) {
            let _ = engine.request("cancel", json!({}));
            panic!("Task timeout; not cancellation evidence: {s}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn hook(path: &Path, value: Value) {
    let next = path.with_extension("next");
    fs::write(&next, serde_json::to_vec(&value).unwrap()).unwrap();
    fs::rename(&next, path).unwrap();
}
fn save(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}
fn file_hash(path: &Path) -> String {
    let mut file = fs::File::open(path).unwrap();
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    format!("{:x}", hash.finalize())
}
fn keep_all(project: &Project) -> Project {
    let mut p = project.clone();
    p.project_id = uuid::Uuid::new_v4().to_string();
    p.pauses.clear();
    p.music.clear();
    p.mix.tails = false;
    p.mix.normalize = false;
    p.mix.current_track = None;
    p.export.quality = "high".into();
    p
}
fn disk_reservation_arithmetic(out: &Path) -> Value {
    let folder = out.join("disk-reservation-arithmetic");
    fs::create_dir(&folder).unwrap();
    let first = folder.join("first.tmp");
    let second = folder.join("second.tmp");
    let controller = arkcut_core::resource::ResourceController::new(Arc::new(
        std::sync::atomic::AtomicBool::new(false),
    ));
    let a = controller
        .reserve_disk(&first, 8 * MIB, "first registration")
        .unwrap();
    let b = controller
        .reserve_disk(&first, 4 * MIB, "duplicate same path")
        .unwrap();
    let c = controller
        .reserve_disk(&second, 2 * MIB, "distinct same-volume path")
        .unwrap();
    let duplicate = controller.summary();
    assert_eq!(duplicate.sample.disks.len(), 1);
    assert_eq!(duplicate.sample.disks[0].remaining, 10 * MIB);
    fs::write(&first, [0u8; 4096]).unwrap();
    controller.refresh(false);
    let growth = controller.summary();
    assert_eq!(growth.sample.disks[0].remaining, 10 * MIB - 4096);
    drop(a);
    controller.refresh(false);
    let smaller = controller.summary();
    assert_eq!(smaller.sample.disks[0].remaining, 6 * MIB - 4096);
    drop((b, c));
    controller.refresh(false);
    let released = controller.summary();
    assert!(released.sample.disks.is_empty());
    // Only this test-created 4 KiB file is removed. No directories are cleaned.
    fs::remove_file(&first).unwrap();
    let row = json!({"status":"passed","method":"Native disk counters, duplicate same-path reservation uses max rather than sum; distinct paths on same volume aggregate; growth is subtracted; RAII releases all registrations. No capacity override.","duplicate":duplicate,"growth":growth,"smallerAfterDrop":smaller,"released":released});
    save(&folder.join("evidence.json"), &row);
    row
}

// Inject only after the actual export phase and a live contained subprocess are
// observed. Failure, not a manual cancel or watchdog exit, is the success path.
fn fail_during_export(
    engine: &Engine,
    source: &Project,
    retry: &Project,
    hook_path: &Path,
    name: &str,
    override_value: Value,
    prefix: &str,
    out: &Path,
) -> Value {
    hook(hook_path, json!({}));
    let folder = out.join(name);
    fs::create_dir(&folder).unwrap();
    let output_dir = folder.join("output");
    fs::create_dir(&output_dir).unwrap();
    let target = output_dir.join("failed-export.mp4");
    // A real, pre-existing media file stands in for a user's existing output.
    let sentinel = output_dir.join(format!(
        "existing-output.{}",
        Path::new(&retry.source.fingerprint.path)
            .extension()
            .and_then(|x| x.to_str())
            .unwrap_or("mp4")
    ));
    fs::copy(&retry.source.fingerprint.path, &sentinel).unwrap();
    let sentinel_hash = file_hash(&sentinel);
    let project = keep_all(source);
    let retry_project = keep_all(retry);
    let saved_project = folder.join("before-fault.arkcut.json");
    let saved_retry = folder.join("retry.arkcut.json");
    for (p, file) in [(&project, &saved_project), (&retry_project, &saved_retry)] {
        engine
            .request("save", json!({"project":p,"path":file}))
            .unwrap();
    }
    let project_hash = file_hash(&saved_project);
    let retry_hash = file_hash(&saved_retry);
    let evidence_file = folder.join("evidence.json");
    engine
        .request(
            "export",
            json!({"project":project,"path":target,"formatChangeAccepted":true}),
        )
        .unwrap();
    let began = Instant::now();
    let mut phases = HashSet::new();
    let (before, handles, pids) = loop {
        let s = state(engine);
        phases.insert(s["job"]["phase"].as_str().unwrap_or("").to_string());
        if !busy(&s) {
            let row = json!({"name":name,"triggered":false,"reason":"exportRender with a live child was not observed before the job ended","observedPhases":phases,"finalState":s});
            save(&evidence_file, &row);
            return row;
        }
        if matches!(
            s["job"]["phase"].as_str(),
            Some("exportRender" | "smartCopy" | "smartBoundary" | "smartAudio")
        ) {
            let pids = native::descendants(&[std::process::id()]);
            let handles = native::hold(&pids);
            if handles.iter().any(|p| !p.exited()) {
                break (s, handles, pids);
            }
        }
        if began.elapsed() > Duration::from_secs(180) {
            engine.request("cancel", json!({})).unwrap();
            let final_state = wait(engine, 15);
            let row = json!({"name":name,"triggered":false,"reason":"Wait for exportRender timed out; cleanup cancel is not fault acceptance","observedPhases":phases,"finalState":final_state});
            save(&evidence_file, &row);
            return row;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let inject = Instant::now();
    hook(hook_path, override_value.clone());
    let mut row = json!({"name":name,"simulation":true,"triggered":true,"status":"injected","hook":override_value,"phaseAtInjection":before["job"]["phase"],"jobId":before["job"]["jobId"],"ownedPids":handles.iter().map(|p|p.pid).collect::<Vec<_>>(),"expectedErrorPrefix":prefix,"target":target,"savedProject":saved_project,"sentinel":sentinel});
    save(&evidence_file, &row);
    let failed = wait(engine, 20);
    let failure_ms = inject.elapsed().as_millis();
    let seeds = std::iter::once(std::process::id())
        .chain(pids.iter().copied())
        .collect::<Vec<_>>();
    let remaining = native::descendants(&seeds);
    let captured_exited = handles.iter().all(|p| p.exited());
    let output_files: Vec<_> = fs::read_dir(&output_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    row["failureMs"] = json!(failure_ms);
    row["failedState"] = failed.clone();
    row["allCapturedProcessesExited"] = json!(captured_exited);
    row["remainingDescendants"] = json!(remaining);
    row["outputFilesAfterFailure"] = json!(output_files);
    row["noPartialFinalFile"] = json!(output_files.len() == 1 && output_files[0] == sentinel);
    save(&evidence_file, &row);
    // Restore the counters before opening a saved project or retrying. A new job
    // must get a fresh controller; the old RESOURCE_* error must not poison it.
    hook(hook_path, json!({}));
    assert_eq!(
        failed["job"]["status"], "failed",
        "Resource stop must not be classified as cancelled: {row}"
    );
    assert!(
        failed["job"]["error"]
            .as_str()
            .unwrap_or("")
            .starts_with(prefix),
        "{row}"
    );
    assert_eq!(failed["job"]["resource"]["simulation"], true);
    assert!(
        captured_exited && remaining.is_empty(),
        "Resource stop left a subprocess: {row}"
    );
    assert_eq!(
        row["noPartialFinalFile"], true,
        "A partial media file or lease survived in the owned output directory: {row}"
    );
    assert_eq!(
        file_hash(&sentinel),
        sentinel_hash,
        "Existing media was changed"
    );
    assert_eq!(
        file_hash(&saved_project),
        project_hash,
        "Saved project was changed"
    );
    assert_eq!(
        file_hash(&saved_retry),
        retry_hash,
        "Saved retry project was changed"
    );
    let restored = engine
        .request("open", json!({"path":saved_project}))
        .unwrap();
    assert_eq!(restored["project"], serde_json::to_value(&project).unwrap());
    assert_eq!(restored["warnings"], json!([]));
    let restored_retry = engine.request("open", json!({"path":saved_retry})).unwrap();
    assert_eq!(
        restored_retry["project"],
        serde_json::to_value(&retry_project).unwrap()
    );
    assert_eq!(restored_retry["warnings"], json!([]));
    let retry_started = Instant::now();
    engine
        .request(
            "preview",
            json!({"project":restored_retry["project"],"full":true}),
        )
        .unwrap();
    let preview = wait(engine, 180);
    assert_eq!(
        preview["job"]["status"], "completed",
        "Retry preview failed: {preview}"
    );
    assert_eq!(
        preview["job"]["result"]["cached"], false,
        "Retry must execute the pipeline, not only hit cache"
    );
    let retry_output = folder.join("retry-export.mp4");
    engine.request("export", json!({"project":restored_retry["project"],"path":retry_output,"formatChangeAccepted":true})).unwrap();
    let exported = wait(engine, 180);
    assert_eq!(
        exported["job"]["status"], "completed",
        "Retry export failed: {exported}"
    );
    assert!(Path::new(exported["job"]["result"]["path"].as_str().unwrap()).is_file());
    assert!(native::descendants(&[std::process::id()]).is_empty());
    assert_eq!(file_hash(&sentinel), sentinel_hash);
    assert_eq!(file_hash(&saved_project), project_hash);
    row["status"] = json!("passed");
    row["sameEngineProjectReopened"] = json!(true);
    row["sentinelUnchanged"] = json!(true);
    row["savedProjectUnchanged"] = json!(true);
    row["retry"] = json!({"status":"passed","source":retry.source.fingerprint.path,"durationUs":retry.source.duration_us,"sameEngine":true,"elapsedMs":retry_started.elapsed().as_millis(),"preview":preview["job"],"export":exported["job"]});
    save(&evidence_file, &row);
    row
}

mod native {
    use super::*;
    use std::{
        ffi::c_void,
        mem::{size_of, zeroed},
    };
    type Handle = *mut c_void;
    #[repr(C)]
    struct Entry {
        size: u32,
        usage: u32,
        pid: u32,
        heap: usize,
        module: u32,
        threads: u32,
        parent: u32,
        priority: i32,
        flags: u32,
        name: [u16; 260],
    }
    #[repr(C)]
    struct Memory {
        cb: u32,
        faults: u32,
        peak_ws: usize,
        ws: usize,
        peak_paged: usize,
        paged: usize,
        peak_nonpaged: usize,
        nonpaged: usize,
        pagefile: usize,
        peak_pagefile: usize,
        private: usize,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> Handle;
        fn Process32FirstW(h: Handle, p: *mut Entry) -> i32;
        fn Process32NextW(h: Handle, p: *mut Entry) -> i32;
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
        fn CloseHandle(h: Handle) -> i32;
        fn WaitForSingleObject(h: Handle, millis: u32) -> u32;
        fn GetProcessHandleCount(h: Handle, count: *mut u32) -> i32;
    }
    #[link(name = "psapi")]
    extern "system" {
        fn GetProcessMemoryInfo(h: Handle, p: *mut Memory, size: u32) -> i32;
    }
    pub struct Process {
        pub pid: u32,
        handle: usize,
    }
    impl Drop for Process {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.handle as Handle);
            }
        }
    }
    impl Process {
        pub fn exited(&self) -> bool {
            unsafe { WaitForSingleObject(self.handle as Handle, 0) == 0 }
        }
    }
    static BASELINE_CONSOLES: std::sync::OnceLock<Vec<Process>> = std::sync::OnceLock::new();
    pub fn capture_baseline_consoles() -> Value {
        // This console test runner has a conhost for its whole lifetime even
        // under CREATE_NO_WINDOW. Hold its exact process handle before any
        // media work, so it cannot be confused with job children or a reused PID.
        let pids = unsafe {
            let h = CreateToolhelp32Snapshot(2, 0);
            assert_ne!(h as isize, -1);
            let mut e: Entry = zeroed();
            e.size = size_of::<Entry>() as u32;
            let mut ok = Process32FirstW(h, &mut e);
            let mut pids = Vec::new();
            while ok != 0 {
                let end = e.name.iter().position(|&x| x == 0).unwrap_or(e.name.len());
                let name = String::from_utf16_lossy(&e.name[..end]);
                if e.parent == std::process::id() && name.eq_ignore_ascii_case("conhost.exe") {
                    pids.push(e.pid);
                }
                ok = Process32NextW(h, &mut e);
            }
            CloseHandle(h);
            pids
        };
        assert!(BASELINE_CONSOLES.set(hold(&pids)).is_ok());
        json!({"method":"Only pre-existing direct conhost children, identified before media jobs and held by native handles, are excluded from job-cleanup counts; all later media children and console hosts remain included.","pids":pids})
    }
    pub fn descendants(seeds: &[u32]) -> Vec<u32> {
        unsafe {
            let h = CreateToolhelp32Snapshot(2, 0);
            assert_ne!(h as isize, -1);
            let mut e: Entry = zeroed();
            e.size = size_of::<Entry>() as u32;
            let mut ok = Process32FirstW(h, &mut e);
            let mut tree = HashMap::new();
            while ok != 0 {
                tree.insert(e.pid, e.parent);
                ok = Process32NextW(h, &mut e);
            }
            CloseHandle(h);
            let mut set: HashSet<u32> = seeds.iter().copied().collect();
            loop {
                let before = set.len();
                for (&pid, &parent) in &tree {
                    if set.contains(&parent) {
                        set.insert(pid);
                    }
                }
                if set.len() == before {
                    break;
                }
            }
            set.into_iter()
                .filter(|pid| {
                    *pid != std::process::id()
                        && tree.contains_key(pid)
                        && !BASELINE_CONSOLES
                            .get()
                            .into_iter()
                            .flatten()
                            .any(|p| p.pid == *pid && !p.exited())
                })
                .collect()
        }
    }
    pub fn hold(pids: &[u32]) -> Vec<Process> {
        pids.iter()
            .filter_map(|&pid| {
                let h = unsafe { OpenProcess(0x100000 | 0x1000, 0, pid) };
                (!h.is_null()).then_some(Process {
                    pid,
                    handle: h as usize,
                })
            })
            .collect()
    }
    pub fn own_counters() -> Value {
        unsafe {
            let h = OpenProcess(0x1000 | 0x10, 0, std::process::id());
            assert!(!h.is_null());
            let mut m: Memory = zeroed();
            m.cb = size_of::<Memory>() as u32;
            let mut count = 0;
            let memory_ok = GetProcessMemoryInfo(h, &mut m, size_of::<Memory>() as u32) != 0;
            let handles_ok = GetProcessHandleCount(h, &mut count) != 0;
            CloseHandle(h);
            json!({"workingSet":if memory_ok{Some(m.ws as u64)}else{None},"privateCommit":if memory_ok{Some(m.private as u64)}else{None},"handleCount":if handles_ok{Some(count)}else{None}})
        }
    }
}

fn directory_bytes(path: &Path) -> u64 {
    fs::read_dir(path)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| {
            let Ok(kind) = e.file_type() else {
                return 0;
            };
            if kind.is_symlink() {
                0
            } else if kind.is_dir() {
                directory_bytes(&e.path())
            } else {
                e.metadata().map(|m| m.len()).unwrap_or(0)
            }
        })
        .sum()
}
fn lifecycle_cycles(engine: &Engine, source: &Path, out: &Path) -> Value {
    let initial = native::own_counters();
    let mut cycles = Vec::new();
    for number in 1..=3 {
        let start = Instant::now();
        engine.request("import", json!({"path":source})).unwrap();
        let imported = wait(engine, 180);
        assert_eq!(imported["job"]["status"], "completed", "{imported}");
        let mut p: Project = serde_json::from_value(imported["job"]["result"].clone()).unwrap();
        if p.source.duration_us > 10_000_000 {
            return json!({"status":"notRun","reason":"资源对象循环需要≤10秒短片；长片请传--cycle-source，避免用短循环冒充长片验收","source":source});
        }
        // Lifecycle pressure is intentionally a keep-all short clip; cut/quality
        // acceptance has its separate exact-PTS/visual/audio matrix.
        p.pauses.clear();
        p.mix.tails = false;
        p.mix.normalize = false;
        engine
            .request("preview", json!({"project":p,"full":true}))
            .unwrap();
        let preview = wait(engine, 180);
        assert_eq!(preview["job"]["status"], "completed", "{preview}");
        let target = out.join(format!("cycle-{number}.mp4"));
        engine
            .request(
                "export",
                json!({"project":p,"path":target,"formatChangeAccepted":true}),
            )
            .unwrap();
        let export = wait(engine, 180);
        assert_eq!(export["job"]["status"], "completed", "{export}");
        std::thread::sleep(Duration::from_millis(500));
        let idle = native::own_counters();
        let remaining = native::descendants(&[std::process::id()]);
        assert!(
            remaining.is_empty(),
            "Lifecycle subprocess survived job completion"
        );
        let row = json!({"cycle":number,"elapsedMs":start.elapsed().as_millis(),"idleAfterMs":500,"idle":idle,"cacheBytes":directory_bytes(&arkcut_core::storage::cache_root()),"remainingDescendants":remaining,"importResource":imported["job"]["resource"],"previewResource":preview["job"]["resource"],"exportResource":export["job"]["resource"],"previewPath":preview["job"]["result"]["path"],"exportPath":export["job"]["result"]["path"]});
        save(&out.join(format!("cycle-{number}.json")), &row);
        cycles.push(row);
    }
    json!({"status":"passed","scope":"Same Engine, three import-preview-export cycles, native process/handle snapshots 500ms after each completion. Not GUI open-close or long-duration low-end acceptance.","initialIdle":initial,"source":source,"cycles":cycles})
}

fn cancel_at(engine: &Engine, op: &str, payload: Value, phase: &str, out: &Path) -> Value {
    let started = Instant::now();
    engine.request(op, payload).unwrap();
    let mut phases = HashSet::new();
    loop {
        let before = state(engine);
        let actual = before["job"]["phase"].as_str().unwrap_or("");
        phases.insert(actual.to_string());
        if !busy(&before) {
            let row = json!({"targetPhase":phase,"triggered":false,"reason":"阶段未出现或未观察到存活媒体子进程","observedPhases":phases,"finalStatus":before["job"]["status"],"error":before["job"]["error"],"elapsedMs":started.elapsed().as_millis()});
            save(out, &json!({"case":row,"state":before}));
            return row;
        }
        if actual == phase {
            let pids = native::descendants(&[std::process::id()]);
            let handles = native::hold(&pids);
            if handles.iter().any(|p| !p.exited()) {
                let click = Instant::now();
                engine.request("cancel", json!({})).unwrap();
                let ack = state(engine);
                let ack_ms = click.elapsed().as_millis();
                let final_state = wait(engine, 15);
                let elapsed = click.elapsed().as_millis();
                let seeds = std::iter::once(std::process::id())
                    .chain(pids.iter().copied())
                    .collect::<Vec<_>>();
                let remaining = native::descendants(&seeds);
                let all_exited = handles.iter().all(|p| p.exited());
                let row = json!({"targetPhase":phase,"triggered":true,"phaseAtRequest":actual,"ackStatus":ack["job"]["status"],"ackMs":ack_ms,"cancelMs":elapsed,"ownedPids":handles.iter().map(|p|p.pid).collect::<Vec<_>>(),"allCapturedProcessesExited":all_exited,"remainingDescendants":remaining,"finalStatus":final_state["job"]["status"],"error":final_state["job"]["error"],"resource":final_state["job"]["resource"]});
                save(out, &json!({"case":row,"state":final_state}));
                assert!(
                    ack_ms < 1000,
                    "Backend cancel acknowledgement exceeded 1 second: {row}"
                );
                assert!(
                    matches!(
                        ack["job"]["status"].as_str(),
                        Some("cancelling" | "cancelled")
                    ),
                    "{row}"
                );
                assert_eq!(final_state["job"]["status"], "cancelled", "{row}");
                assert!(elapsed < 5000, "{row}");
                assert!(
                    all_exited && remaining.is_empty(),
                    "Owned subprocess survived cancellation: {row}"
                );
                return row;
            }
        }
        if started.elapsed() > Duration::from_secs(180) {
            engine.request("cancel", json!({})).unwrap();
            let end = wait(engine, 15);
            let row = json!({"targetPhase":phase,"triggered":false,"reason":"等候目标阶段超时；测试清理取消不计入该阶段验收","observedPhases":phases,"finalStatus":end["job"]["status"]});
            save(out, &json!({"case":row,"state":end}));
            return row;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn pressure_cycle(hook_path: &Path, out: &Path) -> Value {
    hook(hook_path, json!({}));
    let runner = Runner::discover(Arc::new(|_, _| {})).unwrap();
    runner
        .prepare_stage("resourcePressure", 16 * MIB, 2 * MIB)
        .unwrap();
    runner.measure(
        "resourcePressure",
        0,
        None,
        "state",
        "Development resource-pressure acceptance",
    );
    let samples = Arc::new(Mutex::new(Vec::<Summary>::new()));
    let copy = samples.clone();
    let mut monitor = runner.resource().monitor(Arc::new(move |summary| {
        let mut all = copy.lock().unwrap();
        if all.len() < 120 {
            all.push(summary);
        }
    }));
    let initial = runner.resource().summary();
    let memory = initial.sample.memory.as_ref().unwrap();
    let physical_low = (GIB.max(memory.total / 5)).saturating_sub(64 * MIB);
    assert!(physical_low > (512 * MIB).max(memory.total * 8 / 100));
    // Real, contained ffmpeg with realtime 32x32 input keeps this test alive without
    // consuming significant CPU/RAM. It does not validate real-video performance.
    let worker = runner.clone();
    let child = std::thread::spawn(move || {
        worker.ff(args(&[
            "-hide_banner",
            "-v",
            "error",
            "-re",
            "-f",
            "lavfi",
            "-i",
            "color=size=32x32:rate=2",
            "-t",
            "24",
            "-f",
            "null",
            "-",
        ]))
    });
    let start = Instant::now();
    while native::descendants(&[std::process::id()]).is_empty() {
        assert!(start.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(10));
    }
    hook(hook_path, json!({"availableBytes":physical_low}));
    let pressure = loop {
        let s = runner.resource().summary();
        if s.pressure == "pressure" {
            break s;
        }
        assert!(
            s.failure.is_none(),
            "Unexpected critical stop: {}",
            serde_json::to_string(&s).unwrap()
        );
        assert!(start.elapsed() < Duration::from_secs(8));
        std::thread::sleep(Duration::from_millis(20));
    };
    let pressure_at = start.elapsed().as_millis();
    hook(hook_path, json!({}));
    let recovery_started = Instant::now();
    while recovery_started.elapsed() < Duration::from_secs(14) {
        let s = runner.resource().summary();
        assert_ne!(
            s.pressure, "normal",
            "Recovery occurred before 15 complete healthy samples"
        );
        assert!(s.failure.is_none());
        std::thread::sleep(Duration::from_millis(100));
    }
    let media = child.join().unwrap();
    assert!(media.is_ok(), "{media:?}");
    let recovered = loop {
        let s = runner.resource().summary();
        if s.pressure == "normal" {
            break s;
        }
        assert!(s.failure.is_none());
        assert!(start.elapsed() < Duration::from_secs(35));
        std::thread::sleep(Duration::from_millis(100));
    };
    let recovered_after_ms = recovery_started.elapsed().as_millis();
    monitor.stop();
    hook(hook_path, json!({}));
    let row = json!({"simulation":true,"media":"real contained ffmpeg realtime 32x32 lavfi, 24 seconds","initialThreads":initial.threads,"pressureThreads":pressure.threads,"pressureObservedAtMs":pressure_at,"recoveredThreads":recovered.threads,"recoveredAfterMs":recovered_after_ms,"samples":samples.lock().unwrap().clone(),"remainingDescendants":native::descendants(&[std::process::id()])});
    save(out, &row);
    assert!(pressure.threads <= initial.threads);
    assert!(recovered_after_ms >= 15000);
    assert!(row["remainingDescendants"].as_array().unwrap().is_empty());
    row
}

#[test]
#[ignore = "Explicit resource acceptance; requires owned output/source environment and resource-test-hooks"]
fn resource_runtime_acceptance() {
    let source =
        PathBuf::from(std::env::var_os("ARKCUT_RESOURCE_RUNTIME_SOURCE").expect("runtime source"));
    let out = PathBuf::from(
        std::env::var_os("ARKCUT_RESOURCE_RUNTIME_OUT").expect("owned runtime output"),
    );
    fs::create_dir_all(&out).unwrap();
    let data = out.join("appdata");
    fs::create_dir_all(&data).unwrap();
    std::env::set_var("ARKCUT_DATA_DIR", &data);
    let hook_path = data.join(".arkcut-resource-test.json");
    std::env::set_var("ARKCUT_RESOURCE_TEST_FILE", &hook_path);
    hook(&hook_path, json!({}));
    let cycle_source = std::env::var_os("ARKCUT_RESOURCE_RUNTIME_CYCLE_SOURCE")
        .map(PathBuf::from)
        .unwrap_or_else(|| source.clone());
    let source_hash = file_hash(&source);
    let cycle_hash = file_hash(&cycle_source);
    let mut evidence = json!({"method":"Real Engine cancel requests and native process handles. Pressure uses downward-only test hooks; no memory exhaustion.","source":source,"sourceSha256Before":source_hash,"cycleSource":cycle_source,"cycleSourceSha256Before":cycle_hash,"cancellation":[],"pressure":null,"runningFaults":null,"lifecycle":null,"status":"running"});
    evidence["harnessBaseline"] = native::capture_baseline_consoles();
    save(&out.join("evidence.json"), &evidence);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        evidence["diskReservationArithmetic"] = disk_reservation_arithmetic(&out);
        evidence["pressure"] = pressure_cycle(&hook_path, &out.join("pressure.json"));
        save(&out.join("evidence.json"), &evidence);
        let engine = Engine::default();
        let row = cancel_at(
            &engine,
            "probe",
            json!({"path":source}),
            "frameIndex",
            &out.join("cancel-frameIndex.json"),
        );
        evidence["cancellation"].as_array_mut().unwrap().push(row);
        engine.request("probe", json!({"path":source})).unwrap();
        let probed = wait(&engine, 180);
        assert_eq!(probed["job"]["status"], "completed", "{probed}");
        let project: Project = serde_json::from_value(probed["job"]["result"].clone()).unwrap();
        for phase in [
            "pauseScan",
            "audioBridge",
            "loudness",
            "previewRender",
            "smartIndex",
            "smartCopy",
            "smartBoundary",
            "smartAudio",
            "outputValidate",
        ] {
            let mut p = project.clone();
            p.project_id = uuid::Uuid::new_v4().to_string();
            p.mix.tails = false;
            p.mix.normalize = false;
            if phase.starts_with("smart") && p.source.duration_us > 3_000_000 {
                p.pauses=serde_json::from_value(json!([{"id":"smart-runtime-cut","startUs":1_000_000,"endUs":2_135_000,"enabled":true,"source":"manual","state":"confirmed","score":1}])).unwrap();
            }
            if phase == "outputValidate" && p.source.duration_us > 3_000_000 {
                // Exercise the new continuous decode + automatic-cut audit,
                // not just the no-cut validation fallback.
                p.pauses=serde_json::from_value(json!([{"id":"continuous-audit-cut","startUs":1_000_000,"endUs":2_135_000,"enabled":true,"source":"auto","state":"confirmed","score":1}])).unwrap();
            }
            let op = match phase {
                "pauseScan" => "analyze",
                "audioBridge" | "loudness" => "prepare",
                "previewRender" => "preview",
                _ => "export",
            };
            if phase == "audioBridge" && p.source.duration_us > 2_000_000 {
                p.mix.tails = true;
                p.pauses=serde_json::from_value(json!([{"id":"runtime-cut","startUs":1_000_000,"endUs":2_000_000,"enabled":true,"source":"manual","state":"confirmed","score":1}])).unwrap();
            }
            if phase == "loudness" {
                p.mix.normalize = true;
                p.music.push(Music {
                    id: "runtime-music".into(),
                    media: p.source.clone(),
                });
            }
            let target = out.join(format!("cancel-{phase}.mp4"));
            let sentinel = out.join(format!("sentinel-{phase}.mp4"));
            fs::write(&sentinel, b"user-owned sentinel").unwrap();
            let row = cancel_at(
                &engine,
                op,
                json!({"project":p,"path":target,"full":true,"formatChangeAccepted":true}),
                phase,
                &out.join(format!("cancel-{phase}.json")),
            );
            assert_eq!(fs::read(&sentinel).unwrap(), b"user-owned sentinel");
            if row["triggered"] == true {
                assert!(!target.exists(), "Cancelled task committed output");
            }
            evidence["cancellation"].as_array_mut().unwrap().push(row);
            save(&out.join("evidence.json"), &evidence);
        }
        assert!(
            evidence["cancellation"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["triggered"] == true),
            "No cancellation phase was actually triggered"
        );
        engine
            .request("probe", json!({"path":cycle_source}))
            .unwrap();
        let retry_probe = wait(&engine, 180);
        assert_eq!(retry_probe["job"]["status"], "completed", "{retry_probe}");
        let retry: Project = serde_json::from_value(retry_probe["job"]["result"].clone()).unwrap();
        if retry.source.duration_us <= 10_000_000 {
            evidence["runningFaults"] = json!({"status":"running","cases":[]});
            for (name, value, prefix) in [
                (
                    "running-physical-critical",
                    json!({"availableBytes":1}),
                    "RESOURCE_MEMORY",
                ),
                (
                    "running-commit-critical",
                    json!({"commitAvailableBytes":1}),
                    "RESOURCE_MEMORY",
                ),
                (
                    "running-disk-critical",
                    json!({"diskAvailableBytes":1}),
                    "RESOURCE_DISK",
                ),
            ] {
                let row = fail_during_export(
                    &engine, &project, &retry, &hook_path, name, value, prefix, &out,
                );
                evidence["runningFaults"]["cases"]
                    .as_array_mut()
                    .unwrap()
                    .push(row);
                save(&out.join("evidence.json"), &evidence);
            }
            let all = evidence["runningFaults"]["cases"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r["triggered"] == true && r["status"] == "passed");
            evidence["runningFaults"]["status"] = json!(if all { "passed" } else { "partial" });
        } else {
            evidence["runningFaults"] = json!({"status":"notRun","reason":"Provide --cycle-source of at most 10 seconds for post-fault preview/export retry; no long-video retry is silently substituted"});
        }
        evidence["lifecycle"] = lifecycle_cycles(&engine, &cycle_source, &out);
    }));
    hook(&hook_path, json!({}));
    let final_source_hash = file_hash(&source);
    let final_cycle_hash = file_hash(&cycle_source);
    evidence["sourceSha256After"] = json!(final_source_hash);
    evidence["cycleSourceSha256After"] = json!(final_cycle_hash);
    evidence["inputsUnchanged"] =
        json!(source_hash == final_source_hash && cycle_hash == final_cycle_hash);
    evidence["status"] = json!(if result.is_ok()
        && source_hash == final_source_hash
        && cycle_hash == final_cycle_hash
    {
        "passed"
    } else {
        "failed"
    });
    save(&out.join("evidence.json"), &evidence);
    assert!(
        source_hash == final_source_hash && cycle_hash == final_cycle_hash,
        "Original media changed"
    );
    if let Err(error) = result {
        std::panic::resume_unwind(error);
    }
}
