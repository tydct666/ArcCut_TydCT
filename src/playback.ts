export type PlayerStatus='preparing'|'paused'|'playing'|'ended'|'failed';
export interface PlaybackRequest {id:number;projectId:string;revision:number;entryId:string;positionUs:number;play:boolean;reason:string;ticket?:number;rangeUs?:[number,number]}
export interface PlayerRange {start:number;end:number;offsetUs:number}

// One controller owns seek completion, the logical clock, and every end guard.
export class LogicalPlayer {
 private generation=0;private timer:ReturnType<typeof setTimeout>|undefined;private frame=0;
 private waits=new Set<()=>void>();private disposed=false;private finishing=false;
 private listeners: [string,EventListener][]=[];
 constructor(readonly element:HTMLVideoElement,readonly range:PlayerRange,
  private clock:(us:number)=>void,private state:(status:PlayerStatus,error?:string)=>void){
  this.listen('timeupdate',()=>this.sync());this.listen('playing',()=>{if(this.disposed)return;this.state('playing');this.arm()});
  this.listen('pause',()=>{if(!this.finishing&&!this.disposed)this.state(this.atEnd()?'ended':'paused');this.stopGuards()});
  this.listen('ended',()=>this.finish());this.listen('ratechange',()=>this.arm());
 }
 private listen(name:string,fn:()=>void){this.element.addEventListener(name,fn);this.listeners.push([name,fn]);}
 private stopGuards(){if(this.timer)clearTimeout(this.timer);this.timer=undefined;if(this.frame)this.element.cancelVideoFrameCallback(this.frame);this.frame=0;}
 private invalidate(){this.generation++;this.stopGuards();for(const cancel of [...this.waits])cancel();return this.generation;}
 private valid(token:number){return !this.disposed&&token===this.generation;}
 private wait(token:number,events:string[],ready:()=>boolean):Promise<boolean>{
  if(!this.valid(token))return Promise.resolve(false);if(ready())return Promise.resolve(true);
  return new Promise(resolve=>{let timer:ReturnType<typeof setTimeout>,poll:ReturnType<typeof setInterval>;
   const done=(ok:boolean)=>{clearTimeout(timer);clearInterval(poll);events.forEach(e=>this.element.removeEventListener(e,check));this.waits.delete(cancel);resolve(ok)};
   const cancel=()=>done(false);const check=()=>{if(!this.valid(token))done(false);else if(ready())done(true)};
   // WebView can coalesce seek events when adjacent entries share one file.
   // Observe the actual media state as well; all observers belong to this request.
   this.waits.add(cancel);events.forEach(e=>this.element.addEventListener(e,check));poll=setInterval(check,50);timer=setTimeout(()=>done(this.valid(token)&&ready()),12000);
  });
 }
 async request(globalUs:number,play:boolean,allowed:()=>boolean=()=>true){
  const token=this.invalidate();this.finishing=false;this.element.pause();this.state('preparing');
  try{
   if(!await this.wait(token,['loadedmetadata','canplay'],()=>this.element.readyState>=1)){if(this.valid(token))throw Error('读取媒体超时');return}
   const target=Math.min(this.range.end,Math.max(this.range.start,(globalUs-this.range.offsetUs)/1e6));
   if(Math.abs(this.element.currentTime-target)>.000001){this.element.currentTime=target;
    if(!await this.wait(token,['seeked','canplay'],()=>!this.element.seeking&&Math.abs(this.element.currentTime-target)<.04)){if(this.valid(token))throw Error('定位片段超时');return}
   }
   if(!await this.wait(token,['loadeddata','canplay','seeked'],()=>this.element.readyState>=2)){if(this.valid(token))throw Error('片段尚未可播放');return}
   if(!this.valid(token))return;this.sync(false);
   if(play&&allowed()&&target<this.range.end-.002){await this.element.play();if(!this.valid(token)){return}if(!allowed()){this.pause();return}this.state('playing');this.arm()}
   else this.state(this.atEnd()?'ended':'paused');
  }catch(error){if(this.valid(token))this.state('failed',`点击播放重试：${error instanceof Error?error.message:String(error)}`)}
 }
 pause(){this.invalidate();this.element.pause();this.state(this.atEnd()?'ended':'paused');this.sync(false)}
 play(allowed:()=>boolean=()=>true){return this.request(this.atEnd()?this.range.offsetUs+this.range.start*1e6:this.range.offsetUs+this.element.currentTime*1e6,true,allowed)}
 seek(globalUs:number){return this.request(globalUs,!this.element.paused&&!this.atEnd())}
 private atEnd(){return this.element.currentTime>=this.range.end-.003}
 private sync(checkEnd=true){
  if(this.disposed||this.element.readyState<1)return;
  if(checkEnd&&!this.element.seeking&&!this.element.paused&&this.atEnd()){this.finish();return}
  const t=Math.min(this.range.end,Math.max(this.range.start,this.element.currentTime));this.clock(Math.round(t*1e6)+this.range.offsetUs);
 }
 private finish(){
  if(this.disposed||this.finishing)return;this.finishing=true;this.stopGuards();this.element.pause();
  if(Number.isFinite(this.element.duration))this.element.currentTime=Math.min(this.range.end,this.element.duration);
  this.clock(Math.round(this.range.end*1e6)+this.range.offsetUs);this.state('ended');
 }
 private arm(){
  this.stopGuards();if(this.disposed||this.element.paused||this.element.seeking)return;
  const token=this.generation;const check=()=>{if(!this.valid(token)||this.element.paused)return;if(this.atEnd()){this.finish();return}this.timer=setTimeout(check,Math.max(4,(this.range.end-this.element.currentTime)/Math.max(.1,this.element.playbackRate)*1000))};
  const watch=()=>{if(!this.valid(token)||this.element.paused)return;this.sync();if(!this.element.paused)this.frame=this.element.requestVideoFrameCallback(watch)};
  this.timer=setTimeout(check,Math.max(4,(this.range.end-this.element.currentTime)/Math.max(.1,this.element.playbackRate)*1000));this.frame=this.element.requestVideoFrameCallback(watch);
 }
 destroy(){this.disposed=true;this.invalidate();this.listeners.forEach(([name,fn])=>this.element.removeEventListener(name,fn));this.element.pause();}
}
