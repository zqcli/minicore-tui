import {readFileSync,writeFileSync,readdirSync} from 'node:fs';
import {execFileSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {join,relative} from 'node:path';
const dir='docs/verification/scrollbar';
const sha=p=>createHash('sha256').update(readFileSync(p)).digest('hex');
const source=JSON.parse(readFileSync(join(dir,'source-sha256.json'),'utf8'));
for(const [path,expected] of Object.entries(source))if(sha(path)!==expected)throw Error(`source mismatch: ${path}`);
const artifactPaths={debug:'target/debug/minicore-tui',release:'target/release/minicore-tui'};
const delivery={implementation:execFileSync('git',['rev-parse','5feab8d'],{encoding:'utf8'}).trim(),source_files:Object.keys(source).length,source_archive_sha256:sha('/tmp/minicore-scrollbar.jB2v84/source-5feab8d.tar'),source_manifest_sha256:sha(join(dir,'source-sha256.json')),artifacts:Object.fromEntries(Object.entries(artifactPaths).map(([profile,path])=>[profile,{path,sha256:sha(path)}])),backup:'target/preserved-before-scrollbar-jB2v84',agent_implementation:'f1697f78ce48c8f5f3fde0dc9903c153022bfd9e',atomic_per_file:true,user_process_restart:false};
writeFileSync(join(dir,'delivery.json'),JSON.stringify(delivery,null,2)+'\n');
const files=[];
function walk(root){for(const item of readdirSync(root,{withFileTypes:true})){const path=join(root,item.name);if(item.isDirectory())walk(path);else if(relative(dir,path)!=='checksums.sha256')files.push(path);}}
walk(dir);files.sort();writeFileSync(join(dir,'checksums.sha256'),files.map(path=>`${sha(path)}  ${relative(dir,path)}\n`).join(''));
console.log(`sources_match=${Object.keys(source).length} evidence_files=${files.length}`);
