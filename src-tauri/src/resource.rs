//! Task-wide resource policy. Measurements are native Windows counters, not
//! working-set estimates masquerading as system commit availability.
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

pub const MIB: u64 = 1024 * 1024;
pub const GIB: u64 = 1024 * MIB;
pub const MAX_MEDIA_THREADS: usize = 16;
#[derive(Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Memory {
    pub total: u64,
    pub available: u64,
    pub commit_available: u64,
}
#[derive(Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Sample {
    pub memory: Option<Memory>,
    pub working_set: u64,
    pub private_commit: u64,
    pub cpu_percent: f64,
    /// Duration of the last complete CPU interval; zero means not yet measured.
    pub cpu_window_ms: u64,
    pub process_count: usize,
    pub reliable: bool,
    pub disks: Vec<DiskSample>,
    pub elapsed_ms: u64,
}
#[derive(Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskSample {
    pub volume: String,
    pub available: u64,
    pub total: u64,
    pub reserve: u64,
    pub remaining: u64,
}
#[derive(Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StagePeak {
    pub phase: String,
    pub budget_bytes: u64,
    pub physical_budget_bytes: u64,
    pub commit_budget_bytes: u64,
    pub estimated_bytes: u64,
    pub threads: usize,
    pub working_set: u64,
    pub private_commit: u64,
    pub cpu_percent: f64,
}
#[derive(Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub sampling_micros: u64,
    pub budget_micros: u64,
    pub diagnostic_micros: u64,
    pub budget_retry_wait_micros: u64,
    pub budget_decisions: Vec<serde_json::Value>,
    pub notice: String,
    pub pressure: String,
    pub threads: usize,
    pub logical_processors: usize,
    pub sample: Sample,
    pub peak_working_set: u64,
    pub peak_private_commit: u64,
    pub peak_cpu_percent: f64,
    pub minimum_available: Option<u64>,
    pub minimum_commit_available: Option<u64>,
    pub disk_minimum_available: BTreeMap<String, u64>,
    pub samples: u64,
    pub telemetry_failures: u64,
    pub policy_changes: u64,
    pub stages: Vec<StagePeak>,
    pub failure: Option<String>,
    pub cancellation_ms: Option<u64>,
    pub cancellation_overdue: bool,
    pub simulation: bool,
}
struct DiskNeed {
    path: PathBuf,
    volume: PathBuf,
    expected: u64,
    initial: u64,
}
struct State {
    summary: Summary,
    stage: String,
    stage_baseline: u64,
    stage_baseline_ws: u64,
    stage_budget: u64,
    stage_physical_budget: u64,
    stage_commit_budget: u64,
    stage_estimated: u64,
    memory_reserved: u64,
    memory_items: BTreeMap<u64, (String, u64)>,
    next_id: u64,
    disks: BTreeMap<u64, DiskNeed>,
    pressure_samples: u32,
    healthy_samples: u32,
    healthy_since: Option<Instant>,
    pressure_since: Option<Instant>,
}
pub struct ResourceController {
    cancel: Arc<AtomicBool>,
    state: Mutex<State>,
    sampler: Mutex<Sampler>,
    cap: AtomicUsize,
    maximum: usize,
    active: AtomicUsize,
    started: Instant,
}
pub struct MemoryReservation {
    controller: Arc<ResourceController>,
    bytes: u64,
    id: u64,
}
impl Drop for MemoryReservation {
    fn drop(&mut self) {
        let mut s = self.controller.state.lock().unwrap();
        s.memory_reserved = s.memory_reserved.saturating_sub(self.bytes);
        s.memory_items.remove(&self.id);
    }
}
pub struct DiskReservation {
    controller: Arc<ResourceController>,
    id: u64,
}
impl Drop for DiskReservation {
    fn drop(&mut self) {
        self.controller.state.lock().unwrap().disks.remove(&self.id);
    }
}
pub struct ProcessReservation {
    controller: Arc<ResourceController>,
}
impl Drop for ProcessReservation {
    fn drop(&mut self) {
        self.controller.active.fetch_sub(1, Ordering::Relaxed);
    }
}
pub struct Monitor {
    stop: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Monitor {
    pub fn stop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
    }
}
impl Drop for Monitor {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn physical_reserve(total: u64) -> u64 {
    GIB.max(total / 5)
}
pub fn critical_reserve(total: u64) -> u64 {
    (512 * MIB).max(total * 8 / 100)
}
pub fn disk_reserve(total: u64) -> u64 {
    (total / 100).clamp(GIB, 5 * GIB)
}
fn allowance(memory: &Memory) -> u64 {
    (memory
        .available
        .saturating_sub(physical_reserve(memory.total))
        * 7
        / 10)
        .min(memory.commit_available.saturating_sub(GIB))
}
// Reservations cover concrete in-process buffers, not encoder estimates.
// Use actual headroom above the emergency floor. The scheduling reserve (20%
// of RAM) can be crossed without preventing even a small pipe/index buffer.
fn allocation_allowance(memory: &Memory) -> u64 {
    memory.available.saturating_sub(critical_reserve(memory.total))
        .min(memory.commit_available.saturating_sub(512 * MIB))
}
fn fitted_threads(fixed: u64, per_thread: u64, reserved: u64, available: u64, ceiling: usize) -> Option<usize> {
    (1..=ceiling.max(1)).rev().find(|&t| {
        fixed.saturating_add(per_thread.saturating_mul(t as u64)).saturating_add(reserved) <= available
    })
}
fn cpu_ceiling(logical: usize) -> usize {
    // Leave at least one fifth of logical CPUs to the UI and other apps.
    (logical.saturating_mul(4) / 5).clamp(1, MAX_MEDIA_THREADS)
}
fn initial_threads(logical: usize, memory: Option<&Memory>) -> usize {
    let cpu = cpu_ceiling(logical);
    match memory {
        Some(m) if allowance(m) < 512 * MIB => cpu.min(2),
        Some(_) => cpu,
        None => 1,
    }
}
impl ResourceController {
    pub fn new(cancel: Arc<AtomicBool>) -> Arc<Self> {
        let logical = std::thread::available_parallelism()
            .map(|v| v.get())
            .unwrap_or(1);
        Arc::new(Self {
            cancel,
            state: Mutex::new(State {
                summary: Summary {
                    logical_processors: logical,
                    threads: 1,
                    pressure: "conservative".into(),
                    ..Summary::default()
                },
                stage: String::new(),
                stage_baseline: 0,
                stage_baseline_ws: 0,
                stage_budget: 0,
                stage_physical_budget: 0,
                stage_commit_budget: 0,
                stage_estimated: 0,
                memory_reserved: 0,
                memory_items: BTreeMap::new(),
                next_id: 0,
                disks: BTreeMap::new(),
                pressure_samples: 0,
                healthy_samples: 0,
                healthy_since: None,
                pressure_since: None,
            }),
            sampler: Mutex::new(Sampler::default()),
            cap: AtomicUsize::new(1),
            maximum: cpu_ceiling(logical),
            active: AtomicUsize::new(0),
            started: Instant::now(),
        })
    }
    pub fn threads(&self) -> usize {
        self.cap.load(Ordering::Relaxed).max(1)
    }
    pub fn summary(&self) -> Summary {
        self.state.lock().unwrap().summary.clone()
    }
    pub fn check(&self) -> Result<(), String> {
        if let Some(e) = self.state.lock().unwrap().summary.failure.clone() {
            Err(e)
        } else {
            Ok(())
        }
    }
    pub fn fail(&self, reason: String) {
        let mut s = self.state.lock().unwrap();
        if s.summary.failure.is_none() {
            s.summary.notice = reason
                .split_once(": ")
                .map(|(_, v)| v)
                .unwrap_or(&reason)
                .into();
            s.summary.failure = Some(reason);
            s.summary.pressure = "critical".into();
            s.summary.policy_changes += 1;
        }
        self.cancel.store(true, Ordering::Relaxed);
    }
    pub fn cancellation_elapsed(&self, millis: u64) {
        let mut s = self.state.lock().unwrap();
        s.summary.cancellation_ms = Some(millis);
        if s.summary.cancellation_overdue {
            s.summary.notice = s
                .summary
                .failure
                .as_ref()
                .map(|e| e.split_once(": ").map(|(_, v)| v).unwrap_or(e).to_string())
                .unwrap_or_else(|| "媒体进程已退出".into());
        }
    }
    pub fn cancellation_overdue(&self) {
        let mut s = self.state.lock().unwrap();
        if !s.summary.cancellation_overdue {
            s.summary.cancellation_overdue = true;
            s.summary.notice = "正在等待媒体进程退出，本次处理尚未结束".into();
            s.summary.policy_changes += 1;
        }
    }
    pub fn stage(&self, phase: &str) {
        let mut s = self.state.lock().unwrap();
        if s.stage != phase {
            s.stage = phase.chars().take(96).collect();
        }
    }
    /// Background ownership is explicit; stop joins the sampler before a job finishes.
    pub fn monitor(self: &Arc<Self>, callback: Arc<dyn Fn(Summary) + Send + Sync>) -> Monitor {
        let (tx, rx) = std::sync::mpsc::channel();
        let this = self.clone();
        let thread = std::thread::spawn(move || loop {
            this.refresh(true);
            let diagnostic_started = Instant::now();
            callback(this.summary());
            this.state.lock().unwrap().summary.diagnostic_micros +=
                diagnostic_started.elapsed().as_micros() as u64;
            if rx.recv_timeout(Duration::from_secs(1)).is_ok() {
                break;
            }
        });
        Monitor {
            stop: Some(tx),
            thread: Some(thread),
        }
    }
    pub fn refresh(&self, policy_tick: bool) {
        let started = Instant::now();
        let mut sample = self.sampler.lock().unwrap().sample();
        sample.elapsed_ms = self.started.elapsed().as_millis() as u64;
        let needs = {
            let s = self.state.lock().unwrap();
            s.disks
                .values()
                .map(|v| (v.path.clone(), v.volume.clone(), v.expected, v.initial))
                .collect::<Vec<_>>()
        };
        let disks = sample_disks(&needs);
        let disk_error = disks.as_ref().err().cloned();
        sample.disks = disks.unwrap_or_default();
        self.observe(sample, policy_tick, disk_error);
        self.state.lock().unwrap().summary.sampling_micros += started.elapsed().as_micros() as u64;
    }
    fn observe(&self, sample: Sample, policy_tick: bool, mut failure: Option<String>) {
        let mut s = self.state.lock().unwrap();
        #[cfg(feature = "resource-test-hooks")]
        {
            if let Some(hook) = test_hook() {
                s.summary.simulation = true;
                if let Some(logical) = hook.logical_processors {
                    s.summary.logical_processors = s.summary.logical_processors.min(logical.max(1));
                    self.cap
                        .fetch_min(logical.saturating_sub(1).max(1), Ordering::Relaxed);
                }
            }
        }
        let first = s.summary.samples == 0;
        s.summary.samples += 1;
        s.summary.peak_working_set = s.summary.peak_working_set.max(sample.working_set);
        s.summary.peak_private_commit = s.summary.peak_private_commit.max(sample.private_commit);
        s.summary.peak_cpu_percent = s.summary.peak_cpu_percent.max(sample.cpu_percent);
        for d in &sample.disks {
            let min = s
                .summary
                .disk_minimum_available
                .entry(d.volume.clone())
                .or_insert(d.available);
            *min = (*min).min(d.available);
            if d.available < d.reserve.saturating_add(d.remaining) {
                failure=Some(format!("RESOURCE_DISK: {} 可用空间不足；需约 {:.2} GB 并保留 {:.2} GB，请更换位置或清理可回收预览缓存后重试",d.volume,d.remaining as f64/GIB as f64,d.reserve as f64/GIB as f64));
            }
        }
        if let Some(m) = &sample.memory {
            s.summary.minimum_available = Some(
                s.summary
                    .minimum_available
                    .unwrap_or(m.available)
                    .min(m.available),
            );
            s.summary.minimum_commit_available = Some(
                s.summary
                    .minimum_commit_available
                    .unwrap_or(m.commit_available)
                    .min(m.commit_available),
            );
            if m.available < critical_reserve(m.total) || m.commit_available < 512 * MIB {
                failure = Some(
                    "RESOURCE_MEMORY: 可用内存不足，本次处理已停止。请关闭部分应用后重试".into(),
                );
            }
        }
        if s.summary.failure.is_some() {
            // Once protection has stopped this task, later missing telemetry
            // must not downgrade the reason to a generic low-load notice.
            if !sample.reliable {
                s.summary.telemetry_failures += 1;
            }
        } else if !sample.reliable {
            self.cap.store(1, Ordering::Relaxed);
            s.summary.telemetry_failures += 1;
            if s.summary.pressure != "conservative" {
                s.summary.policy_changes += 1;
            }
            s.summary.pressure = "conservative".into();
            s.summary.notice = "资源信息暂不可用，正在使用低负载模式".into();
            s.healthy_samples = 0;
            s.healthy_since = None;
        } else if first {
            self.cap.store(
                initial_threads(s.summary.logical_processors, sample.memory.as_ref()),
                Ordering::Relaxed,
            );
            s.summary.pressure = "normal".into();
            s.summary.notice.clear();
        } else if policy_tick {
            let low = sample
                .memory
                .as_ref()
                .is_some_and(|m| m.available < physical_reserve(m.total));
            let over = s.stage_budget > 0
                && (sample.working_set.saturating_sub(s.stage_baseline_ws)
                    > s.stage_physical_budget
                    || sample.private_commit.saturating_sub(s.stage_baseline)
                        > s.stage_commit_budget);
            if low || over {
                s.healthy_samples = 0;
                s.healthy_since = None;
                s.pressure_samples += 1;
                if over || s.pressure_samples >= 3 {
                    if s.pressure_since.is_none() {
                        s.pressure_since = Some(Instant::now());
                    }
                    let next = (self.threads() / 2).max(1);
                    if next < self.threads() || s.summary.pressure != "pressure" {
                        s.summary.policy_changes += 1;
                    }
                    self.cap.store(next, Ordering::Relaxed);
                    s.summary.pressure = "pressure".into();
                    s.summary.notice = "正在降低处理负载，处理时间可能增加".into();
                    // This is a scheduling hint for the next bounded unit. An
                    // estimate exceeded with healthy physical/commit headroom
                    // is not allocation failure. Existing children finish with
                    // their current thread count; the critical guard above
                    // still stops them if the system actually runs short.
                }
            } else {
                s.pressure_samples = 0;
                s.pressure_since = None;
                s.healthy_samples += 1;
                let healthy_since = *s.healthy_since.get_or_insert_with(Instant::now);
                if s.healthy_samples >= 15
                    && healthy_since.elapsed() >= Duration::from_secs(15)
                    && self.active.load(Ordering::Relaxed) == 0
                {
                    let next = (self.threads() + 1)
                        .min(initial_threads(
                            s.summary.logical_processors,
                            sample.memory.as_ref(),
                        ))
                        .min(self.maximum);
                    if next != self.threads() || s.summary.pressure != "normal" {
                        s.summary.policy_changes += 1;
                    }
                    self.cap.store(next, Ordering::Relaxed);
                    s.summary.pressure = "normal".into();
                    s.summary.notice.clear();
                    s.healthy_samples = 0;
                    s.healthy_since = None;
                }
            }
        }
        let phase = s.stage.clone();
        let budget = s.stage_budget;
        let physical_budget = s.stage_physical_budget;
        let commit_budget = s.stage_commit_budget;
        let estimated = s.stage_estimated;
        let threads = self.threads();
        if !phase.is_empty() {
            let position = s.summary.stages.iter().position(|v| v.phase == phase);
            let index = position.unwrap_or_else(|| {
                if s.summary.stages.len() == 64 {
                    s.summary.stages.remove(0);
                }
                s.summary.stages.push(StagePeak {
                    phase,
                    budget_bytes: budget,
                    threads,
                    ..StagePeak::default()
                });
                s.summary.stages.len() - 1
            });
            let peak = &mut s.summary.stages[index];
            peak.working_set = peak.working_set.max(sample.working_set);
            peak.private_commit = peak.private_commit.max(sample.private_commit);
            peak.cpu_percent = peak.cpu_percent.max(sample.cpu_percent);
            peak.budget_bytes = budget;
            peak.physical_budget_bytes = physical_budget;
            peak.commit_budget_bytes = commit_budget;
            peak.estimated_bytes = estimated;
            peak.threads = threads;
        }
        s.summary.threads = threads;
        s.summary.sample = sample;
        drop(s);
        if let Some(error) = failure {
            self.fail(error);
        }
    }
    pub fn prepare_stage(&self, phase: &str, fixed: u64, per_thread: u64) -> Result<usize, String> {
        self.prepare_stage_limited(phase, fixed, per_thread, self.maximum)
    }
    pub fn prepare_stage_limited(
        &self,
        phase: &str,
        fixed: u64,
        per_thread: u64,
        stage_limit: usize,
    ) -> Result<usize, String> {
        for attempt in 0..2 {
            self.refresh(false);
            self.check()?;
            let budget_started = Instant::now();
            let mut s = self.state.lock().unwrap();
            let available = s
                .summary
                .sample
                .memory
                .as_ref()
                .map(allowance)
                .unwrap_or(128 * MIB);
            let ceiling = if attempt == 0 {
                self.stage_ceiling(&s).min(stage_limit.max(1))
            } else {
                1
            };
            let fitted = fitted_threads(fixed, per_thread, s.memory_reserved, available, ceiling);
            // Budget estimates choose concurrency, never claim that a media
            // allocation failed. At the second sample, proceed with one worker
            // while the live critical-memory and actual-allocation guards stay on.
            let threads = fitted.or((attempt == 1).then_some(1));
            let decision = serde_json::json!({"phase":phase,"attempt":attempt,"fixedBytes":fixed,"perThreadBytes":per_thread,"reservedBytes":s.memory_reserved,"reservations":s.memory_items.values().collect::<Vec<_>>(),"additionalAllowanceBytes":available,"selectedThreads":threads,"advisoryFallback":fitted.is_none() && threads.is_some(),"threadCeiling":ceiling,"systemMemory":s.summary.sample.memory,"residentWorkingSetBytes":s.summary.sample.working_set,"residentPrivateCommitBytes":s.summary.sample.private_commit,"systemReserveBytes":s.summary.sample.memory.as_ref().map(|m|physical_reserve(m.total)),"commitReserveBytes":GIB});
            s.summary.budget_decisions.push(decision);
            if s.summary.budget_decisions.len() > 128 {
                s.summary.budget_decisions.remove(0);
            }
            s.summary.budget_micros += budget_started.elapsed().as_micros() as u64;
            let Some(threads) = threads else {
                drop(s);
                if attempt == 0 {
                    // Let a completed decoder and the suspended idle player release
                    // their resources, then resample the same safety envelope.
                    let waiting = Instant::now();
                    std::thread::sleep(Duration::from_millis(200));
                    self.state.lock().unwrap().summary.budget_retry_wait_micros +=
                        waiting.elapsed().as_micros() as u64;
                    continue;
                }
                unreachable!("the second attempt always selects one worker");
            };
            self.cap.store(threads, Ordering::Relaxed);
            s.summary.threads = threads;
            if phase == "previewRender" && threads <= 2 {
                s.summary.notice = "正在以较低内存占用生成预览，处理时间可能增加".into();
            }
            s.stage = phase.chars().take(96).collect();
            s.stage_baseline = s.summary.sample.private_commit;
            s.stage_baseline_ws = s.summary.sample.working_set;
            s.stage_physical_budget = s
                .summary
                .sample
                .memory
                .as_ref()
                .map(|m| m.available.saturating_sub(physical_reserve(m.total)) * 7 / 10)
                .unwrap_or(128 * MIB);
            s.stage_commit_budget = s
                .summary
                .sample
                .memory
                .as_ref()
                .map(|m| m.commit_available.saturating_sub(GIB))
                .unwrap_or(128 * MIB);
            s.stage_estimated = fixed.saturating_add(per_thread.saturating_mul(threads as u64));
            // Budget is the available envelope, not merely an underestimate of decoder buffers.
            s.stage_budget = available;
            return Ok(threads);
        }
        unreachable!()
    }
    fn stage_ceiling(&self, s: &State) -> usize {
        if !s.summary.sample.reliable {
            1
        } else if s.summary.pressure == "normal" && s.summary.policy_changes == 0 {
            // A stage's buffer budget is not a persistent pressure reduction.
            // Re-evaluate a cheaper stage after the previous child has exited.
            initial_threads(
                s.summary.logical_processors,
                s.summary.sample.memory.as_ref(),
            )
            .min(self.maximum)
        } else {
            // Real pressure/telemetry reductions retain the 15-second recovery
            // and one-step policy; a new phase cannot jump over that cooldown.
            self.threads()
        }
    }
    pub fn reserve_memory(
        self: &Arc<Self>,
        bytes: u64,
        label: &str,
    ) -> Result<MemoryReservation, String> {
        let stale = {
            let s = self.state.lock().unwrap();
            s.summary.samples == 0
                || self.started.elapsed().as_millis() as u64
                    > s.summary.sample.elapsed_ms.saturating_add(1000)
        };
        if stale {
            self.refresh(false);
        }
        self.check()?;
        for attempt in 0..2 {
            let mut s = self.state.lock().unwrap();
            let budget = s
                .summary
                .sample
                .memory
                .as_ref()
                .map(allocation_allowance)
                .unwrap_or(128 * MIB);
            if bytes <= budget.saturating_sub(s.memory_reserved) {
                s.memory_reserved = s.memory_reserved.saturating_add(bytes);
                s.next_id += 1;
                let id = s.next_id;
                s.memory_items
                    .insert(id, (label.chars().take(96).collect(), bytes));
                return Ok(MemoryReservation {
                    controller: self.clone(),
                    bytes,
                    id,
                });
            }
            drop(s);
            // A just-finished decoder can release gigabytes between the last
            // sample and the next stage. Recheck before making failure sticky.
            if attempt == 0 {
                self.refresh(false);
                self.check()?;
            }
        }
        let e = "RESOURCE_MEMORY: 当前可用内存不足，请关闭部分应用后重试".to_string();
        self.fail(e.clone());
        Err(e)
    }
    pub fn reserve_disk(
        self: &Arc<Self>,
        path: &Path,
        expected: u64,
        _label: &str,
    ) -> Result<DiskReservation, String> {
        self.check()?;
        let path = absolute(path)?;
        let volume = platform::volume(&path)?;
        let mut s = self.state.lock().unwrap();
        s.next_id += 1;
        let id = s.next_id;
        s.disks.insert(
            id,
            DiskNeed {
                initial: path.metadata().map(|m| m.len()).unwrap_or(0),
                path,
                volume,
                expected,
            },
        );
        drop(s);
        let reservation = DiskReservation {
            controller: self.clone(),
            id,
        };
        self.refresh(false);
        self.check()?;
        Ok(reservation)
    }
    pub fn process(self: &Arc<Self>) -> Result<ProcessReservation, String> {
        self.refresh(false);
        self.check()?;
        // Soft pressure reduces the next bounded process's thread count; it
        // must not prevent dispatching subsequent layout samples or scan units.
        // refresh/check above still enforce critical physical/commit and disk
        // protection, independently of the advisory scheduling reserve.
        self.active.fetch_add(1, Ordering::Relaxed);
        Ok(ProcessReservation {
            controller: self.clone(),
        })
    }
    pub fn media_error(&self, log: &str) {
        let lower = log.to_ascii_lowercase();
        if [
            "cannot allocate memory",
            "out of memory",
            "not enough memory",
            "insufficient memory",
        ]
        .iter()
        .any(|v| lower.contains(v))
        {
            self.fail("RESOURCE_MEMORY: 媒体处理内存不足，请关闭部分应用后重试".into());
        } else if [
            "no space left on device",
            "disk full",
            "not enough space on the disk",
        ]
        .iter()
        .any(|v| lower.contains(v))
        {
            self.fail(
                "RESOURCE_DISK: 写入空间不足，请更换输出位置或清理可回收预览缓存后重试".into(),
            );
        }
    }
}
fn absolute(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|p| p.join(path))
            .map_err(|e| e.to_string())
    }
}
fn sample_disks(needs: &[(PathBuf, PathBuf, u64, u64)]) -> Result<Vec<DiskSample>, String> {
    // A target registered by both the render stage and its writer counts once.
    let mut files: BTreeMap<(String, String), (PathBuf, u64)> = BTreeMap::new();
    for (path, volume, expected, initial) in needs {
        let growth = path
            .metadata()
            .map(|m| m.len().saturating_sub(*initial))
            .unwrap_or(0);
        let key = (
            volume.to_string_lossy().to_lowercase(),
            path.to_string_lossy().to_lowercase(),
        );
        let e = files.entry(key).or_insert((volume.clone(), 0));
        e.1 = e.1.max(expected.saturating_sub(growth));
    }
    let mut volumes: BTreeMap<String, (PathBuf, u64)> = BTreeMap::new();
    for ((volume, _), (path, remaining)) in files {
        let v = volumes.entry(volume).or_insert((path, 0));
        v.1 = v.1.saturating_add(remaining);
    }
    volumes
        .into_iter()
        .map(|(volume, (path, remaining))| {
            let (available, total) = platform::disk(&path)?;
            #[cfg(feature = "resource-test-hooks")]
            let available = test_hook()
                .and_then(|h| h.disk_available_bytes)
                .map(|v| available.min(v))
                .unwrap_or(available);
            Ok(DiskSample {
                volume,
                available,
                total,
                reserve: disk_reserve(total),
                remaining,
            })
        })
        .collect()
}

