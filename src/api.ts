import {invoke,isTauri} from '@tauri-apps/api/core';
export const api=<T,>(op:string,payload:unknown={}):Promise<T>=>isTauri()?invoke('api',{op,payload}):Promise.reject(new Error('请使用 ArkCut 桌面程序连接本地媒体引擎'));
export const fileName=(p:string)=>p.split(/[\\/]/).pop()||p;
export const timecode=(us:number)=>{let n=Math.max(0,Math.round(us/1000));const ms=n%1000;n=Math.floor(n/1000);const s=n%60;n=Math.floor(n/60);return `${String(Math.floor(n/60)).padStart(2,'0')}:${String(n%60).padStart(2,'0')}:${String(s).padStart(2,'0')}.${String(ms).padStart(3,'0')}`};
// Probe results are immutable. History only copies editable data, so a long
// recording's frame index is shared across edits instead of copied 60 times.
export const clone=(x:import('./types').Project):import('./types').Project=>{const {source,music,...editable}=x;return {...structuredClone(editable),source,music:music.map(item=>({...item}))}};
export const outputCodec=(p:import('./types').Project)=>{const codec=p.source.streams.find(s=>s.index===p.videoIndex&&s.kind==='video')?.codec||'';return ['h264','hevc','av1','vp9'].includes(codec)?codec:'ffv1'};
const outputFamilies:Record<string,string[]>={mp4:['h264','hevc','av1','vp9'],mov:['h264','hevc'],mkv:['h264','hevc','av1','vp9','ffv1'],webm:['vp8','vp9','av1'],ts:['h264','hevc'],mts:['h264','hevc'],m2ts:['h264','hevc'],flv:[]};
export const container=(p:import('./types').Project)=>{
 const ext=p.source.fingerprint.path.split('.').at(-1)!.toLowerCase(),codec=outputCodec(p);
 return outputFamilies[ext]?.includes(codec)?ext:['h264','hevc'].includes(codec)?'mp4':'mkv';
};
export const formatChangeReason=(p:import('./types').Project)=>{
 const ext=p.source.fingerprint.path.split('.').at(-1)!.toLowerCase(),target=container(p),sourceCodec=p.source.streams.find(s=>s.index===p.videoIndex&&s.kind==='video')?.codec||'',codec=outputCodec(p),label=(value:string)=>value==='webm'?'WebM':value.toUpperCase(),changes:string[]=[],reasons:string[]=[];
 if(ext!==target)changes.push(`${label(ext)} → ${label(target)}`);
 if(sourceCodec!==codec){changes.push(`${label(sourceCodec)} → ${label(codec)}`);reasons.push('原视频编码不支持当前无损剪辑路径，改用 FFV1 保留画面像素')}
 if(ext!==target)reasons.push(['avi','wmv','flv'].includes(ext)?'原封装不支持所需的无损编码与精确帧时序':`原封装不支持 ${label(codec)} 无损画面编码`);
 return changes.length?`${changes.join('，')}：${reasons.join('；')}`:'';
};
export const sourceOutput=(p:import('./types').Project,value:string)=>value.replace(/\.[^.\\/]+$/,'')+'.'+container(p);
// Local wall-clock time makes repeated exports identifiable without stacking
// timestamps on a previously exported name. Milliseconds separate quick retries.
export const timestampedOutput=(value:string,now=new Date())=>{
 const pad=(n:number,width=2)=>String(n).padStart(width,'0');
 const stamp=`${now.getFullYear()}${pad(now.getMonth()+1)}${pad(now.getDate())}_${pad(now.getHours())}${pad(now.getMinutes())}${pad(now.getSeconds())}_${pad(now.getMilliseconds(),3)}`;
 const split=Math.max(value.lastIndexOf('/'),value.lastIndexOf('\\')),dir=value.slice(0,split+1),name=value.slice(split+1),dot=name.lastIndexOf('.');
 const stem=dot>0?name.slice(0,dot):name,ext=dot>0?name.slice(dot):'';
 return dir+stem.replace(/_\d{8}_\d{6}(?:_\d{3})?$/,'')+'_'+stamp+ext;
};
const displayCache=new WeakMap<import('./types').Media,Map<number,string>>();
export function displayInfo(p:import('./types').Project){const old=displayCache.get(p.source)?.get(p.videoIndex);if(old)return old;const s=p.source.streams.find(s=>s.index===p.videoIndex);if(!s)return '—';const rotated=p.source.warnings.some(w=>w.includes('旋转')),pts=p.source.framePts,rate=pts.length*1e6/p.source.durationUs;let min=Infinity,max=0;for(let i=1;i<pts.length;i++){const step=pts[i]-pts[i-1];min=Math.min(min,step);max=Math.max(max,step)}const text=`${rotated?s.height:s.width}×${rotated?s.width:s.height} · ${max-min>1000?'VFR':'CFR'} ${rate.toFixed(2)} fps · ${timecode(p.source.durationUs)}`;const cache=displayCache.get(p.source)||new Map<number,string>();cache.set(p.videoIndex,text);displayCache.set(p.source,cache);return text;}

export const readablePath=(value:string)=>value.startsWith("\\\\?\\UNC\\")?"\\\\"+value.slice(8):value.startsWith("\\\\?\\")?value.slice(4):value;
export const shortTime=(us:number)=>us>=3600e6?timecode(us):timecode(us).slice(3);
export function parseTime(value:string){const parts=value.trim().split(":");if(!parts.length||parts.length>3||parts.some(x=>!/^\d+(\.\d+)?$/.test(x)))return NaN;const n=parts.map(Number);if(n.length>1&&n.at(-1)!>=60||n.length===3&&n[1]>=60)return NaN;return Math.round(n.reduce((a,b)=>a*60+b,0)*1e6)}
