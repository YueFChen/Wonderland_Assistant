import {createCipheriv,createDecipheriv,createPrivateKey,createPublicKey,randomBytes,scryptSync,sign} from 'node:crypto'

import {DOMAIN,fail} from './verification.mjs'
export {DOMAIN,fail,verifySignature} from './verification.mjs'
const PREFIX=Buffer.from('302e020100300506032b657004220420','hex')
const KDF={N:32768,r:8,p:1}
export function decodeSeed(value){
  if(typeof value!=='string')fail('INVALID_SIGNING_KEY','Provide the original Base64 Ed25519 seed.')
  const clean=value.trim(),seed=Buffer.from(clean,'base64')
  if(seed.length!==32||seed.toString('base64')!==clean){seed.fill(0);fail('INVALID_SIGNING_KEY','Expected a 32-byte Base64 Ed25519 seed.')}
  return seed
}
const privateKey=seed=>createPrivateKey({key:Buffer.concat([PREFIX,seed]),format:'der',type:'pkcs8'})
export const publicKey=seed=>createPublicKey(privateKey(seed)).export({format:'der',type:'spki'}).subarray(-32).toString('hex')
export function signChecksums(checksums,seed){
  return {schemaVersion:1,algorithm:'ed25519',publicKey:publicKey(seed),signature:sign(null,Buffer.concat([DOMAIN,checksums]),privateKey(seed)).toString('hex')}
}

function password(value){if(typeof value!=='string'||!value.length||value.length>4096)fail('BACKUP_PASSWORD_REQUIRED','Provide a backup password through stdin.');return Buffer.from(value,'utf8')}
export function encryptBackup(id,seed,phrase){
  const pass=password(phrase),salt=randomBytes(16),iv=randomBytes(12)
  const header={schemaVersion:1,kind:'wonderland-signing-key',pluginId:id,publicKey:publicKey(seed),kdf:{name:'scrypt',...KDF,salt:salt.toString('base64')},cipher:{name:'aes-256-gcm',iv:iv.toString('base64')}}
  const derived=scryptSync(pass,salt,32,{...KDF,maxmem:64*1024*1024});pass.fill(0)
  try{const cipher=createCipheriv('aes-256-gcm',derived,iv);cipher.setAAD(Buffer.from(JSON.stringify(header)));const ciphertext=Buffer.concat([cipher.update(seed),cipher.final()]);return Buffer.from(JSON.stringify({...header,ciphertext:ciphertext.toString('base64'),tag:cipher.getAuthTag().toString('base64')})+'\n')}finally{derived.fill(0)}
}
export function decryptBackup(bytes,phrase){
  let data
  try{if(bytes.length>65536)throw Error();data=JSON.parse(bytes.toString('utf8'))}catch{fail('INVALID_BACKUP','Invalid backup.')}
  if(!data||typeof data!=='object'||Array.isArray(data))fail('INVALID_BACKUP','Invalid backup.')
  if(data.schemaVersion!==1||data.kind!=='wonderland-signing-key'||data.kdf?.name!=='scrypt'||Object.entries(KDF).some(([k,v])=>data.kdf[k]!==v)||data.cipher?.name!=='aes-256-gcm'||!/^[a-f0-9]{64}$/.test(data.publicKey??'')||!/^[a-z][a-z0-9_-]{0,63}$/.test(data.pluginId??''))fail('INVALID_BACKUP','Unsupported backup.')
  if([data.kdf.salt,data.cipher.iv,data.tag,data.ciphertext].some(v=>typeof v!=='string'))fail('INVALID_BACKUP','Invalid backup fields.')
  const salt=Buffer.from(data.kdf.salt,'base64'),iv=Buffer.from(data.cipher.iv,'base64'),tag=Buffer.from(data.tag,'base64'),ciphertext=Buffer.from(data.ciphertext,'base64')
  if(salt.length!==16||iv.length!==12||tag.length!==16||ciphertext.length!==32)fail('INVALID_BACKUP','Invalid backup fields.')
  const header={schemaVersion:1,kind:data.kind,pluginId:data.pluginId,publicKey:data.publicKey,kdf:{name:'scrypt',...KDF,salt:data.kdf.salt},cipher:{name:'aes-256-gcm',iv:data.cipher.iv}}
  const pass=password(phrase),derived=scryptSync(pass,salt,32,{...KDF,maxmem:64*1024*1024});pass.fill(0)
  try{const cipher=createDecipheriv('aes-256-gcm',derived,iv);cipher.setAAD(Buffer.from(JSON.stringify(header)));cipher.setAuthTag(tag);const seed=Buffer.concat([cipher.update(ciphertext),cipher.final()]);if(publicKey(seed)!==data.publicKey){seed.fill(0);throw Error()}return{id:data.pluginId,seed}}catch{fail('BACKUP_DECRYPT_FAILED','Wrong password or damaged backup; original keys retained.')}finally{derived.fill(0)}
}
