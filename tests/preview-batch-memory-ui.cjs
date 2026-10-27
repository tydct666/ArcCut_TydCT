// Native UI coverage with a caller-owned data directory. A fresh directory
// measures cold full-batch previews, including WebView and live playback.
const fs=require('fs'),path=require('path'),cp=require('child_process'),assert=require('assert');
const {connect}=require('./cdp-client.cjs');const sleep=ms=>new Promise(r=>setTimeout(r,ms));
(async()=>{
 const out=path.resolve(process.env.ARKCUT_BATCH_UI_OUT),data=path.resolve(process.env.ARKCUT_BATCH_DATA);
 assert(!fs.existsSync(out));const fresh=!fs.existsSync(data);fs.mkdirSync(out,{recursive:true});fs.mkdirSync(data,{recursive:true});const exe=path.join(out,'ArkCut.exe');fs.copyFileSync(process.env.ARKCUT_TEST_APP,exe);
 const project=JSON.parse(fs.readFileSync(process.env.ARKCUT_BATCH_PROJECT,'utf8'));project.name='全部片段预览验收';
 let app,c;const facts={status:'running',freshApplicationCache:fresh,scope:'Native UI, all previews and live muted playback; OS cache not flushed',checks:[]};
 const api=(op,payload={})=>c.evaluate(`window.__TAURI_INTERNALS__.invoke('api',${JSON.stringify({op,payload})})`);
 try{
  app=cp.spawn(exe,[],{stdio:'ignore',windowsHide:true,env:{...process.env,ARKCUT_DATA_DIR:data,WEBVIEW2_USER_DATA_FOLDER:path.join(out,'profile'),WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS:'--remote-debugging-port=9472 --mute-audio'}});
  for(let i=0;i<200&&!c;i++){try{c=await connect(9472)}catch{await sleep(100)}}assert(c);await c.wait('!!document.querySelector(".empty-import")');
  await api('archiveCreate',{project});await c.send('Page.reload');await c.wait('!!document.querySelector(".recent-entry")');await c.evaluate('document.querySelector(".recent-entry").click()');await c.wait('document.querySelectorAll(".check-play").length>1');
  const count=await c.evaluate('document.querySelectorAll(".check-play").length');const started=Date.now();await c.evaluate('document.querySelector(".preview-action").click()');
  await c.wait('document.querySelector(".preview-action")?.textContent==="预览已就绪"',600000);
  const s=await api('status');assert.equal(s.job.status,'completed');assert.equal(s.job.result.ready,count);assert.equal(s.job.result.total,count);assert(s.job.result.previews.every(e=>e.status==='ready'));
  facts.ready=count;facts.elapsedMs=Date.now()-started;facts.resource=s.job.resource;facts.snapshotBytes=Buffer.byteLength(JSON.stringify(s));assert(facts.snapshotBytes<2*1024**2);facts.checks.push('all preview entries ready; status IPC under 2 MiB');
  for(const index of [0,count-1]){
   await c.evaluate(`document.querySelectorAll('.check-play')[${index}].click()`);await c.wait('document.querySelector("video")?.readyState>=2');
   const file=path.basename(s.job.result.previews[index].path);
   await c.wait(`document.querySelector('video')?.currentSrc.includes(${JSON.stringify(file)})&&document.querySelector('video').readyState>=2`);
   await c.evaluate('document.querySelector("video").play()');const before=await c.evaluate('document.querySelector("video").currentTime');await sleep(700);
   assert(await c.evaluate('document.querySelector("video").currentTime')>before);
   const frame=await c.evaluate(`(()=>{const v=document.querySelector('video');return {width:v.videoWidth,height:v.videoHeight,error:v.error?.message||null}})()`);
   assert(frame.width>0&&frame.height>0&&Math.max(frame.width,frame.height)<=960);assert.equal(s.job.result.previews[index].quickPreview,true);assert.equal(frame.error,null);await c.evaluate('document.querySelector("video").pause()');
  }
  facts.checks.push('first and last quick previews play within 960 pixels without media errors');
  assert.equal(await c.evaluate('document.querySelector(".export-main").disabled'),false);facts.checks.push('export enabled after all previews');
  await c.screenshot(path.join(out,'all-ready.png'));facts.jsErrors=c.events;assert.equal(c.events.length,0);facts.status='passed';
 }catch(e){facts.error=String(e);if(c)await c.screenshot(path.join(out,'failure.png')).catch(()=>{});throw e}
 finally{
  if(c){await c.evaluate('document.querySelectorAll("video,audio").forEach(v=>v.pause())').catch(()=>{});await c.evaluate('window.__TAURI_INTERNALS__.invoke("plugin:window|close",{label:"main"})').catch(()=>{});c.close()}
  if(app){for(let i=0;i<50&&app.exitCode===null;i++)await sleep(100);if(app.exitCode===null)app.kill()}
  fs.writeFileSync(path.join(out,'evidence.json'),JSON.stringify(facts,null,2));console.log(facts);
 }
})().catch(e=>{console.error(e);process.exitCode=1});
