import { createFileRoute } from '@tanstack/react-router'
import { createFromSource } from 'fumadocs-core/search/server'
import { source } from '@/lib/source'

const server = createFromSource(source, {
  language: 'english',
  buildIndex: async (page) => ({
    title: page.data.title,
    description: page.data.description,
    url: page.url,
    id: page.url,
    structuredData: {
      headings: [],
      contents: [{ heading: undefined, content: await page.data.getText('processed') }],
    },
  }),
})

export const Route = createFileRoute('/api/search')({
  server: {
    handlers: {
      GET: ({ request }) => {
        const url = new URL(request.url)
        return url.searchParams.has('query')
          ? server.GET(request)
          : server.staticGET()
      },
    },
  },
})
