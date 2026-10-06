import {
  createFileSystemGeneratorCache,
  createGenerator,
  remarkAutoTypeTable,
} from 'fumadocs-typescript'
import { remarkMdxMermaid, remarkStructure } from 'fumadocs-core/mdx-plugins'
import { defineConfig } from 'fumadocs-mdx/config'

const generator = createGenerator({
  cache: createFileSystemGeneratorCache('.cache/fumadocs-typescript'),
})

export default defineConfig({
  mdxOptions: {
    remarkPlugins: [
      remarkMdxMermaid,
      [remarkStructure, { exportAs: 'structuredData' }],
      [remarkAutoTypeTable, { generator }],
    ],
  },
})
