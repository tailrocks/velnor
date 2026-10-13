import { cp, mkdir, readdir } from 'node:fs/promises'
import { readdirSync } from 'node:fs'
import { availableParallelism } from 'node:os'
import { dirname, join, relative, sep } from 'node:path'

const root = join(import.meta.dirname, '..')
const contentRoot = join(root, 'content', 'docs')
const outDir = join(root, '.output', 'public')
const host = '127.0.0.1'
const port = 4173
const origin = `http://${host}:${port}`
const requestConcurrency = Math.max(8, availableParallelism() * 8)

function docsSlugs(dir = contentRoot): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name)
    if (entry.isDirectory()) return docsSlugs(path)
    if (!entry.isFile() || !entry.name.endsWith('.mdx')) return []
    const rel = relative(contentRoot, path).split(sep).join('/')
    const slug = rel.replace(/\.mdx$/, '')
    return slug === 'index'
      ? ['']
      : slug.endsWith('/index')
        ? [slug.slice(0, -'/index'.length)]
        : [slug]
  })
}

function pagePath(slug: string): string {
  return `/${slug}`.replace(/\/+/g, '/').replace(/\/$/, '') || '/'
}

function outputPath(path: string): string {
  if (path === '/') return join(outDir, 'index.html')
  if (path.endsWith('.md') || path.endsWith('.txt') || path.endsWith('.xml') || path.endsWith('.webp')) {
    return join(outDir, path.slice(1))
  }
  if (path === '/api/search') return join(outDir, 'api', 'search')
  return join(outDir, path.slice(1), 'index.html')
}

async function waitForServer() {
  const deadline = Date.now() + 20_000
  while (Date.now() < deadline) {
    try {
      if ((await fetch(`${origin}/`)).ok) return
    } catch {
      // The server is still starting.
    }
    await new Promise((resolve) => setTimeout(resolve, 250))
  }
  throw new Error('production server did not start')
}

async function fetchStatic(path: string) {
  const response = await fetch(`${origin}${path}`)
  if (!response.ok) throw new Error(`Failed to prerender ${path}: ${response.status}`)
  const target = outputPath(path)
  await mkdir(dirname(target), { recursive: true })
  await Bun.write(target, await response.arrayBuffer())
}

async function copySsrAssets() {
  const source = join(root, 'node_modules', '.nitro', 'vite', 'services', 'ssr', 'assets')
  const target = join(outDir, 'assets')
  await mkdir(target, { recursive: true })
  const entries = await readdir(source, { withFileTypes: true })
  await Promise.all(entries.map((entry) => cp(join(source, entry.name), join(target, entry.name), {
    force: true,
    recursive: entry.isDirectory(),
  })))
}

const pages = docsSlugs().flatMap((slug) => {
  const path = pagePath(slug)
  return [path, slug ? `${path}.md` : '/index.md']
})
const paths = [...new Set(['/', '/api/search', '/llms.txt', '/llms-full.txt', ...pages])]
const child = Bun.spawn(['bun', '.output/server/index.mjs'], {
  cwd: root,
  env: { ...process.env, HOST: host, NITRO_HOST: host, NITRO_PORT: String(port) },
  stdout: 'inherit',
  stderr: 'inherit',
})

try {
  await waitForServer()
  let next = 0
  await Promise.all(Array.from({ length: Math.min(requestConcurrency, paths.length) }, async () => {
    while (next < paths.length) {
      const path = paths[next]
      next += 1
      await fetchStatic(path)
    }
  }))
  await copySsrAssets()
  console.log(`[prerender-static] wrote ${paths.length} static routes`)
} finally {
  child.kill()
  await child.exited.catch(() => undefined)
}
