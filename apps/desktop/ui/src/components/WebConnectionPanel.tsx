import { useState } from 'react'
import { QRCodeSVG } from 'qrcode.react'
import { ArrowUpRight } from 'lucide-react'
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
    <div>
      <label htmlFor="web-connection-address" className="block text-ink-muted">{t('webAccess.deviceAddress')}</label>
      <div className="mt-2 flex gap-2">
        <select id="web-connection-address" value={address} onChange={(event) => { setSelected(event.target.value); setCopyResult('') }} className="app-input min-w-0 flex-1">{choices.map((choice) => <option key={choice.url} value={choice.url}>{choice.label}</option>)}</select>
        <a href={address} target="_blank" rel="noreferrer" className="app-icon-button" aria-label={t('webAccess.openAddress')} title={t('webAccess.openAddress')}><ArrowUpRight size={16} aria-hidden /></a>
      </div>
    </div>
    <div className="flex flex-wrap gap-2">
      <button type="button" onClick={() => void copy(address)} className="app-button">{t('webAccess.copyAddress')}</button>
      <button type="button" onClick={() => setShowQr(!showQr)} className="app-button">{t(showQr ? 'webAccess.hideQr' : 'webAccess.showQr')}</button>
    </div>
    {showQr && <div className="space-y-3">
      <QRCodeSVG value={link} size={224} marginSize={4} level="M" title={t('webAccess.qrTitle')} style={{ maxWidth: '100%', height: 'auto' }} />
      <button type="button" onClick={() => void copy(link)} className="app-button">{t('webAccess.copyConnection')}</button>
    </div>}
    {copyResult && <p role="status" className="text-ink-muted">{copyResult}</p>}
  </div>
}
