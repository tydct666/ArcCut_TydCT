"""Read-only Win32 process memory sampling; no third-party dependency."""
import ctypes as c
from ctypes import wintypes as w
class ProcessEntry(c.Structure):
 _fields_=[('size',w.DWORD),('usage',w.DWORD),('pid',w.DWORD),('heap',c.c_size_t),('module',w.DWORD),('threads',w.DWORD),('parent',w.DWORD),('priority',w.LONG),('flags',w.DWORD),('exe',w.WCHAR*260)]
class Counters(c.Structure):
 _fields_=[('size',w.DWORD),('faults',w.DWORD),('peak',c.c_size_t),('working',c.c_size_t),('qpeakpaged',c.c_size_t),('qpaged',c.c_size_t),('qpeaknonpaged',c.c_size_t),('qnonpaged',c.c_size_t),('pagefile',c.c_size_t),('peakpagefile',c.c_size_t)]
k=c.WinDLL('kernel32',use_last_error=True);p=c.WinDLL('psapi',use_last_error=True)
k.CreateToolhelp32Snapshot.argtypes=[w.DWORD,w.DWORD];k.CreateToolhelp32Snapshot.restype=w.HANDLE
k.Process32FirstW.argtypes=[w.HANDLE,c.POINTER(ProcessEntry)];k.Process32NextW.argtypes=[w.HANDLE,c.POINTER(ProcessEntry)]
k.OpenProcess.argtypes=[w.DWORD,w.BOOL,w.DWORD];k.OpenProcess.restype=w.HANDLE;k.CloseHandle.argtypes=[w.HANDLE]
p.GetProcessMemoryInfo.argtypes=[w.HANDLE,c.POINTER(Counters),w.DWORD]
def rss_tree(pid):
 snapshot=k.CreateToolhelp32Snapshot(2,0);entry=ProcessEntry();entry.size=c.sizeof(entry);parents={}
 try:
  ok=k.Process32FirstW(snapshot,c.byref(entry))
  while ok:parents[entry.pid]=entry.parent;ok=k.Process32NextW(snapshot,c.byref(entry))
 finally:k.CloseHandle(snapshot)
 own={pid};changed=True
 while changed:
  expanded=own|{child for child,parent in parents.items()if parent in own};changed=expanded!=own;own=expanded
 total=0
 for process in own:
  h=k.OpenProcess(0x410,False,process)
  if not h:continue
  try:
   m=Counters();m.size=c.sizeof(m)
   if p.GetProcessMemoryInfo(h,c.byref(m),m.size):total+=m.working
  finally:k.CloseHandle(h)
 return total
