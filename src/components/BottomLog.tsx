import {useEffect,useLayoutEffect,useRef,useState,type ReactNode} from 'react';
import {save} from '@tauri-apps/plugin-dialog';
import {logLevels,type UserLog} from '../userLogs';
import {api} from '../api';
import {Icon} from '../icons';
export function LogPanel({logs,jobId,onClear,onError,status}:{status:ReactNode;logs:UserLog[];jobId:string;onClear:()=>void;onError:(s:string)=>void}){
 const [expanded,setExpanded]=useState(false),[height,setHeight]=useState(()=>Math.max(180,Math.min(260,Number(localStorage.getItem('arkcut-log-height-v3'))||220))),[maximum,setMaximum]=useState(260),[filter,setFilter]=useState('all'),[atBottom,setAtBottom]=useState(true),[saving,setSaving]=useState(false);
 const [readError,setReadError]=useState('');
 const shell=useRef<HTMLDivElement>(null),scroller=useRef<HTMLDivElement>(null),follow=useRef(true),dragCleanup=useRef<()=>void>(()=>{});
 useLayoutEffect(()=>{const el=shell.current,parent=el?.parentElement;if(!el||!parent)return;const update=()=>{const work=parent.querySelector('.workbench');if(!work)return;const room=work.getBoundingClientRect().height+el.getBoundingClientRect().height;setMaximum(Math.max(88,Math.min(260,room-300)))};const observer=new ResizeObserver(update);observer.observe(parent);for(const child of parent.children)if(child!==el)observer.observe(child);update();return()=>observer.disconnect()},[]);
 useEffect(()=>{localStorage.setItem('arkcut-log-height-v3',String(height))},[height]);
 useEffect(()=>()=>dragCleanup.current(),[]);
 useEffect(()=>{if(follow.current&&scroller.current)scroller.current.scrollTop=scroller.current.scrollHeight},[logs,expanded,filter]);
 const resize=(e:React.PointerEvent)=>{e.preventDefault();dragCleanup.current();const y=e.clientY,start=Math.min(height,maximum);const move=(v:PointerEvent)=>setHeight(Math.max(Math.min(180,maximum),Math.min(maximum,start+y-v.clientY)));const done=()=>{removeEventListener('pointermove',move);removeEventListener('pointerup',done);removeEventListener('pointercancel',done)};dragCleanup.current=done;addEventListener('pointermove',move);addEventListener('pointerup',done);addEventListener('pointercancel',done)};
 const rows=logs.filter(l=>filter==='all'||l.status===filter).slice(-500),lastError=logs.filter(l=>l.status==='failed').at(-1)?.id||'',hasError=!!lastError&&lastError!==readError;
 useEffect(()=>{if(expanded)setReadError(lastError)},[expanded,lastError]);
 async function exportLogs(){setSaving(true);try{const path=await save({defaultPath:`ArkCut-${jobId||'diagnostics'}.jsonl`,filters:[{name:'诊断日志',extensions:['jsonl']}]});if(path)await api('exportLogs',{path,jobId:''})}catch(e){onError(String(e))}finally{setSaving(false)}}
 return <div ref={shell} className={`log-shell ${expanded?'expanded':''}`} style={{height:expanded?Math.min(height,maximum):48}}><section className="log-panel" aria-label="底部日志">
 {expanded&&<div className="log-resizer" role="separator" aria-label="调整日志高度" onPointerDown={resize}/>}
 <div className="log-header"><button className="log-toggle" onClick={()=>setExpanded(!expanded)} aria-expanded={expanded}><Icon name="chevron"/><span>日志{hasError?' · !':''}</span></button><div className="log-status">{status}</div></div>{expanded&&<div className="log-toolbar"><select aria-label="日志筛选" value={filter} onChange={e=>setFilter(e.target.value)}><option value="all">全部</option><option value="failed">错误</option><option value="completed">完成</option><option value="running">处理过程</option></select><button onClick={()=>navigator.clipboard.writeText(rows.map(l=>`[${new Date(l.time).toLocaleTimeString('zh-CN',{hour12:false})}] ${logLevels[l.status]} ${l.message}`).join('\n')).catch(e=>onError(String(e)))}>复制</button><button onClick={onClear}>清空</button><button disabled={saving} onClick={()=>void exportLogs()}>导出日志</button>{!atBottom&&<button className="latest" onClick={()=>{follow.current=true;setAtBottom(true);if(scroller.current)scroller.current.scrollTop=scroller.current.scrollHeight}}>回到最新</button>}<button className="log-collapse" onClick={()=>setExpanded(false)}>收起日志</button></div>}
 {expanded&&<div className="logs" ref={scroller} onScroll={e=>{const el=e.currentTarget;follow.current=el.scrollHeight-el.scrollTop-el.clientHeight<20;setAtBottom(follow.current)}}>{!rows.length&&<p>暂无日志</p>}{rows.map(l=><article key={l.id} className={l.status}><time>{new Date(l.time).toLocaleTimeString('zh-CN',{hour12:false})}</time><span>{logLevels[l.status]}</span><p title={l.message}>{l.message}</p></article>)}</div>}
 </section></div>
}
