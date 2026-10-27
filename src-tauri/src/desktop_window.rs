use arkcut_core::storage;
use serde::{Deserialize, Serialize};
use tauri::{LogicalSize, Manager, PhysicalPosition, WebviewWindow};

#[derive(Serialize, Deserialize)]
struct Geometry {
    width: f64,
    height: f64,
    maximized: bool,
}
fn path() -> std::path::PathBuf {
    storage::root().join("window.json")
}
pub fn restore(window: &WebviewWindow) -> tauri::Result<()> {
    let saved = std::fs::read(path())
        .ok()
        .and_then(|v| serde_json::from_slice::<Geometry>(&v).ok());
    let monitor = window.current_monitor()?.or(window.primary_monitor()?);
    let (available_w, available_h) = monitor
        .as_ref()
        .map(|m| {
            let area = m.work_area();
            let scale = m.scale_factor();
            (
                area.size.width as f64 / scale - 16.,
                area.size.height as f64 / scale - 48.,
            )
        })
        .unwrap_or((1440., 960.));
    window.set_min_size(Some(LogicalSize::new(
        880f64.min(available_w),
        560f64.min(available_h),
    )))?;
    let width = saved
        .as_ref()
        .map(|s| s.width)
        .unwrap_or(1720.)
        .clamp(480., available_w.max(480.));
    let height = saved
        .as_ref()
        .map(|s| s.height)
        .unwrap_or(960.)
        .clamp(360., available_h.max(360.));
    window.set_size(LogicalSize::new(width, height))?;
    if let Some(m) = monitor {
        let area = m.work_area();
        let size = window.outer_size()?;
        window.set_position(PhysicalPosition::new(
            area.position.x + (area.size.width.saturating_sub(size.width) / 2) as i32,
            area.position.y + (area.size.height.saturating_sub(size.height) / 2) as i32,
        ))?;
    }
    if saved.is_some_and(|s| s.maximized) {
        window.maximize()?;
    }
    window.show()?;
    Ok(())
}
pub fn remember(window: &tauri::Window) -> Result<(), String> {
    if window.is_minimized().map_err(|e| e.to_string())? {
        return Ok(());
    }
    let maximized = window.is_maximized().map_err(|e| e.to_string())?;
    let mut value = std::fs::read(path())
        .ok()
        .and_then(|v| serde_json::from_slice::<Geometry>(&v).ok())
        .unwrap_or(Geometry {
            width: 1440.,
            height: 960.,
            maximized: false,
        });
    if !maximized {
        let scale = window.scale_factor().map_err(|e| e.to_string())?;
        let size = window.inner_size().map_err(|e| e.to_string())?;
        if size.width > 0 && size.height > 0 {
            value.width = size.width as f64 / scale;
            value.height = size.height as f64 / scale;
        }
    }
    value.maximized = maximized;
    std::fs::create_dir_all(storage::root()).map_err(|e| e.to_string())?;
    storage::atomic(
        &path(),
        &serde_json::to_vec(&value).map_err(|e| e.to_string())?,
    )
}
pub fn log_geometry(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _=app.state::<arkcut_core::Engine>().request("clientLog",serde_json::json!({"stage":"window","message":format!("窗口初始化：客户区 {:?}，DPI 比例 {:?}",w.inner_size(),w.scale_factor())}));
    }
}
