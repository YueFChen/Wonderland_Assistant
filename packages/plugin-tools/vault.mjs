import {readFile,mkdir,writeFile,rename,stat} from 'node:fs/promises'
import {spawnSync} from 'node:child_process'
import {randomBytes} from 'node:crypto'
import {fileURLToPath} from 'node:url'
import os from 'node:os'
import path from 'node:path'
import {decodeSeed,publicKey,encryptBackup,decryptBackup,fail} from './signing.mjs'

export const coreRoot=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'../..')
export const identifier=id=>{if(!/^[a-z][a-z0-9_-]{0,63}$/.test(id??''))fail('INVALID_PLUGIN_ID','Invalid plugin ID.');return id}
export function vaultRoot(){return path.resolve(process.env.WONDERLAND_AUTHOR_HOME??path.join(process.env.LOCALAPPDATA??path.join(os.homedir(),'.local/share'),'Wonderland','author-keys'))}
async function protect(operation,bytes){
  if(process.platform!=='win32')fail('KEY_STORE_UNAVAILABLE','Local encrypted keys require Windows; CI can supply its original Secret.')
  const helper=path.resolve(process.env.WONDERLAND_KEY_HELPER??path.join(coreRoot,'target/debug/wla-plugin-key.exe'))
  try{await stat(helper)}catch{
    if(process.env.WONDERLAND_KEY_HELPER)fail('KEY_STORE_UNAVAILABLE','Configured key helper is missing.')
    const build=spawnSync('cargo',['build','--locked','-p','wonderland-plugin-tools','--bin','wla-plugin-key'],{cwd:coreRoot,stdio:['ignore','ignore','pipe']})
    if(build.status!==0)fail('KEY_STORE_UNAVAILABLE','Build the standalone wla-plugin-key helper before using the local vault.')
  }
  const child=spawnSync(helper,[],{input:JSON.stringify({operation,value:bytes.toString('base64')}),encoding:'utf8',maxBuffer:1024*1024,windowsHide:true})
  if(child.status!==0)fail('KEY_STORE_UNAVAILABLE','The local key could not be protected for this Windows user.')
  try{return Buffer.from(JSON.parse(child.stdout).value,'base64')}catch{fail('KEY_STORE_UNAVAILABLE','Invalid key helper response.')}
}
const destination=id=>path.join(vaultRoot(),`${identifier(id)}.json`)
export async function identity(id){
  let data
  try{data=JSON.parse(await readFile(destination(id),'utf8'))}catch(error){if(error.code==='ENOENT')return null;fail('KEY_STORE_INVALID','Original author key record is damaged; it was retained.')}
  if(!data||typeof data!=='object'||Array.isArray(data)||data.schemaVersion!==1||data.pluginId!==id||!/^[a-f0-9]{64}$/.test(data.publicKey??'')||typeof data.sealed!=='string')fail('KEY_STORE_INVALID','Invalid author identity; original retained.')
  return data
}
export async function putIdentity(id,seed,expected){
  identifier(id);const pub=publicKey(seed),old=await identity(id)
  if([expected,old?.publicKey].some(key=>key&&key!==pub))fail('KEY_MISMATCH','Import the original plugin signing identity.')
  if(old){const retained=await protect('unseal',Buffer.from(old.sealed,'base64'));try{if(publicKey(retained)!==pub)fail('KEY_MISMATCH','Retained signing key differs.')}finally{retained.fill(0)}return{pluginId:id,publicKey:pub}}
  const sealed=await protect('seal',seed)
  await mkdir(vaultRoot(),{recursive:true,mode:0o700})
  // Exclusive creation never overwrites an existing original identity.
  await writeFile(destination(id),JSON.stringify({schemaVersion:1,pluginId:id,publicKey:pub,sealed:sealed.toString('base64')})+'\n',{flag:'wx',mode:0o600})
  return{pluginId:id,publicKey:pub}
}
export async function seedFor(id,{required=false,expectedPublicKey}={}){
  identifier(id)
  expectedPublicKey ??= process.env.PLUGIN_UPDATE_SIGNING_PUBLIC_KEY || undefined
  if(expectedPublicKey&&!/^[a-f0-9]{64}$/.test(expectedPublicKey))fail('INVALID_PUBLIC_KEY','Expected original signing public key is invalid.')
  const entry=await identity(id)
  let seed
  if(process.env.PLUGIN_UPDATE_SIGNING_KEY!==undefined)seed=decodeSeed(process.env.PLUGIN_UPDATE_SIGNING_KEY)
  else if(entry)seed=await protect('unseal',Buffer.from(entry.sealed,'base64'))
  else{if(required)fail('SIGNING_KEY_REQUIRED','Import the original author identity or configure the private CI Secret.');return null}
  if(seed.length!==32||[expectedPublicKey,entry?.publicKey].some(key=>key&&key!==publicKey(seed))){seed.fill(0);fail('KEY_MISMATCH','Signing key does not match the original identity.')}
  return seed
}
export async function importKey(id,file,expected){
  if((await stat(file)).size>4096)fail('INVALID_SIGNING_KEY','Invalid seed file.')
  const bytes=await readFile(file);let seed
  try{seed=decodeSeed(bytes.toString('utf8'));return await putIdentity(id,seed,expected)}finally{bytes.fill(0);seed?.fill(0)}
}
export async function generateKey(id){
  if(await identity(id))fail('IDENTITY_EXISTS','An author identity already exists; import the original key.')
  const seed=randomBytes(32);try{return await putIdentity(id,seed)}finally{seed.fill(0)}
}
export async function exportKey(id,file,password){
  const seed=await seedFor(id,{required:true});try{const bytes=encryptBackup(id,seed,password);await writeFile(file,bytes,{flag:'wx',mode:0o600});return{pluginId:id,file,publicKey:publicKey(seed)}}finally{seed.fill(0)}
}
export async function restoreKey(id,file,password,expected){
  if((await stat(file)).size>65536)fail('INVALID_BACKUP','Invalid backup.')
  const {id:backupId,seed}=decryptBackup(await readFile(file),password)
  try{if(backupId!==id)fail('KEY_MISMATCH','Backup belongs to another plugin.');return await putIdentity(id,seed,expected)}finally{seed.fill(0)}
}
