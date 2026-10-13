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
        if (active) setState({ status: 'ready', svg })
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
    <pre aria-busy={state.status === 'loading'} data-mermaid-fallback>
      <code>{chart}</code>
    </pre>
  )
}
