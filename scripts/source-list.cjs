const fs=require('fs'),path=require('path'),crypto=require('crypto');
const root=path.resolve(__dirname,'..');
function list(){const c=JSON.parse(fs.readFileSync(path.join(root,'release/source-files.json')));const out=[...c.files];function walk(dir){for(const f of fs.readdirSync(path.join(root,dir),{withFileTypes:true})){const p=dir+'/'+f.name;if(f.isSymbolicLink())throw Error('Source link: '+p);if(f.isDirectory())walk(p);else if(f.isFile())out.push(p);}}for(const t of c.trees)walk(t);if(c.distributionMaterials===true){require('./verify-distribution.cjs').verify(path.join(root,'distribution-support'));for(const material of JSON.parse(fs.readFileSync(path.join(root,'release/distribution-files.json'))).files)out.push('distribution-support/'+material.path);}const names=[...new Set(out)].sort();for(const n of names){if(n.includes('..')||/\.(pk8|key|pem|exe|dll|mp4|mp3|wav|log)$/i.test(n)||n.includes('.local-signing'))throw Error('Forbidden source: '+n);const b=fs.readFileSync(path.join(root,n));if(/\.(rs|tsx?|json|md|ya?ml|cjs|ps1)$/.test(n)){const s=b.toString('utf8');if(/[A-Z]:[\\/]Users[\\/]|codex_wo[r]kspace|-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----|gh[pousr]_[A-Za-z0-9]{30,}/i.test(s))throw Error('Private machine path or credential in '+n);}}return names;}
function snapshot(){const h=crypto.createHash('sha256');for(const n of list())h.update(n+'\0').update(fs.readFileSync(path.join(root,n)));return h.digest('hex');}
if(require.main===module){
 const argv=process.argv.slice(2);
 if(argv.length===1 && ['--help','-h'].includes(argv[0])){
  console.log('Usage: node scripts/source-list.cjs [new-output-directory]\nWithout a directory, prints the reviewed source inventory. --help never writes files.');
 }else{
  if(argv.length>1 || argv.some(a=>a.startsWith('-')))throw Error('Expected one new output directory or --help');
  const names=list();
  if(argv.length){const dest=path.resolve(argv[0]);if(fs.existsSync(dest))throw Error('Refusing existing source destination');fs.mkdirSync(dest,{recursive:true});for(const n of names){const p=path.join(dest,n);fs.mkdirSync(path.dirname(p),{recursive:true});fs.copyFileSync(path.join(root,n),p);}}
  console.log(JSON.stringify({files:names.length,bytes:names.reduce((s,n)=>s+fs.statSync(path.join(root,n)).size,0),snapshot:snapshot(),paths:names},null,2));
 }
}module.exports={list,snapshot,root};
