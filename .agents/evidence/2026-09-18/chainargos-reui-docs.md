[Skip to content](https://reui.io/docs#main-content)

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

# Introduction

Copy Markdown [Next](https://reui.io/docs/get-started)

A first class Shadcn registry of components, examples, blocks, icons, and templates, designed for developers and AI agents.

ReUI is a first class [Shadcn](https://ui.shadcn.com/) registry offering a collection of open source components and blocks built for shadcn and compatible with [Base UI](https://base-ui.com/), [Radix UI](https://www.radix-ui.com/), and [Tailwind CSS](https://tailwindcss.com/). Designed for developers and AI. The same catalog that developers browse and install is served to AI agents through the [MCP server](https://reui.io/docs/mcp) and [Agent Skills](https://reui.io/docs/agent-skills), so the code that lands in your project is the same whether a person or an agent puts it there.

## Vision

With over 15 years of experience in UI/UX design and full-stack development for startups and enterprises, we build interfaces that are both visually refined and highly functional. We believe great UI should be intuitive to use, easy to understand, and simple to maintain at scale - and that the same standard should hold in AI-driven workflows, where agents need code they can read, trust, and reuse.

## Approach

ReUI organizes the catalog as a ladder of abstraction, so you can enter at the level that matches the task:

- **[Components](https://reui.io/components)** \- 22 free ReUI primitives such as data-grid, event-calendar, gantt, cascader, kanban, filters, date-selector, stepper, icon-tile, and tree: the parts you compose yourself.
- **Examples** \- 1,105 free open source component examples that show each primitive configured for a real use case. Copy one, adapt it, move on.
- **[Blocks](https://reui.io/blocks)** \- 543 Pro blocks across 6 groups (Application, Data Grid, Solutions, eCommerce, Marketing, and AI & Agents): complete sections you drop into a page.
- **[Templates](https://reui.io/templates)** \- multi-page templates assembled entirely from blocks, for when you need a whole product surface rather than a section.

Alongside the ladder, [Icons](https://reui.io/icons) add 638 icons in 4 styles (outline, solid, duotone, and filled) that match the rest of the catalog.

The levels share one foundation: blocks compose the same primitives, and templates are assembled from blocks. Start high for speed, drop down for control. Components and examples are free; blocks, icons, and templates belong to the paid plans.

## Foundation

Every component, example, and block ships in two versions, one built on [Base UI](https://base-ui.com/) and one on [Radix UI](https://www.radix-ui.com/), so ReUI fits new projects and existing codebases alike. Everything is styled with Tailwind CSS v4 and available in multiple styles.

## Agent optimized

Agents work best with real information, not guesses. The free [MCP server](https://reui.io/docs/mcp) at [mcp.reui.io](https://mcp.reui.io/) works with any MCP-capable agent (Claude, Codex, Cursor, VS Code, Zed, OpenCode, Lovable, Replit, v0) after a quick account sign-in, and exposes 19 tools covering scored search, real component APIs, page composition, and prop validation. Free [Agent Skills](https://reui.io/docs/agent-skills) teach the ReUI workflow on top: find the right item, install it with the shadcn CLI, read the actual API, and adapt by reuse. Instead of guessing at props from training data, your agent works against the real registry.

## Next Steps

Head over to the [Get Started](https://reui.io/docs/get-started) guide to add ReUI to your project, or connect your agent through the [MCP server](https://reui.io/docs/mcp).

[Get Started](https://reui.io/docs/get-started)

On This Page

[Vision](https://reui.io/docs#vision) [Approach](https://reui.io/docs#approach) [Foundation](https://reui.io/docs#foundation) [Agent optimized](https://reui.io/docs#agent-optimized) [Next Steps](https://reui.io/docs#next-steps)

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

[Skip to content](https://reui.io/docs#main-content)

[ReUI home](https://reui.io/)

ProductsResources [Docs](https://reui.io/docs) [Support](https://reui.io/support) [Pricing](https://reui.io/pricing)

## Search

Search pages, docs, components, examples, blocks and templates

[Roadmap (has updates coming soon)](https://reui.io/roadmap) [X](https://x.com/reui_io) [Figma](https://www.figma.com/community/file/1649373313065184861/shadcn-ui-design-system-by-reui) [3.5K](https://github.com/keenthemes/reui)

[Sign in](https://reui.io/login?redirect=%2Fdocs) [Get ProGet All-Access](https://reui.io/pricing)

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

# Introduction

Copy Markdown [Next](https://reui.io/docs/get-started)

A first class Shadcn registry of components, examples, blocks, icons, and templates, designed for developers and AI agents.

ReUI is a first class [Shadcn](https://ui.shadcn.com/) registry offering a collection of open source components and blocks built for shadcn and compatible with [Base UI](https://base-ui.com/), [Radix UI](https://www.radix-ui.com/), and [Tailwind CSS](https://tailwindcss.com/). Designed for developers and AI. The same catalog that developers browse and install is served to AI agents through the [MCP server](https://reui.io/docs/mcp) and [Agent Skills](https://reui.io/docs/agent-skills), so the code that lands in your project is the same whether a person or an agent puts it there.

## Vision

With over 15 years of experience in UI/UX design and full-stack development for startups and enterprises, we build interfaces that are both visually refined and highly functional. We believe great UI should be intuitive to use, easy to understand, and simple to maintain at scale - and that the same standard should hold in AI-driven workflows, where agents need code they can read, trust, and reuse.

## Approach

ReUI organizes the catalog as a ladder of abstraction, so you can enter at the level that matches the task:

- **[Components](https://reui.io/components)** \- 22 free ReUI primitives such as data-grid, event-calendar, gantt, cascader, kanban, filters, date-selector, stepper, icon-tile, and tree: the parts you compose yourself.
- **Examples** \- 1,105 free open source component examples that show each primitive configured for a real use case. Copy one, adapt it, move on.
- **[Blocks](https://reui.io/blocks)** \- 543 Pro blocks across 6 groups (Application, Data Grid, Solutions, eCommerce, Marketing, and AI & Agents): complete sections you drop into a page.
- **[Templates](https://reui.io/templates)** \- multi-page templates assembled entirely from blocks, for when you need a whole product surface rather than a section.

Alongside the ladder, [Icons](https://reui.io/icons) add 638 icons in 4 styles (outline, solid, duotone, and filled) that match the rest of the catalog.

The levels share one foundation: blocks compose the same primitives, and templates are assembled from blocks. Start high for speed, drop down for control. Components and examples are free; blocks, icons, and templates belong to the paid plans.

## Foundation

Every component, example, and block ships in two versions, one built on [Base UI](https://base-ui.com/) and one on [Radix UI](https://www.radix-ui.com/), so ReUI fits new projects and existing codebases alike. Everything is styled with Tailwind CSS v4 and available in multiple styles.

## Agent optimized

Agents work best with real information, not guesses. The free [MCP server](https://reui.io/docs/mcp) at [mcp.reui.io](https://mcp.reui.io/) works with any MCP-capable agent (Claude, Codex, Cursor, VS Code, Zed, OpenCode, Lovable, Replit, v0) after a quick account sign-in, and exposes 19 tools covering scored search, real component APIs, page composition, and prop validation. Free [Agent Skills](https://reui.io/docs/agent-skills) teach the ReUI workflow on top: find the right item, install it with the shadcn CLI, read the actual API, and adapt by reuse. Instead of guessing at props from training data, your agent works against the real registry.

## Next Steps

Head over to the [Get Started](https://reui.io/docs/get-started) guide to add ReUI to your project, or connect your agent through the [MCP server](https://reui.io/docs/mcp).

[Get Started](https://reui.io/docs/get-started)

On This Page

[Vision](https://reui.io/docs#vision) [Approach](https://reui.io/docs#approach) [Foundation](https://reui.io/docs#foundation) [Agent optimized](https://reui.io/docs#agent-optimized) [Next Steps](https://reui.io/docs#next-steps)

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