import type{Project}from './types';

function equal(a:unknown,b:unknown):boolean{
 if(a===b)return true;
 if(!a||!b||typeof a!=='object'||typeof b!=='object')return false;
 if(Array.isArray(a)!==Array.isArray(b))return false;
 const left=a as Record<string,unknown>,right=b as Record<string,unknown>,keys=Object.keys(left);
 return keys.length===Object.keys(right).length&&keys.every(k=>Object.hasOwn(right,k)&&equal(left[k],right[k]));
}

// Archive revisions remain monotonic. Only metadata edits may share a media
// generation; returning to old media through undo never revives stale jobs.
export function sameMedia(a:Project,b:Project):boolean{
 if(a.projectId!==b.projectId)return false;
 const {revision:ar,name:an,outputPath:ao,completedExport:ae,source:as,...ac}=a;
 const {revision:br,name:bn,outputPath:bo,completedExport:be,source:bs,...bc}=b;
 return equal(as,bs)&&equal(ac,bc);
}
export class MediaRevisions{
 generation=0;
 private id='';private first=-1;private last=-1;
 reset(next:Project){this.id=next.projectId;this.first=this.last=next.revision;this.generation++}
 advance(before:Project|null,next:Project){
  const compatible=!!before&&this.accepts(before)&&next.revision===before.revision+1&&sameMedia(before,next);
  if(compatible)this.last=next.revision;else this.reset(next);
  return !compatible;
 }
 accepts(value:{projectId:string;revision:number}|null|undefined){return !!value&&value.projectId===this.id&&value.revision>=this.first&&value.revision<=this.last}
}
