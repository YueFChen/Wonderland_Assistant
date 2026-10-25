import { createHash, createPublicKey, verify } from 'node:crypto'

/** Tauri's outer base64 wraps a Minisign public key/signature document. */
export function verifyInstallerSignature(bytes, signature, publicKey) {
  const keyLines = Buffer.from(publicKey.trim(), 'base64').toString('utf8').trim().split(/\r?\n/)
  const signatureLines = Buffer.from(signature.trim(), 'base64').toString('utf8').trim().split(/\r?\n/)
  const key = Buffer.from(keyLines[1] ?? '', 'base64')
  const signed = Buffer.from(signatureLines[1] ?? '', 'base64')
  const global = Buffer.from(signatureLines[3] ?? '', 'base64')
  if (key.length !== 42 || key.subarray(0, 2).toString() !== 'Ed' || signed.length !== 74
    || global.length !== 64 || !key.subarray(2, 10).equals(signed.subarray(2, 10))
    || !signatureLines[2]?.startsWith('trusted comment: ')) throw new Error('Invalid Tauri signature or signing identity.')
  // Match minisign-verify: current signatures prehash with BLAKE2b-512.
  if (signed.subarray(0, 2).toString() !== 'ED') throw new Error('Expected a current prehashed Tauri signature.')
  const ed25519 = createPublicKey({
    key: Buffer.concat([Buffer.from('302a300506032b6570032100', 'hex'), key.subarray(10)]),
    format: 'der', type: 'spki',
  })
  const digest = createHash('blake2b512').update(bytes).digest()
  const trusted = Buffer.from(signatureLines[2].slice(17), 'utf8')
  if (!verify(null, digest, ed25519, signed.subarray(10))
    || !verify(null, Buffer.concat([signed.subarray(10), trusted]), ed25519, global)) {
    throw new Error('Core installer signature verification failed.')
  }
}