#[derive(Default)]
struct Sampler {
    previous: HashMap<u32, (u64, u64)>,
    known: HashMap<u32, u64>,
    cpu: CpuWindow,
    birth_cutoff: u64,
}
#[derive(Default)]
struct CpuWindow {
    started: Option<Instant>,
    ticks: u64,
    percent: f64,
    window_ms: u64,
}
impl CpuWindow {
    fn add(&mut self, now: Instant, ticks: u64, logical: usize) -> (f64, u64) {
        let start = *self.started.get_or_insert(now);
        self.ticks = self.ticks.saturating_add(ticks);
        let elapsed = now.saturating_duration_since(start);
        if elapsed >= Duration::from_secs(1) {
            self.percent =
                (self.ticks as f64 / 10_000_000. / elapsed.as_secs_f64() / logical.max(1) as f64
                    * 100.)
                    .clamp(0., 100.);
            self.window_ms = elapsed.as_millis() as u64;
            self.ticks = 0;
            self.started = Some(now);
        }
        (self.percent, self.window_ms)
    }
}
impl Sampler {
    fn sample(&mut self) -> Sample {
        let memory = platform::memory().ok();
        let mut reliable = memory.is_some();
        let now = Instant::now();
        let initialized = self.cpu.started.is_some();
        if !initialized {
            self.birth_cutoff = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| (d.as_nanos() / 100) as u64 + 116_444_736_000_000_000)
                .unwrap_or(u64::MAX);
        }
        let entries = platform::processes().unwrap_or_else(|_| {
            reliable = false;
            Vec::new()
        });
        let (owned, tree_reliable) =
            owned_processes(std::process::id(), &entries, &self.known, platform::process);
        reliable &= tree_reliable;
        let mut next = HashMap::new();
        let mut known = HashMap::new();
        let mut sample = Sample {
            memory,
            ..Sample::default()
        };
        let mut ticks = 0u64;
        for (pid, counters) in owned {
            match counters {
                Ok(p) => {
                    sample.working_set = sample.working_set.saturating_add(p.working_set);
                    sample.private_commit = sample.private_commit.saturating_add(p.private_commit);
                    sample.process_count += 1;
                    if let Some(&(born, cpu)) = self.previous.get(&pid) {
                        if born == p.created {
                            ticks = ticks.saturating_add(p.cpu.saturating_sub(cpu));
                        }
                    } else if initialized && p.created >= self.birth_cutoff {
                        // A descendant born between observations contributes its
                        // CPU since creation, rather than losing its first interval.
                        ticks = ticks.saturating_add(p.cpu);
                    }
                    next.insert(pid, (p.created, p.cpu));
                    known.insert(pid, p.created);
                }
                Err(e) if e == 87 || e == 1168 => {}
                Err(_) => {
                    reliable = false;
                    // Preserve the last CPU counter through a transient access
                    // failure so recovery cannot recount the process lifetime.
                    if let Some(&(born, cpu)) = self.previous.get(&pid) {
                        next.insert(pid, (born, cpu));
                        known.insert(pid, born);
                    }
                }
            }
        }
        if sample.process_count == 0 {
            reliable = false;
        }
        let logical = std::thread::available_parallelism()
            .map(|v| v.get())
            .unwrap_or(1);
        (sample.cpu_percent, sample.cpu_window_ms) = self.cpu.add(now, ticks, logical);
        sample.reliable = reliable;
        self.previous = next;
        self.known = known;
        #[cfg(feature = "resource-test-hooks")]
        if let Some(hook) = test_hook() {
            if hook.fail_read {
                sample.memory = None;
                sample.reliable = false;
            } else if let Some(m) = &mut sample.memory {
                if let Some(v) = hook.total_bytes {
                    m.total = m.total.min(v);
                    m.available = m.available.min(m.total);
                }
                if let Some(v) = hook.available_bytes {
                    m.available = m.available.min(v);
                }
                if let Some(v) = hook.commit_available_bytes {
                    m.commit_available = m.commit_available.min(v);
                }
            }
        }
        sample
    }
}
struct ProcessCounters {
    created: u64,
    cpu: u64,
    working_set: u64,
    private_commit: u64,
}

