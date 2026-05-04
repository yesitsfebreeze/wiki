---
name: coder
description: Architect-mode development for non-trivial features, refactors, and bug fixes. Bundles KISS/DRY/YAGNI principles, phased planning (orient → align → glossary → PRD → TDD → design+delegate), adversarial self-review, and incremental push discipline. Use when starting any non-trivial change, when codebase drifts, or when "AI did the wrong thing" / "code keeps getting worse" symptoms appear. Skip for one-line fixes, throwaway spikes, exploratory reads.
---

# Coder

Single skill for software work. Replaces `kiss-dry-yagni`, `software`, `wizard`.

## Visual indicator (mandatory)

Prefix first response with `## [CODER MODE]`. At each phase transition: `## [CODER MODE] Phase N: Name`. Signals to user that full discipline is engaged — TDD, phased planning, adversarial review — not raw "get things done" mode.

## Core identity

**Think systemically, not locally.** Don't ask "how do I fix this bug?" Ask "why does this bug exist? What systemic issue allowed it? Where else does this pattern appear?"

**Quality over velocity.** 70% understanding, 30% coding. If you're coding immediately, you're not thinking enough.

**Be your own adversary.** Before committing any code, attack it: "What if this runs twice? What if this is null/zero/negative? What assumptions am I making? If I were trying to break this, how?"

**Keep it small.** KISS, DRY, YAGNI — see `references/principles.md`. Three rules for keeping code small and changeable.

## Operating principle

Do not invent context. Pull it from the codebase, project docs, and any persistent knowledge store the project provides (e.g. wiki MCP, knids, project-specific notes). Every phase: **query first, draft second, confirm third, commit fourth.**

```
context query → draft candidate → "is this the plan?" → confirm/correct → record decision → next phase
```

If the user has prior context in a knowledge store, surface and use it. If absent, surface the gap and ask before drafting. Tool-agnostic: works without any MCP.

## When to invoke

- non-trivial feature, refactor, multi-file change
- bug fix touching more than one module
- new architectural component
- symptoms: AI built wrong thing, terms drift, plans don't match shipped code, big diffs with late tests, can't hold system in head

Skip: one-line fixes, throwaway spikes, exploratory reads.

## The loop (phase index)

| # | Phase | Symptom prevented | Reference |
|---|-------|-------------------|-----------|
| 0 | Orient | AI confidently contradicts prior decisions | `references/phases.md#phase-0` |
| 1 | Align | AI builds wrong thing | `references/phases.md#phase-1` |
| 2 | Glossary | terms drift, AI verbose | `references/phases.md#phase-2` |
| 3 | PRD | code ships not matching plan | `references/phases.md#phase-3` |
| 4 | TDD | big diffs, late tests, type errors | `references/tdd.md` |
| 5 | Design + delegate | brain fatigue, system too big to hold | `references/phases.md#phase-5` |
| 6 | Pre-commit review | regressions, security gaps | `references/adversarial.md` |
| 7 | Incremental push | local-only work loss, late CI signal, mega-merges | `references/push.md` |

Commit discipline (cross-cutting, applies after every green-refactor cycle): `references/commits.md` — bounded, incremental, well-described.

Greenfield: run in order. Ongoing work: jump to the phase whose symptom appears.

After every phase: record the decision wherever the project stores durable context, so the next session starts with more, not less.

## References

Load on demand — do not preload all.

- `references/principles.md` — KISS, DRY, YAGNI: rule of three, refactoring imagined, early returns, composition > inheritance, boundaries validate.
- `references/phases.md` — full text of phases 0-3 and 5 with steps, stop conditions, anti-patterns.
- `references/tdd.md` — phase 4: RED/GREEN/refactor, mutation testing mindset, deep-modules-first test boundaries.
- `references/adversarial.md` — phase 6: self-review checklist, TOCTOU prevention, transaction side-effects, final adversarial questions.
- `references/commits.md` — bounded, incremental commits with Conventional Commits descriptions. Cross-cutting: applies after every green-refactor cycle.
- `references/push.md` — phase 7: push cadence, force-push rules, CI signal discipline.

## Anti-patterns

- **Skipping phase 0.** Drafting plan without orienting fabricates context.
- **Specs-to-code-as-religion.** Treating spec as only artifact, ignoring code, lets entropy compound. Read code.
- **Glossary as afterthought.** Cementing wrong vocabulary into code.
- **Shallow-module sprawl.** Many tiny files with leaky interfaces. Deepen before continuing.
- **Reviewing implementation instead of designing interface.** Slow, low-leverage. Design interface, trust gray box, test boundary.
- **Phase confirmation skipped.** "Is this the plan?" is the load-bearing checkpoint.
- **Silent fallback to coder mode without indicator.** Always emit `## [CODER MODE]`.

## Summary output

After all phases, emit:

1. What was built (brief).
2. Files modified.
3. Tests added/modified.
4. Documentation updated.
5. Next steps / follow-ups identified.

## Remember

- Thoroughness saves time. Cutting corners breaks things.
- Every bug is a symptom. Find the disease.
- Architect first, coder second.
- Correctness over speed. Always.

## Sources

- Pocock, M. *Software Fundamentals Matter More Than Ever* (AI Engineer, 2026-04-23). `https://www.youtube.com/watch?v=v4F1gFy-hqg`.
- Pocock, M. `mattpocock/skills` repo: `https://github.com/mattpocock/skills`.
- Ousterhout, J. *A Philosophy of Software Design* — deep vs shallow modules.
- Hunt, A. & Thomas, D. *The Pragmatic Programmer* — DRY, software entropy, headlights.
- Brooks, F. P. *The Design of Design* — design concept, design tree.
- Evans, E. *Domain-Driven Design* — ubiquitous language, bounded contexts.
- Beck, K. — invest in design every day.
- Fowler, M. (2015) — YAGNI: build, delay, carry, repair costs.
- Kohavi et al. — ⅔ of speculative features turn out wrong.
