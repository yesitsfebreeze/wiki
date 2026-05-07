---
name: loader
description: Index of available skills — file locations, purpose, when/how to use.
tags:
  - skill
  - index
  - reference
---

# Loader

Skill index. Lists each skill: path, purpose, trigger.

## Skills

### coder
- **Path:** `.claude/skills/coder/SKILL.md`
- **Purpose:** Architect mode. Bundles KISS/DRY/YAGNI, phased planning (orient → align → glossary → PRD → TDD → design+delegate), adversarial self-review, bounded-commit discipline.
- **When:** Non-trivial feature, refactor, multi-file change, bug fix touching >1 module, new architectural component. Skip for one-line fixes, throwaway spikes, exploratory reads.
- **How:** `Skill(skill="coder")`. Prefix first response with `## [CODER MODE]`.

### rust-best-practices
- **Path:** `.claude/skills/rust-best-practices/SKILL.md`
- **Purpose:** Apollo GraphQL Rust idioms — borrowing vs cloning, error handling (`thiserror`/`anyhow`), ownership, clippy lints, type-state pattern.
- **When:** Writing, reviewing, or refactoring Rust code.
- **How:** `Skill(skill="rust-best-practices")`. Read referenced chapters on demand.

### caveman
- **Path:** `.claude/skills/caveman/SKILL.md`
- **Purpose:** Terse output mode. ~75% token cut. Drops articles, filler, hedging. Code/commits/security stay normal.
- **When:** User says "caveman", "be brief", "less tokens", or `/caveman lite|full|ultra`. Auto-active per session hook.
- **How:** `Skill(skill="caveman")`. Default level: `full`.

### learn (wiki plugin)
- **Path:** `.claude/plugins/cache/yesitsfebreeze/wiki/<version>/skills/learn/SKILL.md`
- **Purpose:** Sole entry to wiki MCP — ingest, link, dedupe, Q&A, conclusions, query. Densify pass + research-to-thoughts flow.
- **When:** Task involves persistent wiki built from raw sources. Replaces deprecated `/wiki`.
- **How:** `Skill(skill="wiki:learn")`. First action inside skill: `docs({name: "learn"})`.

## Big-picture docs

Read `docs/*.md` for project overview before non-trivial work.
