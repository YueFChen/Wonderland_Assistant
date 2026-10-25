import {readFile} from 'node:fs/promises'
import path from 'node:path'
import {pathToFileURL} from 'node:url'
import {readArchive,sha256} from './archive.mjs'
import {verifySignature,fail} from './signing.mjs'

export function inspectArtifact(bytes,{requireSignature=false,expectedPublicKey}={}){
 const files=readArchive(bytes)
 let manifest,checksums
 try{manifest=JSON.parse(files.get('manifest.json'));checksums=JSON.parse(files.get('checksums.json'))}catch{fail('INVALID_PACKAGE','Missing or malformed manifest/checksums.')}
 const paths=[...files.keys()].filter(p=>!['checksums.json','signature.json'].includes(p)).sort()
 if(checksums?.algorithm!=='sha256'||!Array.isArray(checksums.files)||checksums.files.length!==paths.length)fail('INVALID_CHECKSUMS','Incomplete checksum file list.')
 for(let i=0;i<paths.length;i++){const entry=checksums.files[i];if(entry?.path!==paths[i]||entry.sha256!==sha256(files.get(paths[i])))fail('INVALID_CHECKSUMS','Checksum mismatch.')}
 const signature=files.has('signature.json')?verifySignature(files.get('checksums.json'),files.get('signature.json')):null
 if(requireSignature&&!signature)fail('PACKAGE_SIGNATURE_REQUIRED','A complete signed package is required.')
 if(expectedPublicKey&&signature?.publicKey!==expectedPublicKey)fail('PUBLISHER_IDENTITY_CHANGED','Original publisher key differs.')
 if(requireSignature&&(typeof manifest.author!=='string'||!manifest.author.trim()))fail('AUTHOR_REQUIRED','Formal packages require an author.')
 return{manifest,signature,sha256:sha256(bytes),sizeBytes:bytes.length}
}
if(process.argv[1]&&import.meta.url===pathToFileURL(path.resolve(process.argv[1])).href){
 try{const file=process.argv[2];if(!file)fail('INVALID_ARGUMENT','Supply a .wplug path.');const result=inspectArtifact(await readFile(file),{requireSignature:process.argv.includes('--signed'),expectedPublicKey:process.env.PLUGIN_UPDATE_SIGNING_PUBLIC_KEY||undefined});process.stdout.write(JSON.stringify(result)+'\n')}catch(error){process.stderr.write(`${error.code??'INVALID_PACKAGE'}: ${error.message}\n`);process.exitCode=1}
}
