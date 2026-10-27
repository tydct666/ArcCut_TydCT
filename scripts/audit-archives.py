"""Bounded recursive ZIP/tar audit for release contents; never executes extracted files."""
import json, os, pathlib, shutil, subprocess, sys, tarfile, tempfile, zipfile

MAX_BYTES = 512 * 1024 * 1024
MAX_FILES = 20000
total = count = 0

def safe(name):
    p = pathlib.PurePosixPath(name)
    if not name or p.is_absolute() or '\\' in name or ':' in name or any(x in ('', '.', '..') for x in name.rstrip('/').split('/')):
        raise ValueError('Unsafe archive path')
    if p.name.lower() in ('ffmpeg.exe', 'ffprobe.exe', 'msedgewebview2.exe') or 'webview2' in [x.lower() for x in p.parts[:-1]]:
        raise ValueError('Bundled runtime forbidden: ' + name)
    if p.name.lower().endswith('.cab'):
        raise ValueError('Runtime cabinet forbidden: ' + name)
    return p

def copy(source, target, size):
    global total, count
    total += size
    count += 1
    if total > MAX_BYTES or count > MAX_FILES:
        raise ValueError('Expanded audit limit exceeded')
    target.parent.mkdir(parents=True, exist_ok=True)
    with target.open('xb') as out:
        remaining = size
        while remaining:
            data = source.read(min(65536, remaining))
            if not data:
                raise ValueError('Truncated archive')
            out.write(data)
            remaining -= len(data)
        if source.read(1):
            raise ValueError('Entry size mismatch')

def expand(archive, dest, depth=0):
    if depth > 5:
        raise ValueError('Nested archive depth exceeded')
    dest.mkdir()
    names = set()
    def target(name):
        p = safe(name)
        key = str(p).lower()
        if key in names:
            raise ValueError('Duplicate archive path')
        names.add(key)
        return dest.joinpath(*p.parts)
    if zipfile.is_zipfile(archive):
        with zipfile.ZipFile(archive) as z:
            for entry in z.infolist():
                p = target(entry.filename)
                if entry.flag_bits & 1 or (entry.external_attr >> 16) & 0o170000 == 0o120000:
                    raise ValueError('Encrypted or linked archive entry')
                if not entry.is_dir():
                    with z.open(entry) as source:
                        copy(source, p, entry.file_size)
    elif tarfile.is_tarfile(archive):
        with tarfile.open(archive, 'r:*') as z:
            for entry in z:
                p = target(entry.name)
                if entry.isdir():
                    continue
                if not entry.isfile():
                    raise ValueError('Special archive entry')
                with z.extractfile(entry) as source:
                    copy(source, p, entry.size)
    else:
        raise ValueError('Unrecognized nested archive')
    for f in list(dest.rglob('*')):
        if f.is_file() and (f.suffix.lower() in ('.zip', '.crate', '.gz', '.tgz', '.tar')):
            expand(f, f.with_name(f.name + '.expanded'), depth + 1)

if __name__ == '__main__':
    with tempfile.TemporaryDirectory(prefix='arkcut-archive-audit-') as temp:
        target = pathlib.Path(temp) / 'contents'
        expand(pathlib.Path(sys.argv[1]), target)
        audit = pathlib.Path(__file__).with_name('audit-public.cjs')
        result = subprocess.run(['node', str(audit), str(target)], capture_output=True, text=True, encoding='utf-8', errors='replace')
        if result.returncode:
            print(result.stdout, result.stderr)
            raise SystemExit(1)
        print(json.dumps({'status':'passed','files':count,'expandedBytes':total,'recursivePrivacyScan':True,'runtimeBinaries':0}))
