// Private key bytes are never printed. Run only for an explicitly requested key.
const fs=require('fs'),path=require('path'),crypto=require('crypto');
const dir=path.resolve(process.argv[2]||'.local-signing');
fs.mkdirSync(dir,{recursive:true});
if(process.platform==='win32'){
 const cp=require('child_process');
 const identity=cp.execFileSync('whoami.exe',['/user','/fo','csv','/nh'],{encoding:'utf8',windowsHide:true});
 const sid=identity.match(/S-1-5-[0-9-]+/);
 if(!sid)throw Error('Cannot identify current Windows user; no key generated');
 cp.execFileSync('icacls.exe',[dir,'/inheritance:r','/grant:r',`*${sid[0]}:(OI)(CI)F`],{stdio:'pipe',windowsHide:true});
}
const keyPath=path.join(dir,'arkcut-release-private.pk8');
if(fs.existsSync(keyPath))throw Error('Refusing to replace an existing signing key');
const {privateKey,publicKey}=crypto.generateKeyPairSync('ed25519');
fs.writeFileSync(keyPath,privateKey.export({format:'der',type:'pkcs8'}),{flag:'wx',mode:0o600});
const publicBytes=publicKey.export({format:'der',type:'spki'}).subarray(-32);
const id=crypto.createHash('sha256').update(publicBytes).digest('hex').slice(0,16);
fs.writeFileSync(path.join(dir,'public-key.json'),JSON.stringify({id,key:publicBytes.toString('base64')},null,2),{flag:'wx'});
console.log(JSON.stringify({privateKeyPath:keyPath,keyId:id,publicKeyPath:path.join(dir,'public-key.json')}));
