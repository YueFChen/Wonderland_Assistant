import { memo } from 'react'
import Markdown, { type Components } from 'react-markdown'
import remarkGfm from 'remark-gfm'

import { releaseNotesUrl } from '../core/releaseNotes'
import './releaseNotes.css'

const plugins = [remarkGfm]
const components: Components = {
  a: ({ href, children }) => href
    ? <a href={href} target="_blank" rel="noopener noreferrer">{children}</a>
    : <span>{children}</span>,
  img: ({ alt }) => alt ? <span>{alt}</span> : null,
  table: ({ children }) => <div className="release-notes-table"><table>{children}</table></div>,
}

function ReleaseNotes({ body }: { body: string }) {
  return <div className="release-notes">
    <Markdown skipHtml remarkPlugins={plugins} urlTransform={releaseNotesUrl} components={components}>{body}</Markdown>
  </div>
}

export default memo(ReleaseNotes)
