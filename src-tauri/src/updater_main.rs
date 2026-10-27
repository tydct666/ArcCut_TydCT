#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use arkcut_core::update::{platform, protocol::*, transaction, VERSION};
use std::{path::PathBuf, process::Command};
fn run() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 5 || !["--install", "--recover"].contains(&args[1].as_str()) {
        return Err("请从 ArkCut 发起更新，或按恢复说明运行助手".into());
    }
    let root = std::fs::canonicalize(PathBuf::from(&args[2])).map_err(|e| e.to_string())?;
    no_links(&root)?;
    let dir = transaction::control(&root)?;
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    if executable
        .parent()
        .and_then(|p| std::fs::canonicalize(p).ok())
        != Some(std::fs::canonicalize(&dir).map_err(|e| e.to_string())?)
    {
        return Err("更新助手必须从本程序的更新暂存目录运行".into());
    }
    platform::idle_priority();
    let pid = args[3].parse::<u32>().map_err(|e| e.to_string())?;
    platform::wait_parent(pid, &dir, &args[4])?;
    let config = Config::embedded()?;
    let result = (|| {
        if args[1] == "--recover" {
            transaction::recover(&root, &config)
        } else {
            let (envelope, r) = verify(
                &read_bounded(&dir.join("offer.json"), MAX_INDEX)?,
                &config,
                Some(VERSION),
            )?;
            transaction::install(&root, &config, envelope, &r, |r, t| {
                transaction::health_process(&root, r, t)
            })
        }
    })();
    let detail = match &result {
        Ok(()) => "更新事务已完成".into(),
        Err(e) => e.clone(),
    };
    platform::json(
        &dir.join("last-result.json"),
        &serde_json::json!({"success":result.is_ok(),"message":detail}),
    )?;
    let recovered = transaction::journal(&root)?
        .is_none_or(|j| ["committed", "rolledBack"].contains(&j.state.as_str()));
    if recovered {
        let mut launch = Command::new(root.join("ArkCut.exe"));
        platform::hidden(&mut launch);
        launch
            .spawn()
            .map_err(|e| format!("程序已恢复，但自动启动失败：{e}"))?;
    }
    result
}
fn main() {
    if let Err(error) = run() {
        if let Some(root) = std::env::args().nth(2) {
            let p = PathBuf::from(root).join(arkcut_core::update::CONTROL);
            if p.is_dir() && no_links(&p).is_ok() {
                let _ = platform::json(
                    &p.join("last-result.json"),
                    &serde_json::json!({"success":false,"message":error}),
                );
            }
        }
        eprintln!("{error}");
        std::process::exit(1);
    }
}
