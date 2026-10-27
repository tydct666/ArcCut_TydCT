export interface Fingerprint {path:string;size:number;modifiedMs:number;sampleHash:string}
export interface Stream {index:number;kind:string;codec:string;language:string;channels:number;channelLayout?:string;width:number;height:number;rate:string;timeBase:string;startUs:number;default:boolean}
export interface Media {title?:string;fingerprint:Fingerprint;durationUs:number;format:string;streams:Stream[];videoIndex:number|null;audioIndex:number|null;originUs:number;framePts:number[];hdr:boolean;warnings:string[]}
export type JoinMode='basic'|'legacy'|'natural';
export interface Pause {id:string;startUs:number;endUs:number;enabled:boolean;source:string;state:string;score:number;audioMode?:JoinMode|null;detectedBounds?:[number,number]|null;transition?:{version:string;originalBounds:[number,number];appliedBounds:[number,number];decision:string;evidence:unknown}|null}
export interface Music {id:string;media:Media}
export interface ExportRecord {signature:string;path:string;bytes:number;sha256:string}
export interface Project {completedExport?:ExportRecord;name?:string;outputPath?:string;schemaVersion:number;projectId:string;revision:number;source:Media;videoIndex:number;audioIndex:number|null;detection:{status?:string;geometry?:{width:number;height:number;scale:number}|null;version:string;scanHz:number;minPauseUs:number;activityDb:number;region:number[]|null;profile:string|null};pauses:Pause[];music:Music[];mix:{gameGain:number;musicGain:number;tails:boolean;joinMode:JoinMode;mode:string;modeManual:boolean;currentTrack:string|null;normalize:boolean};export:{acceleration?:string;container:string;codec:string;quality:string;fps:number|null;width:number|null;height:number|null;previewEdge:number}}
export interface Span {startUs:number;endUs:number;outputUs:number}
export interface Tail {sourceUs:number;outputUs:number;durationUs:number;gain:number;mode:JoinMode;envelope:{timeUs:number;gain:number}[];notes:string[]}
export interface Plan {version:string;projectId:string;revision:number;keep:Span[];removed:Span[];durationUs:number;tails:Tail[];music:{track:number;outputUs:number;durationUs:number;fadeInUs:number;fadeOutUs:number}[];warnings:string[]}
export interface Audit {residualFrames:number;candidateFrames:number;transitionResidualFrames?:number;needsReview?:boolean;checks?:unknown[]}
export interface Preview {completedExport?:ExportRecord;path:string;revision:number;projectId:string;startUs:number;endUs:number;mediaStartUs:number;mediaEndUs:number;cached:boolean;quickPreview?:boolean;plan?:Plan;audit?:Audit;effectiveProject?:Project|null}
export interface PreviewEntry {id:string;pauseIds:string[];index:number;cutUs:number;sourceStartUs?:number;sourceEndUs?:number;startUs:number;endUs:number;projectId:string;revision:number;status:'queued'|'rendering'|'ready'|'failed'|'cancelled';path:string|null;mediaStartUs?:number;mediaEndUs?:number;cached:boolean;quickPreview?:boolean;audit?:Audit;readyMs?:number;error?:string}
export interface Metric {phase:string;completedUnits:number;totalUnits:number|null;unit:string;detail:string;elapsedMs:number}
export interface Job extends Metric {jobId:string;projectId:string;revision:number;stage:string;operation:string;status:string;processedUs:number|null;result:unknown;error:string|null;previews:PreviewEntry[];importedProject:Project|null;resource?:{notice?:string;pressure?:string;threads?:number}}
export interface Log extends Metric {sequence:number;jobId:string;projectId:string;revision:number;stage:string;status:string;processedUs:number|null;message:string}
export interface BoundaryCorrection {reason:string;message:string;baseRevision:number;changes:{pauseId:string;before:{startUs:number;endUs:number}|null;after:{startUs:number;endUs:number}|null}[]}
export interface Batch {outcome?:'ready'|'revised';effectiveProject?:Project;correction?:BoundaryCorrection;interrupted?:boolean;previews:PreviewEntry[];plan:Plan;ready:number;total:number;renderGroups:number;firstReadyMs:number|null;elapsedMs:number;projectId:string;revision:number}

export interface ArchiveEntry {projectId:string;name:string;sourcePath:string;status:string;createdMs:number;updatedMs:number;path:string}
export interface BoundaryDraft {start:string;end:string}
