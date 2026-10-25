import type {InstallCandidate} from './localInstall'

export function initialInstallSelection(items: InstallCandidate[]): Set<string> {
  const counts = new Map<string, number>()
  for (const item of items) if (item.manifest && !item.error) counts.set(item.manifest.id, (counts.get(item.manifest.id) ?? 0) + 1)
  return new Set(items.filter(item => item.manifest && !item.error && counts.get(item.manifest.id) === 1).map(item => item.key))
}
export function chooseInstallCandidate(items: InstallCandidate[], selected: Set<string>, key: string): Set<string> {
  const item = items.find(item => item.key === key)
  const next = new Set(selected)
  if (!item?.manifest || item.error) return next
  if (next.delete(key)) return next
  for (const other of items) if (other.manifest?.id === item.manifest.id) next.delete(other.key)
  next.add(key)
  return next
}

// A directory reader may yield entries in multiple batches. Only direct child files count.
export interface DropEntry {
  isFile: boolean; isDirectory: boolean; name: string
  file: (ok: (file: File) => void, fail: (error: DOMException) => void) => void
  createReader: () => { readEntries: (ok: (entries: DropEntry[]) => void, fail: (error: DOMException) => void) => void }
}
export async function filesFromDrop(entries: (DropEntry | null)[], fallback: File[]): Promise<{ file: File; name: string }[]> {
  const packages: { file: File; name: string }[] = []
  const add = async (entry: DropEntry, prefix = '') => {
    if (entry.isFile && /\.wplug$/i.test(entry.name)) packages.push({ file: await new Promise<File>((ok, fail) => entry.file(ok, fail)), name: prefix + entry.name })
  }
  if (!entries.some(Boolean)) return fallback.filter(file => /\.wplug$/i.test(file.name)).map(file => ({ file, name: file.name }))
  for (const entry of entries) {
    if (!entry) continue
    if (entry.isDirectory) {
      const reader = entry.createReader()
      for (;;) {
        const children = await new Promise<DropEntry[]>((ok, fail) => reader.readEntries(ok, fail))
        if (!children.length) break
        for (const child of children) await add(child, entry.name + '/')
      }
    } else await add(entry)
  }
  return packages
}

export function installSignatureDescription(item:InstallCandidate):string {
 if(!item.signaturePublicKey)return '未签名的本地开发包'
 const original=item.existing?.signature.expectedPublicKey
 if(original===item.signaturePublicKey)return '签名有效，匹配已记录发布公钥'
 if(original)return '签名有效，与正式发布公钥不同；开发覆盖保留原公钥'
 return '签名有效；本地发布身份尚未经目录确认'
}
