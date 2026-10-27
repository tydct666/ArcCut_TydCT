import type{Project}from './types';
export class ArchiveWriter{
 private pending=new Map<string,Project>();private running:Promise<void>|null=null;private timer:ReturnType<typeof setTimeout>|undefined;
 constructor(private write:(project:Project,path:string)=>Promise<number>,private state:(value:'pending'|'saving'|'saved'|'failed',error?:string)=>void,private saved:(project:Project,path:string)=>void){}
 enqueue(project:Project,path:string){if(!path)throw Error('当前项目没有存档位置');this.pending.set(path,project);this.state('pending');if(this.timer)clearTimeout(this.timer);this.timer=setTimeout(()=>void this.flush().catch(()=>{}),400)}
 clear(path:string){this.pending.delete(path)}
 async flush():Promise<void>{if(this.timer)clearTimeout(this.timer);this.timer=undefined;if(this.running)return this.running;const task=(async()=>{try{while(this.pending.size){const[path,project]=this.pending.entries().next().value!;this.state('saving');const revision=await this.write(project,path);if(revision!==project.revision)throw Error('存档已有更新版本，请重新打开项目以免覆盖');if(this.pending.get(path)===project)this.pending.delete(path);this.saved(project,path)}this.state('saved')}catch(error){this.state('failed',error instanceof Error?error.message:String(error));throw error}})();this.running=task;try{await task}finally{this.running=null}}
 get hasPending(){return this.pending.size>0}
}
