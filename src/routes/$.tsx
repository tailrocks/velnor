import { createFileRoute, notFound } from '@tanstack/react-router'
import { createServerFn } from '@tanstack/react-start'
import { Suspense, use } from 'react'
import { DocsLayout } from 'fumadocs-ui/layouts/docs'
import { DocsBody, DocsDescription, DocsPage, DocsTitle } from 'fumadocs-ui/layouts/docs/page'
import { useFumadocsLoader } from 'fumadocs-core/source/client'
import { docs, source } from '@/lib/source'
import { baseOptions } from '@/lib/layout.shared'
import { encodeMarkdownUrl } from '@/lib/shared'
import { useMDXComponents } from '@/components/mdx'

export type PageData = {
  path: string
  title: string
  description?: string
  markdownUrl: string
  pageTree: Awaited<ReturnType<typeof source.serializePageTree>>
}

export const Route = createFileRoute('/$')({
  component: Page,
  head: ({ loaderData }) => ({
    meta: [
      { title: (loaderData as unknown as PageData).title },
      { name: 'description', content: (loaderData as unknown as PageData).description },
    ],
  }),
  loader: async ({ params }): Promise<PageData> => {
    const slugs = params._splat?.split('/') ?? []
    return loader({ data: slugs })
  },
})

export const loader = createServerFn({ method: 'GET' })
  .validator((slugs: string[]) => slugs)
  .handler(async ({ data: slugs }) => {
    const page = source.getPage(slugs)
    if (!page) throw notFound()
    await docs.getPage(page.path)?.preload()

    return {
      path: page.path,
      title: page.data.title,
      description: page.data.description,
      markdownUrl: encodeMarkdownUrl(page.slugs, page.locale),
      pageTree: await source.serializePageTree(source.getPageTree()),
    }
  })

export function Content({ path, markdownUrl }: { path: string; markdownUrl: string }) {
  const page = docs.getPage(path)
  if (!page) throw new Error(`Unknown documentation page: ${path}`)
  const { toc } = use(page.load())
  const MDX = page.body

  return (
    <DocsPage toc={toc}>
      <DocsTitle>{page.title}</DocsTitle>
      <DocsDescription>{page.description}</DocsDescription>
      <div className="-mt-4 border-b pb-6">
        <a className="text-fd-muted-foreground text-sm underline underline-offset-4" href={markdownUrl}>
          View as Markdown
        </a>
      </div>
      <DocsBody><MDX components={useMDXComponents()} /></DocsBody>
    </DocsPage>
  )
}

function Page() {
  const { pageTree, path, markdownUrl } = useFumadocsLoader(Route.useLoaderData() as PageData)
  return (
    <DocsLayout {...baseOptions()} tree={pageTree}>
      <Suspense><Content path={path} markdownUrl={markdownUrl} /></Suspense>
    </DocsLayout>
  )
}
