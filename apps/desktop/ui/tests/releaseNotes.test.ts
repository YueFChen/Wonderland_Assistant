import assert from 'node:assert/strict'
import { test } from 'node:test'
import { releaseDateLabel, releaseNotesPreview, releaseNotesUrl } from '../src/core/releaseNotes.ts'

test('notification excerpts remove common Markdown formatting and preserve meaningful text', () => {
  const notes = '## 新增功能\r\n\r\n- **代理设置**支持热更新。\n- 查看[使用文档](https://example.com/docs)。\n\n```sh\nwla --version\n```'
  assert.equal(releaseNotesPreview(notes), '新增功能 代理设置支持热更新。 查看使用文档。 wla --version')
  assert.equal(releaseNotesPreview('  \n\t '), '')
  assert.equal(releaseNotesPreview(null), '')
  assert.equal(releaseNotesPreview('| 模块 | 改进 |\n| --- | --- |\n| 网络 | 修复认证 |'), '模块 · 改进 网络 · 修复认证')
})

test('excerpts never split emoji and indicate truncation only when necessary', () => {
  assert.equal(releaseNotesPreview('新增🚀功能', 3), '新增🚀…')
  assert.equal(releaseNotesPreview('新增🚀', 3), '新增🚀')
})

test('release links allow only explicit web URLs without embedded credentials', () => {
  assert.equal(releaseNotesUrl('https://example.com/notes#fixes'), 'https://example.com/notes#fixes')
  assert.equal(releaseNotesUrl('http://example.com'), 'http://example.com/')
  for (const url of ['javascript:alert(1)', 'data:text/html,test', 'file:///private', 'tauri://localhost', '/settings', '#fragment', '//example.com', 'https://user:password@example.com']) {
    assert.equal(releaseNotesUrl(url), undefined, url)
  }
  assert.equal(releaseDateLabel('not-a-date'), null)
  assert.equal(releaseDateLabel(null), null)
})
