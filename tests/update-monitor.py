"""Sample only executables under the isolated updater test folder."""
from pathlib import Path
import ctypes as c,json,time,subprocess,os
from ctypes import wintypes as w
from winmetrics import k,p,ProcessEntry,Counters
root=Path(__file__).resolve().parents[1]
out=Path(os.environ.get('ARKCUT_UPDATE_E2E_OUT',root/'tests/results/revision17-light-final')).resolve()
out.mkdir(parents=True,exist_ok=True)
k.QueryFullProcessImageNameW.argtypes=[w.HANDLE,w.DWORD,w.LPWSTR,c.POINTER(w.DWORD)]
def sample():
 result=[];snapshot=k.CreateToolhelp32Snapshot(2,0);entry=ProcessEntry();entry.size=c.sizeof(entry)
 try:
  ok=k.Process32FirstW(snapshot,c.byref(entry))
  while ok:
   if entry.exe.lower().endswith('.exe'):
    h=k.OpenProcess(0x410,False,entry.pid)
    if h:
     try:
      buf=c.create_unicode_buffer(32768);size=w.DWORD(32768)
      if k.QueryFullProcessImageNameW(h,0,buf,c.byref(size)) and Path(buf.value).resolve().is_relative_to(out):
       m=Counters();m.size=c.sizeof(m)
       if p.GetProcessMemoryInfo(h,c.byref(m),m.size):result.append(dict(pid=entry.pid,name=entry.exe,rss=m.working,peak=m.peak))
     finally:k.CloseHandle(h)
   ok=k.Process32NextW(snapshot,c.byref(entry))
 finally:k.CloseHandle(snapshot)
 return result
start=time.monotonic();peak=0;helper=0;rows=[]
with (out/'update-e2e.log').open('w',encoding='utf8')as log:
 child=subprocess.Popen(['node','tests/update-e2e.cjs'],cwd=root,stdout=log,stderr=log)
 while child.poll() is None:
  processes=sample();total=sum(v['rss']for v in processes);peak=max(peak,total)
  helper=max([helper]+[v['peak']for v in processes if v['name'].lower().startswith('helper-')])
  rows.append(dict(seconds=round(time.monotonic()-start,2),rss=total,processes=processes));time.sleep(.1)
result=dict(exitCode=child.returncode,elapsedSeconds=time.monotonic()-start,sampledApplicationTreePeakBytes=peak,helperPeakWorkingSetBytes=helper,sampleIntervalSeconds=.1,samples=rows)
(out/'resource-monitor.json').write_text(json.dumps(result,ensure_ascii=False,indent=2),encoding='utf8')
print(json.dumps({k:v for k,v in result.items()if k!='samples'}));raise SystemExit(child.returncode)
