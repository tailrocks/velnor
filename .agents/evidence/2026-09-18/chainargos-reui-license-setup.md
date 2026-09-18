[Skip to content](https://reui.io/docs/license-setup#main-content)

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

# License Setup

Copy Markdown [Previous](https://reui.io/docs/get-started) [Next](https://reui.io/docs/styling)

Configure your ReUI license key for premium registry installs.

Use this guide when you want to install premium ReUI registry items with the shadcn CLI. Free components do not require a license key, but premium blocks, icons, and templates do.

## Your License Key

Use the license key from your ReUI account:

You need to be logged in to see your license key

[Login](https://reui.io/login?redirect=%2Fdocs%2Flicense-setup)

## CLI Installation

This is the recommended setup for premium registry installs.

### Prerequisites

- A React project with shadcn/ui initialized
- Node.js 18 or newer
- A `components.json` file in your project root

### Add your license key

Put it in `.env.local` in the root of your project:

```

```

### Point components.json at the @reui registry

Update `components.json` to use the authenticated `@reui` registry config:

```

```

The shadcn CLI expands `${REUI_LICENSE_KEY}` from your environment, so leave it as a variable here. A ReUI [MCP server](https://reui.io/docs/mcp) entry is a separate config with its own rules, and they differ by client: Claude Code expands `${VAR}` in `.mcp.json`, Cursor and VS Code only understand the `env:` prefix (`${env:NAME}`, and VS Code also takes an `${input:...}` prompt), OpenCode uses single braces with no dollar sign (`{env:NAME}`), Codex reads the value from `bearer_token_env_var`, and Antigravity's `mcp_config.json` documents no substitution at all, so there the real token gets pasted in. Check your agent's guide before copying this line across, and create a token at [Account → MCP](https://reui.io/account/mcp).

### Install registry items

Install from the same namespace:

```

```

Free components continue to work with the authenticated config, so you do not need a second registry namespace.

## Monorepo (Turborepo)

Installing into a shared package such as `packages/ui` works the same way, with three things to get right. All three follow from one rule: the CLI reads `components.json` and `.env.local` from the directory it runs in, and looks nowhere else.

### Put components.json in the package, not the repo root

Every workspace needs its own `components.json`, and the `@reui` registry block belongs in the one you install from:

packages/ui/components.json

```

```

The aliases are what land the files in `packages/ui` instead of in an app.

### Keep the key next to it

The CLI loads `.env.local` from the directory it runs in and does not walk up to parent folders, so a key kept only in the repo root's `.env.local` is never seen. That is the `Registry "@reui" requires the following environment variables` error, and it stops before any request is made:

packages/ui/.env.local

```

```

### Run the install from the package

```

```

Or stay at the repo root and point the CLI at the package, which is what it suggests itself when run from there:

```

```

Running from a directory whose `components.json` has no `@reui` block (the root, or an app) is the other way this fails: the CLI falls back to shadcn's public directory entry for `@reui`, which carries no license header, so reui.io answers `You are not authorized to access the item` even though the key is set.

Consuming apps then import as usual, and need no license key of their own:

```

```

Prefer shadcn's own monorepo layout, where installs run from `apps/web`? That works too: put the same `registries` block and `.env.local` in `apps/web` instead. The `ui` alias still lands the files in `packages/ui`.

Passing the key from your shell or CI environment instead of the env file,
through a `turbo` task? Turborepo 2 defaults to strict environment mode, which
hands a task only the variables declared in `turbo.json`, so add
`REUI_LICENSE_KEY` to `globalEnv` there. With the key in
`packages/ui/.env.local` this does not apply: the CLI reads that file itself,
inside the task.

## Manual Copy & Paste

Prefer to browse and copy code directly instead of using the CLI?

- Browse premium sections in [Blocks](https://reui.io/blocks)
- Browse premium icon packs in [Icons](https://reui.io/icons)
- See plan details in [Pricing](https://reui.io/pricing)

## Related Guides

- [Get Started](https://reui.io/docs/get-started)
- [Registry](https://reui.io/docs/registry)

[Get Started](https://reui.io/docs/get-started) [Styling](https://reui.io/docs/styling)

On This Page

[Your License Key](https://reui.io/docs/license-setup#your-license-key) [CLI Installation](https://reui.io/docs/license-setup#cli-installation) [Prerequisites](https://reui.io/docs/license-setup#prerequisites) [Monorepo (Turborepo)](https://reui.io/docs/license-setup#monorepo-turborepo) [Manual Copy & Paste](https://reui.io/docs/license-setup#manual-copy--paste) [Related Guides](https://reui.io/docs/license-setup#related-guides)

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

[Skip to content](https://reui.io/docs/license-setup#main-content)

[ReUI home](https://reui.io/)

ProductsResources [Docs](https://reui.io/docs) [Support](https://reui.io/support) [Pricing](https://reui.io/pricing)

## Search

Search pages, docs, components, examples, blocks and templates

[Roadmap (has updates coming soon)](https://reui.io/roadmap) [X](https://x.com/reui_io) [Figma](https://www.figma.com/community/file/1649373313065184861/shadcn-ui-design-system-by-reui) [3.5K](https://github.com/keenthemes/reui)

[Sign in](https://reui.io/login?redirect=%2Fdocs%2Flicense-setup) [Get ProGet All-Access](https://reui.io/pricing)

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

# License Setup

Copy Markdown [Previous](https://reui.io/docs/get-started) [Next](https://reui.io/docs/styling)

Configure your ReUI license key for premium registry installs.

Use this guide when you want to install premium ReUI registry items with the shadcn CLI. Free components do not require a license key, but premium blocks, icons, and templates do.

## Your License Key

Use the license key from your ReUI account:

You need to be logged in to see your license key

[Login](https://reui.io/login?redirect=%2Fdocs%2Flicense-setup)

## CLI Installation

This is the recommended setup for premium registry installs.

### Prerequisites

- A React project with shadcn/ui initialized
- Node.js 18 or newer
- A `components.json` file in your project root

### Add your license key

Put it in `.env.local` in the root of your project:

```

```

### Point components.json at the @reui registry

Update `components.json` to use the authenticated `@reui` registry config:

```

```

The shadcn CLI expands `${REUI_LICENSE_KEY}` from your environment, so leave it as a variable here. A ReUI [MCP server](https://reui.io/docs/mcp) entry is a separate config with its own rules, and they differ by client: Claude Code expands `${VAR}` in `.mcp.json`, Cursor and VS Code only understand the `env:` prefix (`${env:NAME}`, and VS Code also takes an `${input:...}` prompt), OpenCode uses single braces with no dollar sign (`{env:NAME}`), Codex reads the value from `bearer_token_env_var`, and Antigravity's `mcp_config.json` documents no substitution at all, so there the real token gets pasted in. Check your agent's guide before copying this line across, and create a token at [Account → MCP](https://reui.io/account/mcp).

### Install registry items

Install from the same namespace:

```

```

Free components continue to work with the authenticated config, so you do not need a second registry namespace.

## Monorepo (Turborepo)

Installing into a shared package such as `packages/ui` works the same way, with three things to get right. All three follow from one rule: the CLI reads `components.json` and `.env.local` from the directory it runs in, and looks nowhere else.

### Put components.json in the package, not the repo root

Every workspace needs its own `components.json`, and the `@reui` registry block belongs in the one you install from:

packages/ui/components.json

```

```

The aliases are what land the files in `packages/ui` instead of in an app.

### Keep the key next to it

The CLI loads `.env.local` from the directory it runs in and does not walk up to parent folders, so a key kept only in the repo root's `.env.local` is never seen. That is the `Registry "@reui" requires the following environment variables` error, and it stops before any request is made:

packages/ui/.env.local

```

```

### Run the install from the package

```

```

Or stay at the repo root and point the CLI at the package, which is what it suggests itself when run from there:

```

```

Running from a directory whose `components.json` has no `@reui` block (the root, or an app) is the other way this fails: the CLI falls back to shadcn's public directory entry for `@reui`, which carries no license header, so reui.io answers `You are not authorized to access the item` even though the key is set.

Consuming apps then import as usual, and need no license key of their own:

```

```

Prefer shadcn's own monorepo layout, where installs run from `apps/web`? That works too: put the same `registries` block and `.env.local` in `apps/web` instead. The `ui` alias still lands the files in `packages/ui`.

Passing the key from your shell or CI environment instead of the env file,
through a `turbo` task? Turborepo 2 defaults to strict environment mode, which
hands a task only the variables declared in `turbo.json`, so add
`REUI_LICENSE_KEY` to `globalEnv` there. With the key in
`packages/ui/.env.local` this does not apply: the CLI reads that file itself,
inside the task.

## Manual Copy & Paste

Prefer to browse and copy code directly instead of using the CLI?

- Browse premium sections in [Blocks](https://reui.io/blocks)
- Browse premium icon packs in [Icons](https://reui.io/icons)
- See plan details in [Pricing](https://reui.io/pricing)

## Related Guides

- [Get Started](https://reui.io/docs/get-started)
- [Registry](https://reui.io/docs/registry)

[Get Started](https://reui.io/docs/get-started) [Styling](https://reui.io/docs/styling)

On This Page

[Your License Key](https://reui.io/docs/license-setup#your-license-key) [CLI Installation](https://reui.io/docs/license-setup#cli-installation) [Prerequisites](https://reui.io/docs/license-setup#prerequisites) [Monorepo (Turborepo)](https://reui.io/docs/license-setup#monorepo-turborepo) [Manual Copy & Paste](https://reui.io/docs/license-setup#manual-copy--paste) [Related Guides](https://reui.io/docs/license-setup#related-guides)

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