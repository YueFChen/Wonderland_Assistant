import {createPublicKey,verify} from 'node:crypto'

export const DOMAIN=Buffer.from('wonderland-wplug-signature-v1\0','utf8')
export const fail=(code,message)=>{throw Object.assign(new Error(message),{code})}
export function verifySignature(checksums,bytes){
  let value,raw
  try{if(bytes.length>4096)throw Error();raw=new TextDecoder('utf-8',{fatal:true}).decode(bytes);value=JSON.parse(raw)}catch{fail('INVALID_PACKAGE_SIGNATURE','Malformed package signature.')}
  if(!value||typeof value!=='object'||Array.isArray(value))fail('INVALID_PACKAGE_SIGNATURE','Invalid package signature fields.')
  if(Object.keys(value).sort().join(',')!=='algorithm,publicKey,schemaVersion,signature'||!Number.isInteger(value.schemaVersion)||value.schemaVersion<0||value.schemaVersion>0xffffffff||[value.algorithm,value.publicKey,value.signature].some(v=>typeof v!=='string'))fail('INVALID_PACKAGE_SIGNATURE','Invalid package signature fields.')
  // Match Rust's strict struct decoding: four unique fields, a u32 integer
  // token, and string values. JSON.parse alone silently accepts duplicate keys.
  const entries=[...raw.matchAll(/[{,]\s*("(?:[^"\\]|\\.)*")\s*:\s*("(?:[^"\\]|\\.)*"|(?:0|[1-9]\d*))(?=\s*[,}])/g)]
  if(entries.length!==4||new Set(entries.map(e=>JSON.parse(e[1]))).size!==4)fail('INVALID_PACKAGE_SIGNATURE','Duplicate or invalid package signature fields.')
  if(value.schemaVersion!==1||value.algorithm!=='ed25519')fail('UNSUPPORTED_PACKAGE_SIGNATURE','Unsupported package signature format.')
  if(!/^[a-f0-9]{64}$/.test(value.publicKey)||!/^[a-f0-9]{128}$/.test(value.signature))fail('INVALID_PACKAGE_SIGNATURE','Invalid package signature fields.')
  const key=createPublicKey({key:Buffer.concat([Buffer.from('302a300506032b6570032100','hex'),Buffer.from(value.publicKey,'hex')]),format:'der',type:'spki'})
  if(!verify(null,Buffer.concat([DOMAIN,checksums]),key,Buffer.from(value.signature,'hex')))fail('INVALID_PACKAGE_SIGNATURE','Package content signature does not match.')
  return value
}
