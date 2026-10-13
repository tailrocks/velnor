import { loader } from 'fumadocs-core/source'
import { remarkMdxMermaid, remarkStructure } from 'fumadocs-core/mdx-plugins'
import { lucideIconsPlugin } from 'fumadocs-core/source/lucide-icons'
import { defineDocs } from 'fumadocs-mdx/macro'
import { docsRoute } from './shared'

export const docs = defineDocs({
  dir: 'content/docs',
  docs: {
    async: true,
    postprocess: {
      includeProcessedMarkdown: true,
    },
    mdxOptions: {
      remarkPlugins: [remarkMdxMermaid, [remarkStructure, { exportAs: 'structuredData' }]],
    },
  },
})

export const source = loader({
  source: docs.toFumadocsSource(),
  baseUrl: docsRoute,
  plugins: [lucideIconsPlugin()],
})

export async function getLLMText(page: (typeof source)['$inferPage']): Promise<string> {
  const processed = await page.data.getText('processed')
  return `# ${page.data.title} (${page.url})\n\n${processed}`
}
