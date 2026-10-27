import {detectionMessage}from './labels';
import type {Job,Log,Project} from './types';

export type UserLogLevel='running'|'completed'|'failed'|'cancelled'|'notice';
export interface UserLog {id:string;time:number;status:UserLogLevel;message:string}
export const logLevels:Record<UserLogLevel,string>={running:'信息',completed:'完成',failed:'错误',cancelled:'取消',notice:'提示'};
const operations:Record<string,[string,string]>={import:['开始导入录屏','录屏导入完成'],analyze:['开始识别暂停画面','识别完成'],previewBatch:['开始生成剪辑预览','剪辑预览已就绪'],preview:['开始生成片段预览','片段预览已就绪'],export:['开始导出','导出完成'],musicBatch:['开始添加音乐','音乐已添加'],music:['开始添加音乐','音乐已添加'],sourceProxy:['开始准备原片预览','原片预览已就绪']};

// Only known, actionable causes reach the interface. Raw errors remain in diagnostics.
export function failureSummary(raw:string):string {
 if(/PAUSE_REVIEW_REQUIRED/.test(raw))return '检查发现画面问题，请查看问题并复核边界';
 if(/RESOURCE_MEMORY|RESOURCE_COMMIT|内存不足|提交余量/.test(raw))return '当前内存不足，请关闭暂时不用的应用后重试';
 if(/RESOURCE_DISK|DISK_FULL|磁盘空间|磁盘已满/.test(raw))return '磁盘可用空间不足，请释放空间或更换保存位置后重试';
 if(/denied|permission|拒绝访问|权限|只读/i.test(raw))return '无法写入文件，请选择可写的保存位置后重试';
 if(/not found|No such file|不存在|找不到|已移走|已删除|路径失效/i.test(raw))return '找不到所需文件，请检查文件位置后重试';
 if(/decode|invalid data|损坏|解码失败/i.test(raw))return '无法读取媒体内容，请检查文件能否正常播放或重新选择素材';
 if(/CANCELLED|任务已取消/.test(raw))return '任务已取消，可重新开始';
 return '操作未完成，请重试；若仍失败，可导出日志排查';
}
const resultOf=(event:Log,job:Job|null):Record<string,unknown>|null=>event.jobId===job?.jobId&&job.result&&typeof job.result==='object'?job.result as Record<string,unknown>:null;
export function presentEvent(event:Log,job:Job|null):Omit<UserLog,'id'|'time'>|null {
 const {stage,status}=event,operation=operations[stage];
 if(status==='failed')return {status:'failed',message:`${operation?.[1].replace(/完成|已就绪|已添加/g,'')||'操作'}未完成：${failureSummary(event.message)}`};
 if(status==='cancelled'&&operation)return {status:'cancelled',message:`已取消${{import:'导入',analyze:'识别',previewBatch:'剪辑预览',preview:'片段预览',export:'导出',musicBatch:'添加音乐',music:'添加音乐',sourceProxy:'原片预览'}[stage]}`};
 if(stage==='user.interval'){
  const index=Number(event.message);return {status:'completed',message:Number.isInteger(index)&&index>0?`已调整第 ${index} 处删除区间`:'已更新删除区间'};
 }
 if(stage==='user.musicRemoved')return {status:'completed',message:'已移除音乐'};
 if(stage==='user.history'&&['undo','redo'].includes(event.message))return {status:'completed',message:event.message==='undo'?'已撤销修改':'已重做修改'};
 if(!operation)return null;
 if(status==='running')return {status:'running',message:operation[0]};
 if(status!=='completed')return null;
 const result=resultOf(event,job);
 let message=operation[1];
 if((stage==='import'||stage==='analyze')&&Array.isArray(result?.pauses)){
  const count=result.pauses.filter((p:unknown)=>{const v=p as {enabled:boolean;state:string};return v.enabled&&v.state==='confirmed'}).length;
  const state=stage==='import'?(result.detection as Project['detection'])?.status:result.status;
  message=state?detectionMessage({pauses:result.pauses,detection:stage==='import'?result.detection:{status:state}} as Project):`识别完成，找到 ${count} 段暂停画面`;
  if(state==='unsupported')return {status:'notice',message};
 }
 if(stage==='previewBatch'){
  if(result?.outcome==='revised')message='暂停边界已校正，继续生成预览';
  else if(typeof result?.ready==='number')message=(result.plan as {removed?:unknown[]})?.removed?.length===0?'开头预览已就绪':`剪辑预览已就绪，共 ${result.ready} 处`;
 }
 if(stage==='export')message+=`，用时 ${Math.max(1,Math.round(event.elapsedMs/1000))} 秒`;
 return {status:'completed',message};
}

/** A bounded presentation layer; diagnostic events are never changed or discarded on disk. */
export class UserLogFeed {
 private seen=new Set<string>();
 private resourceJob='';
 private constrained=false;
 private resourceSequence=0;
 consume(events:Log[],job:Job|null,now=Date.now()):UserLog[]{
  const rows:UserLog[]=[];
  for(const event of events){
   const row=presentEvent(event,job);if(!row)continue;
   const key=event.stage.startsWith('user.')?`event:${event.sequence}`:`${event.jobId}:${event.stage}:${event.status}`;
   const failureKey=event.status==='failed'?`${event.jobId}:cause:${failureSummary(event.message)}`:'';
   if(this.seen.has(key)||failureKey&&this.seen.has(failureKey))continue;
   this.seen.add(key);if(failureKey)this.seen.add(failureKey);while(this.seen.size>1000)this.seen.delete(this.seen.values().next().value!);
   rows.push({...row,id:key,time:now});
  }
  if(job?.jobId!==this.resourceJob){this.resourceJob=job?.jobId||'';this.constrained=false}
  if(job?.status==='running'){
   const constrained=!!job.resource?.notice;
   if(constrained!==this.constrained){rows.push({id:`resource:${++this.resourceSequence}`,time:now,status:'notice',message:constrained?'资源紧张，已降低处理负载':'资源已恢复，继续正常处理'});this.constrained=constrained}
  }
  return rows;
 }
}
