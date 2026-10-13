import { createFileRoute } from '@tanstack/react-router'

const favicon = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64">
  <rect width="64" height="64" rx="14" fill="#121212"/>
  <path d="M16 16h10l6 18 6-18h10L37 48h-10z" fill="#f5f5f5"/>
</svg>`

export const Route = createFileRoute('/favicon.ico')({
  server: {
    handlers: {
      GET: () => new Response(favicon, { headers: { 'content-type': 'image/svg+xml' } }),
    },
  },
})
