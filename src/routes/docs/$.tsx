import { createFileRoute, Link, notFound } from '@tanstack/react-router'
import { createServerFn } from '@tanstack/react-start'
import { Suspense, use } from 'react'
import { DocsLayout } from 'fumadocs-ui/layouts/docs'
import { DocsBody, DocsDescription, DocsPage, DocsTitle } from 'fumadocs-ui/layouts/docs/page'
import { useFumadocsLoader } from 'fumadocs-core/source/client'
import { docs, source } from '@/lib/source'
import { baseOptions } from '@/lib/layout.shared'
import { encodeMarkdownUrl } from '@/lib/shared'
import { useMDXComponents } from '@/components/mdx'

export const Route = createFileRoute('/docs/$')({
  component: Page,
  loader: async ({ params }) => {
    const slugs = params._splat?.split('/') ?? []
    return loader({ data: slugs })
  },
})

const loader = createServerFn({ method: 'GET' })
  .validator((slugs: string[]) => slugs)
  .handler(async ({ data: slugs }) => {
    const page = source.getPage(slugs)
    if (!page) throw notFound()
    await docs.getPage(page.path)?.preload()

    return {
      path: page.path,
      markdownUrl: encodeMarkdownUrl(page.slugs, page.locale),
      pageTree: await source.serializePageTree(source.getPageTree()),
    }
  })

function Content({ path, markdownUrl }: { path: string; markdownUrl: string }) {
  const page = docs.getPage(path)
  if (!page) throw new Error(`Unknown documentation page: ${path}`)
  const { toc } = use(page.load())
  const MDX = page.body

  return (
    <DocsPage toc={toc}>
      <DocsTitle>{page.title}</DocsTitle>
      <DocsDescription>{page.description}</DocsDescription>
      <div className="-mt-4 border-b pb-6">
        <Link className="text-fd-muted-foreground text-sm underline underline-offset-4" to={markdownUrl}>
          View as Markdown
        </Link>
      </div>
      <DocsBody><MDX components={useMDXComponents()} /></DocsBody>
    </DocsPage>
  )
}

function Page() {
  const { pageTree, path, markdownUrl } = useFumadocsLoader(Route.useLoaderData())
  return (
    <DocsLayout {...baseOptions()} tree={pageTree}>
      <Suspense><Content path={path} markdownUrl={markdownUrl} /></Suspense>
    </DocsLayout>
  )
}
