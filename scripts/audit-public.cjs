// Read-only, bounded-memory audit. Reports file names and categories, never matched secrets.
const fs=require('fs'),path=require('path'),os=require('os');
const root=path.resolve(__dirname,'..');
const terms=[root,os.homedir(),process.env.CARGO_HOME,process.env.RUSTUP_HOME].filter(Boolean);
const needles=[];for(const term of terms){for(const s of new Set([term,term.replaceAll('\\','/'),term.replaceAll('/','\\')])){for(const encoding of ['utf8','utf16le'])needles.push(Buffer.from(s.toLowerCase(),encoding));}}
// Crypto libraries contain PEM delimiter constants; require an encoded body, not just a marker.
const patterns=[['credential',/github_pat_[A-Za-z0-9_]{25,}|gh[pousr]_[A-Za-z0-9]{30,}|-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----\r?\n(?:[A-Za-z0-9+/=]{32,}\r?\n){2}/],['personal-path',/[a-z]:[\\/]+users[\\/]+(?!public\b|default\b)[a-z0-9_.-]+[\\/]/i],['machine-report',/"(?:memoryBytes|Win32_Processor|CSName)"\s*:|(?:Intel\(R\) Core\(TM\).*?\d{4,}|NVIDIA GeForce RTX \d{4}.*?GPU)/]];
patterns.push(['source-media-name',/(?:Videoshot|Screenrecording|QQ)[_-]?\d{8}[_-]?\d{6}[^\s\0\\/<>]{0,120}\.(?:png|jpe?g|webp|mp4|mov|mkv)\b|codex-clipboard-[a-f0-9-]{36}\.(?:png|jpe?g)/i],['legacy-version',/\b0[.]10[.]0-dev\b/]);
function audit(dir){const hits=[],files=[];function walk(p){for(const e of fs.readdirSync(p,{withFileTypes:true})){const f=path.join(p,e.name);if(e.isSymbolicLink())throw Error('Public artifact contains symlink');if(e.isDirectory())walk(f);else files.push(f)}}walk(dir);
 for(const f of files){const name=path.relative(dir,f).replaceAll('\\','/'),found=new Set();if(/(?:^|\/)(?:environment\.json|本机环境\.json|验收数据\.json)|\.(?:pk8|pem|key|pdb)$/i.test(name))found.add('private-file');
 if(/(?:^|\/)cn-mobile-20x9-v[123]\.json$|(?:^|\/)components\/(?:LogPanel|DiagnosticSidebar)\.tsx$/.test(name))found.add('historical-file');
 if(/(?:^|\/)profiles\/[^/]+\.json$/.test(name)){const profile=JSON.parse(fs.readFileSync(f,'utf8').replace(/^\uFEFF/,''));if(Object.hasOwn(profile,'sources'))found.add('source-provenance');}
 const fd=fs.openSync(f,'r'),buf=Buffer.alloc(1024*1024);let tail=Buffer.alloc(0),n;try{while((n=fs.readSync(fd,buf,0,buf.length,null))>0){const b=Buffer.concat([tail,buf.subarray(0,n)]);const texts=[b.toString('utf8'),b.toString('utf16le'),Buffer.from(b.subarray(1)).toString('utf16le')];const lower=texts.map((s,i)=>Buffer.from(s.toLowerCase(),i?'utf16le':'utf8'));if(needles.some(v=>lower.some(s=>s.includes(v))))found.add('build-path');for(const [kind,re] of patterns)if(texts.some(s=>re.test(s)))found.add(kind);tail=Buffer.from(b.subarray(Math.max(0,b.length-4096)));}}finally{fs.closeSync(fd)}
 for(const kind of found)hits.push({file:name,kind});}
 return {files:files.length,hits,status:hits.length?'failed':'passed'};}
if(require.main===module){try{const r=audit(path.resolve(process.argv[2]));console.log(JSON.stringify(r,null,2));if(r.hits.length)process.exitCode=1;}catch(e){console.error(e.message);process.exitCode=1;}}
module.exports={audit};
