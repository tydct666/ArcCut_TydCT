// Caller supplies an extracted package, a short video fixture and fresh output.
const fs=require('fs'),path=require('path'),cp=require('child_process'),assert=require('assert');
const {connect}=require('./cdp-client.cjs');const sleep=ms=>new Promise(r=>setTimeout(r,ms));
(async()=>{
 const root=path.resolve(process.env.ARKCUT_SMOKE_PACKAGE),out=path.resolve(process.env.ARKCUT_SMOKE_OUT),offline=process.env.ARKCUT_SMOKE_OFFLINE==='1';
 assert(!fs.existsSync(out));fs.mkdirSync(out,{recursive:true});const local=path.join(out,'local');fs.mkdirSync(local);
 const env={...process.env,ARKCUT_DATA_DIR:path.join(out,'data'),WEBVIEW2_USER_DATA_FOLDER:path.join(out,'profile'),WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS:'--remote-debugging-port=9481 --mute-audio'};
 if(offline)Object.assign(env,{LOCALAPPDATA:local,ARKCUT_MEDIA_DIR:path.join(out,'invalid-media'),WEBVIEW2_BROWSER_EXECUTABLE_FOLDER:path.join(out,'invalid-web'),HTTP_PROXY:'http://127.0.0.1:1',HTTPS_PROXY:'http://127.0.0.1:1'});
 let c;const facts={status:'running',offline,checks:[]},app=cp.spawn(path.join(root,'ArkCut.exe'),[],{windowsHide:true,stdio:'ignore',env});
 try {
  for(let i=0;i<400&&!c;i++){try{c=await connect(9481)}catch{assert.equal(app.exitCode,null,'App exited before UI');await sleep(100)}}assert(c);
  await c.wait('!!document.querySelector(".empty-import")');assert.equal(await c.evaluate('document.querySelectorAll(".recent-entry").length'),0);facts.checks.push('clean startup without recent projects');
  if(offline){
   const script=`[Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false); Get-CimInstance Win32_Process -Filter "ParentProcessId = ${app.pid}" | Where-Object Name -eq 'msedgewebview2.exe' | Select-Object -ExpandProperty ExecutablePath`;
   const actual=cp.execFileSync('powershell',['-NoProfile','-Command',script],{encoding:'utf8',windowsHide:true}).trim().split(/\r?\n/);
   assert(actual.some(p=>p.toLowerCase()===path.join(root,'WebView2','msedgewebview2.exe').toLowerCase()));facts.checks.push('uses bundled fixed runtime with empty user component cache and unusable proxy');
  }
  const api=(op,payload={})=>c.evaluate(`window.__TAURI_INTERNALS__.invoke('api',${JSON.stringify({op,payload})})`);
  const finish=async()=>{await c.wait(`window.__TAURI_INTERNALS__.invoke('api',{op:'status',payload:{}}).then(s=>s.job&&!['running','cancelling'].includes(s.job.status))`,180000);const s=await api('status');assert.equal(s.job.status,'completed',s.job.error);return s.job.result};
  await api('import',{path:path.resolve(process.env.ARKCUT_SMOKE_VIDEO)});const project=await finish();
  project.pauses=[{id:'local-smoke',startUs:1000000,endUs:1500000,enabled:true,source:'manual',state:'confirmed',score:1}];project.revision++;
  await api('previewBatch',{project,halfSeconds:3});const previews=await finish();assert.equal(previews.ready,1);assert.equal(previews.total,1);
  await api('export',{project,path:path.join(out,'edited.mp4'),formatChangeAccepted:true});const exported=await finish();
  assert(fs.statSync(path.join(out,'edited.mp4')).size>0);facts.checks.push('import, complete preview and cut/export succeed');
  facts.preview={ready:previews.ready,total:previews.total};facts.outputBytes=fs.statSync(path.join(out,'edited.mp4')).size;
  fs.writeFileSync(path.join(out,'export-result.json'),JSON.stringify(exported,null,2));
  assert.equal(await c.evaluate('document.querySelectorAll(".update-dialog").length'),0);
  if(offline)assert(!fs.existsSync(path.join(local,'ArkCut','components')));
  facts.jsErrors=c.events;assert.equal(c.events.length,0);await c.screenshot(path.join(out,'smoke.png'));facts.status='passed';
 }catch(e){facts.status='failed';facts.error=String(e);throw e}
 finally {
  if(c){await c.evaluate('document.querySelectorAll("video,audio").forEach(v=>v.pause())').catch(()=>{});await c.evaluate('window.__TAURI_INTERNALS__.invoke("plugin:window|close",{label:"main"})').catch(()=>{});c.close()}
  for(let i=0;i<50&&app.exitCode===null;i++)await sleep(100);if(app.exitCode===null)app.kill();
  fs.writeFileSync(path.join(out,'evidence.json'),JSON.stringify(facts,null,2));console.log(JSON.stringify(facts));
 }
})().catch(e=>{console.error(e);process.exitCode=1});
