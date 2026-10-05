import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { HashRouter } from 'react-router-dom'
import { MobileApp } from './MobileApp'
import { consumeConnectionLink } from '../core/connectionLink'
import { connectWeb } from '../core/transport'
import { rememberedConnection } from '../core/connectionMemory'
import '../styles/index.css'
import '../layouts/workspace.css'
import './mobile.css'

const token = consumeConnectionLink(window.location.href, (url) => history.replaceState(null, '', url))
// Outside React effects: StrictMode cannot consume or submit the scanned key twice.
const initialToken = token ?? rememberedConnection()?.token ?? ''
const initialConnection = connectWeb(initialToken)
createRoot(document.getElementById('root')!).render(
  <StrictMode><HashRouter><MobileApp initialConnection={initialConnection} initialToken={initialToken} /></HashRouter></StrictMode>,
)
