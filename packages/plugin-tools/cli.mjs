import {parseArgs} from 'node:util'
import {identity,importKey,generateKey,exportKey,restoreKey,identifier} from './vault.mjs'
const {values,positionals}=parseArgs({allowPositionals:true,options:{plugin:{type:'string'},file:{type:'string'},'expected-public-key':{type:'string'}}})
async function password(){let input='';for await(const chunk of process.stdin){input+=chunk;if(input.length>8192)throw Error('Password input too large.')}return JSON.parse(input).password}
try{
  const id=identifier(values.plugin),operation=positionals[0];let result
  if(operation==='status'){const entry=await identity(id);result={pluginId:id,configured:!!entry,publicKey:entry?.publicKey??null}}
  else if(operation==='import')result=await importKey(id,values.file,values['expected-public-key'])
  else if(operation==='init')result=await generateKey(id)
  else if(operation==='export')result=await exportKey(id,values.file,await password())
  else if(operation==='restore')result=await restoreKey(id,values.file,await password(),values['expected-public-key'])
  else throw Error('Use status/import/init/export/restore --plugin ID; import/export/restore use --file. Backup passwords are JSON on stdin.')
  process.stdout.write(JSON.stringify(result)+'\n')
}catch(error){process.stderr.write(JSON.stringify({code:error.code??'AUTHOR_TOOL_FAILED',message:error.message})+'\n');process.exitCode=1}
