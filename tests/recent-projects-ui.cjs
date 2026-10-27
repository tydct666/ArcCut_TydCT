// Native regression with two portable copies and one Windows-user data directory.
const fs=require('fs'),path=require('path'),cp=require('child_process'),assert=require('assert');
const {connect}=require('./cdp-client.cjs');
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
(async()=>{
 const out=path.resolve(process.env.ARKCUT_RECENT_TEST_OUT||'outputs/recent-projects-ui');
 assert(!fs.existsSync(out),'Use a fresh ARKCUT_RECENT_TEST_OUT');fs.mkdirSync(out,{recursive:true});
 const data=path.join(out,'data'),external=path.join(out,'shared-archive');fs.mkdirSync(data);fs.mkdirSync(external);
 fs.writeFileSync(path.join(data,'settings.json'),JSON.stringify({archiveDirectory:external,cacheDirectory:null,cacheLimit:5*1024**3}));
 const legacy=path.join(external,'legacy');fs.mkdirSync(legacy);const legacyPath=path.join(legacy,'project.arkcut.json');fs.writeFileSync(legacyPath,'{}');
 fs.writeFileSync(path.join(legacy,'archive-state.json'),JSON.stringify({projectId:'legacy',name:'旧副本记录',sourcePath:'fixture',status:'ready',createdMs:1,updatedMs:2,path:legacyPath}));
 fs.writeFileSync(path.join(data,'recent-projects.json'),JSON.stringify([legacyPath]));
 const binary=path.resolve(process.env.ARKCUT_TEST_APP||'src-tauri/target/release/arkcut.exe');
 const copies=['A','B'].map(n=>{const dir=path.join(out,n);fs.mkdirSync(dir);const exe=path.join(dir,'ArkCut.exe');fs.copyFileSync(binary,exe);return exe});
 const source=path.join(out,'synthetic.mp4');
 cp.execFileSync(path.resolve('tools/media/ffmpeg.exe'),['-v','error','-f','lavfi','-i','testsrc2=s=320x180:r=30:d=1','-c:v','libx264','-preset','ultrafast',source],{windowsHide:true});
 let app,c;const facts={status:'running',checks:[]};
 const api=(op,payload={})=>c.evaluate(`window.__TAURI_INTERNALS__.invoke('api',${JSON.stringify({op,payload})})`);
 const stop=async()=>{if(c){await c.evaluate('window.__TAURI_INTERNALS__.invoke("plugin:window|close",{label:"main"})').catch(()=>{});c.close();c=null;}if(app){for(let i=0;i<50&&app.exitCode===null;i++)await sleep(100);if(app.exitCode===null)app.kill();app=null;}};
 const start=async exe=>{app=cp.spawn(exe,[],{windowsHide:true,stdio:'ignore',env:{...process.env,ARKCUT_DATA_DIR:data,WEBVIEW2_USER_DATA_FOLDER:path.join(path.dirname(exe),'profile'),WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS:'--remote-debugging-port=9471 --mute-audio'}});for(let i=0;i<200&&!c;i++){try{c=await connect(9471)}catch{await sleep(100)}}assert(c);await c.wait('!!document.querySelector(".empty-import")');};
 try{
  await start(copies[0]);assert.deepEqual(await api('archiveRecent'),[]);await c.evaluate('document.querySelector(".project-menu").open=true');assert.equal(await c.evaluate('document.querySelectorAll(".recent-projects").length'),0);await c.screenshot(path.join(out,'fresh-empty.png'));facts.checks.push('fresh copy ignores legacy shared history/custom archive and hides empty section');
  await api('probe',{path:source});await c.wait(`window.__TAURI_INTERNALS__.invoke('api',{op:'status',payload:{}}).then(s=>s.job?.status==='completed')`,30000);const status=await api('status');const project=status.job.result;project.name='可点击的长项目名_'.repeat(8);const entry=await api('archiveCreate',{project});
  await c.send('Page.reload');await c.wait('!!document.querySelector(".recent-entry")');await c.evaluate('document.querySelector(".project-menu").open=true');
  await c.wait('getComputedStyle(document.querySelector(".recent-entry")).opacity==="1"');
  const layout=await c.evaluate(`(()=>{const b=document.querySelector('.recent-entry'),s=b.querySelector('span'),t=document.querySelector('.recent-projects>summary');return {disabled:b.disabled,opacity:getComputedStyle(b).opacity,font:getComputedStyle(s).fontSize,width:s.getBoundingClientRect().width,bottom:s.getBoundingClientRect().bottom,timeTop:b.querySelector('small').getBoundingClientRect().top,titleColor:getComputedStyle(t).color,textColor:getComputedStyle(b).color}})()`);
  assert.equal(layout.disabled,false);assert.equal(layout.opacity,'1');assert.equal(layout.font,'14px');assert(layout.width>150);assert(layout.timeTop>=layout.bottom);assert.equal(layout.titleColor,layout.textColor);facts.layout=layout;await c.screenshot(path.join(out,'recent-dark.png'));facts.checks.push('recent item stays readable and enabled; long title and date occupy separate lines');
  await c.evaluate('document.querySelector(".recent-entry").click()');await c.wait(`document.querySelector('.project-title')?.textContent===${JSON.stringify(project.name)}`);facts.checks.push('clicking recent item opens the actual saved project');
  await c.click('浅色');await c.evaluate('document.querySelector(".project-menu").open=true');await c.screenshot(path.join(out,'recent-light.png'));await stop();
  await start(copies[0]);assert.equal((await api('archiveRecent')).length,1);await stop();facts.checks.push('same copy retains history on restart');
  await start(copies[1]);assert.deepEqual(await api('archiveRecent'),[]);assert.equal(await c.evaluate('document.querySelectorAll(".recent-projects").length'),0);facts.checks.push('second fresh copy stays empty with the same user data and archive preference');
  const opened=await api('archiveOpen',{path:entry.path});assert.equal(opened.project.projectId,project.projectId);assert.equal((await api('archiveRecent')).length,1);assert(fs.existsSync(entry.path));facts.checks.push('manual open adopts existing project into this copy without deleting it');facts.status='passed';facts.jsErrors=c.events;
 }catch(e){facts.error=String(e);if(c)await c.screenshot(path.join(out,'failure.png')).catch(()=>{});throw e}
 finally{await stop();fs.writeFileSync(path.join(out,'evidence.json'),JSON.stringify(facts,null,2));console.log(JSON.stringify(facts));}
})().catch(e=>{console.error(e);process.exitCode=1});
