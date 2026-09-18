[Skip to content](https://reui.io/docs/registry#main-content)

[ReUI home](https://reui.io/)

ProductsResources [Docs](https://reui.io/docs) [Support](https://reui.io/support) [Pricing](https://reui.io/pricing)

[Roadmap (has updates coming soon)](https://reui.io/roadmap) [X](https://x.com/reui_io) [Figma](https://www.figma.com/community/file/1649373313065184861/shadcn-ui-design-system-by-reui) [3.5K](https://github.com/keenthemes/reui)

Sign inGet ProGet All-Access

Overview

- [Introduction](https://reui.io/docs)
- [Get Started](https://reui.io/docs/get-started)
- [License Setup](https://reui.io/docs/license-setup)
- [Styling](https://reui.io/docs/styling)
- [RegistryReUI items now import cn from the cn package, installed for you](https://reui.io/docs/registry)
- [MCP ServerMCP results carry a preview image, and agents can look at it before installing](https://reui.io/docs/mcp)
- [Embed](https://reui.io/docs/embed)
- [Agent Skills4 slash commands run the ReUI workflow from your agent's own command surface](https://reui.io/docs/agent-skills)
- [llms.txt](https://reui.io/llms.txt)
- [RTL](https://reui.io/docs/rtl)
- [Changelog](https://reui.io/docs/changelog)

MCP Server

- [Claude](https://reui.io/docs/claude)
- [CodexCodexCodex](https://reui.io/docs/codex)
- [Cursor](https://reui.io/docs/cursor)
- [Grok](https://reui.io/docs/grok)
- [Conductor](https://reui.io/docs/conductor)
- [v0](https://reui.io/docs/v0)
- [Lovable](https://reui.io/docs/lovable)
- [Replit](https://reui.io/docs/replit)
- [Bolt](https://reui.io/docs/bolt)
- [OpenCode](https://reui.io/docs/opencode)
- [VS Code](https://reui.io/docs/vscode)
- [GitHub Copilot](https://reui.io/docs/github-copilot)
- [Kilo Code](https://reui.io/docs/kilo-code)
- [Zed](https://reui.io/docs/zed)
- [Antigravity](https://reui.io/docs/antigravity)
- [WSWindsurf](https://reui.io/docs/windsurf)
- [CLCline](https://reui.io/docs/cline)
- [Gemini CLI](https://reui.io/docs/gemini-cli)
- [AMAmp](https://reui.io/docs/amp)
- [JBJetBrains Junie](https://reui.io/docs/jetbrains-junie)

Components

- [Alert](https://reui.io/docs/components/base/alert)
- [Autocomplete](https://reui.io/docs/components/base/autocomplete)
- [Badge](https://reui.io/docs/components/base/badge)
- [CascaderVirtual rows no longer freeze mid scroll in apps built with React Compiler](https://reui.io/docs/components/base/cascader)
- [Code Block](https://reui.io/docs/components/base/code-block)
- [Data GridPagination now always carries the first and last page, so the end of a long Data Grid is one click away](https://reui.io/docs/components/base/data-grid)
- [Date Selector](https://reui.io/docs/components/base/date-selector)
- [Event CalendarTimed views keep a clickable strip at the end of each day column, so a full day still takes a new one](https://reui.io/docs/components/base/event-calendar)
- [File Upload](https://reui.io/docs/components/base/file-upload)
- [FiltersdefaultOperator is documented for what it actually does](https://reui.io/docs/components/base/filters)
- [Frame](https://reui.io/docs/components/base/frame)
- [Gantt](https://reui.io/docs/components/base/gantt)
- [Icon Stack](https://reui.io/docs/components/base/icon-stack)
- [Icon Tile](https://reui.io/docs/components/base/icon-tile)
- [Kanban](https://reui.io/docs/components/base/kanban)
- [Number Field](https://reui.io/docs/components/base/number-field)
- [Phone Input](https://reui.io/docs/components/base/phone-input)
- [Rating](https://reui.io/docs/components/base/rating)
- [Scrollspy](https://reui.io/docs/components/base/scrollspy)
- [Sortable](https://reui.io/docs/components/base/sortable)
- [Stepper](https://reui.io/docs/components/base/stepper)
- [Timeline](https://reui.io/docs/components/base/timeline)
- [Tree](https://reui.io/docs/components/base/tree)

# Registry

Copy Markdown [Previous](https://reui.io/docs/styling) [Next](https://reui.io/docs/mcp)

Learn how to use the ReUI Registry with shadcn/ui.

The ReUI Registry uses a single `@reui` namespace for free `c-*` components, paid blocks, paid icons, and paid templates. The shadcn CLI resolves the full alias suffix into the registry URL, so you can install everything from one namespace and let ReUI handle access rules on the server.

@reui is a registry namespace, not an npm scope

There is no ReUI npm package to install. Running `npm i @reui/button` pulls
an unrelated publisher's package from npm's own `@reui` scope, which is not
ours. ReUI items are installed only with the shadcn CLI, which resolves
`@reui/<name>` against the registry URL in your `components.json`.

## Setup ReUI Registry

Add the ReUI registry namespace to your `components.json`. Learn more about registry config from [shadcn registry docs](https://ui.shadcn.com/docs/registry).

```

```

### Style Values

`{style}` is not free-form. The shadcn CLI substitutes it with the `style` value from your `components.json`, which ReUI reads as `<base>-<variant>`: the base picks the primitive library the code is built on, the variant picks the visual style. Every pairing of the two lists below is served, so `base-nova` (the default), `radix-lyra` and `base-sera` are all valid. A `{style}` outside these lists is not served and the install fails as not found.

| `<base>` | Primitive library |
| --- | --- |
| `base` | [Base UI](https://base-ui.com/) |
| `radix` | [Radix UI](https://www.radix-ui.com/) |

| `<variant>` | Look |
| --- | --- |
| `vega` | Clean, neutral, and familiar |
| `nova` | Reduced padding and margins |
| `maia` | Rounded, with generous spacing |
| `lyra` | Boxy and sharp. For mono fonts |
| `mira` | Made for compact interfaces |
| `luma` | Fluid, luminous, and soft |
| `sera` | Editorial and typographic |
| `rhea` | Like Luma but compact |

### Class Merging

ReUI items that merge classes use [`cn`](https://www.npmjs.com/package/cn), a compiled drop-in for `clsx` plus `tailwind-merge`, imported as a package:

```

```

The CLI installs it for you: any item that imports it lists `cn` in its `dependencies`, so `shadcn add` adds it to your `package.json` on the first such install and skips it after that.

This follows shadcn, which moved every component onto the package in its [September 2026 `cn` release](https://ui.shadcn.com/docs/changelog/2026-09-cn): one dependency instead of two, and no local helper to maintain.

Earlier ReUI releases imported `cn` from your own `@/lib/utils`. Items already in your project keep working, and your `@/lib/utils` is untouched.

To drop `clsx` and `tailwind-merge` from your own project, run the upstream migration:

```

```

If you had customized `cn` in `@/lib/utils`, rebuild it with `createCn` from `cn/config`. Newly installed ReUI items import the package directly, so they no longer route through that file.

## Free Components

Free component examples use the `c-*` naming pattern and can be installed without a license key.

```

```

Some free `c-*` items pull shared `@reui/*` primitives such as `@reui/alert` or `@reui/data-grid` as registry dependencies. Those supporting files remain publicly accessible so free component installs keep working.

## Paid Blocks, Icons, And Templates

Premium blocks, icons, and templates use the same `@reui` namespace, but they require a valid license key.

If you want the full premium setup flow with a visible account key state, copy-ready snippets, and install examples in one place, see the [License Setup guide](https://reui.io/docs/license-setup).

1. Add your license key to `.env.local`.

```

```

2. Change the registry entry to the authenticated object form.

```

```

3. Install premium items from the same namespace.

```

```

Free items keep working with the authenticated config too, so you do not need a second registry namespace.

[Styling](https://reui.io/docs/styling) [MCP Server](https://reui.io/docs/mcp)

On This Page

[Setup ReUI Registry](https://reui.io/docs/registry#setup-reui-registry) [Style Values](https://reui.io/docs/registry#style-values) [Class Merging](https://reui.io/docs/registry#class-merging) [Free Components](https://reui.io/docs/registry#free-components) [Paid Blocks, Icons, And Templates](https://reui.io/docs/registry#paid-blocks-icons-and-templates)

### Application

- [App Shell](https://reui.io/blocks/application/app-shell)
- [Auth](https://reui.io/blocks/application/auth)
- [Card](https://reui.io/blocks/application/card)
- [Chart](https://reui.io/blocks/application/chart)
- [Dashboard](https://reui.io/blocks/application/dashboard)
- [Dialog](https://reui.io/blocks/application/dialog)
- [Empty State](https://reui.io/blocks/application/empty-state)
- [Event Calendar](https://reui.io/blocks/application/event-calendar)
- [Flow](https://reui.io/blocks/application/flow)
- [Form](https://reui.io/blocks/application/form)
- [Gantt](https://reui.io/blocks/application/gantt)
- [Kanban Board](https://reui.io/blocks/application/kanban-board)
- [List](https://reui.io/blocks/application/list)
- [Navbar](https://reui.io/blocks/application/navbar)
- [Onboarding](https://reui.io/blocks/application/onboarding)
- [Profile](https://reui.io/blocks/application/profile)
- [Schedule](https://reui.io/blocks/application/schedule)
- [Settings](https://reui.io/blocks/application/settings)
- [Sheet](https://reui.io/blocks/application/sheet)
- [Stats](https://reui.io/blocks/application/stats)
- [Timeline](https://reui.io/blocks/application/timeline)
- [Wizard](https://reui.io/blocks/application/wizard)

### Solutions

- [Agents](https://reui.io/blocks/solutions/agents)
- [AI Ops](https://reui.io/blocks/solutions/ai-ops)
- [Analytics](https://reui.io/blocks/solutions/analytics)
- [Billing](https://reui.io/blocks/solutions/billing)
- [Bookings](https://reui.io/blocks/solutions/bookings)
- [CRM](https://reui.io/blocks/solutions/crm)
- [Files](https://reui.io/blocks/solutions/files)
- [Inventory](https://reui.io/blocks/solutions/inventory)
- [Users](https://reui.io/blocks/solutions/users)

### AI & Agents

- [AI Chat](https://reui.io/blocks/ai-agents/ai-chat)
- [Agent Activity](https://reui.io/blocks/ai-agents/agent-activity)

### Templates

- [E-commerce](https://reui.io/templates/e-commerce)
- [SaaS](https://reui.io/templates/saas)
- [Dashboard](https://reui.io/templates/dashboard)
- [Landing](https://reui.io/templates/landing)
- [All templates](https://reui.io/templates)

### eCommerce

- [Category Card](https://reui.io/blocks/ecommerce/category-card)
- [Checkout](https://reui.io/blocks/ecommerce/checkout)
- [Comparison](https://reui.io/blocks/ecommerce/comparison)
- [Coupon](https://reui.io/blocks/ecommerce/coupon)
- [Filter Sidebar](https://reui.io/blocks/ecommerce/filter-sidebar)
- [Product Card](https://reui.io/blocks/ecommerce/product-card)
- [Product Detail](https://reui.io/blocks/ecommerce/product-detail)
- [Product Grid](https://reui.io/blocks/ecommerce/product-grid)
- [Receipt](https://reui.io/blocks/ecommerce/receipt)
- [Review](https://reui.io/blocks/ecommerce/review)
- [Shopping Cart](https://reui.io/blocks/ecommerce/shopping-cart)
- [Wishlist](https://reui.io/blocks/ecommerce/wishlist)
- [Shop Hero](https://reui.io/blocks/ecommerce/shop-hero)

### Data Grid

- [Base](https://reui.io/blocks/data-grid/base)
- [Columns](https://reui.io/blocks/data-grid/columns)
- [Drag & Drop](https://reui.io/blocks/data-grid/drag-drop)
- [Editing](https://reui.io/blocks/data-grid/editing)
- [Expansion](https://reui.io/blocks/data-grid/expansion)
- [Filtering](https://reui.io/blocks/data-grid/filtering)
- [Grouping](https://reui.io/blocks/data-grid/grouping)
- [Virtualization](https://reui.io/blocks/data-grid/virtualization)

### Marketing

- [Blog](https://reui.io/blocks/marketing/blog)
- [Contact](https://reui.io/blocks/marketing/contact)
- [CTA](https://reui.io/blocks/marketing/cta)
- [FAQ](https://reui.io/blocks/marketing/faq)
- [Hero](https://reui.io/blocks/marketing/hero)

### Resources

- [Components](https://reui.io/components)
- [Blocks](https://reui.io/blocks)
- [Icons](https://reui.io/icons)
- [MCP for Agents](https://reui.io/mcp)
- [Docs](https://reui.io/docs)
- [Support](https://reui.io/support)
- [Pricing](https://reui.io/pricing)
- [Roadmap(has updates coming soon)](https://reui.io/roadmap)
- AffiliateSoon

### Legal

- [Privacy Policy](https://reui.io/legal/privacy-policy)
- [Terms & Conditions](https://reui.io/legal/terms-and-conditions)
- [License](https://reui.io/legal/license)
- [Refunds](https://reui.io/legal/refund-policy)
- [Cookies](https://reui.io/legal/cookies)

© 2026 ReUI. All rights reserved.

[Follow us on X](https://x.com/reui_io)[View ReUI on Figma](https://www.figma.com/community/file/1649373313065184861/shadcn-ui-design-system-by-reui)[3.5K](https://github.com/keenthemes)

[Skip to content](https://reui.io/docs/registry#main-content)

[ReUI home](https://reui.io/)

ProductsResources [Docs](https://reui.io/docs) [Support](https://reui.io/support) [Pricing](https://reui.io/pricing)

## Search

Search pages, docs, components, examples, blocks and templates

[Roadmap (has updates coming soon)](https://reui.io/roadmap) [X](https://x.com/reui_io) [Figma](https://www.figma.com/community/file/1649373313065184861/shadcn-ui-design-system-by-reui) [3.5K](https://github.com/keenthemes/reui)

[Sign in](https://reui.io/login?redirect=%2Fdocs%2Fregistry) [Get ProGet All-Access](https://reui.io/pricing)

Overview

- [Introduction](https://reui.io/docs)
- [Get Started](https://reui.io/docs/get-started)
- [License Setup](https://reui.io/docs/license-setup)
- [Styling](https://reui.io/docs/styling)
- [RegistryReUI items now import cn from the cn package, installed for you](https://reui.io/docs/registry)
- [MCP ServerMCP results carry a preview image, and agents can look at it before installing](https://reui.io/docs/mcp)
- [Embed](https://reui.io/docs/embed)
- [Agent Skills4 slash commands run the ReUI workflow from your agent's own command surface](https://reui.io/docs/agent-skills)
- [llms.txt](https://reui.io/llms.txt)
- [RTL](https://reui.io/docs/rtl)
- [Changelog](https://reui.io/docs/changelog)

MCP Server

- [Claude](https://reui.io/docs/claude)
- [CodexCodexCodex](https://reui.io/docs/codex)
- [Cursor](https://reui.io/docs/cursor)
- [Grok](https://reui.io/docs/grok)
- [Conductor](https://reui.io/docs/conductor)
- [v0](https://reui.io/docs/v0)
- [Lovable](https://reui.io/docs/lovable)
- [Replit](https://reui.io/docs/replit)
- [Bolt](https://reui.io/docs/bolt)
- [OpenCode](https://reui.io/docs/opencode)
- [VS Code](https://reui.io/docs/vscode)
- [GitHub Copilot](https://reui.io/docs/github-copilot)
- [Kilo Code](https://reui.io/docs/kilo-code)
- [Zed](https://reui.io/docs/zed)
- [Antigravity](https://reui.io/docs/antigravity)
- [WSWindsurf](https://reui.io/docs/windsurf)
- [CLCline](https://reui.io/docs/cline)
- [Gemini CLI](https://reui.io/docs/gemini-cli)
- [AMAmp](https://reui.io/docs/amp)
- [JBJetBrains Junie](https://reui.io/docs/jetbrains-junie)

Components

- [Alert](https://reui.io/docs/components/base/alert)
- [Autocomplete](https://reui.io/docs/components/base/autocomplete)
- [Badge](https://reui.io/docs/components/base/badge)
- [CascaderVirtual rows no longer freeze mid scroll in apps built with React Compiler](https://reui.io/docs/components/base/cascader)
- [Code Block](https://reui.io/docs/components/base/code-block)
- [Data GridPagination now always carries the first and last page, so the end of a long Data Grid is one click away](https://reui.io/docs/components/base/data-grid)
- [Date Selector](https://reui.io/docs/components/base/date-selector)
- [Event CalendarTimed views keep a clickable strip at the end of each day column, so a full day still takes a new one](https://reui.io/docs/components/base/event-calendar)
- [File Upload](https://reui.io/docs/components/base/file-upload)
- [FiltersdefaultOperator is documented for what it actually does](https://reui.io/docs/components/base/filters)
- [Frame](https://reui.io/docs/components/base/frame)
- [Gantt](https://reui.io/docs/components/base/gantt)
- [Icon Stack](https://reui.io/docs/components/base/icon-stack)
- [Icon Tile](https://reui.io/docs/components/base/icon-tile)
- [Kanban](https://reui.io/docs/components/base/kanban)
- [Number Field](https://reui.io/docs/components/base/number-field)
- [Phone Input](https://reui.io/docs/components/base/phone-input)
- [Rating](https://reui.io/docs/components/base/rating)
- [Scrollspy](https://reui.io/docs/components/base/scrollspy)
- [Sortable](https://reui.io/docs/components/base/sortable)
- [Stepper](https://reui.io/docs/components/base/stepper)
- [Timeline](https://reui.io/docs/components/base/timeline)
- [Tree](https://reui.io/docs/components/base/tree)

# Registry

Copy Markdown [Previous](https://reui.io/docs/styling) [Next](https://reui.io/docs/mcp)

Learn how to use the ReUI Registry with shadcn/ui.

The ReUI Registry uses a single `@reui` namespace for free `c-*` components, paid blocks, paid icons, and paid templates. The shadcn CLI resolves the full alias suffix into the registry URL, so you can install everything from one namespace and let ReUI handle access rules on the server.

@reui is a registry namespace, not an npm scope

There is no ReUI npm package to install. Running `npm i @reui/button` pulls
an unrelated publisher's package from npm's own `@reui` scope, which is not
ours. ReUI items are installed only with the shadcn CLI, which resolves
`@reui/<name>` against the registry URL in your `components.json`.

## Setup ReUI Registry

Add the ReUI registry namespace to your `components.json`. Learn more about registry config from [shadcn registry docs](https://ui.shadcn.com/docs/registry).

```

```

### Style Values

`{style}` is not free-form. The shadcn CLI substitutes it with the `style` value from your `components.json`, which ReUI reads as `<base>-<variant>`: the base picks the primitive library the code is built on, the variant picks the visual style. Every pairing of the two lists below is served, so `base-nova` (the default), `radix-lyra` and `base-sera` are all valid. A `{style}` outside these lists is not served and the install fails as not found.

| `<base>` | Primitive library |
| --- | --- |
| `base` | [Base UI](https://base-ui.com/) |
| `radix` | [Radix UI](https://www.radix-ui.com/) |

| `<variant>` | Look |
| --- | --- |
| `vega` | Clean, neutral, and familiar |
| `nova` | Reduced padding and margins |
| `maia` | Rounded, with generous spacing |
| `lyra` | Boxy and sharp. For mono fonts |
| `mira` | Made for compact interfaces |
| `luma` | Fluid, luminous, and soft |
| `sera` | Editorial and typographic |
| `rhea` | Like Luma but compact |

### Class Merging

ReUI items that merge classes use [`cn`](https://www.npmjs.com/package/cn), a compiled drop-in for `clsx` plus `tailwind-merge`, imported as a package:

```

```

The CLI installs it for you: any item that imports it lists `cn` in its `dependencies`, so `shadcn add` adds it to your `package.json` on the first such install and skips it after that.

This follows shadcn, which moved every component onto the package in its [September 2026 `cn` release](https://ui.shadcn.com/docs/changelog/2026-09-cn): one dependency instead of two, and no local helper to maintain.

Earlier ReUI releases imported `cn` from your own `@/lib/utils`. Items already in your project keep working, and your `@/lib/utils` is untouched.

To drop `clsx` and `tailwind-merge` from your own project, run the upstream migration:

```

```

If you had customized `cn` in `@/lib/utils`, rebuild it with `createCn` from `cn/config`. Newly installed ReUI items import the package directly, so they no longer route through that file.

## Free Components

Free component examples use the `c-*` naming pattern and can be installed without a license key.

```

```

Some free `c-*` items pull shared `@reui/*` primitives such as `@reui/alert` or `@reui/data-grid` as registry dependencies. Those supporting files remain publicly accessible so free component installs keep working.

## Paid Blocks, Icons, And Templates

Premium blocks, icons, and templates use the same `@reui` namespace, but they require a valid license key.

If you want the full premium setup flow with a visible account key state, copy-ready snippets, and install examples in one place, see the [License Setup guide](https://reui.io/docs/license-setup).

1. Add your license key to `.env.local`.

```

```

2. Change the registry entry to the authenticated object form.

```

```

3. Install premium items from the same namespace.

```

```

Free items keep working with the authenticated config too, so you do not need a second registry namespace.

[Styling](https://reui.io/docs/styling) [MCP Server](https://reui.io/docs/mcp)

On This Page

[Setup ReUI Registry](https://reui.io/docs/registry#setup-reui-registry) [Style Values](https://reui.io/docs/registry#style-values) [Class Merging](https://reui.io/docs/registry#class-merging) [Free Components](https://reui.io/docs/registry#free-components) [Paid Blocks, Icons, And Templates](https://reui.io/docs/registry#paid-blocks-icons-and-templates)

### Application

- [App Shell](https://reui.io/blocks/application/app-shell)
- [Auth](https://reui.io/blocks/application/auth)
- [Card](https://reui.io/blocks/application/card)
- [Chart](https://reui.io/blocks/application/chart)
- [Dashboard](https://reui.io/blocks/application/dashboard)
- [Dialog](https://reui.io/blocks/application/dialog)
- [Empty State](https://reui.io/blocks/application/empty-state)
- [Event Calendar](https://reui.io/blocks/application/event-calendar)
- [Flow](https://reui.io/blocks/application/flow)
- [Form](https://reui.io/blocks/application/form)
- [Gantt](https://reui.io/blocks/application/gantt)
- [Kanban Board](https://reui.io/blocks/application/kanban-board)
- [List](https://reui.io/blocks/application/list)
- [Navbar](https://reui.io/blocks/application/navbar)
- [Onboarding](https://reui.io/blocks/application/onboarding)
- [Profile](https://reui.io/blocks/application/profile)
- [Schedule](https://reui.io/blocks/application/schedule)
- [Settings](https://reui.io/blocks/application/settings)
- [Sheet](https://reui.io/blocks/application/sheet)
- [Stats](https://reui.io/blocks/application/stats)
- [Timeline](https://reui.io/blocks/application/timeline)
- [Wizard](https://reui.io/blocks/application/wizard)

### Solutions

- [Agents](https://reui.io/blocks/solutions/agents)
- [AI Ops](https://reui.io/blocks/solutions/ai-ops)
- [Analytics](https://reui.io/blocks/solutions/analytics)
- [Billing](https://reui.io/blocks/solutions/billing)
- [Bookings](https://reui.io/blocks/solutions/bookings)
- [CRM](https://reui.io/blocks/solutions/crm)
- [Files](https://reui.io/blocks/solutions/files)
- [Inventory](https://reui.io/blocks/solutions/inventory)
- [Users](https://reui.io/blocks/solutions/users)

### AI & Agents

- [AI Chat](https://reui.io/blocks/ai-agents/ai-chat)
- [Agent Activity](https://reui.io/blocks/ai-agents/agent-activity)

### Templates

- [E-commerce](https://reui.io/templates/e-commerce)
- [SaaS](https://reui.io/templates/saas)
- [Dashboard](https://reui.io/templates/dashboard)
- [Landing](https://reui.io/templates/landing)
- [All templates](https://reui.io/templates)

### eCommerce

- [Category Card](https://reui.io/blocks/ecommerce/category-card)
- [Checkout](https://reui.io/blocks/ecommerce/checkout)
- [Comparison](https://reui.io/blocks/ecommerce/comparison)
- [Coupon](https://reui.io/blocks/ecommerce/coupon)
- [Filter Sidebar](https://reui.io/blocks/ecommerce/filter-sidebar)
- [Product Card](https://reui.io/blocks/ecommerce/product-card)
- [Product Detail](https://reui.io/blocks/ecommerce/product-detail)
- [Product Grid](https://reui.io/blocks/ecommerce/product-grid)
- [Receipt](https://reui.io/blocks/ecommerce/receipt)
- [Review](https://reui.io/blocks/ecommerce/review)
- [Shopping Cart](https://reui.io/blocks/ecommerce/shopping-cart)
- [Wishlist](https://reui.io/blocks/ecommerce/wishlist)
- [Shop Hero](https://reui.io/blocks/ecommerce/shop-hero)

### Data Grid

- [Base](https://reui.io/blocks/data-grid/base)
- [Columns](https://reui.io/blocks/data-grid/columns)
- [Drag & Drop](https://reui.io/blocks/data-grid/drag-drop)
- [Editing](https://reui.io/blocks/data-grid/editing)
- [Expansion](https://reui.io/blocks/data-grid/expansion)
- [Filtering](https://reui.io/blocks/data-grid/filtering)
- [Grouping](https://reui.io/blocks/data-grid/grouping)
- [Virtualization](https://reui.io/blocks/data-grid/virtualization)

### Marketing

- [Blog](https://reui.io/blocks/marketing/blog)
- [Contact](https://reui.io/blocks/marketing/contact)
- [CTA](https://reui.io/blocks/marketing/cta)
- [FAQ](https://reui.io/blocks/marketing/faq)
- [Hero](https://reui.io/blocks/marketing/hero)

### Resources

- [Components](https://reui.io/components)
- [Blocks](https://reui.io/blocks)
- [Icons](https://reui.io/icons)
- [MCP for Agents](https://reui.io/mcp)
- [Docs](https://reui.io/docs)
- [Support](https://reui.io/support)
- [Pricing](https://reui.io/pricing)
- [Roadmap(has updates coming soon)](https://reui.io/roadmap)
- AffiliateSoon

### Legal

- [Privacy Policy](https://reui.io/legal/privacy-policy)
- [Terms & Conditions](https://reui.io/legal/terms-and-conditions)
- [License](https://reui.io/legal/license)
- [Refunds](https://reui.io/legal/refund-policy)
- [Cookies](https://reui.io/legal/cookies)

© 2026 ReUI. All rights reserved.

[Follow us on X](https://x.com/reui_io)[View ReUI on Figma](https://www.figma.com/community/file/1649373313065184861/shadcn-ui-design-system-by-reui)[3.5K](https://github.com/keenthemes)