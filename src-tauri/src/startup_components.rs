//! Native preparation UI: no browser, consent gate or shell window is needed.
use arkcut_core::{components as core, update::platform};
use std::{sync::{Arc,Mutex,atomic::{AtomicBool,Ordering}},path::{Path,PathBuf},process::Command,time::{Duration,Instant}};
use windows_sys::Win32::{UI::{Controls::*,WindowsAndMessaging::{SendMessageW,ShowWindow,SW_SHOW}},Foundation::HWND};
type Result<T>=std::result::Result<T,String>;
fn wide(s:&str)->Vec<u16>{s.encode_utf16().chain(Some(0)).collect()}
pub fn web_ready()->bool{
    #[cfg(feature="component-test-hooks")]
    if std::env::var("ARKCUT_COMPONENT_TEST_NO_WEB").as_deref()==Ok("1"){return false}
    tauri::webview_version().ok().is_some_and(|v|v.split('.').next().and_then(|n|n.parse::<u32>().ok()).is_some_and(|n|n>=core::spec().webview2.minimum_major)&&!v.contains([' ','-']))
}
pub fn ready()->Result<PathBuf>{if !web_ready(){return Err("界面组件尚未准备好".into())}core::media_ready()}
fn install_web(base:&Path,c:&AtomicBool,p:&core::Progress)->Result<()>{
    if web_ready(){return Ok(())}
    #[cfg(feature="component-test-hooks")]
    if std::env::var("ARKCUT_COMPONENT_TEST_WEB_FAILURE").as_deref()==Ok("1") {
        return Err("界面组件准备失败（模拟缺少运行时），请重试".into());
    }
    let s=core::spec().webview2;
    let _lock=platform::lock(&base.join("webview-prepare.lock"))?;
    if web_ready(){return Ok(())}
    platform::space(base,350*1024*1024).map_err(|_|"界面组件准备空间不足，请腾出至少 900 MB 后重试")?;
    let setup=base.join("MicrosoftEdgeWebview2Setup.exe");
    arkcut_core::update::protocol::no_links(&setup)?;
    if setup.exists(){std::fs::remove_file(&setup).map_err(|_|"界面组件安装文件被占用，请稍后重试")?;}
    core::download(&s.url,&setup,s.max_bytes,None,c,p,"正在下载界面组件")?;
    let result=(||{
        core::check_cancel(c)?;p("正在验证界面组件",0,None);
        // Windows validates the full Authenticode chain and file digest. The signer must be Microsoft.
        let ps=std::env::var_os("SystemRoot").map(PathBuf::from).ok_or("无法定位系统验证工具")?.join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let mut command=Command::new(ps);command.args(["-NoProfile","-NonInteractive","-Command","$s=Get-AuthenticodeSignature -LiteralPath $env:ARKCUT_VERIFY_INSTALLER; if($s.Status -eq 'Valid' -and $s.SignerCertificate.Subject -match '(^|, )CN=Microsoft Corporation(,|$)'){exit 0}; exit 1"]).env("ARKCUT_VERIFY_INSTALLER",&setup);
        platform::hidden(&mut command);let mut verifier=command.spawn().map_err(|_|"无法验证微软组件签名")?;
        let start=Instant::now();loop{if c.load(Ordering::Relaxed)||start.elapsed()>Duration::from_secs(60){let _=verifier.kill();let _=verifier.wait();return Err("界面组件验证已取消或超时，请重试".into())}if let Some(status)=verifier.try_wait().map_err(|e|e.to_string())?{if !status.success(){return Err("界面组件不是有效的微软签名文件，已停止安装".into())}break}std::thread::sleep(Duration::from_millis(100));}
        core::check_cancel(c)?;p("正在准备界面组件",0,None);
        let mut command=Command::new(&setup);command.args(["/silent","/install"]);platform::hidden(&mut command);
        let mut child=command.spawn().map_err(|_|"无法启动微软界面组件安装程序")?;
        // Do not terminate a system installer during its transaction. Cancellation stops subsequent app work.
        loop{if c.load(Ordering::Relaxed){p("正在等待系统安装结束，随后退出",0,None)}if let Some(status)=child.try_wait().map_err(|e|e.to_string())?{if !status.success()&&!web_ready(){return Err(format!("界面组件准备失败（{}），请重试",status.code().unwrap_or(-1)))}break}std::thread::sleep(Duration::from_millis(100));}
        core::check_cancel(c)?;if !web_ready(){return Err("系统尚未提供兼容的界面组件，请稍后重试".into())}Ok(())
    })();let _=std::fs::remove_file(setup);result
}
#[derive(Default,Clone)]
struct Progress{stage:String,done:u64,total:Option<u64>,result:Option<Result<()>>}
struct Dialog{progress:Arc<Mutex<Progress>>,cancel:Arc<AtomicBool>,worker:Option<std::thread::JoinHandle<()>>,close:bool,success:bool,last:String,marquee:bool,failed_page:bool,progress_config:*const TASKDIALOGCONFIG,error_config:*const TASKDIALOGCONFIG}
impl Dialog{
    fn start(&mut self){
        if let Some(w)=self.worker.take(){let _=w.join();}self.cancel.store(false,Ordering::Relaxed);*self.progress.lock().unwrap()=Progress{stage:"正在检查运行组件".into(),..Default::default()};
        let state=self.progress.clone();let cancel=self.cancel.clone();
        self.worker=Some(std::thread::spawn(move||{
            let state2=state.clone();let progress:core::Progress=Arc::new(move|stage,done,total|{let mut s=state2.lock().unwrap();s.stage=stage.into();s.done=done;s.total=total;});
            let result=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||{
                let base=core::root()?;core::prepare_media(&base,&core::spec().ffmpeg,&cancel,&progress)?;
                core::check_cancel(&cancel)?;install_web(&base,&cancel,&progress)?;ready()?;Ok(())
            })).unwrap_or_else(|_|Err("组件准备意外中断，请重试".into()));state.lock().unwrap().result=Some(result);
        }));
    }
}
unsafe extern "system" fn callback(hwnd:HWND,msg:u32,wp:usize,_:isize,data:isize)->i32{
    let d=&mut *(data as *mut Dialog);
    if msg==TDN_CREATED as u32{
        // Updater launches child processes without console windows; explicitly show this native UI.
        ShowWindow(hwnd,SW_SHOW);
        d.start();
    }else if msg==TDN_BUTTON_CLICKED as u32{
        if d.close{return 0}
        if wp==101{d.failed_page=false;d.last.clear();d.marquee=false;SendMessageW(hwnd,TDM_NAVIGATE_PAGE as u32,0,d.progress_config as isize);d.start();return 1}
        if d.progress.lock().unwrap().result.is_some(){return 0}
        d.cancel.store(true,Ordering::Relaxed);
        #[cfg(feature="component-test-hooks")]
        if let Some(p)=std::env::var_os("ARKCUT_COMPONENT_TEST_CANCEL_RECORD") {
            let ms=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis();
            let _=std::fs::write(p,ms.to_string());
        }
        return 1
    }else if msg==TDN_TIMER as u32{
        let s=d.progress.lock().unwrap().clone();
        if let Some(Ok(()))=&s.result{d.success=true;d.close=true;SendMessageW(hwnd,TDM_CLICK_BUTTON as u32,2,0);return 0}
        if d.cancel.load(Ordering::Relaxed)&&s.result.is_some(){d.close=true;SendMessageW(hwnd,TDM_CLICK_BUTTON as u32,2,0);return 0}
        let text=if let Some(Err(e))=&s.result{if !d.failed_page{d.failed_page=true;d.last.clear();SendMessageW(hwnd,TDM_NAVIGATE_PAGE as u32,0,d.error_config as isize);return 0;}e.clone()}
        else if let Some(total)=s.total.filter(|n|*n>0){format!("{}\n{:.1} / {:.1} MB",s.stage,s.done as f64/1e6,total as f64/1e6)}else{s.stage.clone()};
        if text!=d.last{SendMessageW(hwnd,TDM_SET_ELEMENT_TEXT as u32,TDE_CONTENT as usize,wide(&text).as_ptr() as isize);d.last=text;}
        let marquee=s.total.is_none()&&s.result.is_none();if marquee!=d.marquee{SendMessageW(hwnd,TDM_SET_MARQUEE_PROGRESS_BAR as u32,marquee as usize,0);SendMessageW(hwnd,TDM_SET_PROGRESS_BAR_MARQUEE as u32,marquee as usize,30);d.marquee=marquee;}
        if let Some(n)=s.total.filter(|n|*n>0){SendMessageW(hwnd,TDM_SET_PROGRESS_BAR_POS as u32,((s.done.saturating_mul(100)/n).min(100)) as usize,0);}
    }0
}
pub fn ensure(health:bool)->Result<PathBuf>{
    // Ignore inherited browser overrides; only accept the pinned local bundle.
    std::env::remove_var("WEBVIEW2_BROWSER_EXECUTABLE_FOLDER");
    std::env::set_var("WEBVIEW2_RELEASE_CHANNELS","0");
    let exe=std::env::current_exe().map_err(|e|e.to_string())?;
    let base=exe.parent().ok_or("无法定位程序目录")?;
    if let Some(media)=core::offline_media_at(base)? {
        std::env::set_var("WEBVIEW2_BROWSER_EXECUTABLE_FOLDER",base.join("WebView2"));
        std::env::set_var("ARKCUT_MEDIA_DIR",&media);
        return Ok(media);
    }
    if let Ok(p)=ready(){return Ok(p)}if health{return Err("健康检查失败：运行组件缺失或损坏，请正常启动后重试更新".into())}
    let mut d=Box::new(Dialog{progress:Arc::new(Mutex::new(Progress::default())),cancel:Arc::new(AtomicBool::new(false)),worker:None,close:false,success:false,last:String::new(),marquee:false,failed_page:false,progress_config:std::ptr::null(),error_config:std::ptr::null()});
    let title=wide("ArkCut");let heading=wide("准备运行组件");let content=wide("正在检查运行组件");let retry=wide("重试");let cancel=wide("取消");let exit=wide("退出");
    let buttons=[TASKDIALOG_BUTTON{nButtonID:2,pszButtonText:cancel.as_ptr()}];
    let error_buttons=[TASKDIALOG_BUTTON{nButtonID:101,pszButtonText:retry.as_ptr()},TASKDIALOG_BUTTON{nButtonID:2,pszButtonText:exit.as_ptr()}];
    let config=TASKDIALOGCONFIG{cbSize:std::mem::size_of::<TASKDIALOGCONFIG>() as u32,pszWindowTitle:title.as_ptr(),pszMainInstruction:heading.as_ptr(),pszContent:content.as_ptr(),dwFlags:TDF_CALLBACK_TIMER|TDF_ALLOW_DIALOG_CANCELLATION|TDF_SHOW_PROGRESS_BAR|TDF_CAN_BE_MINIMIZED,cButtons:1,pButtons:buttons.as_ptr(),nDefaultButton:2,pfCallback:Some(callback),lpCallbackData:(&mut *d as *mut Dialog) as isize,cxWidth:300,..Default::default()};
    let error_config=TASKDIALOGCONFIG{dwFlags:TDF_CALLBACK_TIMER|TDF_ALLOW_DIALOG_CANCELLATION|TDF_CAN_BE_MINIMIZED,cButtons:2,pButtons:error_buttons.as_ptr(),..config};
    d.progress_config=&config;d.error_config=&error_config;
    let hr=unsafe{TaskDialogIndirect(&config,std::ptr::null_mut(),std::ptr::null_mut(),std::ptr::null_mut())};d.cancel.store(true,Ordering::Relaxed);if let Some(w)=d.worker.take(){let _=w.join();}
    if hr<0{return Err(format!("无法打开组件准备窗口（{hr}）"))}if d.success{ready()}else{Err("已退出组件准备".into())}
}
