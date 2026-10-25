import assert from 'node:assert/strict'
import {test} from 'node:test'
import {createManagementState} from '../src/plugins/managementState.ts'
import {catalogErrorMessageKey} from '../src/plugins/catalogErrors.ts'
import {catalogAction,catalogInstallMode} from '../src/plugins/catalogActions.ts'
import type {PluginCatalogSnapshot} from '../src/plugins/api.ts'

test('desktop mutations share admission and stale progress/completions cannot affect another job',()=>{
  const state=createManagementState()
  const local=state.begin({id:'local',label:'Local install',kind:'local'})!
  assert.equal(state.begin({id:'uninstall',label:'Remove',kind:'management'}),null)
  local.finish()
  const catalog=state.begin({id:'online',label:'Online update',kind:'catalog',cancellable:true})!
  local.finish();local.update({id:'stale'})
  assert.equal(state.getSnapshot()?.id,'online')
  state.progress({operationId:'local',phase:'installing',downloadedBytes:100,totalBytes:100,cancellable:false})
  assert.equal(state.getSnapshot()?.phase,undefined)
  state.progress({operationId:'online',phase:'downloading',downloadedBytes:5,totalBytes:10,cancellable:true})
  state.requestCancel();assert.equal(catalog.cancelled(),true)
  catalog.finish();assert.equal(state.getSnapshot(),null)
})

test('single commits reject cancellation while batches can cancel only their remaining items',()=>{
  const state=createManagementState()
  const single=state.begin({id:'single',label:'Single install',kind:'catalog',cancellable:false})!
  state.requestCancel();assert.equal(single.cancelled(),false);single.finish()
  const batch=state.begin({id:'first',label:'Batch',kind:'catalog',batch:true,cancellable:false})!
  state.requestCancel();assert.equal(batch.cancelled(),true)
  batch.update({id:'next',cancellable:true})
  state.progress({operationId:'first',phase:'starting',downloadedBytes:0,totalBytes:0,cancellable:false})
  assert.equal(state.getSnapshot()?.id,'next');assert.equal(state.getSnapshot()?.cancellable,true)
  batch.finish()
})

test('repair and rollback are explicit modes and never silently become normal updates',()=>{
  const base={entry:{version:'1.0.0'},installedVersion:'1.0.0'} as PluginCatalogSnapshot['plugins'][number]
  assert.equal(catalogAction(base),'repair');assert.equal(catalogInstallMode(base),'repair')
  assert.equal(catalogInstallMode({...base,action:'rollback'}),'rollback')
  assert.equal(catalogInstallMode({...base,action:'update'}),'update')
  assert.equal(catalogAction({...base,entry:null}),'unavailable')
})

test('identity, package and recovery failures keep specific messages without reflecting arbitrary input',()=>{
  for(const code of ['PUBLISHER_IDENTITY_CONFLICT','PUBLISHER_IDENTITY_UNAVAILABLE','PUBLISHER_IDENTITY_PERSIST_FAILED','PACKAGE_SIGNATURE_REQUIRED','INVALID_PACKAGE_SIGNATURE','UNSUPPORTED_PACKAGE_SIGNATURE','INSTALL_RECOVERY_REQUIRED','PLUGIN_BUSY','LOCAL_IO','CANCELLED']) {
    assert.notEqual(catalogErrorMessageKey(code),'settings.plugins.distribution.operationFailed',code)
  }
  assert.equal(catalogErrorMessageKey('SENSITIVE_UNRECOGNIZED_SERVER_VALUE'),'settings.plugins.distribution.operationFailed')
})
