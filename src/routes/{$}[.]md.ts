import { createFileRoute, notFound } from '@tanstack/react-router'
import { getLLMText, source } from '@/lib/source'
import { decodeMarkdownUrl } from '@/lib/shared'

export const Route = createFileRoute('/{$}.md')({
  server: {
    handlers: {
      GET: async ({ params }) => {
        const page = source.getPage(decodeMarkdownUrl(params._splat?.split('/') ?? []))
        if (!page) throw notFound()
        return new Response(await getLLMText(page), { headers: { 'Content-Type': 'text/markdown' } })
      },
    },
  },
})
