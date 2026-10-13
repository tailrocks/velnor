'use client'

import { useTheme } from 'next-themes'
import { useEffect, useId, useState } from 'react'

type MermaidState =
  | { readonly status: 'loading'; readonly svg: null }
  | { readonly status: 'ready'; readonly svg: string }
  | { readonly status: 'error'; readonly svg: null }

export function Mermaid({ chart }: { readonly chart: string }) {
  const { resolvedTheme } = useTheme()
  const id = useId().replaceAll(':', '')
  const titleId = `mermaid-title-${id}`
  const [state, setState] = useState<MermaidState>({ status: 'loading', svg: null })

  useEffect(() => {
    let active = true
    setState({ status: 'loading', svg: null })

    void import('mermaid')
      .then(({ default: mermaid }) => {
        mermaid.initialize({
          startOnLoad: false,
          securityLevel: 'strict',
          fontFamily: 'inherit',
          theme: resolvedTheme === 'dark' ? 'dark' : 'default',
        })
        return mermaid.render(`mermaid-${id}`, chart.replaceAll('\\n', '\n'))
      })
      .then(({ svg }) => {
        const accessibleSvg = svg.replace(
          /<svg\b([^>]*)>/,
          `<svg$1 role="img" aria-labelledby="${titleId}"><title id="${titleId}">Mermaid diagram</title>`,
        )
        if (active) setState({ status: 'ready', svg: accessibleSvg })
      })
      .catch((error: unknown) => {
        if (import.meta.env.DEV) {
          console.error('Unable to render Mermaid diagram', error)
        }
        if (active) setState({ status: 'error', svg: null })
      })

    return () => {
      active = false
    }
  }, [chart, id, resolvedTheme])

  if (state.status === 'ready') {
    return <div aria-label="Mermaid diagram" dangerouslySetInnerHTML={{ __html: state.svg }} />
  }

  return (
    <div
      aria-busy={state.status === 'loading'}
      data-mermaid-fallback
      data-mermaid-error={state.status === 'error' ? '' : undefined}
    >
      <p
        aria-live="polite"
        data-mermaid-status
        role={state.status === 'error' ? 'alert' : 'status'}
      >
        {state.status === 'loading'
          ? 'Loading diagram…'
          : 'This diagram could not be rendered. Showing the source instead.'}
      </p>
      <pre>
        <code>{chart}</code>
      </pre>
    </div>
  )
}
