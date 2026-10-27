import React,{useEffect,useState,useRef}from 'react';
import {invoke,isTauri}from '@tauri-apps/api/core';
import {getCurrentWindow}from '@tauri-apps/api/window';
import {Dialog}from './Dialog';
export type UpdateStatus={currentVersion:string;status:string;version:string|null;notes:string;downloaded:number;total:number;error:string|null;configured:boolean};
export function useUpdates(idle:boolean){
 const[state,setState]=useState<UpdateStatus>({currentVersion:__APP_VERSION__,status:'idle',version:null,notes:'',downloaded:0,total:0,error:null,configured:true});
 const automatic=useRef(false),revision=useRef(0);
 const command=async(op:string)=>{revision.current++;try{const s=await invoke<UpdateStatus>('update_api',{op});revision.current++;setState(s);return s}catch(e){revision.current++;setState(s=>({...s,error:String(e),status:'error'}));throw e}};
 useEffect(()=>{if(!isTauri())return;let disposed=false,inflight=false;const read=async()=>{if(inflight)return;inflight=true;const started=revision.current;try{const s=await invoke<UpdateStatus>('update_api',{op:'status'});if(!disposed&&started===revision.current)setState(s)}catch{}finally{inflight=false}};void read();const timer=setInterval(read,500);return()=>{disposed=true;clearInterval(timer)}},[]);
 useEffect(()=>{if(!isTauri()||!idle||automatic.current)return;const timer=setTimeout(()=>{automatic.current=true;void command('automatic').catch(()=>{})},3000);return()=>clearTimeout(timer)},[idle]);
 useEffect(()=>{if(!isTauri())return;void invoke<boolean>('update_health_mode').then(async health=>{if(health){try{await invoke('update_health_ready');await getCurrentWindow().destroy()}catch{await getCurrentWindow().destroy()}}}).catch(()=>{})},[]);
 return {state,command,hasUpdate:!!state.version&&state.version!==state.currentVersion};
}
export function UpdateControls({state,command,busy,onInstall}:{state:UpdateStatus;command:(op:string)=>Promise<UpdateStatus>;busy:boolean;onInstall:()=>Promise<void>}){
 const[open,setOpen]=useState(false),[checking,setChecking]=useState(false),[agreed,setAgreed]=useState(''),[installing,setInstalling]=useState(false),[error,setError]=useState(''),[feedback,setFeedback]=useState('');
 const approved=useRef(''),pending=useRef(false),attempt=useRef(0),live=useRef({state,command});live.current={state,command};
 const hasUpdate=!!state.version&&state.version!==state.currentVersion;
 const downloading=['downloading','cancelling'].includes(state.status);
 // A completed manual check may open a new-version offer. Background status never opens it.
 const finishCheck=(result:UpdateStatus)=>{pending.current=false;setChecking(false);if(result.version&&result.version!==result.currentVersion&&['available','ready','cancelled'].includes(result.status)){setFeedback('');setOpen(true)}else{setFeedback(result.error||(result.status==='current'?'当前已是最新版本':result.configured?'暂时无法检查更新，请稍后重试':'更新服务暂未配置'))}};
 const check=async()=>{const id=++attempt.current;approved.current='';setAgreed('');pending.current=false;setError('');setFeedback('正在检查更新…');setChecking(true);setOpen(false);try{const result=await command('check');if(id!==attempt.current)return;if(result.status==='checking'){pending.current=true}else{finishCheck(result)}}catch(e){if(id!==attempt.current)return;setChecking(false);setFeedback(String(e))}};
 useEffect(()=>{if(pending.current&&state.status!=='checking')finishCheck(state)},[state]);
 const cancel=()=>{approved.current='';setAgreed('');if(downloading)void command('cancel').catch(e=>setError(String(e)))};
 const close=()=>{if(installing||state.status==='installing')return;cancel();setOpen(false)};
 useEffect(()=>()=>{attempt.current++;pending.current=false;approved.current='';if(['downloading','cancelling'].includes(live.current.state.status))void live.current.command('cancel').catch(()=>{})},[]);
 useEffect(()=>{
  if(!open||busy||installing||state.status!=='ready'||!agreed||approved.current!==state.version||agreed!==state.version)return;
  approved.current='';setAgreed('');setInstalling(true);
  void onInstall().catch(e=>setError(`更新尚未开始：${String(e)}`)).finally(()=>setInstalling(false));
 },[open,busy,installing,state.status,state.version,agreed,onInstall]);
 const agree=async()=>{if(busy||!state.version||checking)return;setError('');approved.current=state.version;setAgreed(state.version);if(state.status!=='ready'){try{await command('download')}catch(e){approved.current='';setAgreed('');setError(String(e))}}};
 const working=checking||downloading||installing||state.status==='installing';
 const offered=!checking&&hasUpdate&&['available','ready','cancelled'].includes(state.status);
 const message=error||(!checking?state.error:null)||(checking?'正在检查更新…':installing||state.status==='installing'?'正在保存项目并准备更新…':downloading?'正在下载更新…':state.status==='current'?'当前已是最新版本':state.status==='cancelled'?'下载已取消':agreed&&busy?'等待当前任务结束后更新…':'');
 return <div className="update-controls"><button className="update-check" disabled={checking||installing} onClick={()=>void check()} title={hasUpdate?'检查更新，有新版本':'检查更新'}>{checking?'正在检查…':'检查更新'}{hasUpdate&&<span className="update-dot" aria-hidden="true"/>}</button>{feedback&&<p className="update-feedback" role="status">{feedback}</p>}
 {open&&<Dialog label="检查更新" className="confirmation-overlay update-dialog" onEscape={close}><div className="drawer-title"><h2>检查更新</h2><button aria-label="关闭更新窗口" disabled={installing||state.status==='installing'} onClick={close}>关闭</button></div>
 {message&&<p role={error||state.error?'alert':'status'}>{message}</p>}
 {offered&&<><p>发现新版本 <strong>{state.version}</strong> · {(state.total/1024**2).toFixed(1)} MB</p>{state.notes&&<div className="update-notes"><p>{state.notes}</p></div>}<p>同意后下载更新，完成后保存项目并重启。</p></>}
 {downloading&&<div className="update-progress"><progress max={state.total||1} value={state.downloaded}/><span>{(state.downloaded/1024**2).toFixed(1)} / {(state.total/1024**2).toFixed(1)} MB</span><button disabled={state.status==='cancelling'} onClick={cancel}>取消下载</button></div>}
 <div className="row">{offered&&!agreed&&!working&&<button className="primary" disabled={busy} onClick={()=>void agree()}>同意更新</button>}{!working&&!offered&&<button onClick={()=>void check()}>重新检查</button>}{!working&&<button onClick={close}>{offered?'暂不更新':'返回'}</button>}</div>{offered&&busy&&!agreed&&<p>当前任务完成后可更新。</p>}
 </Dialog>}</div>
}
