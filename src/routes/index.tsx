import { createFileRoute } from '@tanstack/react-router'
import { useFumadocsLoader } from 'fumadocs-core/source/client'
import { DocsLayout } from 'fumadocs-ui/layouts/docs'
import { baseOptions } from '@/lib/layout.shared'
import { Content, loader } from './$'

export const Route = createFileRoute('/')({
  component: Home,
  loader: () => loader({ data: [] }),
})

function Home() {
  const { pageTree, path, markdownUrl } = useFumadocsLoader(Route.useLoaderData())
  return (
    <DocsLayout {...baseOptions()} tree={pageTree}>
      <Content path={path} markdownUrl={markdownUrl} />
    </DocsLayout>
  )
}
