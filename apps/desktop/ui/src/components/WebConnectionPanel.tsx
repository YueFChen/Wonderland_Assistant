import { useState } from 'react'
import { QRCodeSVG } from 'qrcode.react'
import { connectionLink } from '../core/connectionLink'
import { t } from '../i18n'

export function WebConnectionPanel({ token, publicUrl, addresses }: {
  token: string
  publicUrl: string
  addresses: { interfaceName: string; ip: string; url: string }[]
}) {
  const [selected, setSelected] = useState('')
  const [showQr, setShowQr] = useState(false)
  const [copyResult, setCopyResult] = useState('')
  const choices = [
    ...(publicUrl ? [{ url: publicUrl, label: `${t('webAccess.publicEntry')} ${publicUrl}` }] : []),
    ...addresses.map((address) => ({ url: address.url, label: `${address.interfaceName} · ${address.url}` })),
  ]
  // Derive the fallback every render: unplugged interfaces must never leave a stale QR visible.
  const address = choices.find((choice) => choice.url === selected)?.url ?? choices[0]?.url
  const link = address ? connectionLink(address, token) : ''
  const copy = async (value: string) => {
    try { await navigator.clipboard.writeText(value); setCopyResult(t('webAccess.copied')) }
    catch { setCopyResult(t('webAccess.copyFailed')) }
  }
  if (!address) return <p className="text-ink-muted">{t('webAccess.noAddress')}</p>
  return <div className="space-y-3">
    <label className="block text-ink-muted">{t('webAccess.deviceAddress')}<select value={address} onChange={(event) => { setSelected(event.target.value); setCopyResult('') }} className="mt-2 w-full rounded-lg border border-glass-line bg-[var(--app-field)] px-3 py-2 text-sm text-ink">{choices.map((choice) => <option key={choice.url} value={choice.url}>{choice.label}</option>)}</select></label>
    <p className="break-all"><a href={address} target="_blank" rel="noreferrer" className="text-[var(--app-accent)] underline">{address}</a></p>
    <p className="text-ink-muted">{t('webAccess.addressHint')}</p>
    <div className="flex flex-wrap gap-2">
      <button type="button" onClick={() => void copy(address)} className="rounded-lg border border-glass-line px-3 py-2 text-ink">{t('webAccess.copyAddress')}</button>
      <button type="button" onClick={() => setShowQr(!showQr)} className="rounded-lg border border-glass-line px-3 py-2 text-ink">{t(showQr ? 'webAccess.hideQr' : 'webAccess.showQr')}</button>
    </div>
    {showQr && <div className="space-y-3">
      <QRCodeSVG value={link} size={224} marginSize={4} level="M" title={t('webAccess.qrTitle')} style={{ maxWidth: '100%', height: 'auto' }} />
      <p className="text-ink-muted">{t('webAccess.qrHint')}</p>
      <button type="button" onClick={() => void copy(link)} className="rounded-lg border border-glass-line px-3 py-2 text-ink">{t('webAccess.copyConnection')}</button>
    </div>}
    {copyResult && <p role="status" className="text-ink-muted">{copyResult}</p>}
  </div>
}
