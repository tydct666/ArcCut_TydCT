// Include licenses from the resolved Windows build, not unrelated Cargo cache entries.
const fs=require('fs'),path=require('path'),cp=require('child_process');
const root=path.resolve(__dirname,'..');
const destination=path.resolve(process.argv[2]);
const metadata=JSON.parse(cp.execFileSync('cargo',['metadata','--locked','--offline','--format-version','1','--manifest-path','src-tauri/Cargo.toml','--features','desktop,custom-protocol','--filter-platform','x86_64-pc-windows-msvc'],{cwd:root,encoding:'utf8',maxBuffer:32*1024*1024,windowsHide:true}));
const ids=new Set(metadata.resolve.nodes.map(n=>n.id));let packages=0,files=0;
for(const p of metadata.packages){
 if(!ids.has(p.id)||!p.source)continue;
 const dir=path.dirname(p.manifest_path),names=new Set(fs.readdirSync(dir).filter(n=>/^(LICENSE|LICENCE|COPYING|NOTICE)(?:[._-]|$)/i.test(n)&&fs.statSync(path.join(dir,n)).isFile()));
 if(p.license_file){const file=path.resolve(dir,p.license_file);if(!file.startsWith(dir+path.sep)||!fs.statSync(file).isFile())throw Error('Invalid dependency license path');names.add(path.relative(dir,file));}
 if(!names.size)continue;
 if(!/^[A-Za-z0-9_.-]+$/.test(p.name)||!/^[A-Za-z0-9_.+-]+$/.test(p.version))throw Error('Invalid dependency name');
 const out=path.join(destination,p.name+'-'+p.version);fs.mkdirSync(out,{recursive:true});
 for(const n of names){fs.copyFileSync(path.join(dir,n),path.join(out,n.replace(/[\\/]/g,'-')));files++;}
 packages++;
}
console.log(JSON.stringify({resolvedPackages:ids.size,licensePackages:packages,licenseFiles:files}));
