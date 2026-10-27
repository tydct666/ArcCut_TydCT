// Exercise exports in isolated fixtures; existing repositories must survive intact.
const assert=require('assert'),fs=require('fs'),os=require('os'),path=require('path'),cp=require('child_process');
const root=path.resolve(__dirname,'..');
const temp=fs.mkdtempSync(path.join(os.tmpdir(),'arkcut-source-preparation-'));
const run=(program,args,cwd)=>cp.spawnSync(program,args,{cwd,encoding:'utf8',windowsHide:true});
let checks=0;
try{
 for(const kind of ['directory','file']){
  const fixture=path.join(temp,kind),scripts=path.join(fixture,'scripts'),repo=path.join(fixture,'dist/ArcCut_TydCT');
  fs.mkdirSync(scripts,{recursive:true});fs.mkdirSync(repo,{recursive:true});
  fs.writeFileSync(path.join(fixture,'package.json'),JSON.stringify({version:'1.0.0'}));
  fs.copyFileSync(path.join(root,'scripts/prepare-github.ps1'),path.join(scripts,'prepare-github.ps1'));
  const git=path.join(repo,'.git');
  if(kind==='directory'){fs.mkdirSync(git);fs.writeFileSync(path.join(git,'HEAD'),'ref: refs/heads/main\n');}
  else fs.writeFileSync(git,'gitdir: ../separate-worktree\n');
  fs.writeFileSync(path.join(repo,'local.txt'),'retain uncommitted work');
  const result=run('pwsh',['-NoProfile','-File',path.join(scripts,'prepare-github.ps1')],fixture);
  assert.notEqual(result.status,0);assert.match(result.stderr+'\n'+result.stdout,/contains \.git/);
  assert.equal(fs.readFileSync(path.join(repo,'local.txt'),'utf8'),'retain uncommitted work');
  assert.equal(fs.readFileSync(kind==='directory'?path.join(git,'HEAD'):git,'utf8'),kind==='directory'?'ref: refs/heads/main\n':'gitdir: ../separate-worktree\n');
  assert.deepEqual(fs.readdirSync(path.join(fixture,'dist')),['ArcCut_TydCT']);assert(!fs.existsSync(path.join(fixture,'outputs')));checks++;
 }
 const cwd=path.join(temp,'cli');fs.mkdirSync(cwd);
 const command=path.join(root,'scripts/source-list.cjs');
 for(const arg of ['--help','-h']){const r=run(process.execPath,[command,arg],cwd);assert.equal(r.status,0,r.stderr);assert.match(r.stdout,/Usage:/);assert.deepEqual(fs.readdirSync(cwd),[]);checks++;}
 const invalid=run(process.execPath,[command,'--unknown'],cwd);assert.notEqual(invalid.status,0);assert.deepEqual(fs.readdirSync(cwd),[]);checks++;
 const exported=path.join(cwd,'source');const normal=run(process.execPath,[command,exported],cwd);
 assert.equal(normal.status,0,normal.stderr);assert(fs.existsSync(path.join(exported,'README.md')));
 assert(!fs.existsSync(path.join(exported,'.git')));checks++;
 const keep=path.join(exported,'local.txt');fs.writeFileSync(keep,'keep');
 const repeated=run(process.execPath,[command,exported],cwd);assert.notEqual(repeated.status,0);assert.equal(fs.readFileSync(keep,'utf8'),'keep');checks++;
 console.log(JSON.stringify({status:'passed',checks}));
}finally{
 const resolved=fs.realpathSync(temp),base=fs.realpathSync(os.tmpdir());
 if(path.dirname(resolved)!==base||!path.basename(resolved).startsWith('arkcut-source-preparation-'))throw Error('Unexpected fixture cleanup path');
 fs.rmSync(resolved,{recursive:true,force:true});
}
