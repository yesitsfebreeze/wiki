# Principles — KISS, DRY, YAGNI

Three rules for keeping code small and changeable.

## KISS — Keep It Simple, Stupid

The simplest design that works is the right one. Short functions, shallow nesting, plain control flow.

## DRY — Don't Repeat Yourself

> Every piece of knowledge must have a single, unambiguous, authoritative representation within a system.
> — Hunt & Thomas, *The Pragmatic Programmer*, 1999.

DRY is about **knowledge**, not character-level duplication. Two functions with the same shape but different reasons to change are not DRY violations.

## YAGNI — You Aren't Gonna Need It

Don't build a capability until it's actually required. The four costs of speculative features (Fowler, 2015):

1. **Build** — time spent on the unused capability.
2. **Delay** — work it pushed back.
3. **Carry** — code mass slowing every future change.
4. **Repair** — fixing it when it turns out wrong (⅔ of speculative features do — Kohavi et al.).

## Rules of thumb

- **Rule of three.** First time: write it. Second time: notice. Third time: extract.
- **Imagine the refactoring.** Before adding a hook for a future feature, picture the diff to introduce it later. Usually no worse than doing it now.
- **Early returns over nested else.** Guard clauses flatten code.
- **Composition over inheritance.** Inheritance couples; composition combines.
- **Boundaries validate, internals trust.** Validate at edges (user input, external APIs). Don't re-validate inside.
- **Three similar lines beat a premature abstraction.** Wait for the third occurrence.
- **No half-finished implementations.** Either ship the slice or don't start it.

## What not to add

- Error handling for cases that can't happen.
- Fallbacks for scenarios outside system boundaries.
- Validation of values from internal callers.
- Feature flags / backwards-compat shims when you can change the code directly.
- Comments explaining *what* well-named code already says.
- References to current task/PR/issue inside code comments — those rot.

## What to add

- A comment when the *why* is non-obvious: hidden constraint, subtle invariant, workaround for a specific bug, behavior that would surprise a reader.

If removing a comment wouldn't confuse a future reader, don't write it.
