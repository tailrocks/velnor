export const appName = 'Velnor Documentation'
export const docsRoute = '/docs'

export const gitConfig = {
  user: 'tailrocks',
  repo: 'velnor',
  branch: 'main',
} as const

export function encodeMarkdownUrl(slugs: readonly string[], locale?: string): string {
  const segments = [...slugs]
  if (segments.length === 0) segments.push('index.md')
  else segments[segments.length - 1] = `${segments[segments.length - 1]}.md`

  return `/${[locale, ...docsRoute.split('/'), ...segments].filter(Boolean).join('/')}`
}

export function decodeMarkdownUrl(segments: readonly string[]): string[] {
  if (segments.length === 0) return []
  const output = [...segments]
  output[output.length - 1] = output[output.length - 1].replace(/\.md$/, '')
  if (output.length === 1 && output[0] === 'index') output.pop()
  return output
}
