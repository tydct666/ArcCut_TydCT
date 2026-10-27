//! User-scoped, content-addressed components. Discovery and health checks never use the network.
use crate::update::{platform, protocol::{self, Result}};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{fs::{self, File}, io::{Read, Write}, path::{Path, PathBuf}, sync::{Arc, atomic::{AtomicBool, Ordering}}, time::Duration};
#[derive(Clone, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct MediaSpec {pub version:String,pub url:String,pub bytes:u64,pub sha256:String,pub ffmpeg_sha256:String,pub ffprobe_sha256:String}
#[derive(Clone, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct WebSpec {pub version:String,pub minimum_major:u32,pub url:String,pub max_bytes:u64}
#[derive(Clone, Deserialize)]
pub struct Spec {pub ffmpeg:MediaSpec,pub webview2:WebSpec}
pub fn spec()->Spec {serde_json::from_str(include_str!("../../release/runtime-lock.json")).expect("invalid embedded component specification")}
pub type Progress=Arc<dyn Fn(&str,u64,Option<u64>)+Send+Sync>;
pub fn root()->Result<PathBuf>{
    #[cfg(feature="component-test-hooks")]
    if let Some(p)=std::env::var_os("ARKCUT_COMPONENT_TEST_ROOT"){return Ok(p.into())}
    std::env::var_os("LOCALAPPDATA").map(|p|PathBuf::from(p).join("ArkCut/components")).ok_or("无法定位用户组件目录".into())
}
pub fn media_path_at(base:&Path,s:&MediaSpec)->PathBuf{base.join(format!("ffmpeg-{}-{}",s.version,&s.sha256[..16]))}
pub fn check_cancel(c:&AtomicBool)->Result<()>{if c.load(Ordering::Relaxed){Err("已取消组件准备".into())}else{Ok(())}}
fn hash(path:&Path,c:&AtomicBool)->Result<String>{
    protocol::no_links(path)?;let mut f=File::open(path).map_err(|_|"组件文件不存在或不可读")?;
    let mut sha=Sha256::new();let mut buf=[0u8;65536];loop {check_cancel(c)?;let n=f.read(&mut buf).map_err(|e|e.to_string())?;if n==0{break}sha.update(&buf[..n]);}Ok(format!("{:x}",sha.finalize()))
}
pub fn verify_media(path:&Path,s:&MediaSpec,c:&AtomicBool)->Result<()>{
    for (name,expected) in [("ffmpeg.exe",&s.ffmpeg_sha256),("ffprobe.exe",&s.ffprobe_sha256)]{
        if hash(&path.join(name),c)?!=*expected{return Err("视频处理组件校验失败，需要重新准备".into())}
    }Ok(())
}
pub fn media_ready()->Result<PathBuf>{let s=spec();let p=media_path_at(&root()?,&s.ffmpeg);verify_media(&p,&s.ffmpeg,&AtomicBool::new(false))?;Ok(p)}
/// A damaged local bundle fails locally instead of downloading replacements.
pub fn offline_media_at(base:&Path)->Result<Option<PathBuf>>{
    let web=base.join("WebView2");if !web.exists(){return Ok(None)}
    protocol::no_links(&web)?;
    let lock:serde_json::Value=serde_json::from_str(include_str!("../../release/offline-runtime.json")).map_err(|e|e.to_string())?;
    let cancel=AtomicBool::new(false);
    for (name,expected) in lock["files"].as_object().ok_or("离线组件清单无效")? {
        if hash(&web.join(name),&cancel)?!=expected.as_str().unwrap_or(""){return Err("离线界面组件校验失败，请重新解压完整包".into())}
    }
    let media=base.join("media");verify_media(&media,&spec().ffmpeg,&cancel)?;Ok(Some(media))
}
pub fn allowed_url(u:&reqwest::Url)->bool{
    u.scheme()=="https"&&u.username().is_empty()&&u.password().is_none()&&u.fragment().is_none()&&matches!(u.host_str(),Some("www.gyan.dev"|"go.microsoft.com"|"msedge.sf.dl.delivery.mp.microsoft.com"))
}
async fn cancellable<T>(f:impl std::future::Future<Output=std::result::Result<T,reqwest::Error>>,c:&AtomicBool)->Result<T>{
    let mut f=std::pin::pin!(f);loop{check_cancel(c)?;match tokio::time::timeout(Duration::from_millis(100),&mut f).await{Ok(r)=>return r.map_err(|_|"组件下载失败，请检查网络后重试".into()),Err(_)=>{}}}
}
pub fn download(url:&str,dest:&Path,limit:u64,exact:Option<(u64,&str)>,c:&AtomicBool,p:&Progress,stage:&str)->Result<()> {
    let u=reqwest::Url::parse(url).map_err(|_|"组件地址无效")?;if !allowed_url(&u){return Err("组件地址不属于许可的 HTTPS 上游".into())}
    protocol::no_links(dest)?;let part=dest.with_extension("part");protocol::no_links(&part)?;
    let result=(||{
        let runtime=tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e|e.to_string())?;
        runtime.block_on(async{
            let client=reqwest::Client::builder().https_only(true).connect_timeout(Duration::from_secs(10)).read_timeout(Duration::from_secs(15)).timeout(Duration::from_secs(1800))
                .redirect(reqwest::redirect::Policy::custom(|a|if a.previous().len()<5&&allowed_url(a.url()){a.follow()}else{a.error("组件重定向来源不受信任")})).build().map_err(|e|e.to_string())?;
            let mut response=cancellable(client.get(u).send(),c).await?.error_for_status().map_err(|_|"组件服务器暂不可用，请稍后重试")?;
            let length=response.content_length();if length.is_some_and(|n|n>limit)||exact.is_some_and(|(n,_)|length.is_some_and(|v|n!=v)){return Err("组件下载长度与锁定版本不符".into())}
            let mut f=File::create(&part).map_err(|_|"组件目录不可写或磁盘空间不足")?;let mut sha=Sha256::new();let mut count=0;
            while let Some(chunk)=cancellable(response.chunk(),c).await?{
                count+=chunk.len() as u64;if count>limit{return Err("组件下载超过大小限制".into())}
                f.write_all(&chunk).map_err(|_|"组件保存失败，请检查磁盘空间")?;sha.update(&chunk);p(stage,count,exact.map(|x|x.0).or(length));
                #[cfg(feature="component-test-hooks")]
                if let Ok(ms)=std::env::var("ARKCUT_COMPONENT_TEST_CHUNK_DELAY_MS").unwrap_or_default().parse::<u64>() {
                    tokio::time::sleep(Duration::from_millis(ms.min(100))).await;
                }
            }
            check_cancel(c)?;if length.is_some_and(|n|n!=count)||exact.is_some_and(|(n,h)|n!=count||format!("{:x}",sha.finalize())!=h){return Err("组件下载不完整或校验失败，请重试".into())}
            f.sync_all().map_err(|e|e.to_string())?;drop(f);fs::rename(&part,dest).map_err(|e|e.to_string())?;Ok(())
        })
    })();if result.is_err(){let _=fs::remove_file(&part);}result
}
fn clean_stage(path:&Path)->Result<()>{
    protocol::no_links(path)?;if !path.exists(){return Ok(())}
    crate::update::transaction::remove_owned_tree(path)
}
pub fn extract_media(archive:&Path,dest:&Path,s:&MediaSpec,c:&AtomicBool,p:&Progress)->Result<()>{
    if fs::metadata(archive).map_err(|e|e.to_string())?.len()!=s.bytes||hash(archive,c)?!=s.sha256{return Err("视频组件归档校验失败".into())}
    let mut z=zip::ZipArchive::new(File::open(archive).map_err(|e|e.to_string())?).map_err(|_|"视频组件归档损坏")?;
    let mut names=std::collections::BTreeSet::new();let mut total=0u64;
    if z.len()>10000{return Err("视频组件条目过多".into())}
    for i in 0..z.len(){let f=z.by_index(i).map_err(|e|e.to_string())?;let raw=f.name().map_err(|e|e.to_string())?;let name=raw.trim_end_matches('/');protocol::safe_relative(name)?;
        if !names.insert(name.to_lowercase())||f.is_symlink()||f.encrypted(){return Err("视频组件归档含不安全条目".into())}
        total=total.checked_add(f.size()).ok_or("组件大小溢出")?;if total>1024*1024*1024{return Err("视频组件展开大小超限".into())}
    }
    fs::create_dir(dest).map_err(|e|e.to_string())?;
    let prefix=format!("ffmpeg-{}-essentials_build/",s.version);
    for (entry,name) in [("bin/ffmpeg.exe","ffmpeg.exe"),("bin/ffprobe.exe","ffprobe.exe"),("LICENSE","LICENSE"),("README.txt","README.txt")]{
        check_cancel(c)?;let mut f=z.by_name(&(prefix.clone()+entry)).map_err(|_|"视频组件缺少必要文件")?;
        let mut out=File::create(dest.join(name)).map_err(|e|e.to_string())?;let mut buf=[0u8;65536];let mut done=0;
        loop{check_cancel(c)?;let n=f.read(&mut buf).map_err(|_|"视频组件解压校验失败")?;if n==0{break}out.write_all(&buf[..n]).map_err(|_|"组件解压失败，请检查磁盘空间")?;done+=n as u64;p("正在准备视频处理组件",done,Some(f.size()));}out.sync_all().map_err(|e|e.to_string())?;
    }verify_media(dest,s,c)
}
pub fn prepare_media(base:&Path,s:&MediaSpec,c:&AtomicBool,p:&Progress)->Result<PathBuf>{
    protocol::no_links(base)?;fs::create_dir_all(base).map_err(|_|"无法创建用户组件目录")?;
    let _lock=platform::lock(&base.join("prepare.lock"))?;check_cancel(c)?;
    let target=media_path_at(base,s);if verify_media(&target,s,c).is_ok(){return Ok(target)}
    let work=base.join(format!("prepare-{}",&s.sha256[..16]));protocol::no_links(&work)?;fs::create_dir_all(&work).map_err(|e|e.to_string())?;
    let stage=work.join("expanded");clean_stage(&stage)?;
    let partial=work.join("verified.part");protocol::no_links(&partial)?;
    if partial.exists(){fs::remove_file(partial).map_err(|_|"无法清理未完成的组件下载，请关闭其他 ArkCut 后重试")?;}
    platform::space(base,s.bytes+220*1024*1024).map_err(|_|"组件准备空间不足，请腾出至少 850 MB 后重试")?;
    let archive=work.join("verified.zip");
    let result=(||{
        if !archive.is_file()||fs::metadata(&archive).map_err(|e|e.to_string())?.len()!=s.bytes||hash(&archive,c)?!=s.sha256{
            if archive.exists(){protocol::no_links(&archive)?;fs::remove_file(&archive).map_err(|e|e.to_string())?;}
            download(&s.url,&archive,s.bytes,Some((s.bytes,&s.sha256)),c,p,"正在下载视频处理组件")?;
        }
        extract_media(&archive,&stage,s,c,p)?;check_cancel(c)?;
        platform::json(&stage.join("component.json"),&serde_json::json!({"schema":1,"provider":"Gyan FFmpeg Windows builds","version":s.version,"source":s.url,"archiveSha256":s.sha256,"ffmpegSha256":s.ffmpeg_sha256,"ffprobeSha256":s.ffprobe_sha256,"verification":"sha256-verified","preparedAt":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()}))?;
        if target.exists(){protocol::no_links(&target)?;fs::rename(&target,base.join(format!("damaged-{}",uuid::Uuid::new_v4()))).map_err(|e|e.to_string())?;}
        fs::rename(&stage,&target).map_err(|e|e.to_string())?;Ok(target)
    })();let _=clean_stage(&stage);if result.is_ok(){let _=fs::remove_file(&archive);}result
}
#[cfg(test)]
mod tests{
    use super::*;
    #[test]fn offline_bundle_is_optional_but_partial_runtime_is_rejected(){
        let dir=std::env::temp_dir().join(format!("arkcut-offline-test-{}",uuid::Uuid::new_v4()));fs::create_dir(&dir).unwrap();
        assert!(offline_media_at(&dir).unwrap().is_none());fs::create_dir(dir.join("WebView2")).unwrap();
        assert!(offline_media_at(&dir).is_err());
        fs::write(dir.join("WebView2/msedge.dll"),b"untrusted").unwrap();assert!(offline_media_at(&dir).is_err());
        clean_stage(&dir).unwrap();
    }
    fn fixture(entries:&[(&str,&[u8])])->(PathBuf,MediaSpec){
        let dir=std::env::temp_dir().join(format!("arkcut-component-test-{}",uuid::Uuid::new_v4()));fs::create_dir(&dir).unwrap();
        let archive=dir.join("fixture.zip");let mut z=zip::ZipWriter::new(File::create(&archive).unwrap());
        for (name,bytes) in entries{z.start_file(*name,zip::write::SimpleFileOptions::default()).unwrap();z.write_all(bytes).unwrap();}z.finish().unwrap();
        let mut s=spec().ffmpeg;s.bytes=fs::metadata(&archive).unwrap().len();s.sha256=hash(&archive,&AtomicBool::new(false)).unwrap();s.ffmpeg_sha256=format!("{:x}",Sha256::digest(b"ffmpeg"));s.ffprobe_sha256=format!("{:x}",Sha256::digest(b"ffprobe"));(dir,s)
    }
    #[test]fn verified_archive_reuse_and_atomic_commit(){
        let (dir,s)=fixture(&[("ffmpeg-9.0.2-essentials_build/bin/ffmpeg.exe",b"ffmpeg"),("ffmpeg-9.0.2-essentials_build/bin/ffprobe.exe",b"ffprobe"),("ffmpeg-9.0.2-essentials_build/LICENSE",b"license"),("ffmpeg-9.0.2-essentials_build/README.txt",b"readme")]);
        let work=dir.join(format!("prepare-{}",&s.sha256[..16]));fs::create_dir(&work).unwrap();fs::rename(dir.join("fixture.zip"),work.join("verified.zip")).unwrap();
        let cancel=Arc::new(AtomicBool::new(false));let c=cancel.clone();let progress:Progress=Arc::new(move|_,_,_|c.store(true,Ordering::Relaxed));
        assert!(prepare_media(&dir,&s,&cancel,&progress).is_err());assert!(!media_path_at(&dir,&s).exists());assert!(!work.join("expanded").exists());assert!(work.join("verified.zip").is_file());
        cancel.store(false,Ordering::Relaxed);let p:Progress=Arc::new(|_,_,_|{});let target=prepare_media(&dir,&s,&cancel,&p).unwrap();assert!(!work.join("verified.zip").exists());verify_media(&target,&s,&cancel).unwrap();assert_eq!(prepare_media(&dir,&s,&cancel,&p).unwrap(),target);clean_stage(&dir).unwrap();
    }
    #[test]fn reject_traversal_before_extraction(){let(dir,s)=fixture(&[("../escape.exe",b"no")]);let dest=dir.join("expanded");let p:Progress=Arc::new(|_,_,_|{});assert!(extract_media(&dir.join("fixture.zip"),&dest,&s,&AtomicBool::new(false),&p).is_err());assert!(!dest.exists());clean_stage(&dir).unwrap();}
    #[test]fn reject_archive_hash_mismatch(){let(dir,mut s)=fixture(&[("safe",b"ok")]);s.sha256="0".repeat(64);let p:Progress=Arc::new(|_,_,_|{});assert!(extract_media(&dir.join("fixture.zip"),&dir.join("expanded"),&s,&AtomicBool::new(false),&p).is_err());assert!(!dir.join("expanded").exists());clean_stage(&dir).unwrap();}
    #[cfg(feature="update-test-hooks")]
    #[test]fn component_space_failure_and_retry(){
        if std::env::var("ARKCUT_COMPONENT_SPACE_CHILD").as_deref()!=Ok("1"){
            let status=std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","components::tests::component_space_failure_and_retry","--test-threads=1"]).env("ARKCUT_COMPONENT_SPACE_CHILD","1").env("ARKCUT_UPDATE_TEST_LOW_SPACE","1").status().unwrap();assert!(status.success());return
        }
        let(dir,s)=fixture(&[("ffmpeg-9.0.2-essentials_build/bin/ffmpeg.exe",b"ffmpeg"),("ffmpeg-9.0.2-essentials_build/bin/ffprobe.exe",b"ffprobe"),("ffmpeg-9.0.2-essentials_build/LICENSE",b"license"),("ffmpeg-9.0.2-essentials_build/README.txt",b"readme")]);
        let work=dir.join(format!("prepare-{}",&s.sha256[..16]));fs::create_dir(&work).unwrap();fs::rename(dir.join("fixture.zip"),work.join("verified.zip")).unwrap();fs::write(work.join("verified.part"),b"interrupted").unwrap();
        let p:Progress=Arc::new(|_,_,_|{});let c=AtomicBool::new(false);assert!(prepare_media(&dir,&s,&c,&p).unwrap_err().contains("空间不足"));assert!(!media_path_at(&dir,&s).exists());assert!(!work.join("verified.part").exists());assert!(work.join("verified.zip").exists());
        std::env::remove_var("ARKCUT_UPDATE_TEST_LOW_SPACE");assert!(prepare_media(&dir,&s,&c,&p).is_ok());clean_stage(&dir).unwrap();
    }
    #[test]fn upstream_allowlist(){for s in ["http://www.gyan.dev/a","https://www.gyan.dev.evil/a","https://user@www.gyan.dev/a","https://go.microsoft.com/a#x","https://example.com/a"]{assert!(!allowed_url(&reqwest::Url::parse(s).unwrap()))}assert!(allowed_url(&reqwest::Url::parse(&spec().ffmpeg.url).unwrap()));assert!(allowed_url(&reqwest::Url::parse(&spec().webview2.url).unwrap()));}
    #[test]fn damaged_media_never_ready(){let s=spec();let dir=std::env::temp_dir().join(format!("arkcut-components-{}",uuid::Uuid::new_v4()));fs::create_dir(&dir).unwrap();fs::write(dir.join("ffmpeg.exe"),b"wrong").unwrap();fs::write(dir.join("ffprobe.exe"),b"wrong").unwrap();assert!(verify_media(&dir,&s.ffmpeg,&AtomicBool::new(false)).is_err());clean_stage(&dir).unwrap();}
    #[test]fn versions_are_isolated(){let s=spec();let a=media_path_at(Path::new("cache"),&s.ffmpeg);let mut other=s.ffmpeg.clone();other.sha256="0".repeat(64);assert_ne!(a,media_path_at(Path::new("cache"),&other));}
    #[test]fn cancel_before_work(){assert!(check_cancel(&AtomicBool::new(true)).is_err());}
}
