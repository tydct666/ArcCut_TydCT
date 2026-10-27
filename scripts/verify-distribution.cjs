// Verify the exact collected materials; completeness remains an explicit reviewed state.
const fs=require('fs'),path=require('path'),crypto=require('crypto');
const root=path.resolve(__dirname,'..');
function read(p){return JSON.parse(fs.readFileSync(p,'utf8').replace(/^\uFEFF/,''));}
function verify(base,requireComplete=false){
 const spec=read(path.join(root,'release/distribution-files.json')),runtime=read(path.join(root,'release/runtime-lock.json')),version=read(path.join(root,'package.json')).version;
 if(spec.schema!==1||spec.productVersion!==version||!Array.isArray(spec.files)||!spec.files.length)throw Error('Invalid distribution inventory');
 if(spec.distributionMode!=='light-online-prepare'||spec.scope.ffmpeg!=='downloaded-directly-from-provider-not-bundled')throw Error('Expected lightweight distribution scope');
 const seen=new Set();base=path.resolve(base);
 for(const f of spec.files){
  if(!/^(licenses|sources)\//.test(f.path)||f.path.split('/').some(p=>!p||p==='.'||p==='..'||/[\\:*?"<>|]/.test(p))||seen.has(f.path)||!/^[a-f0-9]{64}$/.test(f.sha256))throw Error('Invalid material path or hash');
  seen.add(f.path);const file=path.resolve(base,f.path);
  if(!file.startsWith(base+path.sep)||!fs.realpathSync(file).startsWith(fs.realpathSync(base)+path.sep))throw Error('Material path escapes root');
  const st=fs.lstatSync(file);if(!st.isFile()||st.isSymbolicLink()||st.size!==f.bytes)throw Error('Material missing or wrong size: '+f.path);
  if(crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex')!==f.sha256)throw Error('Material hash mismatch: '+f.path);
 }
 const complete=spec.status==='complete'&&spec.distributionApproved===true&&Array.isArray(spec.blockingItems)&&spec.blockingItems.length===0;
 if(requireComplete&&!complete)throw Error('Formal distribution is blocked: '+spec.blockingItems.join('; '));
 return {files:seen.size,bytes:spec.files.reduce((n,f)=>n+f.bytes,0),status:spec.status,complete,blockingItems:spec.blockingItems};
}
if(require.main===module){try{console.log(JSON.stringify(verify(process.argv[2]||path.join(root,'distribution-support'),process.argv.includes('--require-complete'))));}catch(e){console.error(e.message);process.exitCode=1;}}
module.exports={verify};
