// Rebuild dependency inventories from the locked installed graph, without local paths.
const fs=require('fs'),path=require('path'),cp=require('child_process');
const root=path.resolve(__dirname,'..');
const read=p=>JSON.parse(fs.readFileSync(p,'utf8').replace(/^\uFEFF/,''));
const inventory=read(path.join(root,'release/distribution-files.json'));
const metadata=JSON.parse(cp.execFileSync('cargo',['metadata','--locked','--offline','--format-version','1','--manifest-path','src-tauri/Cargo.toml','--features','desktop,custom-protocol','--filter-platform','x86_64-pc-windows-msvc'],{cwd:root,encoding:'utf8',maxBuffer:32*1024*1024,windowsHide:true}));
const resolved=new Set(metadata.resolve.nodes.map(n=>n.id));
const order=(a,b)=>(a.name+' '+a.version).localeCompare(b.name+' '+b.version,'en');
const rust=metadata.packages.filter(p=>p.source&&resolved.has(p.id)).map(p=>({name:p.name,version:p.version,license:p.license,repository:p.repository,licenseDirectory:`licenses/rust/${p.name}-${p.version}`})).sort(order);
const npm=new Map();
function visit(name,from){
 let dir=path.dirname(require.resolve(name,{paths:[from]})),pkg;
 for(;;){const file=path.join(dir,'package.json');if(fs.existsSync(file)){const candidate=read(file);if(candidate.name===name){pkg=candidate;break}}const parent=path.dirname(dir);if(parent===dir)throw Error('Package metadata missing: '+name);dir=parent;}
 const id=pkg.name+'@'+pkg.version;if(npm.has(id))return;
 npm.set(id,{name:pkg.name,version:pkg.version,license:pkg.license,licenseFiles:fs.readdirSync(dir).filter(n=>/^(LICENSE|LICENCE|COPYING|NOTICE)(?:[._-]|$)/i.test(n)&&fs.statSync(path.join(dir,n)).isFile()).sort()});
 for(const dependency of Object.keys(pkg.dependencies||{}).sort())visit(dependency,dir);
}
for(const name of Object.keys(read(path.join(root,'package.json')).dependencies).sort())visit(name,root);
const archives=inventory.files.filter(f=>f.path.endsWith('.crate'));
const mpl=rust.filter(p=>p.license?.includes('MPL-2.0'));
if(mpl.length!==archives.length||mpl.some(p=>!archives.some(f=>f.component===p.name&&f.version===p.version)))throw Error('MPL source inventory does not match the locked Windows graph');
const generated={
 'licenses/non-media/RUST-PACKAGES.json':{scope:'Locked Windows desktop graph, including build dependencies.',packages:rust},
 'licenses/non-media/NPM-RUNTIME-PACKAGES.json':[...npm.values()].sort(order),
 'sources/non-media/SOURCE-MANIFEST.json':{description:'Exact registry source archives for MPL-2.0 Rust packages present in the resolved Windows desktop dependency graph. Includes build-time packages; no local modifications.',files:archives.map(f=>({name:f.component,version:f.version,license:f.license,file:f.path,sha256:f.sha256,source:f.source}))}
};
const write=process.argv.includes('--write');
for(const [relative,value] of Object.entries(generated)){
 const file=path.join(root,'distribution-support',relative),contents=JSON.stringify(value,null,2)+'\n';
 if(write){fs.mkdirSync(path.dirname(file),{recursive:true});fs.writeFileSync(file,contents)}
 else if(fs.readFileSync(file,'utf8')!==contents)throw Error('Stale generated distribution document: '+relative+'; regenerate with --write, review, then refresh its inventory hash');
}
console.log(JSON.stringify({mode:write?'written':'verified',documents:Object.keys(generated).length,rustPackages:rust.length,npmPackages:npm.size,mplSources:archives.length}));
