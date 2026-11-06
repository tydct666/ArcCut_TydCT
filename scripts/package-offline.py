"""Build a separate local offline bundle from the verified lightweight stage."""
import argparse, hashlib, json, shutil, zipfile
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
def read(p):return json.loads(p.read_text(encoding='utf-8-sig'))
def sha(p):
    h=hashlib.sha256()
    with p.open('rb') as f:
        for block in iter(lambda:f.read(1024*1024),b''):h.update(block)
    return h.hexdigest()
def main():
    parser=argparse.ArgumentParser();parser.add_argument('--runtime',type=Path,required=True);parser.add_argument('--media',type=Path,required=True);parser.add_argument('--media-notices',type=Path,required=True);a=parser.parse_args()
    version=read(ROOT/'package.json')['version'];light=ROOT/f'dist/ArkCut-{version}-win64-light';stage=ROOT/f'dist/ArkCut-{version}-win64-offline'
    assert not stage.exists(),'Use a fresh offline staging directory'
    manifest=read(light/'manifest.json');lock=read(ROOT/'release/offline-runtime.json');media=read(ROOT/'release/runtime-lock.json')['ffmpeg']
    for name,digest in lock['files'].items():assert sha(a.runtime/name)==digest,name
    for name,key in [('ffmpeg.exe','ffmpegSha256'),('ffprobe.exe','ffprobeSha256')]:assert sha(a.media/name)==media[key],name
    assert not any(p.is_symlink() or (hasattr(p,'is_junction') and p.is_junction()) for p in a.runtime.rglob('*'))
    stage.mkdir()
    for item in manifest['files']:
        rel=Path(item['path'].replace('\\','/'));assert not rel.is_absolute() and '..' not in rel.parts
        src=light/rel;assert src.stat().st_size==item['bytes'] and sha(src)==item['sha256']
        dest=stage/rel;dest.parent.mkdir(parents=True,exist_ok=True);shutil.copyfile(src,dest)
    shutil.copytree(a.runtime,stage/'WebView2')
    (stage/'media').mkdir()
    for name in ['ffmpeg.exe','ffprobe.exe']:shutil.copyfile(a.media/name,stage/'media'/name)
    for name in ['LICENSE','README.txt']:shutil.copyfile(a.media_notices/name,stage/'media'/name)
    for name in ['Microsoft-WebView2-Fixed-Runtime-License.txt','Microsoft-WebView2-Fixed-Runtime-License.html']:
        shutil.copyfile(ROOT/'distribution-support/licenses/non-media'/name,stage/'licenses'/name)
    shutil.copyfile(ROOT/'release/offline-README.md',stage/'README.md')
    manifest.update(distributionMode='offline-local',webView2=lock['version'],releaseStatus='local-use',distributionSourceAudit='offline-public-distribution-not-verified')
    manifest['files']=[dict(path=str(p.relative_to(stage)).replace('\\','/'),bytes=p.stat().st_size,sha256=sha(p)) for p in sorted(stage.rglob('*')) if p.is_file()]
    manifest['unpackedBytes']=sum(v['bytes'] for v in manifest['files']);(stage/'manifest.json').write_text(json.dumps(manifest,ensure_ascii=False,indent=2),encoding='utf8')
    package=Path(str(stage)+'.zip')
    assert not package.exists(),'Preserve the previous offline ZIP before packaging'
    with zipfile.ZipFile(package,'w',compression=zipfile.ZIP_DEFLATED,compresslevel=6,strict_timestamps=False) as z:
        for p in sorted(stage.rglob('*')):
            if p.is_file():z.write(p,str(p.relative_to(stage.parent)))
    with zipfile.ZipFile(package) as z:
        expected={stage.name+'/'+f['path']:f for f in manifest['files']}
        assert set(z.namelist())==set(expected)|{stage.name+'/manifest.json'}
        for name,item in expected.items():
            h=hashlib.sha256();size=0
            with z.open(name) as f:
                for block in iter(lambda:f.read(1024*1024),b''):h.update(block);size+=len(block)
            assert size==item['bytes'] and h.hexdigest()==item['sha256'],name
    result=dict(status='passed',zip=package.name,bytes=package.stat().st_size,sha256=sha(package),sourceSnapshot=manifest['sourceSnapshot'],files=len(manifest['files']),unpackedBytes=manifest['unpackedBytes'])
    out=ROOT/f'outputs/release-audit/{version}';out.mkdir(parents=True,exist_ok=True);(out/'offline-package.json').write_text(json.dumps(result,indent=2),encoding='utf8');print(json.dumps(result))
if __name__=='__main__':main()
