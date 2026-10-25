import {createHash} from 'node:crypto'
import {inflateRawSync} from 'node:zlib'
import {fail} from './verification.mjs'
export const sha256 = (bytes) => createHash('sha256').update(bytes).digest('hex')
const safePath = (value) => typeof value === 'string'
  && /^[A-Za-z0-9_.-]+(?:\/[A-Za-z0-9_.-]+)*$/.test(value)
  && value.split('/').every((part) => part !== '.' && part !== '..')
const crcTable = Array.from({ length: 256 }, (_, value) => {
  let crc = value
  for (let i = 0; i < 8; i++) crc = (crc >>> 1) ^ ((crc & 1) ? 0xedb88320 : 0)
  return crc >>> 0
})
export function crc32(bytes) {
  let value = 0xffffffff
  for (const byte of bytes) value = crcTable[(value ^ byte) & 0xff] ^ (value >>> 8)
  return (value ^ 0xffffffff) >>> 0
}

export function readArchive(bytes, {maxArchiveBytes=100*1024*1024,maxTotalBytes=256*1024*1024,maxEntryBytes=128*1024*1024} = {}) {
  if (!Buffer.isBuffer(bytes) || bytes.length < 22 || bytes.length > maxArchiveBytes) fail('INVALID_PACKAGE', 'Package size is outside the permitted range.')
  let end = -1
  for (let offset = bytes.length - 22; offset >= Math.max(0, bytes.length - 65_557); offset--) {
    if (bytes.readUInt32LE(offset) === 0x06054b50 && offset + 22 + bytes.readUInt16LE(offset + 20) === bytes.length) { end = offset; break }
  }
  if (end < 0 || bytes.readUInt16LE(end + 4) !== 0 || bytes.readUInt16LE(end + 6) !== 0
    || bytes.readUInt16LE(end + 8) !== bytes.readUInt16LE(end + 10)) fail('INVALID_PACKAGE', 'A single-volume ZIP archive is required.')
  const count = bytes.readUInt16LE(end + 10), centralSize = bytes.readUInt32LE(end + 12), centralOffset = bytes.readUInt32LE(end + 16)
  if (!count || count > 2048 || centralOffset + centralSize !== end) fail('INVALID_PACKAGE', 'The ZIP directory is invalid or exceeds the entry limit.')
  const files = new Map(), names = new Set(), ranges = []
  let cursor = centralOffset, total = 0
  for (let i = 0; i < count; i++) {
    if (cursor + 46 > end || bytes.readUInt32LE(cursor) !== 0x02014b50) fail('INVALID_PACKAGE', 'The ZIP directory is invalid.')
    const flags = bytes.readUInt16LE(cursor + 8), method = bytes.readUInt16LE(cursor + 10)
    const crc = bytes.readUInt32LE(cursor + 16), compressed = bytes.readUInt32LE(cursor + 20), size = bytes.readUInt32LE(cursor + 24)
    const nameLength = bytes.readUInt16LE(cursor + 28), extra = bytes.readUInt16LE(cursor + 30), comment = bytes.readUInt16LE(cursor + 32)
    const disk = bytes.readUInt16LE(cursor + 34), attributes = bytes.readUInt32LE(cursor + 38), local = bytes.readUInt32LE(cursor + 42)
    if (cursor + 46 + nameLength + extra + comment > end || disk || (flags & 1) || ![0, 8].includes(method)
      || size > maxEntryBytes || (attributes >>> 16 & 0xf000) === 0xa000) fail('INVALID_PACKAGE', 'The ZIP entry is unsupported or too large.')
    const rawName = bytes.subarray(cursor + 46, cursor + 46 + nameLength)
    let name
    try { name = new TextDecoder('utf-8', { fatal: true }).decode(rawName) } catch { fail('INVALID_PACKAGE', 'The ZIP entry name is invalid.') }
    const directory = name.endsWith('/')
    const normalized = directory ? name.slice(0, -1) : name
    if (!safePath(normalized) || names.has(normalized.toLowerCase())) fail('INVALID_PACKAGE', 'Unsafe or duplicate ZIP paths are not allowed.')
    names.add(normalized.toLowerCase())
    if (local + 30 > centralOffset || bytes.readUInt32LE(local) !== 0x04034b50
      || bytes.readUInt16LE(local + 6) !== flags || bytes.readUInt16LE(local + 8) !== method) fail('INVALID_PACKAGE', 'The ZIP local header does not match its directory.')
    const localNameLength = bytes.readUInt16LE(local + 26), localExtra = bytes.readUInt16LE(local + 28)
    const start = local + 30 + localNameLength + localExtra, stop = start + compressed
    if (stop > centralOffset || !bytes.subarray(local + 30, local + 30 + localNameLength).equals(rawName)
      || ranges.some(([a, b]) => local < b && stop > a)) fail('INVALID_PACKAGE', 'Overlapping or inconsistent ZIP entries are not allowed.')
    ranges.push([local, stop])
    total += size
    if (total > maxTotalBytes) fail('INVALID_PACKAGE', 'Expanded package exceeds the size limit.')
    let data
    try { data = method === 0 ? bytes.subarray(start, stop) : inflateRawSync(bytes.subarray(start, stop), { maxOutputLength: Math.max(1, size) }) }
    catch { fail('INVALID_PACKAGE', 'ZIP data could not be expanded safely.') }
    if (data.length !== size || crc32(data) !== crc || (directory && size !== 0)) fail('INVALID_PACKAGE', 'ZIP data failed integrity validation.')
    if (!directory) files.set(name, data)
    cursor += 46 + nameLength + extra + comment
  }
  if (cursor !== end) fail('INVALID_PACKAGE', 'The ZIP directory contains unexpected data.')
  for (const name of files.keys()) {
    const parts = name.split('/')
    for (let index = 1; index < parts.length; index++) if (files.has(parts.slice(0, index).join('/'))) fail('INVALID_PACKAGE', 'ZIP file and directory paths overlap.')
  }
  return files
}
