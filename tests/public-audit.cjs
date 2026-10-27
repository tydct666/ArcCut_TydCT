const fs=require('fs'),os=require('os'),path=require('path'),assert=require('assert');
const {audit}=require('../scripts/audit-public.cjs');
const d=fs.mkdtempSync(path.join(os.tmpdir(),'arkcut-public-audit-'));
try{
 fs.writeFileSync(path.join(d,'clean.txt'),'ArkCut');assert.equal(audit(d).status,'passed');
 fs.writeFileSync(path.join(d,'parser.bin'),'-----BEGIN '+'PRIVATE KEY-----');assert.equal(audit(d).status,'passed');
 fs.writeFileSync(path.join(d,'utf8.bin'),path.resolve(__dirname,'..'));
 fs.writeFileSync(path.join(d,'wide.bin'),Buffer.from(path.resolve(__dirname,'..'),'utf16le'));
 fs.writeFileSync(path.join(d,'environment.json'),'{}');
 fs.writeFileSync(path.join(d,'secret.txt'),'-----BEGIN '+'PRIVATE KEY-----\n'+'A'.repeat(64)+'\n'+'B'.repeat(64)+'\n');
 const r=audit(d);for(const [file,kind] of [['utf8.bin','build-path'],['wide.bin','build-path'],['environment.json','private-file'],['secret.txt','credential']])assert(r.hits.some(x=>x.file===file&&x.kind===kind));
 fs.writeFileSync(path.join(d,'odd-wide.bin'),Buffer.concat([Buffer.from([255]),Buffer.from(path.resolve(__dirname,'..'),'utf16le')]));
 fs.writeFileSync(path.join(d,'template.bin'),'Video'+'shot_'+('20000101'+'_'+'000000')+'.jpg');
 fs.writeFileSync(path.join(d,'old-version.bin'),'0.'+'10.0'+'-dev');
 fs.mkdirSync(path.join(d,'profiles'));fs.writeFileSync(path.join(d,'profiles','current.json'),JSON.stringify({sources:[{name:'sample',sha256:'x'}]}));
 fs.writeFileSync(path.join(d,'profiles','cn-mobile-20x9-v1.json'),'{}');
 const extra=audit(d);for(const [file,kind] of [['odd-wide.bin','build-path'],['template.bin','source-media-name'],['old-version.bin','legacy-version'],['profiles/current.json','source-provenance'],['profiles/cn-mobile-20x9-v1.json','historical-file']])assert(extra.hits.some(x=>x.file===file&&x.kind===kind));
 console.log(JSON.stringify({status:'passed',checks:11}));
}finally{fs.rmSync(d,{recursive:true,force:true});}
