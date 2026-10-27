import type{Pause}from './types';
export type BoundaryKind='move'|'start'|'end';
export function lowerBound(values:number[],value:number){let a=0,b=values.length;while(a<b){const m=(a+b)>>>1;if(values[m]<value)a=m+1;else b=m}return a}
export function snapFrame(pts:number[],duration:number,value:number){if(value<=0)return 0;if(value>=duration)return duration;const at=lowerBound(pts,value);if(!at)return pts[0]??0;if(at===pts.length)return pts.at(-1)??duration;return value-pts[at-1]<=pts[at]-value?pts[at-1]:pts[at]}
export function previousFrame(pts:number[],value:number){const at=lowerBound(pts,value);return at?pts[at-1]:0}
export function nextFrame(pts:number[],duration:number,value:number){let at=lowerBound(pts,value);while(at<pts.length&&pts[at]<=value)at++;return pts[at]??duration}
export function viewport(start:number,end:number,duration:number,minSpan=1000000):[number,number]{const span=Math.max(1,Math.min(duration,Math.max(Math.min(minSpan,duration),end-start))),left=Math.max(0,Math.min(start,duration-span));return[left,left+span]}
export function totalRemoved(pauses:Pause[],replacement?:{id:string;startUs:number;endUs:number}){const spans=pauses.filter(p=>p.enabled&&p.state==='confirmed').map(p=>p.id===replacement?.id?replacement:p).sort((a,b)=>a.startUs-b.startUs);let total=0,end=0;for(const p of spans){total+=Math.max(0,p.endUs-Math.max(end,p.startUs));end=Math.max(end,p.endUs)}return total}
export function snappedBounds(pts:number[],duration:number,start:number,end:number,kind?:BoundaryKind,original?:{startUs:number;endUs:number}):[number,number]{
 let a=snapFrame(pts,duration,start),b=snapFrame(pts,duration,end);
 if(kind==='move'&&original){const length=original.endUs-original.startUs;a=snapFrame(pts,duration,Math.min(duration-length,Math.max(0,start)));b=snapFrame(pts,duration,a+length);
  // Prefer exact-duration translations whenever both real frame boundaries exist.
  const at=lowerBound(pts,a);for(const i of[at,at-1,at+1]){const candidate=pts[i];if(candidate===undefined||candidate+length>duration)continue;const match=lowerBound(pts,candidate+length);if(candidate+length===duration||pts[match]===candidate+length){a=candidate;b=candidate+length;break}}
 }
 if(b<=a){if(kind==='start')a=previousFrame(pts,b);else b=nextFrame(pts,duration,a)}
 return[a,b];
}
