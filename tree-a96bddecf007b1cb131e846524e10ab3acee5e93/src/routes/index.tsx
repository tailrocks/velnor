import { createFileRoute, Link } from '@tanstack/react-router'
import { HomeLayout } from 'fumadocs-ui/layouts/home'
import { baseOptions } from '@/lib/layout.shared'

export const Route = createFileRoute('/')({ component: Home })

function Home() {
  return (
    <HomeLayout {...baseOptions()}>
      <main className="mx-auto flex w-full max-w-5xl flex-1 flex-col justify-center gap-6 px-6 py-24">
      <p className="text-fd-muted-foreground text-sm font-medium uppercase tracking-widest">Velnor</p>
      <h1 className="text-4xl font-semibold tracking-tight md:text-6xl">Linux GitHub Actions runner documentation</h1>
      <p className="text-fd-muted-foreground max-w-2xl text-lg">Research-grade runner control plane for ephemeral jobs, strict admission, and explicit Docker or Firecracker execution.</p>
      <Link className="text-fd-primary underline underline-offset-4" to="/docs/$" params={{ _splat: '' }}>Read the documentation →</Link>
      </main>
    </HomeLayout>
  )
}
