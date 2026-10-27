use std::{
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};
pub type Progress = Arc<dyn Fn(&str, Option<i64>) + Send + Sync>;
// FFmpeg's progress writer does not share av_log's line lock. When progress
// and showinfo use stderr together, progress can split a frame's PTS line.
// Keep that protocol on an owned, bounded side file; stderr carries only logs.
struct FrameProgress {
    file: std::fs::File,
    temp: crate::storage::OwnedTemp,
    _disk: crate::resource::DiskReservation,
    pending: Vec<u8>,
    read_bytes: u64,
}
impl FrameProgress {
    const LIMIT: u64 = 8 * 1024 * 1024;
    fn new(r: &Runner) -> Result<Self, String> {
        let temp = crate::storage::temp_file(&crate::storage::index_dir()?, "frame-progress")?;
        let disk = r.reserve_disk(temp.path(), Self::LIMIT, "校验阶段进度")?;
        let file = std::fs::OpenOptions::new().read(true).write(true).create_new(true)
            .open(temp.path()).map_err(|e| format!("RESOURCE_DISK: 无法创建校验进度文件：{e}"))?;
        Ok(Self { file, temp, _disk: disk, pending: Vec::with_capacity(256), read_bytes: 0 })
    }
    fn poll(&mut self, r: &Runner) -> Result<(), String> {
        let mut buffer = [0u8; 4096];
        loop {
            let n = self.file.read(&mut buffer).map_err(|e| format!("PROCESS_READ: 校验进度读取失败：{e}"))?;
            if n == 0 { return Ok(()) }
            self.read_bytes += n as u64;
            if self.read_bytes > Self::LIMIT { return Err("校验进度数据超过安全上限".into()) }
            for &byte in &buffer[..n] {
                if byte == b'\n' {
                    if let Ok(line) = std::str::from_utf8(&self.pending) {
                        if let Some(done) = line.trim_end_matches('\r').strip_prefix("out_time_us=")
                            .and_then(|s| s.parse::<i64>().ok()) {
                            let state = r.metric_state.lock().unwrap().clone();
                            r.tick(done.max(state.completed_units + state.origin));
                        }
                    }
                    self.pending.clear();
                } else if self.pending.len() < 256 { self.pending.push(byte) }
            }
        }
    }
}
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metric {
    pub phase: String,
    pub completed_units: i64,
    pub total_units: Option<i64>,
    pub unit: String,
    pub detail: String,
    #[serde(skip)]
    pub origin: i64,
}
#[derive(Clone)]
pub struct Runner {
    pub content_ids: Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
    pub repair_used: Arc<std::sync::atomic::AtomicBool>,
    pub validated_audit: Arc<std::sync::Mutex<Option<(PathBuf, serde_json::Value)>>>,
    pub cancel: Arc<AtomicBool>,
    pub progress: Progress,
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    pub metrics: Arc<dyn Fn(Metric) + Send + Sync>,
    pub metric_state: Arc<std::sync::Mutex<Metric>>,
    pub last_metric: Arc<std::sync::Mutex<std::time::Instant>>,
    resource: Arc<crate::resource::ResourceController>,
}
impl Runner {
    pub fn discover(progress: Progress) -> Result<Self, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut roots = vec![crate::components::media_path_at(&crate::components::root()?,&crate::components::spec().ffmpeg)];
        if cfg!(debug_assertions) {
            roots.push(exe.parent().unwrap().join("media"));
            roots.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tools/media"));
        }
        if let Ok(v) = std::env::var("ARKCUT_MEDIA_DIR") {
            roots.insert(0, v.into())
        }
        let spec=crate::components::spec();
        let root = roots
            .iter()
            .find(|p| crate::components::verify_media(p,&spec.ffmpeg,&AtomicBool::new(false)).is_ok())
            .ok_or("视频处理组件尚未准备好，请重新打开 ArkCut 完成准备")?;
        let cancel = Arc::new(AtomicBool::new(false));
        Ok(Self {
            content_ids: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            repair_used: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            validated_audit: Arc::new(std::sync::Mutex::new(None)),
            resource: crate::resource::ResourceController::new(cancel.clone()),
            cancel,
            progress,
            ffmpeg: root.join("ffmpeg.exe"),
            ffprobe: root.join("ffprobe.exe"),
            metrics: Arc::new(|_| {}),
            metric_state: Arc::new(std::sync::Mutex::new(Metric::default())),
            last_metric: Arc::new(std::sync::Mutex::new(
                std::time::Instant::now() - Duration::from_secs(1),
            )),
        })
    }
    pub fn measure(&self, phase: &str, done: i64, total: Option<i64>, unit: &str, detail: &str) {
        self.resource.stage(phase);
        let mut state = self.metric_state.lock().unwrap();
        let changed = state.phase != phase || state.detail != detail || state.total_units != total;
        *state = Metric {
            phase: phase.into(),
            completed_units: done.max(0),
            total_units: total,
            unit: unit.into(),
            detail: detail.into(),
            origin: 0,
        };
        let mut last = self.last_metric.lock().unwrap();
        if changed
            || last.elapsed() >= Duration::from_millis(200)
            || total.is_some_and(|n| done >= n)
        {
            (self.metrics)(state.clone());
            *last = std::time::Instant::now();
        }
    }
    pub fn tick(&self, done: i64) {
        let state = self.metric_state.lock().unwrap().clone();
        self.measure(
            &state.phase,
            (done - state.origin).max(0),
            state.total_units,
            &state.unit,
            &state.detail,
        );
        self.metric_state.lock().unwrap().origin = state.origin;
    }
    pub fn check(&self) -> Result<(), String> {
        self.resource.check()?;
        if self.cancel.load(Ordering::Relaxed) {
            Err("CANCELLED: 用户取消".into())
        } else {
            Ok(())
        }
    }
    pub fn resource(&self) -> &Arc<crate::resource::ResourceController> {
        &self.resource
    }
    pub fn threads(&self) -> usize {
        self.resource.threads()
    }
    pub fn prepare_stage(
        &self,
        phase: &str,
        fixed_bytes: u64,
        per_thread_bytes: u64,
    ) -> Result<usize, String> {
        self.check()?;
        self.resource
            .prepare_stage(phase, fixed_bytes, per_thread_bytes)
    }
    pub fn reserve_memory(
        &self,
        bytes: u64,
        label: &str,
    ) -> Result<crate::resource::MemoryReservation, String> {
        self.check()?;
        self.resource.reserve_memory(bytes, label)
    }
    pub fn reserve_disk(
        &self,
        path: &Path,
        bytes: u64,
        label: &str,
    ) -> Result<crate::resource::DiskReservation, String> {
        self.check()?;
        self.resource.reserve_disk(path, bytes, label)
    }
    fn spawn(
        &self,
        probe: bool,
        args: &[String],
    ) -> Result<(Child, ProcessGroup, crate::resource::ProcessReservation), String> {
        self.check()?;
        let reservation = self.resource.process()?;
        let mut cmd = Command::new(if probe { &self.ffprobe } else { &self.ffmpeg });
        cmd.args(limited_args(probe, args, self.threads()))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000 | 0x00004000 | 0x00000004);
        }
        let mut child = cmd.spawn().map_err(|e| format!("PROCESS_START: {e}"))?;
        let group = match ProcessGroup::attach(&child) {
            Ok(v) => v,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        };
        Ok((child, group, reservation))
    }
    pub fn run(
        &self,
        probe: bool,
        args: &[String],
        limit: usize,
    ) -> Result<(Vec<u8>, String), String> {
        self.run_inner(probe, args, limit as u64, None)
    }
    /// Only a newly created task-owned file is removed on failure. Existing files
    /// are never opened with truncate; quota/active-file ownership stays with caller.
    pub fn run_to_file(
        &self,
        probe: bool,
        args: &[String],
        output: &Path,
        limit: u64,
    ) -> Result<String, String> {
        self.check()?;
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output)
            .map_err(|e| format!("TEMP_FILE: {e}"))?;
        let result = self
            .run_inner(probe, args, limit, Some(file))
            .map(|(_, log)| log);
        if result.is_err() {
            if let Err(e) = std::fs::remove_file(output) {
                (self.progress)(
                    &format!("WARN: 临时文件清理失败 {}：{e}", output.display()),
                    None,
                );
            }
        }
        result
    }
    fn run_inner(
        &self,
        probe: bool,
        args: &[String],
        limit: u64,
        mut file: Option<std::fs::File>,
    ) -> Result<(Vec<u8>, String), String> {
        let _memory = if file.is_none() {
            Some(self.reserve_memory(limit, "媒体命令输出缓冲")?)
        } else {
            Some(self.reserve_memory(128 * 1024, "流式媒体输出缓冲")?)
        };
        let (mut child, group, _reservation) = self.spawn(probe, args)?;
        let mut stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let failed = Arc::new(AtomicBool::new(false));
        let out_failed = failed.clone();
        let runner = self.clone();
        let index = probe && args.iter().any(|v| v == "-show_frames");
        let out = thread::spawn(move || {
            let result = (|| -> Result<Vec<u8>, String> {
                let mut data = Vec::new();
                let mut buffer = [0u8; 65536];
                let mut written = 0u64;
                let mut progress_line = Vec::with_capacity(256);
                loop {
                    runner.check()?;
                    let n = stdout
                        .read(&mut buffer)
                        .map_err(|e| format!("PROCESS_READ: {e}"))?;
                    if n == 0 {
                        break;
                    }
                    written = written
                        .checked_add(n as u64)
                        .ok_or("进程输出超过安全上限")?;
                    if written > limit {
                        return Err("进程输出超过安全上限".into());
                    }
                    if let Some(f) = &mut file {
                        f.write_all(&buffer[..n])
                            .map_err(|e| format!("RESOURCE_DISK: 临时文件写入失败：{e}"))?;
                    } else {
                        data.try_reserve(n).map_err(|_| {
                            "RESOURCE_MEMORY: 当前可用内存不足，请关闭部分应用后重试"
                        })?;
                        data.extend_from_slice(&buffer[..n]);
                    }
                    if index {
                        for &byte in &buffer[..n] {
                            if byte == b'\n' {
                                if let Ok(line) = std::str::from_utf8(&progress_line) {
                                    if let Some(t) =
                                        line.split(',').next().and_then(|v| v.parse::<f64>().ok())
                                    {
                                        runner.tick((t * 1_000_000.).round() as i64);
                                    }
                                }
                                progress_line.clear();
                            } else if progress_line.len() < 256 {
                                progress_line.push(byte);
                            }
                        }
                    }
                }
                if let Some(f) = &mut file {
                    f.flush()
                        .map_err(|e| format!("RESOURCE_DISK: 临时文件写入失败：{e}"))?;
                }
                Ok(data)
            })();
            if result.is_err() {
                out_failed.store(true, Ordering::Relaxed);
            }
            result
        });
        let runner = self.clone();
        let err = thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut tail = String::new();
            let mut last = std::time::Instant::now() - Duration::from_secs(1);
            while let Ok(Some(line)) = bounded_line(&mut reader) {
                if let Some(v) = line
                    .strip_prefix("out_time_us=")
                    .and_then(|v| v.parse().ok())
                {
                    runner.tick(v);
                    if last.elapsed() >= Duration::from_millis(200) {
                        (runner.progress)("render", Some(v));
                        last = std::time::Instant::now();
                    }
                }
                append_tail(&mut tail, &line);
            }
            tail
        });
        let status = self.wait_child(&mut child, &group, &failed);
        // Closing the Job Object also reaps descendants holding inherited pipes.
        drop(group);
        let bytes = out
            .join()
            .map_err(|_| "读取输出线程异常".to_string())
            .and_then(|v| v);
        let log = err.join().unwrap_or_default();
        if let Err(e) = &bytes {
            if e.starts_with("RESOURCE_") {
                self.resource.fail(e.clone());
            }
        }
        if !log.is_empty() && (status.as_ref().is_ok_and(|s| !s.success()) || log.to_ascii_lowercase().contains("memory")) {
            let detail: String = log.chars().rev().take(6000).collect::<String>().chars().rev().collect();
            (self.progress)(&format!("PROCESS_ERROR: exit={status:?}; {detail}"), None);
        }
        self.resource.media_error(&log);
        self.check()?;
        let status = status?;
        let bytes = bytes?;
        if !status.success() {
            return Err(format!(
                "MEDIA_PROCESS {status}: {}",
                log.chars()
                    .rev()
                    .take(6000)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect::<String>()
            ));
        }
        Ok((bytes, log))
    }
    fn wait_child(
        &self,
        child: &mut Child,
        group: &ProcessGroup,
        failed: &AtomicBool,
    ) -> Result<std::process::ExitStatus, String> {
        let mut stopping = None;
        let mut overdue_reported = false;
        loop {
            if self.check().is_err() || failed.load(Ordering::Relaxed) {
                if stopping.is_none() {
                    stopping = Some(std::time::Instant::now());
                    group.terminate(child);
                }
            }
            if !overdue_reported
                && stopping.is_some_and(|start| start.elapsed() >= Duration::from_secs(5))
            {
                self.resource.cancellation_overdue();
                overdue_reported = true;
                // Retry only the owned job tree; the truthful cancelling state
                // remains until the operating system confirms its exit.
                group.terminate(child);
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    self.record_io(child);
                    if let Some(start) = stopping {
                        self.resource
                            .cancellation_elapsed(start.elapsed().as_millis() as u64);
                    }
                    return Ok(status);
                }
                Ok(None) => thread::sleep(Duration::from_millis(30)),
                Err(e) => {
                    group.terminate(child);
                    let _ = child.wait();
                    return Err(format!("PROCESS_WAIT: {e}"));
                }
            }
        }
    }
    pub fn ff(&self, args: Vec<String>) -> Result<String, String> {
        self.run(false, &args, 16 * 1024 * 1024).map(|(_, log)| log)
    }
    fn record_io(&self, child: &Child) {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            #[repr(C)]
            #[derive(Default)]
            struct Counters {
                reads: u64,
                writes: u64,
                others: u64,
                read_bytes: u64,
                write_bytes: u64,
                other_bytes: u64,
            }
            #[link(name = "kernel32")]
            unsafe extern "system" {
                fn GetProcessIoCounters(
                    handle: *mut std::ffi::c_void,
                    counters: *mut Counters,
                ) -> i32;
            }
            let mut counters = Counters::default();
            if unsafe { GetProcessIoCounters(child.as_raw_handle(), &mut counters) } != 0 {
                let phase = self.metric_state.lock().unwrap().phase.clone();
                (self.progress)(
                    &format!(
                        "PROCESS_IO: {}",
                        serde_json::json!({"pid":child.id(),"phase":phase,"readBytes":counters.read_bytes,"writeBytes":counters.write_bytes,"otherBytes":counters.other_bytes,"scope":"process-logical-io-including-pipes"})
                    ),
                    None,
                );
            }
        }
    }
    pub fn frames(
        &self,
        args: &[String],
        size: usize,
        mut consume: impl FnMut(usize, &[u8]) -> Result<(), String>,
    ) -> Result<(), String> {
        self.frames_inner(args, size, false, |i, _, frame| consume(i, frame))
    }
    /// Decode pixels and their actual presentation timestamps in one pass.
    /// The caller ends its filter with `showinfo@arkcut_pts=checksum=0`, after
    /// setting a 1/1000000 time base, and enables info logging with no stats.
    pub fn frames_with_timestamps(
        &self,
        args: &[String],
        size: usize,
        mut consume: impl FnMut(usize, i64, &[u8]) -> Result<(), String>,
    ) -> Result<(), String> {
        self.frames_inner(args, size, true, |i, pts, frame| {
            consume(i, pts.ok_or("解码帧缺少实际 PTS")?, frame)
        })
    }
    fn frames_inner(
        &self,
        args: &[String],
        size: usize,
        timed: bool,
        mut consume: impl FnMut(usize, Option<i64>, &[u8]) -> Result<(), String>,
    ) -> Result<(), String> {
        if size == 0 {
            return Err("解码帧尺寸无效".into());
        }
        let _memory = self.reserve_memory((size as u64).saturating_mul(4), "rawFrames")?;
        let mut command = args.to_vec();
        let mut frame_progress = None;
        if timed {
            if let Some(i) = command.windows(2).position(|v| v[0] == "-progress" && v[1] == "pipe:2") {
                let progress = FrameProgress::new(self)?;
                command[i + 1] = progress.temp.path().to_string_lossy().into_owned();
                frame_progress = Some(progress);
            }
        }
        let (mut child, group, _reservation) = self.spawn(false, &command)?;
        let mut output = child.stdout.take().unwrap();
        let err = child.stderr.take().unwrap();
        let (pts_sender, pts_receiver) = std::sync::mpsc::sync_channel(256);
        let metadata_error = Arc::new(std::sync::Mutex::new(None::<String>));
        let log_error = metadata_error.clone();
        let progress_runner=self.clone();
        let log = thread::spawn(move || {
            let mut tail = String::new();
            let mut timestamps = 0;
            let mut stderr = BufReader::new(err);
            while let Ok(Some(line)) = bounded_line(&mut stderr) {
                if let Some(t)=line.strip_prefix("out_time_us=").and_then(|v|v.parse::<i64>().ok()){progress_runner.tick(t);}
                if timed && line.contains("showinfo@arkcut_pts") && line.contains(" n:") {
                    let pts = line
                        .split_once("pts:")
                        .and_then(|(_, rest)| rest.split_whitespace().next())
                        .and_then(|value| value.parse::<i64>().ok());
                    if let Some(pts) = pts {
                        // Never block stderr on metadata: a full channel signals
                        // a decoder/encoder contract failure and we keep draining
                        // both pipes while the owner cancels the child.
                        if let Err(error) = pts_sender.try_send((timestamps, pts)) {
                            *log_error.lock().unwrap() =
                                Some(format!("PTS 元数据管道异常：{error}"));
                        }
                        timestamps += 1;
                    } else {
                        *log_error.lock().unwrap() = Some("解码器返回无效 PTS".into());
                    }
                }
                append_tail(&mut tail, &line);
            }
            (tail, timestamps)
        });
        let (sender, receiver) =
            std::sync::mpsc::sync_channel::<Result<Option<Vec<u8>>, String>>(2);
        let reader = thread::spawn(move || loop {
            let message = (|| {
                let mut frame = Vec::new();
                frame.try_reserve_exact(size).map_err(|_| {
                    "RESOURCE_MEMORY: 当前可用内存不足，请关闭部分应用后重试".to_string()
                })?;
                frame.resize(size, 0);
                let mut bytes = 0;
                while bytes < size {
                    match output.read(&mut frame[bytes..]) {
                        Ok(0) if bytes == 0 => return Ok(None),
                        Ok(0) => return Err(format!("解码帧数据截断：{bytes} / {size}")),
                        Ok(n) => bytes += n,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(e) => return Err(e.to_string()),
                    }
                }
                Ok(Some(frame))
            })();
            let last = !matches!(&message, Ok(Some(_)));
            if sender.send(message).is_err() || last {
                break;
            }
        });
        let mut i = 0;
        let result = (|| {
            loop {
                self.check()?;
                if let Some(progress) = &mut frame_progress { progress.poll(self)?; }
                match receiver.recv_timeout(Duration::from_millis(50)) {
                    Ok(Ok(Some(frame))) => {
                        let pts = if timed {
                            let wait_start = std::time::Instant::now();
                            Some(loop {
                                self.check()?;
                                if let Some(error) = metadata_error.lock().unwrap().as_ref() {
                                    return Err(error.clone());
                                }
                                match pts_receiver.recv_timeout(Duration::from_millis(50)) {
                                    Ok((index, pts)) if index == i => break pts,
                                    Ok(_) => return Err("实际 PTS 与解码帧序号不一致".into()),
                                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                                        if wait_start.elapsed() >= Duration::from_secs(5) {
                                            return Err("解码帧实际 PTS 等待超时".into());
                                        }
                                    }
                                    Err(_) => return Err("解码帧缺少实际 PTS".into()),
                                }
                            })
                        } else {
                            None
                        };
                        consume(i, pts, &frame)?;
                        i += 1;
                    }
                    Ok(Ok(None)) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    Ok(Err(e)) => return Err(e),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                }
            }
            Ok(())
        })();
        if result.is_err() {
            group.terminate(&mut child);
        }
        drop(receiver);
        let status = self.wait_child(&mut child, &group, &AtomicBool::new(result.is_err()));
        drop(group);
        let _ = reader.join();
        let (log, timestamps) = log.join().unwrap_or_default();
        if let Err(e) = &result {
            if e.starts_with("RESOURCE_") {
                self.resource.fail(e.clone());
            }
        }
        self.resource.media_error(&log);
        self.check()?;
        result?;
        let status = status?;
        if !status.success() {
            return Err(format!("FRAME_DECODE: {log}"));
        }
        if let Some(progress) = &mut frame_progress { progress.poll(self)?; }
        if let Some(error) = metadata_error.lock().unwrap().as_ref() {
            return Err(error.clone());
        }
        if timed && timestamps != i {
            return Err(format!("实际 PTS 数量与解码帧数不一致：{timestamps} / {i}"));
        }
        Ok(())
    }
}
/// ffmpeg options are scoped to inputs/outputs: cap each explicit option and
/// provide a decoder cap before every input plus a cap for the final encoder.
fn limited_args(probe: bool, input: &[String], threads: usize) -> Vec<String> {
    let cap = threads.clamp(1, crate::resource::MAX_MEDIA_THREADS);
    let filter = cap.min(2);
    let mut result = Vec::with_capacity(input.len() + 16);
    let mut i = 0;
    if !probe && input.iter().any(|v| v == "-i") {
        result.extend(args(&[
            "-filter_threads",
            &filter.to_string(),
            "-filter_complex_threads",
            &filter.to_string(),
        ]));
    }
    while i < input.len() {
        let name = &input[i];
        if [
            "-threads",
            "-threads:v",
            "-threads:a",
            "-filter_threads",
            "-filter_complex_threads",
        ]
        .contains(&name.as_str())
            && i + 1 < input.len()
        {
            let max = if name.starts_with("-filter") {
                filter
            } else {
                cap
            };
            let requested = input[i + 1].parse::<usize>().unwrap_or(max);
            let value = if requested == 0 {
                max
            } else {
                requested.min(max)
            };
            result.push(name.clone());
            result.push(value.to_string());
            i += 2;
            continue;
        }
        if name == "-i" {
            result.extend(args(&["-threads", &cap.to_string()]));
        }
        result.push(name.clone());
        i += 1;
    }
    if !probe && input.iter().any(|v| v == "-i") && !result.is_empty() {
        let pos = result.len() - 1;
        result.splice(pos..pos, args(&["-threads:v", &cap.to_string()]));
    }
    result
}
fn bounded_line(reader: &mut impl BufRead) -> std::io::Result<Option<String>> {
    let mut line = Vec::with_capacity(256);
    let mut seen = false;
    loop {
        let data = reader.fill_buf()?;
        if data.is_empty() {
            return Ok(if seen {
                Some(String::from_utf8_lossy(&line).into_owned())
            } else {
                None
            });
        }
        seen = true;
        let end = data.iter().position(|&v| v == b'\n');
        let used = end.map(|n| n + 1).unwrap_or(data.len());
        let content = end.unwrap_or(used);
        let keep = content.min(16384usize.saturating_sub(line.len()));
        line.extend_from_slice(&data[..keep]);
        reader.consume(used);
        if end.is_some() {
            return Ok(Some(
                String::from_utf8_lossy(&line)
                    .trim_end_matches('\r')
                    .to_string(),
            ));
        }
    }
}
fn append_tail(tail: &mut String, line: &str) {
    if tail.len() + line.len() > 65536 {
        let mut start = tail.len().saturating_sub(32768);
        while !tail.is_char_boundary(start) {
            start += 1;
        }
        tail.drain(..start);
    }
    tail.push_str(line);
    tail.push('\n');
}
#[cfg(windows)]
struct ProcessGroup(crate::resource::platform::ProcessGroup);
#[cfg(windows)]
impl ProcessGroup {
    fn attach(child: &Child) -> Result<Self, String> {
        crate::resource::platform::ProcessGroup::attach(child).map(Self)
    }
    fn terminate(&self, _child: &mut Child) {
        self.0.terminate();
    }
}
#[cfg(not(windows))]
struct ProcessGroup;
#[cfg(not(windows))]
impl ProcessGroup {
    fn attach(_child: &Child) -> Result<Self, String> {
        Ok(Self)
    }
    fn terminate(&self, child: &mut Child) {
        let _ = child.kill();
    }
}
pub fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|x| x.to_string()).collect()
}
#[cfg(test)]
mod frame_tests {
    use super::*;
    fn fixture(filter: &str, count: &str) -> Vec<String> {
        args(&[
            "-v",
            "info",
            "-nostats",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=8x8:rate=25",
            "-vf",
            &format!("trim=end_frame={count},{filter}"),
            "-fps_mode",
            "passthrough",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "gray",
            "pipe:1",
        ])
    }
    #[test]
    fn timed_pixels_match_actual_pts_and_cancel_cleanly() {
        let r = Runner::discover(Arc::new(|_, _| {})).unwrap();
        let mut actual = vec![];
        r.frames_with_timestamps(
            &fixture(
                "settb=1/1000000,setpts=PTS+123,format=gray,showinfo@arkcut_pts=checksum=0",
                "6",
            ),
            64,
            |index, pts, bytes| {
                actual.push((index, pts, bytes.len()));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            actual,
            (0..6)
                .map(|i| (i, i as i64 * 40000 + 123, 64))
                .collect::<Vec<_>>()
        );
        let cancel = r.cancel.clone();
        let error = r
            .frames_with_timestamps(
                &fixture(
                    "settb=1/1000000,format=gray,showinfo@arkcut_pts=checksum=0",
                    "600",
                ),
                64,
                |index, _, _| {
                    if index == 1 {
                        cancel.store(true, Ordering::Relaxed);
                    }
                    Ok(())
                },
            )
            .unwrap_err();
        assert!(error.starts_with("CANCELLED"), "{error}");
    }
    #[test]
    fn partial_raw_frame_is_an_error() {
        let r = Runner::discover(Arc::new(|_, _| {})).unwrap();
        let error = r
            .frames(&fixture("format=gray", "1"), 65, |_, _| Ok(()))
            .unwrap_err();
        assert!(error.contains("截断"), "{error}");
    }
    #[test]
    fn timed_frames_keep_progress_separate_and_clean_it_after_cancel() {
        let mut r = Runner::discover(Arc::new(|_, _| {})).unwrap();
        let values = Arc::new(std::sync::Mutex::new(Vec::new()));
        let collected = values.clone();
        r.metrics = Arc::new(move |m| collected.lock().unwrap().push(m.completed_units));
        r.measure("outputValidate", 0, Some(48_000_000), "us", "check");
        let mut command = args(&["-stats_period", "0.001", "-progress", "pipe:2"]);
        command.extend(fixture("settb=1/1000000,format=gray,showinfo@arkcut_pts=checksum=0", "1200"));
        let mut count = 0;
        r.frames_with_timestamps(&command, 64, |index, pts, _| {
            assert_eq!(pts, index as i64 * 40_000);
            count += 1;
            Ok(())
        }).unwrap();
        assert_eq!(count, 1200);
        assert!(values.lock().unwrap().iter().any(|&v| v > 0));
        assert!(values.lock().unwrap().windows(2).all(|v| v[1] >= v[0]));
        let r = Runner::discover(Arc::new(|_, _| {})).unwrap();
        let cancel = r.cancel.clone();
        let error = r.frames_with_timestamps(&command, 64, |index, _, _| {
            if index == 5 { cancel.store(true, Ordering::Relaxed); }
            Ok(())
        }).unwrap_err();
        assert!(error.starts_with("CANCELLED"), "{error}");
        assert!(!std::fs::read_dir(crate::storage::index_dir().unwrap()).unwrap()
            .filter_map(Result::ok).any(|e| e.file_name().to_string_lossy().starts_with(".arkcut-frame-progress-")));
    }
}

#[cfg(test)]
mod process_policy_tests {
    use super::*;
    #[test]
    fn thread_limits_apply_to_each_input_output_and_filters() {
        let input = args(&[
            "-threads",
            "0",
            "-i",
            "a.mp4",
            "-threads",
            "8",
            "-i",
            "b.wav",
            "-filter_complex_threads",
            "20",
            "-threads:v",
            "12",
            "out.mp4",
        ]);
        let result = limited_args(false, &input, 2);
        for pair in result.windows(2) {
            if pair[0].starts_with("-threads") || pair[0].starts_with("-filter_") {
                assert!(pair[1].parse::<usize>().unwrap() <= 2);
            }
        }
        assert_eq!(result.last().unwrap(), "out.mp4");
        let probe = limited_args(true, &args(&["-threads", "8", "-show_frames", "a.mp4"]), 1);
        assert_eq!(probe[1], "1");
        assert_eq!(probe.last().unwrap(), "a.mp4");
        let available = limited_args(false, &args(&["-i", "a.mp4", "out.mp4"]), 16);
        assert!(available
            .windows(2)
            .any(|p| p[0] == "-threads" && p[1] == "16"));
        assert!(available
            .windows(2)
            .any(|p| p[0] == "-filter_threads" && p[1] == "2"));
    }
    #[test]
    fn oversized_stderr_line_stays_bounded_and_next_line_survives() {
        let mut input = vec![b'x'; 100000];
        input.extend_from_slice(b"\npts: 123\n");
        let mut reader = BufReader::new(&input[..]);
        assert_eq!(bounded_line(&mut reader).unwrap().unwrap().len(), 16384);
        assert_eq!(bounded_line(&mut reader).unwrap().unwrap(), "pts: 123");
        assert!(bounded_line(&mut reader).unwrap().is_none());
    }
}
