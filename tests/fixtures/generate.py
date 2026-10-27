"""Reproducible signal sources. Never reads or writes user recordings/music."""
from pathlib import Path
import subprocess,json
ROOT=Path(__file__).resolve().parents[2]
OUT=Path(__file__).parent/'generated';OUT.mkdir(exist_ok=True)
FF=str(ROOT/'tools/media/ffmpeg.exe')
def ff(*args):
    subprocess.run([FF,'-hide_banner','-v','error','-y',*map(str,args)],check=True)
font="drawtext=fontfile='C\\:/Windows/Fonts/consola.ttf':text='FRAME %{n}  %{pts\\:hms}':x=100:y=40:fontsize=20:fontcolor=white:box=1:boxcolor=black"
flash="drawbox=x=0:y=0:w=64:h=64:color=white:t=fill:enable='lt(mod(t,0.5),0.03332)'"
pulse="aevalsrc=if(lt(mod(t\\,0.5)\\,0.02)\\,0.6*sin(2*PI*1000*t)\\,0):s=48000:d=10"
base=OUT/'中文 空格 & 信号.mp4'
ff('-f','lavfi','-i','testsrc2=s=640x360:r=30:d=10','-f','lavfi','-i',pulse,'-f','lavfi','-i','sine=frequency=440:sample_rate=48000:duration=10','-map','0:v','-map','1:a','-map','2:a','-vf',flash+','+font,'-c:v','libx264','-preset','ultrafast','-crf','18','-c:a','aac','-metadata:s:a:0','language=zho','-metadata:s:a:1','language=eng',base)
for ext,vc,ac in [('mkv','libx264','aac'),('mov','libx264','aac'),('webm','libvpx-vp9','libopus'),('avi','mpeg4','mp3'),('flv','flv','mp3'),('ts','libx264','aac'),('mts','libx264','aac'),('wmv','wmv2','wmav2')]:
    ff('-i',base,'-map','0:v:0','-map','0:a:0','-c:v',vc,'-c:a',ac,'-t','2',OUT/f'container.{ext}')
ff('-i',base,'-map','0:v:0','-an','-c:v','copy',OUT/'silent.mp4')
ff('-i',base,'-map','0:v:0','-map','0:a:0','-vf',"select='not(eq(mod(n,5),1)+eq(mod(n,5),2))'",'-fps_mode','vfr','-c:v','libx264','-preset','ultrafast','-c:a','aac','-output_ts_offset','3',OUT/'vfr-offset.mkv')
for index,length in [(1,1.3),(2,.18)]:
    ff('-f','lavfi','-i',f'sine=frequency={220*index}:sample_rate=44100:duration={length}','-c:a','pcm_s16le',OUT/f'music-{index}.wav')
# 20 removed windows, frame/pulse sync markers every 0.5s.
ff('-f','lavfi','-i','color=c=black:s=160x90:r=30:d=44','-f','lavfi','-i','aevalsrc=if(lt(mod(t\\,0.5)\\,0.02)\\,0.6*sin(2*PI*1000*t)\\,0):s=48000:d=44','-vf',flash,'-c:v','libx264','-preset','ultrafast','-c:a','aac',OUT/'sync-20-cuts.mp4')
annotations={'schemaVersion':1,'purpose':'Media pipeline only, not game recognition','baseDurationUs':10000000,'exampleCutsUs':[[2000000,4000000],[6000000,7000000]],'syncCutsUs':[[int((2*i+1.2)*1e6),int((2*i+1.4)*1e6)]for i in range(20)],'pulsePeriodUs':500000,'flashFrames':1,'sampleRate':48000}
(OUT/'annotations.json').write_text(json.dumps(annotations,indent=2),encoding='utf-8')
print('Generated',len(list(OUT.iterdir())),'synthetic files in',OUT)
