import type{Batch,Project,PreviewEntry}from './types';

export const MAX_PREVIEW_REBUILDS=1;
export interface BatchHost {
 request:(snapshot:Project)=>Promise<Batch>;
 current:(snapshot:Project)=>boolean;
 adopt:(result:Batch,snapshot:Project)=>Promise<Project>;
 stopped:()=>boolean;
}

// Only this coordinator repeats work. Adopting a revision does not create a new
// playback intent; the host carries the current intent across its save barrier.
export async function rebuildPreviews(initial:Project,host:BatchHost):Promise<Batch|null>{
 let snapshot=initial;
 for(let rebuilds=0;host.current(snapshot);rebuilds++){
  if(host.stopped())throw Error('CANCELLED: 预览已停止，已完成内容保留。');
  const result=await host.request(snapshot);
  if(!host.current(snapshot))return null;
  if(result.outcome!=='revised'){
   if(host.stopped())throw Error('CANCELLED: 已停止预览，已完成内容保留。');
   return result;
  }
  const next=result.effectiveProject;
  if(!next||next.projectId!==snapshot.projectId||next.revision<=snapshot.revision
   ||result.plan.projectId!==next.projectId||result.plan.revision!==next.revision||!result.correction)
   throw Error('边界校正结果不完整，工程未被替换。请重试预览。');
  snapshot=await host.adopt(result,snapshot);
  if(!host.current(snapshot))return null;
  if(host.stopped()||result.interrupted)throw Error('CANCELLED: 边界校正已保存，预览已停止，可重新生成。');
  if(rebuilds>=MAX_PREVIEW_REBUILDS)throw Error(`已自动重建 ${MAX_PREVIEW_REBUILDS} 次，边界仍需复核。请检查时间戳后重试。`);
 }
 return null;
}

export function remapPreviewTarget(target:string,before:Project,after:Project,entries:PreviewEntry[]):string{
 if(!target)return '';
 const id=entries.find(e=>e.id===target)?.pauseIds[0]||target;
 if(after.pauses.some(p=>p.id===id&&p.enabled&&p.state==='confirmed'))return id;
 const old=before.pauses.find(p=>p.id===id);
 if(!old)return '';
 const matches=after.pauses.filter(p=>p.enabled&&p.state==='confirmed').map(p=>({p,overlap:Math.min(p.endUs,old.endUs)-Math.max(p.startUs,old.startUs)})).filter(x=>x.overlap>0).sort((a,b)=>b.overlap-a.overlap);
 return matches[0]?.p.id||'';
}
