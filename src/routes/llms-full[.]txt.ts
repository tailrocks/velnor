import { createFileRoute } from '@tanstack/react-router'
import { getLLMText, source } from '@/lib/source'

export const Route = createFileRoute('/llms-full.txt')({
  server: {
    handlers: {
      GET: async () => new Response((await Promise.all(source.getPages().map(getLLMText))).join('\n\n')),
    },
  },
})