// Parent PID alone is not ancestry: Windows can recycle it while an older
// orphan keeps that number as its parent. Validate birth order at every edge.
// Previously observed orphans remain owned only while their identity matches.
fn owned_processes(
    root: u32,
    entries: &[(u32, u32)],
    known: &HashMap<u32, u64>,
    mut read: impl FnMut(u32) -> Result<ProcessCounters, u32>,
) -> (HashMap<u32, Result<ProcessCounters, u32>>, bool) {
    let live: HashSet<_> = entries.iter().map(|(pid, _)| *pid).collect();
    let mut counters = HashMap::new();
    counters.insert(root, read(root));
    let mut selected = HashSet::from([root]);
    let mut reliable = true;
    for (&pid, &created) in known {
        if !live.contains(&pid) || pid == root {
            continue;
        }
        let value = counters.entry(pid).or_insert_with(|| read(pid));
        match value {
            Ok(p) if p.created == created => {
                selected.insert(pid);
            }
            Err(e) if *e != 87 && *e != 1168 => {
                // Retain the inaccessible identity so the caller preserves
                // its prior CPU counter and selects conservative operation.
                selected.insert(pid);
                reliable = false;
            }
            _ => {}
        }
    }
    loop {
        let before = selected.len();
        for &(pid, parent) in entries {
            if selected.contains(&pid) || !selected.contains(&parent) {
                continue;
            }
            let Some(parent_created) = counters
                .get(&parent)
                .and_then(|p| p.as_ref().ok())
                .map(|p| p.created)
            else {
                continue;
            };
            match counters.entry(pid).or_insert_with(|| read(pid)) {
                Ok(p) if p.created >= parent_created => {
                    selected.insert(pid);
                }
                Err(e) if *e != 87 && *e != 1168 => {
                    reliable = false;
                }
                _ => {}
            }
        }
        if selected.len() == before {
            break;
        }
    }
    counters.retain(|pid, _| selected.contains(pid));
    (counters, reliable)
}

