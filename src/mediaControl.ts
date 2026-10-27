export type MediaKind='video'|'music';
export interface MediaAdapter {canPlay:()=>boolean;isPlaying:()=>boolean;play:(ticket:number)=>Promise<unknown>|void;pause:()=>void}

// One intent clock covers both loaded media and playback promised by background work.
export class MediaControl {
 private adapters=new Map<MediaKind,MediaAdapter>();private listeners=new Set<()=>void>();private version=0;private serial=0;private blocked=false;
 target:MediaKind='video';pending:MediaKind|null=null;onIntent=()=>{};onPause=()=>{};
 subscribe=(listener:()=>void)=>{this.listeners.add(listener);return()=>{this.listeners.delete(listener)}};
 getVersion=()=>this.version;
 changed=()=>{this.version++;for(const listener of this.listeners)listener()};
 register(kind:MediaKind,adapter:MediaAdapter){this.adapters.set(kind,adapter);this.changed();return()=>{if(this.adapters.get(kind)===adapter){adapter.pause();this.adapters.delete(kind);this.changed()}}}
 isPlaying(kind?:MediaKind){return kind?!!this.adapters.get(kind)?.isPlaying():[...this.adapters.values()].some(a=>a.isPlaying())}
 valid(ticket:number){return ticket===this.serial&&!this.blocked}
 cancelPending(preservePlaying=false){if(!preservePlaying||!this.isPlaying())this.serial++;this.pending=null;this.changed()}
 focus(kind:MediaKind){this.target=kind;if(this.pending&&this.pending!==kind)this.cancelPending();for(const[k,a]of this.adapters)if(k!==kind)a.pause();this.changed()}
 prepare(kind:MediaKind){if(this.blocked)return-1;this.onIntent();for(const a of this.adapters.values())a.pause();return this.claim(kind)}
 claim(kind:MediaKind){if(this.blocked)return-1;this.target=kind;for(const[k,a]of this.adapters)if(k!==kind)a.pause();this.pending=kind;const ticket=++this.serial;this.changed();return ticket}
 settled(ticket:number){if(ticket===this.serial){this.pending=null;this.changed()}}
 pauseAll(){this.serial++;this.pending=null;for(const a of this.adapters.values())a.pause();this.onPause();this.changed()}
 setBlocked(value:boolean){if(this.blocked===value)return;this.blocked=value;if(value)this.pauseAll();else this.changed()}
 play(kind:MediaKind){if(this.blocked||!this.adapters.get(kind)?.canPlay())return;this.onIntent();const ticket=this.claim(kind);Promise.resolve(this.adapters.get(kind)?.play(ticket)).finally(()=>this.settled(ticket)).catch(()=>{})}
 toggleTarget(kind:MediaKind){if(this.isPlaying(kind)||this.pending===kind)this.pauseAll();else this.play(kind)}
 toggle(){if(this.blocked)return;if(this.isPlaying()||this.pending)this.pauseAll();else this.play(this.target)}
}
