import {readFile,writeFile,readdir,lstat,mkdir,rename,copyFile,rm} from 'node:fs/promises'
import {deflateRawSync} from 'node:zlib'
import {createHash,randomUUID} from 'node:crypto'
import path from 'node:path'
import {seedFor} from './vault.mjs'
import {signChecksums,verifySignature,fail} from './signing.mjs'
const sha=bytes=>createHash('sha256').update(bytes).digest('hex')
export async function packageFiles(directory,prefix=''){
  const result=[]
  for(const entry of await readdir(directory,{withFileTypes:true})){
    const name=prefix?`${prefix}/${entry.name}`:entry.name,file=path.join(directory,entry.name)
    if(!/^[A-Za-z0-9_.-]+(?:\/[A-Za-z0-9_.-]+)*$/.test(name)||name.split('/').some(p=>p==='.'||p==='..'))fail('INVALID_PACKAGE','Unsafe package path.')
    if(entry.isDirectory())result.push(...await packageFiles(file,name))
    else if(entry.isFile())result.push(name)
    else fail('INVALID_PACKAGE','Package must not contain links or special files.')
  }
  return result.sort()
}
const table=Array.from({length:256},(_,i)=>{let c=i;for(let n=0;n<8;n++)c=c&1?0xedb88320^(c>>>1):c>>>1;return c>>>0})
function crc(bytes){let c=0xffffffff;for(const b of bytes)c=table[(c^b)&255]^(c>>>8);return(c^0xffffffff)>>>0}
export function zipFiles(files){
  if(files.length>65535||new Set(files.map(([name])=>name.toLowerCase())).size!==files.length)fail('INVALID_PACKAGE','ZIP32 requires unique file names and at most 65535 entries.')
  const chunks=[],central=[];let offset=0
  for(const [name,bytes] of files){
    const filename=Buffer.from(name),compressed=deflateRawSync(bytes,{level:9}),checksum=crc(bytes)
    if(filename.length>65535||bytes.length>0xffffffff||compressed.length>0xffffffff||offset+30+filename.length+compressed.length>0xffffffff)fail('INVALID_PACKAGE','Package exceeds the supported ZIP32 representation.')
    const local=Buffer.alloc(30);local.writeUInt32LE(0x04034b50);local.writeUInt16LE(20,4);local.writeUInt16LE(8,8);local.writeUInt16LE(33,12);local.writeUInt32LE(checksum,14);local.writeUInt32LE(compressed.length,18);local.writeUInt32LE(bytes.length,22);local.writeUInt16LE(filename.length,26)
    chunks.push(local,filename,compressed)
    const dir=Buffer.alloc(46);dir.writeUInt32LE(0x02014b50);dir.writeUInt16LE(20,4);dir.writeUInt16LE(20,6);dir.writeUInt16LE(8,10);dir.writeUInt16LE(33,14);dir.writeUInt32LE(checksum,16);dir.writeUInt32LE(compressed.length,20);dir.writeUInt32LE(bytes.length,24);dir.writeUInt16LE(filename.length,28);dir.writeUInt32LE(offset,42)
    central.push(dir,filename);offset+=local.length+filename.length+compressed.length
  }
  const directory=Buffer.concat(central),end=Buffer.alloc(22);end.writeUInt32LE(0x06054b50);end.writeUInt16LE(files.length,8);end.writeUInt16LE(files.length,10);end.writeUInt32LE(directory.length,12);end.writeUInt32LE(offset,16)
  return Buffer.concat([...chunks,directory,end])
}
export async function finishPackage({root,stage,archivePath,signing=process.argv.includes('--signed')?'required':'auto'}){
  const target=path.resolve(root,'target'),relative=path.relative(target,path.resolve(stage))
  if(!relative||relative.startsWith('..')||path.isAbsolute(relative))fail('INVALID_PACKAGE','Staging directory must be inside this plugin target directory.')
  if(!(await lstat(stage)).isDirectory()||(await lstat(stage)).isSymbolicLink())fail('INVALID_PACKAGE','Invalid staging directory.')
  const manifest=JSON.parse(await readFile(path.join(stage,'manifest.json'),'utf8'))
  for(const field of ['author','license']){
    const value=manifest[field]
    if(value!=null&&(typeof value!=='string'||!value.trim()||[...value].length>(field==='author'?120:256)||/[\x00-\x1f\x7f]/.test(value)))fail('INVALID_MANIFEST',`Invalid ${field} metadata.`)
  }
  if(manifest.homepage!=null){
    let url;try{url=new URL(manifest.homepage)}catch{fail('INVALID_MANIFEST','Invalid homepage URL.')}
    if(typeof manifest.homepage!=='string'||manifest.homepage.length>2048||!['http:','https:'].includes(url.protocol)||url.username||url.password||/[\x00-\x20\x7f]/.test(manifest.homepage))fail('INVALID_MANIFEST','Homepage must use HTTP(S) without credentials.')
  }
  if(signing==='required'&&(typeof manifest.author!=='string'||!manifest.author.trim()))fail('AUTHOR_REQUIRED','Formal packages require author metadata.')
  if(manifest.backend?.activation!=null&&!['on_demand','background_restore','on_startup'].includes(manifest.backend.activation))fail('INVALID_MANIFEST','Unsupported backend activation policy.')
  const files=(await packageFiles(stage)).filter(file=>!['checksums.json','signature.json'].includes(file))
  if(manifest.manifestVersion!==3||!/^[a-z][a-z0-9_-]{0,63}$/.test(manifest.id??'')||!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(manifest.version??''))fail('INVALID_MANIFEST','Expected a v3 manifest with a plugin ID and version.')
  for(const file of files){if(!['manifest.json','contract.json'].includes(file)&&!['backend','ui'].includes(file.split('/')[0]))fail('INVALID_PACKAGE','Unexpected root file: '+file)}
  for(const file of [manifest.contract,manifest.backend?.entry,...(manifest.ui?[manifest.ui.entry]:[])]){if(!files.includes(file))fail('INVALID_MANIFEST','Manifest entry is missing from the package.')}
  const hashes=await Promise.all(files.map(async file=>{
    const bytes=await readFile(path.join(stage,...file.split('/')))
    return {path:file,sha256:sha(bytes)}
  }))
  const checksums=Buffer.from(JSON.stringify({algorithm:'sha256',files:hashes},null,2)+'\n')
  const seed=signing==='none'?null:await seedFor(manifest.id,{required:signing==='required'})
  await writeFile(path.join(stage,'checksums.json'),checksums)
  let signature=null
  if(seed){try{signature=signChecksums(checksums,seed);verifySignature(checksums,Buffer.from(JSON.stringify(signature)));await writeFile(path.join(stage,'signature.json'),JSON.stringify(signature,null,2)+'\n')}finally{seed.fill(0)}}
  else await rm(path.join(stage,'signature.json'),{force:true})
  if(!archivePath)return{manifest,signature,state:signature?'signed':'unsigned'}
  const destination=path.resolve(archivePath),archiveRelative=path.relative(target,destination)
  if(!archiveRelative||archiveRelative.startsWith('..')||path.isAbsolute(archiveRelative))fail('INVALID_PACKAGE','Archive must be inside this plugin target directory.')
  const entries=await Promise.all((await packageFiles(stage)).map(async file=>[file,await readFile(path.join(stage,...file.split('/')))]))
  const bytes=zipFiles(entries),digest=sha(bytes)
  await mkdir(path.dirname(destination),{recursive:true})
  try{const old=await readFile(destination);if(sha(old)!==digest){const history=path.join(target,'package-history');await mkdir(history,{recursive:true});const saved=path.join(history,`${sha(old)}.wplug`);try{await writeFile(saved,old,{flag:'wx'})}catch(error){if(error.code!=='EEXIST')throw error}}}catch(error){if(error.code!=='ENOENT')throw error}
  const temp=destination+`.${randomUUID()}.tmp`;await writeFile(temp,bytes,{flag:'wx'});await rename(temp,destination)
  return{manifest,signature,state:signature?'signed':'unsigned',archivePath:destination,sha256:digest,sizeBytes:bytes.length}
}