// Explicit development build only. The fixture must be a small, owned data-dir
// file and can only lower actual capacity; it cannot waive any safety check.
#[cfg(feature = "resource-test-hooks")]
#[derive(serde::Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct TestHook {
    total_bytes: Option<u64>,
    available_bytes: Option<u64>,
    commit_available_bytes: Option<u64>,
    disk_available_bytes: Option<u64>,
    logical_processors: Option<usize>,
    fail_read: bool,
}
#[cfg(feature = "resource-test-hooks")]
fn test_hook() -> Option<TestHook> {
    let requested = PathBuf::from(std::env::var_os("ARKCUT_RESOURCE_TEST_FILE")?);
    if requested.file_name()?.to_str()? != ".arkcut-resource-test.json" {
        return None;
    }
    let path = std::fs::canonicalize(requested).ok()?;
    let root = std::fs::canonicalize(crate::storage::root()).ok()?;
    if !path.starts_with(root) || path.metadata().ok()?.len() > 16384 {
        return None;
    }
    serde_json::from_reader(std::fs::File::open(path).ok()?).ok()
}

#[cfg(windows)]
pub(crate) mod platform {
    use super::*;
    use std::{
        ffi::c_void,
        mem::{size_of, zeroed},
        os::windows::{ffi::OsStrExt, io::AsRawHandle},
    };
    type Handle = *mut c_void;
    #[repr(C)]
    struct MemoryStatus {
        length: u32,
        load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page: u64,
        avail_page: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_extended: u64,
    }
    #[repr(C)]
    struct Performance {
        cb: u32,
        commit_total: usize,
        commit_limit: usize,
        commit_peak: usize,
        physical_total: usize,
        physical_available: usize,
        system_cache: usize,
        kernel_total: usize,
        kernel_paged: usize,
        kernel_nonpaged: usize,
        page_size: usize,
        handles: u32,
        processes: u32,
        threads: u32,
    }
    #[repr(C)]
    struct ProcessMemory {
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
    #[repr(C)]
    struct ProcessEntry {
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
    struct ThreadEntry {
        size: u32,
        usage: u32,
        id: u32,
        owner: u32,
        priority: i32,
        delta: i32,
        flags: u32,
    }
    #[repr(C)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    impl FileTime {
        fn ticks(&self) -> u64 {
            (self.high as u64) << 32 | self.low as u64
        }
    }
    #[repr(C)]
    struct BasicLimit {
        process_time: i64,
        job_time: i64,
        flags: u32,
        min_ws: usize,
        max_ws: usize,
        active: u32,
        affinity: usize,
        priority: u32,
        scheduling: u32,
    }
    #[repr(C)]
    struct ExtendedLimit {
        basic: BasicLimit,
        io: [u64; 6],
        process_memory: usize,
        job_memory: usize,
        peak_process: usize,
        peak_job: usize,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalMemoryStatusEx(p: *mut MemoryStatus) -> i32;
        fn GetDiskFreeSpaceExW(
            path: *const u16,
            avail: *mut u64,
            total: *mut u64,
            free: *mut u64,
        ) -> i32;
        fn GetVolumePathNameW(path: *const u16, volume: *mut u16, len: u32) -> i32;
        fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> Handle;
        fn Process32FirstW(h: Handle, p: *mut ProcessEntry) -> i32;
        fn Process32NextW(h: Handle, p: *mut ProcessEntry) -> i32;
        fn Thread32First(h: Handle, p: *mut ThreadEntry) -> i32;
        fn Thread32Next(h: Handle, p: *mut ThreadEntry) -> i32;
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
        fn OpenThread(access: u32, inherit: i32, id: u32) -> Handle;
        fn GetProcessTimes(
            h: Handle,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
        fn CloseHandle(h: Handle) -> i32;
        fn GetLastError() -> u32;
        fn ResumeThread(h: Handle) -> u32;
        fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> Handle;
        fn SetInformationJobObject(h: Handle, class: i32, data: *const c_void, size: u32) -> i32;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        fn TerminateJobObject(job: Handle, code: u32) -> i32;
    }
    #[link(name = "psapi")]
    extern "system" {
        fn GetPerformanceInfo(p: *mut Performance, size: u32) -> i32;
        fn GetProcessMemoryInfo(h: Handle, p: *mut ProcessMemory, size: u32) -> i32;
    }
    struct Owned(Handle);
    impl Drop for Owned {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    fn wide(p: &Path) -> Vec<u16> {
        p.as_os_str().encode_wide().chain(Some(0)).collect()
    }
    fn error(prefix: &str) -> String {
        format!("{prefix}: Windows {}", unsafe { GetLastError() })
    }
    pub(super) fn memory() -> Result<Memory, String> {
        unsafe {
            let mut m: MemoryStatus = zeroed();
            m.length = size_of::<MemoryStatus>() as u32;
            let mut p: Performance = zeroed();
            p.cb = size_of::<Performance>() as u32;
            if GlobalMemoryStatusEx(&mut m) == 0
                || GetPerformanceInfo(&mut p, size_of::<Performance>() as u32) == 0
            {
                return Err(error("RESOURCE_TELEMETRY"));
            }
            Ok(Memory {
                total: m.total_phys,
                available: m.avail_phys,
                commit_available: (p.commit_limit.saturating_sub(p.commit_total) as u64)
                    .saturating_mul(p.page_size as u64),
            })
        }
    }
    pub(super) fn volume(path: &Path) -> Result<PathBuf, String> {
        let mut ancestor = path;
        while !ancestor.exists() {
            ancestor = ancestor.parent().ok_or("RESOURCE_DISK: 无法访问目标磁盘")?;
        }
        let mut buffer = vec![0u16; 32768];
        if unsafe {
            GetVolumePathNameW(
                wide(ancestor).as_ptr(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
            )
        } == 0
        {
            return Err(error("RESOURCE_DISK: 无法读取目标磁盘"));
        }
        let len = buffer.iter().position(|&v| v == 0).unwrap_or(buffer.len());
        Ok(PathBuf::from(String::from_utf16_lossy(&buffer[..len])))
    }
    pub(super) fn disk(path: &Path) -> Result<(u64, u64), String> {
        let (mut available, mut total, mut free) = (0, 0, 0);
        if unsafe {
            GetDiskFreeSpaceExW(wide(path).as_ptr(), &mut available, &mut total, &mut free)
        } == 0
        {
            return Err(format!(
                "RESOURCE_DISK: 无法访问磁盘 {}，请检查连接后重试",
                path.display()
            ));
        }
        Ok((available, total))
    }
    pub(super) fn processes() -> Result<Vec<(u32, u32)>, String> {
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(2, 0);
            if snapshot as isize == -1 {
                return Err(error("RESOURCE_TELEMETRY"));
            }
            let handle = Owned(snapshot);
            let mut entry: ProcessEntry = zeroed();
            entry.size = size_of::<ProcessEntry>() as u32;
            let mut ok = Process32FirstW(handle.0, &mut entry);
            let mut result = Vec::new();
            while ok != 0 {
                if result.len() == 65536 {
                    return Err("进程列表超过安全上限".into());
                }
                result.push((entry.pid, entry.parent));
                ok = Process32NextW(handle.0, &mut entry);
            }
            Ok(result)
        }
    }
    pub(super) fn process(pid: u32) -> Result<ProcessCounters, u32> {
        unsafe {
            let mut h = OpenProcess(0x1000 | 0x10, 0, pid);
            if h.is_null() {
                h = OpenProcess(0x1000, 0, pid);
            }
            if h.is_null() {
                return Err(GetLastError());
            }
            let h = Owned(h);
            let (mut c, mut e, mut k, mut u) = (zeroed(), zeroed(), zeroed(), zeroed());
            let mut m: ProcessMemory = zeroed();
            m.cb = size_of::<ProcessMemory>() as u32;
            if GetProcessTimes(h.0, &mut c, &mut e, &mut k, &mut u) == 0
                || GetProcessMemoryInfo(h.0, &mut m, size_of::<ProcessMemory>() as u32) == 0
            {
                return Err(GetLastError());
            }
            Ok(ProcessCounters {
                created: c.ticks(),
                cpu: k.ticks().saturating_add(u.ticks()),
                working_set: m.ws as u64,
                private_commit: m.private as u64,
            })
        }
    }
    /// Children start suspended, enter a kill-on-close Job Object, then resume.
    /// No uncontained execution window exists, even if the child immediately spawns descendants.
    pub(crate) struct ProcessGroup(Owned);
    impl ProcessGroup {
        pub(crate) fn attach(child: &std::process::Child) -> Result<Self, String> {
            unsafe {
                let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if h.is_null() {
                    return Err(error("PROCESS_CONTAINMENT"));
                }
                let group = Self(Owned(h));
                let mut limit: ExtendedLimit = zeroed();
                limit.basic.flags = 0x2000;
                if SetInformationJobObject(
                    h,
                    9,
                    &limit as *const _ as *const c_void,
                    size_of::<ExtendedLimit>() as u32,
                ) == 0
                    || AssignProcessToJobObject(h, child.as_raw_handle() as Handle) == 0
                {
                    return Err(error("PROCESS_CONTAINMENT"));
                }
                let snapshot = CreateToolhelp32Snapshot(4, 0);
                if snapshot as isize == -1 {
                    return Err(error("PROCESS_CONTAINMENT"));
                }
                let snapshot = Owned(snapshot);
                let mut entry: ThreadEntry = zeroed();
                entry.size = size_of::<ThreadEntry>() as u32;
                let mut ok = Thread32First(snapshot.0, &mut entry);
                let mut resumed = false;
                while ok != 0 {
                    if entry.owner == child.id() {
                        let thread = OpenThread(2, 0, entry.id);
                        if thread.is_null() {
                            return Err(error("PROCESS_CONTAINMENT"));
                        }
                        let thread = Owned(thread);
                        if ResumeThread(thread.0) == u32::MAX {
                            return Err(error("PROCESS_CONTAINMENT"));
                        }
                        resumed = true;
                    }
                    ok = Thread32Next(snapshot.0, &mut entry);
                }
                if !resumed {
                    return Err("PROCESS_CONTAINMENT: 未找到媒体进程主线程".into());
                }
                Ok(group)
            }
        }
        pub(crate) fn terminate(&self) {
            unsafe {
                TerminateJobObject(self.0 .0, 1);
            }
        }
    }
}
#[cfg(not(windows))]
mod platform {
    use super::*;
    pub(super) fn memory() -> Result<Memory, String> {
        Err("Windows telemetry unavailable".into())
    }
    pub(super) fn processes() -> Result<Vec<(u32, u32)>, String> {
        Err("Windows telemetry unavailable".into())
    }
    pub(super) fn process(_: u32) -> Result<ProcessCounters, u32> {
        Err(1)
    }
    pub(super) fn volume(path: &Path) -> Result<PathBuf, String> {
        Ok(path.to_path_buf())
    }
    pub(super) fn disk(_: &Path) -> Result<(u64, u64), String> {
        Err("RESOURCE_DISK: Windows 磁盘信息不可用".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn process_tree_rejects_recycled_parent_ids_and_preserves_known_orphans() {
        let entries = [
            (1, 0),
            (2, 1),
            (3, 2),
            (4, 1),
            (5, 4),
            (7, 99),
            (8, 99),
            (9, 4),
        ];
        let births = HashMap::from([
            (1, 100),
            (2, 50),
            (3, 80),
            (4, 110),
            (5, 120),
            (7, 130),
            (8, 140),
            (9, 150),
        ]);
        let known = HashMap::from([(7, 130), (8, 90), (9, 95)]);
        let (owned, reliable) = owned_processes(1, &entries, &known, |pid| {
            Ok(ProcessCounters {
                created: births[&pid],
                cpu: 1,
                working_set: 10,
                private_commit: 20,
            })
        });
        assert!(reliable);
        let mut ids = owned.keys().copied().collect::<Vec<_>>();
        ids.sort_unstable();
        assert_eq!(ids, vec![1, 4, 5, 7, 9]);
        // Old child 2 and its descendant 3 cannot attach to reused root PID 1.
        // Known orphan 7 is retained, reused unrelated 8 is excluded, and
        // reused PID 9 is accepted only through its freshly verified ancestry.
    }
    #[test]
    fn memory_policy_reserves_system_and_commit_independently() {
        let m = Memory {
            total: 8 * GIB,
            available: 4 * GIB,
            commit_available: 8 * GIB,
        };
        assert_eq!(allowance(&m), (4 * GIB - 8 * GIB / 5) * 7 / 10);
        assert_eq!(
            allowance(&Memory {
                commit_available: GIB + 10,
                ..m.clone()
            }),
            10
        );
        assert_eq!(physical_reserve(4 * GIB), GIB);
        assert_eq!(critical_reserve(4 * GIB), 512 * MIB);
        assert_eq!(initial_threads(20, Some(&m)), 16);
        assert_eq!(initial_threads(20, Some(&Memory { available: 2 * GIB, ..m.clone() })), 2);
        assert_eq!(initial_threads(1, Some(&m)), 1);
        assert_eq!(initial_threads(20, None), 1);
        assert_eq!(cpu_ceiling(20), 16);
        assert_eq!(cpu_ceiling(8), 6);
        assert_eq!(cpu_ceiling(1), 1);
        let pressured = Memory { total: 16 * GIB, available: 2 * GIB, commit_available: 8 * GIB };
        assert_eq!(allowance(&pressured), 0);
        assert!(allocation_allowance(&pressured) > 512 * MIB);
        assert_eq!(
            initial_threads(
                20,
                Some(&Memory {
                    total: 16 * GIB,
                    ..m
                })
            ),
            16
        );
    }
    #[test]
    fn disk_margin_has_floor_and_ceiling() {
        assert_eq!(disk_reserve(16 * GIB), GIB);
        assert_eq!(disk_reserve(200 * GIB), 2 * GIB);
        assert_eq!(disk_reserve(2000 * GIB), 5 * GIB);
    }
    #[test]
    fn memory_leases_release_and_do_not_hide_resource_reason() {
        let c = ResourceController::new(Arc::new(AtomicBool::new(false)));
        {
            let mut s = c.state.lock().unwrap();
            s.summary.samples = 1;
        }
        {
            let _r = c.reserve_memory(64 * MIB, "test").unwrap();
            assert_eq!(c.state.lock().unwrap().memory_reserved, 64 * MIB);
        }
        assert_eq!(c.state.lock().unwrap().memory_reserved, 0);
        assert!(c.reserve_memory(u64::MAX, "test").is_err());
        assert!(c.check().unwrap_err().starts_with("RESOURCE_MEMORY"));
        assert!(c.cancel.load(Ordering::Relaxed));
    }
}

#[cfg(test)]
mod policy_tests {
    use super::*;
    fn sample(available: u64) -> Sample {
        Sample {
            memory: Some(Memory {
                total: 16 * GIB,
                available,
                commit_available: 8 * GIB,
            }),
            working_set: 100 * MIB,
            private_commit: 80 * MIB,
            process_count: 1,
            reliable: true,
            ..Sample::default()
        }
    }
    #[test]
    fn stage_budget_does_not_latch_but_pressure_recovery_stays_gradual() {
        let c = ResourceController::new(Arc::new(AtomicBool::new(false)));
        c.observe(sample(8 * GIB), true, None);
        c.cap.store(2, Ordering::Relaxed);
        {
            let mut s = c.state.lock().unwrap();
            s.summary.logical_processors = 20;
            assert_eq!(c.stage_ceiling(&s), MAX_MEDIA_THREADS.min(c.maximum));
        }
        for _ in 0..3 {
            c.observe(sample(3 * GIB), true, None);
        }
        let reduced = c.threads();
        {
            let mut s = c.state.lock().unwrap();
            assert_eq!(s.summary.pressure, "pressure");
            assert_eq!(c.stage_ceiling(&s), reduced);
            s.summary.pressure = "normal".into();
            s.summary.sample = sample(8 * GIB);
            assert!(s.summary.policy_changes > 0);
            assert_eq!(c.stage_ceiling(&s), reduced);
        }
    }
    #[test]
    fn three_pressure_samples_and_fifteen_seconds_of_stable_recovery() {
        let c = ResourceController::new(Arc::new(AtomicBool::new(false)));
        c.observe(sample(8 * GIB), true, None);
        let initial = c.threads();
        c.observe(sample(3 * GIB), true, None);
        c.observe(sample(3 * GIB), true, None);
        assert_eq!(c.summary().pressure, "normal");
        c.observe(sample(3 * GIB), true, None);
        assert_eq!(c.summary().pressure, "pressure");
        assert_eq!(c.threads(), (initial / 2).max(1));
        for _ in 0..14 {
            c.observe(sample(8 * GIB), true, None);
        }
        assert_eq!(c.summary().pressure, "pressure");
        c.observe(sample(8 * GIB), true, None);
        assert_eq!(
            c.summary().pressure,
            "pressure",
            "Fifteen rapid samples are not fifteen stable seconds"
        );
        c.state.lock().unwrap().healthy_since = Some(Instant::now() - Duration::from_secs(15));
        c.observe(sample(8 * GIB), true, None);
        assert_eq!(c.summary().pressure, "normal");
        assert!(c.check().is_ok());
    }
    #[test]
    fn critical_commit_and_physical_are_independent() {
        let c = ResourceController::new(Arc::new(AtomicBool::new(false)));
        c.observe(sample(512 * MIB), true, None);
        assert!(c.check().unwrap_err().starts_with("RESOURCE_MEMORY"));
        let c = ResourceController::new(Arc::new(AtomicBool::new(false)));
        let mut v = sample(8 * GIB);
        v.memory.as_mut().unwrap().commit_available = 511 * MIB;
        c.observe(v, true, None);
        assert!(c.check().is_err());
    }
    #[test]
    fn later_telemetry_failure_preserves_critical_notice_and_wait_timeout_is_explicit() {
        let c = ResourceController::new(Arc::new(AtomicBool::new(false)));
        c.observe(sample(512 * MIB), true, None);
        let notice = c.summary().notice;
        c.observe(Sample::default(), true, None);
        assert_eq!(c.summary().pressure, "critical");
        assert_eq!(c.summary().notice, notice);
        c.cancellation_overdue();
        let changes = c.summary().policy_changes;
        c.cancellation_overdue();
        assert_eq!(c.summary().policy_changes, changes);
        assert!(c.summary().notice.contains("尚未结束"));
        c.cancellation_elapsed(5500);
        assert_eq!(c.summary().notice, notice);
        assert_eq!(c.summary().cancellation_ms, Some(5500));
    }
    #[test]
    fn unavailable_telemetry_uses_one_thread_without_fabricating_zero_memory() {
        let c = ResourceController::new(Arc::new(AtomicBool::new(false)));
        c.observe(Sample::default(), true, None);
        assert_eq!(c.threads(), 1);
        assert!(c.summary().sample.memory.is_none());
        assert!(c.check().is_ok());
    }
    #[test]
    fn disk_reservation_is_independent_of_memory() {
        let c = ResourceController::new(Arc::new(AtomicBool::new(false)));
        let mut v = sample(8 * GIB);
        v.disks.push(DiskSample {
            volume: "test".into(),
            available: GIB,
            remaining: 1,
            reserve: GIB,
            total: 100 * GIB,
        });
        c.observe(v, true, None);
        assert!(c.check().unwrap_err().starts_with("RESOURCE_DISK"));
    }
    #[test]
    fn busy_stage_finishes_under_soft_pressure_but_stops_at_critical_memory() {
        let c = ResourceController::new(Arc::new(AtomicBool::new(false)));
        c.observe(sample(8 * GIB), true, None);
        c.active.store(1, Ordering::Relaxed);
        {
            let mut s = c.state.lock().unwrap();
            s.pressure_samples = 3;
            s.pressure_since = Some(Instant::now() - Duration::from_secs(4));
        }
        c.observe(sample(3 * GIB), true, None);
        assert!(c.check().is_ok());
        assert_eq!(c.summary().pressure, "pressure");
        c.observe(sample(512 * MIB), true, None);
        assert!(c.check().unwrap_err().starts_with("RESOURCE_MEMORY"));
    }
    #[test]
    fn real_preview_admission_shortfall_is_only_an_advisory() {
        // Actual 1440p/120 user's failure: 4.35 GiB available, model only 16 MiB over.
        assert_eq!(fitted_threads(887_750_656, 52_625_408, 0, 923_752_776, 4), None);
        assert_eq!(fitted_threads(887_750_656, 52_625_408, 0, 1_100_000_000, 4), Some(4));
        let c = ResourceController::new(Arc::new(AtomicBool::new(false)));
        c.observe(sample(4_675_735_552), true, None);
        // An over-budget active child can finish, rather than failing after three seconds.
        c.active.store(1, Ordering::Relaxed);
        {let mut s=c.state.lock().unwrap();s.stage_budget=1;s.stage_physical_budget=1;s.stage_commit_budget=1;s.pressure_since=Some(Instant::now()-Duration::from_secs(10));}
        c.observe(sample(4_675_735_552), true, None);
        assert!(c.check().is_ok());
        assert!(!c.cancel.load(Ordering::Relaxed));
    }
}

#[cfg(test)]
mod independent_budget_tests {
    use super::*;
    #[test]
    fn committed_but_nonresident_memory_does_not_consume_physical_budget() {
        let c = ResourceController::new(Arc::new(AtomicBool::new(false)));
        let sample = Sample {
            memory: Some(Memory {
                total: 16 * GIB,
                available: 8 * GIB,
                commit_available: 12 * GIB,
            }),
            working_set: 500 * MIB,
            private_commit: 2 * GIB,
            reliable: true,
            process_count: 1,
            ..Sample::default()
        };
        {
            let mut s = c.state.lock().unwrap();
            s.summary.samples = 1;
            s.stage_budget = GIB;
            s.stage_physical_budget = GIB;
            s.stage_commit_budget = 8 * GIB;
        }
        c.observe(sample.clone(), true, None);
        assert_ne!(c.summary().pressure, "pressure");
        let mut high_ws = sample.clone();
        high_ws.working_set = GIB + 1;
        c.observe(high_ws, true, None);
        assert_eq!(c.summary().pressure, "pressure");
        let c = ResourceController::new(Arc::new(AtomicBool::new(false)));
        {
            let mut s = c.state.lock().unwrap();
            s.summary.samples = 1;
            s.stage_budget = GIB;
            s.stage_physical_budget = GIB;
            s.stage_commit_budget = GIB;
        }
        c.observe(sample, true, None);
        assert_eq!(c.summary().pressure, "pressure");
    }
}

#[cfg(test)]
mod cpu_window_tests {
    use super::*;
    #[test]
    fn frequent_preflights_do_not_reset_or_spike_the_cpu_window() {
        let now = Instant::now();
        let mut cpu = CpuWindow::default();
        assert_eq!(cpu.add(now, 0, 2), (0., 0));
        for i in 1..100 {
            assert_eq!(
                cpu.add(now + Duration::from_millis(i * 10), 100_000, 2),
                (0., 0)
            );
        }
        assert_eq!(
            cpu.add(now + Duration::from_secs(1), 100_000, 2),
            (50., 1000)
        );
        assert_eq!(
            cpu.add(now + Duration::from_millis(1001), 10_000, 2),
            (50., 1000)
        );
        assert_eq!(cpu.add(now + Duration::from_secs(2), 0, 2), (0.05, 1000));
    }
}
