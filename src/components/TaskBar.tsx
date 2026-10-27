import type {Job} from '../types';
import {shortTime} from '../api';
import {taskDetail,phaseNames as phases} from '../labels';
import{IconButton}from './Controls';
export function TaskBar({job,message,error,onDismiss,onCancel,suppressed=false}:{job:Job|null;message:string;error:string;onDismiss:()=>void;onCancel:()=>void;suppressed?:boolean}){
 if(suppressed)return <div className="taskbar" aria-hidden="true"/>;
 const active=job&&['running','cancelling'].includes(job.status),total=job?.totalUnits,percent=active&&total&&total>0?Math.min(100,Math.max(0,job!.completedUnits/total*100)):null;
 const units=job?.unit==='us'?`${shortTime(job.completedUnits)} / ${shortTime(total||0)}`:total!==null&&total!==undefined?`${job?.completedUnits} / ${total}`:'';
 const phase=phases[job?.phase||'']||'处理任务',label=error?'需要处理':active?job?.status==='cancelling'?'正在停止':`正在${phase}`:message?'✓':'就绪';
 const detail=error||(!active?message:job?.status==='cancelling'?'正在回收资源，请稍候':job?.resource?.notice?'资源紧张，正在降低处理负载':taskDetail(job?.phase,job?.detail))||'';
 const same=(s:string)=>s.replace(/正在|[，。…\s]/g,'');
 return <div className={`taskbar ${active?'running':''} ${error?'error':''}`} role={error?'alert':'status'}><span className="task-label">{label}</span><span className="task-message" title={same(detail)===same(label)?undefined:detail}>{same(detail)===same(label)?'':detail}</span>{active&&<><progress aria-label="当前任务进度" max="100" value={percent??undefined}/><code>{percent!==null?`${percent.toFixed(0)}% · `:''}{units}</code><small>{(job!.elapsedMs/1000).toFixed(0)} 秒</small><button onClick={onCancel} disabled={job?.status==='cancelling'}>{job?.status==='cancelling'?'等待回收…':'停止'}</button></>}{error&&<IconButton icon="close" label="关闭错误提示" onClick={onDismiss}/>}</div>
}
