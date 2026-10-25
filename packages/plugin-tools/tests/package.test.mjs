import assert from 'node:assert/strict'
import test from 'node:test'
import {mkdtemp,mkdir,writeFile,readFile,rm,readdir} from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import {finishPackage} from '../pack.mjs'
import {importKey,exportKey,restoreKey,identity} from '../vault.mjs'
import {signChecksums,verifySignature,publicKey} from '../signing.mjs'
import {inspectArtifact} from '../verify.mjs'

async function fixture(t) {
 const root=await mkdtemp(path.join(os.tmpdir(),'wplug-author-'));t.after(()=>rm(root,{recursive:true,force:true}))
 const oldHome=process.env.WONDERLAND_AUTHOR_HOME,oldSecret=process.env.PLUGIN_UPDATE_SIGNING_KEY,oldPublic=process.env.PLUGIN_UPDATE_SIGNING_PUBLIC_KEY
 process.env.WONDERLAND_AUTHOR_HOME=path.join(root,'keys');delete process.env.PLUGIN_UPDATE_SIGNING_KEY;delete process.env.PLUGIN_UPDATE_SIGNING_PUBLIC_KEY
 t.after(()=>{for(const [name,value] of Object.entries({WONDERLAND_AUTHOR_HOME:oldHome,PLUGIN_UPDATE_SIGNING_KEY:oldSecret,PLUGIN_UPDATE_SIGNING_PUBLIC_KEY:oldPublic})){if(value===undefined)delete process.env[name];else process.env[name]=value}})
 const stage=path.join(root,'target/package'),archivePath=path.join(root,'target/fixture.wplug')
 const manifest={manifestVersion:3,id:'signing_fixture',version:'0.1.0',name:'Signing fixture',author:'Build fixture',
  hostCompatibility:{minCoreVersion:'0.2.0',maxCoreVersionExclusive:'0.3.0',protocol:{minVersion:'1.0.0',maxVersionExclusive:'2.0.0'}},
  platform:{os:'windows',architecture:'x86_64',abi:'msvc'},backend:{entry:'backend/test.exe',transport:'stdio-ndjson-v1'},contract:'contract.json'}
 await mkdir(path.join(stage,'backend'),{recursive:true})
 await writeFile(path.join(stage,'manifest.json'),JSON.stringify(manifest))
 await writeFile(path.join(stage,'backend/test.exe'),'fixture')
 await writeFile(path.join(stage,'contract.json'),await readFile(new URL('../../../tests/fixtures/direct-plugin/contract.json',import.meta.url)))
 return {root,stage,archivePath,manifest}
}

test('Rust and Node share exact signature message bytes and format',async()=>{
 const {checksums,...expected}=JSON.parse(await readFile(new URL('../../../crates/plugin-signing/tests/vector.json',import.meta.url)))
 assert.deepEqual(signChecksums(Buffer.from(checksums),Buffer.alloc(32,42)),expected)
 assert.deepEqual(verifySignature(Buffer.from(checksums),Buffer.from(JSON.stringify(expected))),expected)
 assert.throws(()=>verifySignature(Buffer.from(checksums.trim()),Buffer.from(JSON.stringify(expected))),{code:'INVALID_PACKAGE_SIGNATURE'})
 for(const raw of ['{}','null',JSON.stringify(expected).replace('"schemaVersion":1','"schemaVersion":1.0'),JSON.stringify(expected).replace('"schemaVersion":1','"schemaVersion":1,"schemaVersion":1')])assert.throws(()=>verifySignature(Buffer.from(checksums),Buffer.from(raw)),{code:'INVALID_PACKAGE_SIGNATURE'})
 assert.throws(()=>verifySignature(Buffer.from(checksums),Buffer.from(JSON.stringify({...expected,schemaVersion:2}))),{code:'UNSUPPORTED_PACKAGE_SIGNATURE'})
})

test('portable author build verifies CI signatures, reproducibility, history and expected identity',async t=>{
 const {root,stage,archivePath}=await fixture(t)
 assert.equal((await finishPackage({root,stage,archivePath})).state,'unsigned')
 await assert.rejects(finishPackage({root,stage,archivePath,signing:'required'}),{code:'SIGNING_KEY_REQUIRED'})
 const seed=Buffer.alloc(32,23)
 process.env.PLUGIN_UPDATE_SIGNING_KEY=seed.toString('base64')
 process.env.PLUGIN_UPDATE_SIGNING_PUBLIC_KEY=publicKey(seed)
 const first=await finishPackage({root,stage,archivePath,signing:'required'}),bytes=await readFile(archivePath)
 assert.equal(inspectArtifact(bytes,{requireSignature:true}).signature.publicKey,publicKey(seed))
 assert.equal((await finishPackage({root,stage,archivePath,signing:'required'})).sha256,first.sha256)
 await writeFile(path.join(stage,'backend/test.exe'),'changed local candidate')
 assert.notEqual((await finishPackage({root,stage,archivePath,signing:'required'})).sha256,first.sha256)
 assert((await readdir(path.join(root,'target/package-history'))).includes(first.sha256+'.wplug'))
 process.env.PLUGIN_UPDATE_SIGNING_PUBLIC_KEY=publicKey(Buffer.alloc(32,24))
 await assert.rejects(finishPackage({root,stage,archivePath,signing:'required'}),{code:'KEY_MISMATCH'})
})

test('Windows author vault preserves the original encrypted identity through backup and restore',{skip:process.platform!=='win32'},async t=>{
 const {root,stage,archivePath,manifest}=await fixture(t)
 const seed=Buffer.alloc(32,23),seedFile=path.join(root,'seed.txt');await writeFile(seedFile,seed.toString('base64'))
 await importKey(manifest.id,seedFile,publicKey(seed))
 assert(!(await readFile(path.join(root,'keys',manifest.id+'.json'),'utf8')).includes(seed.toString('base64')))
 const first=await finishPackage({root,stage,archivePath,signing:'required'}),bytes=await readFile(archivePath)
 assert.equal(inspectArtifact(bytes,{requireSignature:true}).signature.publicKey,publicKey(seed))
 const backup=path.join(root,'backup.json');await exportKey(manifest.id,backup,'fixture password')
 await assert.rejects(restoreKey(manifest.id,backup,'wrong'),{code:'BACKUP_DECRYPT_FAILED'})
 await restoreKey(manifest.id,backup,'fixture password',publicKey(seed))
 assert.equal((await finishPackage({root,stage,archivePath,signing:'required'})).sha256,first.sha256)
 await writeFile(seedFile,Buffer.alloc(32,24).toString('base64'))
 await assert.rejects(importKey(manifest.id,seedFile),{code:'KEY_MISMATCH'})
 assert.equal((await identity(manifest.id)).publicKey,publicKey(seed))
 await writeFile(path.join(root,'keys',manifest.id+'.json'),'{broken')
 await assert.rejects(finishPackage({root,stage,archivePath}),{code:'KEY_STORE_INVALID'})
})
