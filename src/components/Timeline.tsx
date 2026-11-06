import{useEffect,useLayoutEffect,useRef,useState}from 'react';
import type{Project,Plan,PreviewEntry}from '../types';
import{shortTime}from '../api';
import{HistoryIcon}from '../icons';
import{IconButton}from './Controls';
import{snapFrame,viewport}from '../timelineMath';

interface Props{p:Project|null;plan:Plan|null;view:'source'|'result';current:number;selected:string;entry:PreviewEntry|null;onPause:(id:string)=>void;onCut:(i:number)=>void;onSeek:(us:number,final?:boolean)=>void;onUndo:()=>void;onRedo:()=>void;undoLabel:string;redoLabel:string;historyDisabled:boolean}
type Kind='scrub'|'pan'|'cut'|'nav-move'|'nav-start'|'nav-end'|'nav-page';
interface Gesture{kind:Kind;pointerId:number;capture:HTMLElement;x:number;y:number;lastX:number;lastY:number;width:number;left:number;range:[number,number];moved:boolean;auxiliary:boolean;id?:string;index?:number}

export function Timeline(props:Props){
 const{p,plan,view,current,selected,entry,onUndo,onRedo,undoLabel,redoLabel,historyDisabled}=props;
 const [teaching,setTeaching]=useState(()=>{try{return localStorage.getItem('arkcut-timeline-learned')!=='1'}catch{return true}});
 const dismissTeaching=()=>{setTeaching(false);try{localStorage.setItem('arkcut-timeline-learned','1')}catch{}};
 const duration=(view==='source'?p?.source.durationUs:plan?.durationUs)||0;
 const[range,setRange]=useState<[number,number]>([0,duration]),[mode,setMode]=useState<'all'|'cut'>('all'),[dragging,setDragging]=useState(''),[hover,setHover]=useState(''),[trackWidth,setTrackWidth]=useState(700);
 const track=useRef<HTMLDivElement>(null),nav=useRef<HTMLDivElement>(null),gesture=useRef<Gesture|null>(null),live=useRef({props,duration,range,mode}),prior=useRef({projectId:p?.projectId,view,duration});live.current={props,duration,range,mode};
 const setWindow=(next:[number,number],nextMode:'all'|'cut'='cut')=>{const resolved=next[0]<=0&&next[1]>=live.current.duration?'all':nextMode;live.current.range=next;live.current.mode=resolved;setRange(next);setMode(resolved)};
 const seekAt=(x:number,final=false)=>{const r=live.current.range,b=track.current?.getBoundingClientRect();if(!b||!live.current.duration)return;let us=Math.max(0,Math.min(live.current.duration,Math.round(r[0]+(x-b.left)/b.width*(r[1]-r[0]))));if(live.current.props.view==='source'&&live.current.props.p)us=snapFrame(live.current.props.p.source.framePts,live.current.duration,us);live.current.props.onSeek(us,final)};
 const selectNear=(x:number)=>{const b=track.current!.getBoundingClientRect(),r=live.current.range,t=r[0]+(x-b.left)/b.width*(r[1]-r[0]),tolerance=(r[1]-r[0])*7/b.width;
  if(live.current.props.view==='result'){const item=live.current.props.plan?.removed.map((c,index)=>({id:'',index,startUs:c.outputUs,endUs:c.outputUs,d:Math.abs(c.outputUs-t)})).sort((a,b)=>a.d-b.d)[0];return item&&item.d<=tolerance?item:null}
  const item=live.current.props.p?.pauses.filter(c=>c.state!=='rejected').map(c=>({...c,d:t<c.startUs?c.startUs-t:t>c.endUs?t-c.endUs:0})).sort((a,b)=>a.d-b.d)[0];return item&&item.d<=tolerance?item:null;
 };
 const focus=(start?:number,end?:number)=>{const state=live.current,active=state.props.p?.pauses.find(c=>c.id===state.props.selected),a=start??(state.props.view==='source'&&active?active.startUs:state.props.entry?.cutUs??state.props.current),b=end??(state.props.view==='source'&&active?active.endUs:a);setWindow(viewport(a-3000000,b+3000000,state.duration))};
 const zoom=(factor:number,anchor:number,fraction=.5)=>{const r=live.current.range,size=(r[1]-r[0])/factor;setWindow(viewport(anchor-size*fraction,anchor+size*(1-fraction),live.current.duration))};
 const finish=(commit:boolean)=>{const g=gesture.current;if(!g)return;gesture.current=null;
  if(g.capture.hasPointerCapture(g.pointerId))g.capture.releasePointerCapture(g.pointerId);setDragging('');setHover('');
  if(!commit){setWindow(g.range,g.range[0]===0&&g.range[1]===live.current.duration?'all':'cut');return}
  const state=live.current;
  if(g.kind==='pan'&&!g.moved&&!g.auxiliary&&g.id)state.props.onPause(g.id);
  else if(g.kind==='scrub'||g.kind==='cut'&&g.moved||g.kind==='pan'&&!g.moved&&!g.auxiliary)seekAt(g.lastX,true);
  else if(g.kind==='cut'&&!g.moved&&g.index!==undefined)state.props.onCut(g.index);
  else if(g.kind==='nav-page'&&!g.moved){const size=g.range[1]-g.range[0],at=(g.lastX-g.left)/g.width*state.duration;setWindow(viewport(at-size/2,at+size/2,state.duration))}
 };
 const start=(e:React.PointerEvent<HTMLDivElement>,navigation=false)=>{
  if(!duration||gesture.current||e.button!==0&&e.button!==1)return;const target=e.target as HTMLElement,hit=target.closest<HTMLElement>('[data-drag]'),capture=e.currentTarget,b=capture.getBoundingClientRect();let kind:Kind=navigation?(hit?.dataset.drag as Kind||'nav-page'):(hit?.dataset.drag as Kind||'pan'),id=hit?.dataset.id,index=hit?.dataset.index?Number(hit.dataset.index):undefined;
  if(navigation&&kind!=='nav-page'&&(range[1]-range[0])/duration*b.width<24){const startX=b.left+range[0]/duration*b.width,endX=b.left+range[1]/duration*b.width;kind=Math.abs(e.clientX-startX)<=Math.abs(e.clientX-endX)?'nav-start':'nav-end'}
  if(!navigation&&(e.shiftKey||e.button===1))kind='pan';
  if(!navigation&&kind!=='scrub'&&!e.shiftKey&&e.button===0){const nearby=selectNear(e.clientX);if(nearby){id=nearby.id;kind=view==='source'?'pan':'cut';index='index'in nearby?Number(nearby.index):undefined}}
  dismissTeaching();e.preventDefault();capture.focus({preventScroll:true});capture.setPointerCapture(e.pointerId);
  const g:Gesture={kind,pointerId:e.pointerId,capture,x:e.clientX,y:e.clientY,lastX:e.clientX,lastY:e.clientY,width:b.width,left:b.left,range:[...live.current.range],moved:false,auxiliary:e.shiftKey||e.button===1,id,index};gesture.current=g;
  if(kind==='scrub'){setDragging('scrub');seekAt(e.clientX)}
 };
 const move=(e:React.PointerEvent<HTMLDivElement>)=>{const g=gesture.current;if(!g||g.pointerId!==e.pointerId)return;g.lastX=e.clientX;g.lastY=e.clientY;if(Math.hypot(e.clientX-g.x,e.clientY-g.y)>=4){g.moved=true;setDragging(g.kind)}if(g.kind==='scrub'){seekAt(e.clientX);return}if(!g.moved)return;
  if(g.kind==='cut'){seekAt(e.clientX);return}
  const state=live.current,dx=(e.clientX-g.x)/g.width;
  if(g.kind==='pan')setWindow(viewport(g.range[0]-dx*(g.range[1]-g.range[0]),g.range[1]-dx*(g.range[1]-g.range[0]),state.duration));
  if(g.kind==='nav-move'||g.kind==='nav-page')setWindow(viewport(g.range[0]+dx*state.duration,g.range[1]+dx*state.duration,state.duration));
  if(g.kind==='nav-start'){const a=Math.max(0,Math.min(g.range[0]+dx*state.duration,g.range[1]-Math.min(1000000,state.duration)));setWindow([a,g.range[1]])}
  if(g.kind==='nav-end'){const b=Math.min(state.duration,Math.max(g.range[1]+dx*state.duration,g.range[0]+Math.min(1000000,state.duration)));setWindow([g.range[0],b])}
 };
 const up=(e:React.PointerEvent<HTMLDivElement>)=>{const g=gesture.current;if(g?.pointerId===e.pointerId){g.lastX=e.clientX;g.lastY=e.clientY;finish(true)}};
 useEffect(()=>{const cancel=()=>finish(false),escape=(e:KeyboardEvent)=>{if(e.key==='Escape'&&gesture.current){e.preventDefault();e.stopImmediatePropagation();cancel()}},hidden=()=>{if(document.hidden)cancel()};window.addEventListener('blur',cancel);window.addEventListener('keydown',escape,true);document.addEventListener('visibilitychange',hidden);return()=>{window.removeEventListener('blur',cancel);window.removeEventListener('keydown',escape,true);document.removeEventListener('visibilitychange',hidden);const g=gesture.current;gesture.current=null;if(g?.capture.hasPointerCapture(g.pointerId))g.capture.releasePointerCapture(g.pointerId)}},[]);
 useLayoutEffect(()=>{const el=track.current;if(!el)return;const measure=()=>setTrackWidth(el.getBoundingClientRect().width),observer=new ResizeObserver(measure);measure();observer.observe(el);return()=>observer.disconnect()},[]);
 useEffect(()=>{finish(false);const before=prior.current,changed=before.projectId!==p?.projectId||before.view!==view;prior.current={projectId:p?.projectId,view,duration};if(changed||live.current.mode==='all')setWindow([0,duration],'all');else setWindow(viewport(live.current.range[0],live.current.range[1],duration))},[p?.projectId,view,duration]);
 useEffect(()=>{const el=track.current;if(!el)return;const wheel=(e:WheelEvent)=>{if(!live.current.duration)return;dismissTeaching();e.preventDefault();finish(false);const b=el.getBoundingClientRect(),r=live.current.range;if(e.shiftKey||Math.abs(e.deltaX)>Math.abs(e.deltaY)){const shift=(e.deltaX||e.deltaY)*(r[1]-r[0])/900;setWindow(viewport(r[0]+shift,r[1]+shift,live.current.duration))}else{const f=Math.max(0,Math.min(1,(e.clientX-b.left)/b.width));zoom(e.deltaY<0?1.3:1/1.3,r[0]+f*(r[1]-r[0]),f)}};el.addEventListener('wheel',wheel,{passive:false});return()=>el.removeEventListener('wheel',wheel)},[]);
 const span=Math.max(1,range[1]-range[0]),position=(t:number)=>(t-range[0])/span*100,active=p?.pauses.find(c=>c.id===selected),shown=active,spacing=[1,2,5,10,15,30,60,120,300,600,1800,3600,7200].find(s=>s*1e6>=span/Math.max(2,(track.current?.clientWidth||700)/95))||7200,ticks:number[]=[];
 for(let t=Math.ceil(range[0]/(spacing*1e6))*spacing*1e6;t<=range[1];t+=spacing*1e6)ticks.push(t);
 const label=shown&&view==='source'?`原片 ${shortTime(shown.startUs)}—${shortTime(shown.endUs)} · 删除 ${((shown.endUs-shown.startUs)/1e6).toFixed(3)} 秒`:view==='result'&&entry?(entry.pauseIds.length?'当前为剪辑点附近预览':'开头预览'):span>=duration?'滚轮放大后可平移 · 刻度或播放头拖动定位':'拖动轨道或黄色区间平移 · 双击黄色区间聚焦';
 const pixel=(t:number)=>{const dpr=window.devicePixelRatio||1;return Math.round(position(t)/100*trackWidth*dpr)/dpr};
 return <section className="timeline-shell" aria-label="剪辑时间轴" data-range-start={range[0]} data-range-end={range[1]} data-dragging={dragging}>
  <div className="timeline-tools"><div className="history-actions"><button title={undoLabel?`撤销：${undoLabel}（Ctrl+Z）`:'暂无可撤销操作'} aria-label="撤销" disabled={historyDisabled||!undoLabel} onClick={onUndo}><HistoryIcon/><span>撤销</span></button><button title={redoLabel?`重做：${redoLabel}（Ctrl+Shift+Z）`:'暂无可重做操作'} aria-label="重做" disabled={historyDisabled||!redoLabel} onClick={onRedo}><HistoryIcon redo/><span>重做</span></button></div><div className="timeline-view"><button className={mode==='all'?'active':''} onClick={()=>{finish(false);setWindow([0,duration],'all')}} aria-label="时间轴显示全片">全片</button><button className={mode==='cut'?'active':''} onClick={()=>focus()} aria-label="时间轴显示当前区间">当前区间</button><IconButton icon="plus" label="放大时间轴" onClick={()=>zoom(1.5,current)}/><IconButton icon="minus" label="缩小时间轴" onClick={()=>zoom(1/1.5,current)}/></div></div>
  <div ref={track} className={`timeline ${dragging?'is-dragging '+dragging:''}`} tabIndex={0} aria-label="时间轴轨道" onPointerDown={e=>start(e)} onPointerMove={move} onPointerUp={up} onPointerCancel={()=>finish(false)} onLostPointerCapture={()=>finish(false)} onMouseLeave={()=>{if(!gesture.current)setHover('')}} onDoubleClick={e=>{const item=selectNear(e.clientX);if(item)focus(item.startUs,item.endUs)}}>
   <div className="ruler" data-drag="scrub">{ticks.map(t=>{const pct=position(t);return <span key={t} className={pct>90?'edge-right':pct<3?'edge-left':''} style={{left:`${pct}%`}}>{shortTime(t).replace(/\.\d+$/,'')}</span>})}</div>
   <div className="timeline-lane"/>
   {view==='source'?p?.pauses.filter(c=>c.state!=='rejected'&&c.endUs>=range[0]&&c.startUs<=range[1]).map(c=>{const a=c,text=`原片 ${shortTime(a.startUs)}—${shortTime(a.endUs)}`;return <button key={c.id} type="button" data-drag="pan" data-id={c.id} className={`cut ${c.state} ${c.id===selected?'chosen':''} ${!c.enabled?'disabled':''}`} aria-label={`删除区间 ${shortTime(c.startUs)}`} title={`${text} · 拖动平移时间轴，双击放大`} style={{left:Math.max(0,pixel(a.startUs)),width:Math.max(1,Math.min(trackWidth,pixel(a.endUs))-Math.max(0,pixel(a.startUs)))}} onMouseEnter={()=>setHover(text)} onClick={e=>{e.preventDefault();if(e.detail===0)props.onPause(c.id)}}/>}):plan?.removed.map((c,i)=>c.outputUs>=range[0]&&c.outputUs<=range[1]?<button key={i} data-drag="cut" data-index={i} className={`cut-point ${entry?.index===i?'chosen':''}`} aria-label={`剪辑点 ${i+1}`} title={`第 ${i+1} 处 · 成片 ${shortTime(c.outputUs)}`} style={{left:pixel(c.outputUs)}} onMouseEnter={()=>setHover(`第 ${i+1} 处 · 成片 ${shortTime(c.outputUs)}`)} onClick={e=>{e.preventDefault();if(e.detail===0)props.onCut(i)}}/>:null)}
   {view==='result'&&entry&&<div className="preview-coverage" style={{left:`${Math.max(0,position(entry.startUs))}%`,width:`${Math.max(0,Math.min(100,position(entry.endUs))-Math.max(0,position(entry.startUs)))}%`}}/>}
   {current>=range[0]&&current<=range[1]&&<div className={`playhead-hit ${position(current)<1?'at-left':position(current)>99?'at-right':''}`} data-drag="scrub" aria-label="拖动播放头" style={{left:pixel(current)}}><i className="playhead"/></div>}
  </div>
  <div ref={nav} className="timeline-nav" aria-label="时间轴范围导航" tabIndex={0} onPointerDown={e=>start(e,true)} onPointerMove={move} onPointerUp={up} onPointerCancel={()=>finish(false)} onLostPointerCapture={()=>finish(false)} title={span>=duration?'滚轮放大后可平移':'拖动视窗平移，两侧调整可视范围'}><div className="nav-window" data-drag="nav-move" style={{left:`${duration?range[0]/duration*100:0}%`,width:`${duration?span/duration*100:100}%`}}/><button data-drag="nav-start" className="nav-handle start" aria-label="调整可视范围起点" style={{left:`${duration?range[0]/duration*100:0}%`}}/><button data-drag="nav-end" className="nav-handle end" aria-label="调整可视范围终点" style={{left:`${duration?range[1]/duration*100:100}%`}}/></div>
  <div className="timeline-caption" title="滚轮缩放；拖动轨道或黄色区间平移；拖动刻度或播放头定位；双击黄色区间聚焦"><span>{shown&&view==='source'||view==='result'&&entry||teaching?hover||label:hover||'时间轴'}</span></div>
 </section>;
}


