import type {Project} from '../types';
import {shortTime} from '../api';
import {Button} from './Controls';
export interface RepairIssue {kind:string;message:string;sourceUs:number|null;outputUs:number|null;pauseIds:string[];scope:string;status:string}
export interface RepairReport {state:string;issues:RepairIssue[];effectiveProject:Project|null;beforeProject:Project|null;changes:{pauseId:string;before:{startUs:number;endUs:number};after:{startUs:number;endUs:number}}[]}
export function RepairNotice({report,busy,onLocate,onRetry,onConfirm,onUndo}:{report:RepairReport|null;busy:boolean;onLocate:(v:RepairIssue)=>void;onRetry:()=>void;onConfirm:()=>void;onUndo:()=>void}){
 if(!report)return null;
 if(report.state==='passed')return report.changes.length?<section className="repair-notice" aria-label="边界修正结果"><span>{report.changes.map(c=>`已修正 ${shortTime(c.before.startUs)}–${shortTime(c.before.endUs)} → ${shortTime(c.after.startUs)}–${shortTime(c.after.endUs)}`).join('；')}</span><Button disabled={busy} onClick={onUndo}>撤销自动修正</Button></section>:null;
 if(!report.issues.length)return null;
 return <section className="repair-notice" aria-label="检查与修复" role="status"><strong>成片尚未通过检查</strong><div className="repair-issues">{report.issues.map((issue,index)=><div className="repair-issue" key={index}><span>{issue.kind==='suspectedFrame'?'待确认 · ':''}{issue.message}{issue.sourceUs!==null?` · 原片 ${shortTime(issue.sourceUs)} / 成片 ${shortTime(issue.outputUs??0)}`:' · 文件级问题'}</span>{issue.sourceUs!==null&&<Button disabled={busy} onClick={()=>onLocate(issue)}>查看问题</Button>}</div>)}</div><div className="repair-actions"><Button disabled={busy} onClick={onRetry}>重试修复</Button>{report.issues.every(v=>v.kind==='suspectedFrame')&&<Button disabled={busy} onClick={onConfirm}>确认保留画面</Button>}{report.beforeProject&&<Button disabled={busy} onClick={onUndo}>撤销自动修正</Button>}</div></section>;
}
