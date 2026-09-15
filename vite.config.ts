import { defineConfig } from 'vite'
import tailwindcss from '@tailwindcss/vite'
import { tanstackStart } from '@tanstack/react-start/plugin/vite'
import viteReact from '@vitejs/plugin-react'
import { fumadocsMdx } from 'fumadocs-mdx/vite'
import { nitro } from 'nitro/vite'

const config = defineConfig({
  server: { port: 3000 },
  resolve: { tsconfigPaths: true, dedupe: ['react', 'react-dom'] },
  optimizeDeps: {
    exclude: ['react/jsx-runtime', 'react/jsx-dev-runtime'],
  },
  plugins: [fumadocsMdx(), tailwindcss(), tanstackStart(), viteReact(), nitro({ preset: 'bun' })],
})

export default config
