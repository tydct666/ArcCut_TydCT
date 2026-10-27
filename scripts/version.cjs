const fs=require('fs'),path=require('path');
const root=path.resolve(__dirname,'..');const p=JSON.parse(fs.readFileSync(path.join(root,'package.json')));
if(!/^\d+\.\d+\.\d+$/.test(p.version))throw Error('A stable semver is required');
const file=path.join(root,'src-tauri/tauri.conf.json'),config=JSON.parse(fs.readFileSync(file));
const cargoPath=path.join(root,'src-tauri/Cargo.toml'),cargo=fs.readFileSync(cargoPath,'utf8');
if(process.argv.includes('--write')){config.version=p.version;config.productName='ArkCut';config.app.windows[0].title='ArkCut';fs.writeFileSync(file,JSON.stringify(config,null,2)+'\n');fs.writeFileSync(cargoPath,cargo.replace(/^(version\s*=\s*)"[^"]+"/m,`$1"${p.version}"`));}
else if(config.version!==p.version||!cargo.includes(`version = "${p.version}"`)||config.productName!=='ArkCut'||config.app.windows[0].title!=='ArkCut')throw Error('Version/product mismatch');
console.log(p.version);
