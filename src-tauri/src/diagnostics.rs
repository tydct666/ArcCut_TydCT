use crate::storage;
use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::Path,
};

pub fn export(target: &Path, job: &str) -> Result<Value, String> {
    if target.exists() {
        return Err("日志目标已存在，请另存新名".into());
    }
    let parent = target.parent().ok_or("无效日志目录")?;
    let temp = storage::temp_file(parent, "diagnostics")?;
    let mut out = fs::File::create(temp.path()).map_err(|e| e.to_string())?;
    writeln!(out, "{}", json!({"product":"ArkCut","jobId":job,"scope":"retained rotated diagnostics","format":"JSONL"})).map_err(|e|e.to_string())?;
    let root = storage::root().join("logs");
    let files = [
        "arkcut.4.log",
        "arkcut.3.log",
        "arkcut.2.log",
        "arkcut.1.log",
        "arkcut.log",
        "resources.2.jsonl",
        "resources.1.jsonl",
        "resources.jsonl",
    ];
    let mut count = 0;
    for name in files {
        let Ok(file) = fs::File::open(root.join(name)) else {
            continue;
        };
        for line in BufReader::new(file).lines() {
            let line = line.map_err(|e| e.to_string())?;
            if job.is_empty()
                || serde_json::from_str::<Value>(&line)
                    .ok()
                    .is_some_and(|x| x["jobId"] == job)
            {
                writeln!(out, "{line}").map_err(|e| e.to_string())?;
                count += 1;
            }
        }
    }
    // Recovery evidence is bounded on disk independently of verbose logs.
    // Include it in the existing diagnostic export, never in ordinary UI rows.
    if job.is_empty() {
        if let Ok(entries) = fs::read_dir(storage::root().join("recovery")) {
            for entry in entries.flatten().take(32) {
                if entry.file_type().is_ok_and(|t| t.is_file())
                    && entry.metadata().is_ok_and(|m| m.len() <= 16 * 1024 * 1024)
                {
                    if let Ok(file) = fs::File::open(entry.path()) {
                        if let Ok(report) =
                            serde_json::from_reader::<_, Value>(BufReader::new(file))
                        {
                            writeln!(out, "{}", json!({"type":"repairEvidence","report":report}))
                                .map_err(|e| e.to_string())?;
                            count += 1;
                        }
                    }
                }
            }
        }
    }
    out.sync_all().map_err(|e| e.to_string())?;
    drop(out);
    if target.exists() {
        return Err("日志目标已存在，请另存新名".into());
    }
    fs::rename(temp.path(), target).map_err(|e| e.to_string())?;
    Ok(json!({"path":target,"records":count}))
}
