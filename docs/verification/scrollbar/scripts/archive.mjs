import {readFileSync,writeFileSync,mkdirSync,copyFileSync,readdirSync} from 'node:fs';
import {execFileSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {join,dirname} from 'node:path';
const tmp='/tmp/minicore-scrollbar.jB2v84', dest='docs/verification/scrollbar';
const copy=(from,to)=>{mkdirSync(dirname(to),{recursive:true});copyFileSync(from,to);};
for(const name of ['stable','msrv','fmt-check','clippy','doc','linux-build','agent-build','e2e-stable','e2e-msrv','toolchains','macos-debug','macos-release','macos-hashes','dsym','stable-api-error','stable-layout-fixture-error',...['baseline','candidate'].flatMap(v=>[1,2,3].map(n=>`benchmark-${v}-${n}`))]) copy(`${tmp}/logs/${name}.log`,`${dest}/logs/${name}.log`);
for(const name of ['render-red','review-red','reference-tests-2','iteration-1','iteration-3','native-debug','native-release','macho-debug','macho-release']) copy(`${tmp}/${name}.log`,`${dest}/logs/${name}.log`);
for(const name of ['verify.sh','benchmark.sh','build-macos.sh','native_scrollbar.py','prepare-baseline.mjs','benchmark-instrumentation.rs']) copy(`${tmp}/${name}`,`${dest}/scripts/${name}`);
for(const profile of ['debug','release']) {
 const result=JSON.parse(readFileSync(`${tmp}/native-${profile}.log`,'utf8').trim().split('\n').at(-1));
 if(result.status!=='PASS'||!result.root.includes('/minicore-native-scrollbar-'))throw Error('unexpected native root');
 for(const file of readdirSync(join(result.root,'evidence'))) {
  if(!/^[a-zA-Z0-9-]+\.(txt|json)$/.test(file))throw Error('unexpected evidence file');
  copy(join(result.root,'evidence',file),`${dest}/native/${profile}/${file}`);
 }
 for(const file of ['tty-before','tty-after'])copy(join(result.root,file),`${dest}/native/${profile}/${file}`);
}
const paths=['Cargo.toml','Cargo.lock','.cargo/config.toml','src','tests','snapshots','scripts/macos-linker.sh','tools/scrollbar_oracle.mjs'];
const tracked=execFileSync('git',['ls-files','-z','--',...paths],{encoding:'utf8'});
const added=execFileSync('git',['ls-files','--others','--exclude-standard','-z','--',...paths],{encoding:'utf8'});
const files=[...new Set((tracked+added).split('\0').filter(Boolean))].sort();
const manifest=Object.fromEntries(files.map(path=>[path,createHash('sha256').update(readFileSync(path)).digest('hex')]));
writeFileSync(`${dest}/source-sha256.json`,JSON.stringify(manifest,null,2)+'\n');
console.log(`source_files=${files.length}`);
