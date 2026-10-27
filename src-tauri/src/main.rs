#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use arkcut_core::Engine;
use arkcut_core::update::{self,Updater,protocol::Config};
use tauri::{Manager, State};
mod desktop_window;
mod startup_components;
#[tauri::command]
async fn api(
    app: tauri::AppHandle,
    engine: State<'_, Engine>,
    updater: State<'_, Updater>,
    op: String,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let engine = engine.inner().clone();
    let updater=updater.inner().clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let _gate=updater.gate.lock().unwrap();
        if updater.status().status=="installing"&&!matches!(op.as_str(),"status"|"clientLog"){return Err("程序正在准备更新，请稍候".into())}
        engine.request(&op, payload)
    })
        .await
        .map_err(|e| e.to_string())??;
    fn grant(app: &tauri::AppHandle, v: &serde_json::Value) {
        match v {
            serde_json::Value::Object(m) => {
                for (k, value) in m {
                    if k == "path" {
                        if let Some(p) = value.as_str() {
                            if std::path::Path::new(p).is_file() {
                                let _ = app.asset_protocol_scope().allow_file(p);
                            }
                        }
                    }
                    grant(app, value)
                }
            }
            serde_json::Value::Array(a) => {
                for value in a {
                    grant(app, value)
                }
            }
            _ => {}
        }
    }
    grant(&app, &result);
    Ok(result)
}
#[tauri::command]
async fn update_api(engine:State<'_,Engine>,updater:State<'_,Updater>,op:String)->Result<update::client::Status,String>{
 let updater=updater.inner().clone();let engine=engine.inner().clone();
 tauri::async_runtime::spawn_blocking(move||{let result=match op.as_str(){
  "status"=>Ok(updater.status()),"check"=>updater.check(false),"automatic"=>if engine.busy(){Ok(updater.status())}else{updater.check(true)},
  "download"=>updater.download(),"cancel"=>Ok(updater.cancel()),"install"=>updater.prepare_install(||engine.busy()),_=>Err("未知更新操作".into())
 };if let Err(e)=&result{updater.report_action_error(e);}result}).await.map_err(|e|e.to_string())?
}
#[tauri::command]fn update_health_mode()->bool{update::client::health_token().is_some()}
#[tauri::command]fn update_health_ready(updater:State<'_,Updater>)->Result<(),String>{update::client::health_ready(updater.root())}
fn main() {
    let exe=std::env::current_exe().expect("无法定位程序");let root=exe.parent().unwrap().to_path_buf();
    if std::env::args().any(|a|a=="--prepare-components") {
        let args:Vec<_>=std::env::args().collect();let spec=arkcut_core::components::spec();
        for (key,value) in [("--component-ffmpeg",&spec.ffmpeg.version),("--component-webview2",&spec.webview2.version)] {
            if let Some(i)=args.iter().position(|a|a==key){if args.get(i+1)!=Some(value){std::process::exit(2)}}
        }
        std::process::exit(if startup_components::ensure(false).is_ok(){0}else{1});
    }
    let health=update::client::health_token().is_some();
    if !health {if let Ok(Some(j))=update::transaction::journal(&root){if !["committed","rolledBack"].contains(&j.state.as_str()){
        match update::client::start_helper(&root,"--recover"){Ok(())=>return,Err(e)=>{eprintln!("更新恢复尚未完成：{e}。请运行 .arkcut-update 中的恢复更新.cmd");return}}
    }}}
    if health {
        if update::client::validate_health(&root).is_err(){return}
        let base=root.join(update::CONTROL).join("health-data");std::env::set_var("ARKCUT_DATA_DIR",&base);
        for child in ["cache","archives"]{if std::fs::create_dir_all(base.join(child)).is_err(){return}}
        let settings=arkcut_core::storage::Settings{archive_directory:Some(base.join("archives").to_string_lossy().into()),cache_directory:Some(base.join("cache").to_string_lossy().into()),cache_limit:128*1024*1024};
        if arkcut_core::storage::save_settings(&settings).is_err(){return}
    }
    // Health checks never download, install or display preparation UI.
    if let Err(e)=startup_components::ensure(health){eprintln!("{e}");std::process::exit(1)}
    let _instance=if health{None}else{match update::platform::shared_instance(&root){Ok(f)=>Some(f),Err(e)=>{if update::transaction::control(&root).is_ok(){eprintln!("{e}");return}None}}};
    let updater=Updater::new(root,Config::embedded().expect("更新验证配置无效"));
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(Engine::default())
        .manage(updater)
        .setup(|app| {
            if let Some(window) = app.get_webview_window("main") {
                desktop_window::restore(&window)?;
            }
            desktop_window::log_geometry(app.handle());
            Ok(())
        })
        .on_window_event(|window,event|{
            if matches!(event,tauri::WindowEvent::Resized(_)|tauri::WindowEvent::Moved(_)) {
                if let Err(error)=desktop_window::remember(window) {
                    let _=window.state::<Engine>().request("clientLog",serde_json::json!({"stage":"window","status":"failed","message":format!("记录窗口尺寸失败：{error}")}));
                }
            }
        })
        .invoke_handler(tauri::generate_handler![api,update_api,update_health_mode,update_health_ready])
        .build(tauri::generate_context!())
        .expect("ArkCut 启动失败")
        .run(|app,event|{
            if let tauri::RunEvent::ExitRequested{api,..}=event {
                let engine=app.state::<Engine>().inner().clone();
                if engine.busy() {
                    api.prevent_exit();
                    let _=engine.request("cancel",serde_json::json!({}));
                    let app=app.clone();
                    std::thread::spawn(move||{while engine.busy(){std::thread::sleep(std::time::Duration::from_millis(50));}app.exit(0);});
                }
            }
        });
}
