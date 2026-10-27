const fs=require('fs'),path=require('path'),crypto=require('crypto');
const {execFileSync}=require('child_process');
const {files,hash,read}=require('./sign-update.cjs');
const {audit}=require('./audit-public.cjs');
const {verify:verifyDistribution}=require('./verify-distribution.cjs');
const source=require('./source-list.cjs');

function verifySourceIdentity(manifest,release){
 if(!/^[a-f0-9]{64}$/.test(manifest.sourceSnapshot)||manifest.sourceSnapshot!==source.snapshot())throw Error('Current source snapshot differs from package manifest');
 if(release&&release.sourceCommit!==(manifest.sourceCommit||manifest.sourceSnapshot))throw Error('Signed source identity differs from package manifest');
 return manifest.sourceSnapshot;
}

async function verify(stage,zip,envelopePath,requireComplete=false){
 if(!stage||!zip)throw Error('Usage: verify-release stage package.zip [update.json] [--require-complete]');
 stage=path.resolve(stage);zip=path.resolve(zip);
 verifyDistribution(stage,requireComplete);
 const manifest=read(path.join(stage,'manifest.json'));
 const p=read(path.join(source.root,'package.json'));
 if(manifest.product!=='ArkCut'||manifest.version!==p.version)throw Error('Package version mismatch');
 if(manifest.distributionMode!=='light-online-prepare'||fs.existsSync(path.join(stage,'media'))||fs.existsSync(path.join(stage,'WebView2')))throw Error('Expected a lightweight package without bundled runtimes');
 const userDocs=['更新与恢复.md','用户指南.md'].sort();
 const entries=fs.readdirSync(path.join(stage,'docs'),{withFileTypes:true});
 if(entries.some(e=>!e.isFile())||JSON.stringify(entries.map(e=>e.name).sort())!==JSON.stringify(userDocs))throw Error('Unexpected user documentation');
 for(const [published,original] of [['README.md','release/portable-README.md'],...userDocs.map(n=>['docs/'+n,'docs/'+n])]){
  if(!fs.readFileSync(path.join(stage,published)).equals(fs.readFileSync(path.join(source.root,original))))throw Error('User documentation mismatch: '+published);
 }
 const all=await files(stage);
 const expected=manifest.files.map(f=>({...f,path:f.path.replaceAll('\\','/')}));
 const actual=all.filter(f=>f.path!=='manifest.json');
 const byName=new Map(actual.map(f=>[f.path,f]));
 if(actual.length!==expected.length||new Set(expected.map(f=>f.path)).size!==expected.length||expected.some(f=>{const a=byName.get(f.path);return !a||a.bytes!==f.bytes||a.sha256!==f.sha256}))throw Error('Package inventory mismatch');
 if(!byName.has('ArkCutUpdater.exe'))throw Error('Missing updater');
 const snapshot=verifySourceIdentity(manifest);
 const sourceFiles=await Promise.all(source.list().map(async n=>({path:n,bytes:fs.statSync(path.join(source.root,n)).size,sha256:await hash(path.join(source.root,n))})));
 const python=process.env.ARKCUT_TEST_PYTHON||'python';
 // Inspect source and ZIP contents independently of the packaged manifest.
 const consistency=JSON.parse(execFileSync(python,[path.join(__dirname,'verify-source-snapshot.py'),stage,zip,path.join(path.dirname(stage),'ArkCut-'+p.version+'-source.zip'),path.join(source.root,'dist/ArcCut_TydCT')],{
  input:JSON.stringify({sourceFiles,snapshot,packageFiles:all}),encoding:'utf8',windowsHide:true,maxBuffer:1024*1024
 }));
 execFileSync(python,[path.join(__dirname,'audit-archives.py'),zip],{stdio:'inherit',windowsHide:true});
 const privacy=audit(stage);
 if(privacy.hits.length)throw Error('Package privacy audit failed: '+JSON.stringify(privacy.hits));
 if(envelopePath){
  const e=read(envelopePath),c=read(path.join(source.root,'release/update-config.json')),k=c.keys.find(k=>k.id===e.keyId);
  if(!k)throw Error('Unknown key');
  const pub=crypto.createPublicKey({key:Buffer.concat([Buffer.from('302a300506032b6570032100','hex'),Buffer.from(k.key,'base64')]),format:'der',type:'spki'});
  const bytes=Buffer.from(e.payload,'base64');
  if(!crypto.verify(null,bytes,pub,Buffer.from(e.signature,'base64')))throw Error('Bad signature');
  const r=JSON.parse(bytes);
  verifySourceIdentity(manifest,r);
  if(r.version!==p.version||r.product!=='ArkCut'||r.asset.root!==path.basename(stage)||r.asset.bytes!==fs.statSync(zip).size||r.asset.sha256!==await hash(zip)||r.files.length!==all.length||new Set(r.files.map(f=>f.path)).size!==all.length||all.some(f=>!r.files.some(a=>a.path===f.path&&a.bytes===f.bytes&&a.sha256===f.sha256)))throw Error('Signed release mismatch');
  if(fs.readFileSync(envelopePath+'.sig','utf8').trim()!==e.signature)throw Error('Detached signature mismatch');
 }
 return {version:p.version,files:all.length,zipBytes:fs.statSync(zip).size,sha256:await hash(zip),sourceSnapshot:snapshot,sourceConsistency:consistency,signatureVerified:!!envelopePath};
}
if(require.main===module){const args=process.argv.slice(2).filter(a=>!a.startsWith('--'));verify(args[0],args[1],args[2],process.argv.includes('--require-complete')).then(r=>console.log(JSON.stringify(r))).catch(e=>{console.error(e.message);process.exitCode=1});}
module.exports={verify,verifySourceIdentity};
