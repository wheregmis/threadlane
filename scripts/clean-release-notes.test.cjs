const assert = require('node:assert/strict');
const {cleanNotes} = require('./clean-release-notes.cjs');
const a='a'.repeat(40), b='b'.repeat(40), c='c'.repeat(40);
const line=(sha,description='same fix') => '* '+description+' (['+sha.slice(0,7)+'](https://github.com/wheregmis/threadlane/commit/'+sha+'))';
const notes='### Bug Fixes\n'+line(a)+'\n'+line(b)+'\n'+line(c,'distinct fix')+'\n';
const commits={
[a]:{parents:[{sha:c},{sha:b}],commit:{message:'Merge pull request #1 from wheregmis/test\n\nfix: same fix'}},
[b]:{parents:[{sha:c}],commit:{message:'fix: same fix'}},
[c]:{parents:[{sha:b}],commit:{message:'fix: distinct fix'}}
};
const github={rest:{repos:{getCommit:async({ref})=>({data:commits[ref]}),compareCommits:async()=>({data:{status:'identical'}})}}};
(async()=>{
const cleaned=await cleanNotes(notes,{github,owner:'wheregmis',repo:'threadlane'});
assert.equal(cleaned,'### Bug Fixes\n'+line(b)+'\n'+line(c,'distinct fix')+'\n');
assert.equal(await cleanNotes(cleaned,{github,owner:'wheregmis',repo:'threadlane'}),cleaned);
commits[a].parents=[{sha:c}];
assert.equal(await cleanNotes(notes,{github,owner:'wheregmis',repo:'threadlane'}),notes);
commits[a].parents=[{sha:c},{sha:b}];
github.rest.repos.compareCommits=async()=>({data:{status:'diverged'}});
assert.equal(await cleanNotes(notes,{github,owner:'wheregmis',repo:'threadlane'}),notes);
github.rest.repos.compareCommits=async()=>({data:{status:'identical'}});
commits[a].commit.message='Merge pull request #1 from wheregmis/test\n\nfix: different';
assert.equal(await cleanNotes(notes,{github,owner:'wheregmis',repo:'threadlane'}),notes);
assert.equal(await cleanNotes('### Features\n'+line(a)+'\n### Bug Fixes\n'+line(b),{github,owner:'wheregmis',repo:'threadlane'}),'### Features\n'+line(a)+'\n### Bug Fixes\n'+line(b));
console.log('Passed: verified merge removal, distinct fix preservation, idempotence, independent commits, divergent ancestry, different messages, section isolation');
})().catch(e=>{console.error(e);process.exit(1)});
