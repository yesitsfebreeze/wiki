---
name: loader
description: loads all necessary skills at the start of a session.
tags:
  - skill
  - streaming
  - code
  - truth
  - ui
  - agent
---

**Invoke `/coder` immediately** at the start of the session via the Skill tool (`Skill(skill="coder")`).
   - File path: `.claude/skills/coder/SKILL.md`.
   - Single architect mode. Bundles KISS/DRY/YAGNI, phased planning (orient → align → glossary → PRD → TDD → design+delegate), adversarial self-review, bounded-commit discipline.
   - Prefix the first response with `## [CODER MODE]` per the coder skill's contract.

**Invoke `/rust-best-practices` immediately** at the start of the session via the Skill tool (`Skill(skill="rust-best-practices")`).
   - File path: `.claude/skills/rust-best-practices/SKILL.md`.
   - All Rust code must follow these idioms — borrowing, error handling, ownership.

**Invoke `/caveman` immediately** at the start of the session via the Skill tool (`Skill(skill="caveman")`).
   - File path: `.claude/skills/caveman/SKILL.md`.
   - Terse mode for all narrative output. Code/commits/security messages stay normal.

**Invoke `/learn` immediately** when task involves persistent wiki built from raw sources. `/learn` is sole entry: ingest, link, dedupe, Q&A, conclusions, query.
   - File path: `skills/learn/SKILL.md`.

**READ docs/*.md** for the big picture before starting.
