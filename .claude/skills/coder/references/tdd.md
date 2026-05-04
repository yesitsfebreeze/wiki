# Phase 4 — TDD with deep modules

**Symptom prevented:** AI ships large diffs, then "remembers" to type-check or test.
**Cause:** outrunning headlights (Pragmatic Programmer). Rate of feedback is the speed limit.

## RED → GREEN → Refactor

For each behavior in the PRD test plan:

### 4.1 RED — write failing test first

Write a test for behavior that doesn't exist yet. Run it — it MUST fail. A test that passes before implementation is testing nothing.

Test at the **interface boundary** identified in the PRD. Use canonical glossary terms in test names and assertions.

Test naming: descriptive — `process_should_return_error_when_input_empty()`.

### 4.2 GREEN — minimal implementation

Write the minimum code to make the test pass. No gold-plating. No "while I'm here" additions.

### 4.3 Refactor — deepen the module

Hide complexity, simplify the interface, reduce surface area. Run after green, before next RED.

## Mutation testing mindset

- Don't just assert success — assert specific values, counts, state changes.
- Test boundary conditions: if code checks `> 0`, test with 0, 1, and -1.
- Verify side effects: if a method updates multiple fields, assert ALL of them.
- One assertion per test when possible.
- If someone changed `>` to `>=` in your code, would a test catch it? If not, add one.

## Feedback loop after each cycle

Run the full feedback loop after every RED/GREEN/refactor cycle:

- Static types (TypeScript / Rust types).
- Automated tests.
- Where applicable: browser access (frontend), real database (integration).

For UI/frontend: start the dev server, exercise the feature in a browser. Type checking and test suites verify code correctness, not feature correctness.

## Deep modules, not shallow

Per Ousterhout (*A Philosophy of Software Design*):

- **Deep:** lots of functionality behind a simple interface; complexity hidden inside.
- **Shallow:** little functionality, complex interface; AI cannot navigate, dependencies leak.

Test at deep-module interfaces, **not** at every shallow function.

If the codebase already has shallow-module sprawl in the area you're touching, **stop and run phase 5 first** on the affected slice. AI cannot extend a shallow-module thicket correctly.

## Test strategy by complexity

| Change type | Test strategy |
|---|---|
| Single file fix, < 20 lines | Related test class only |
| Single file, 20-50 lines | Related tests + quick sanity |
| Multiple files, same feature | Feature test suite |
| Cross-cutting changes | All affected test modules |
| Database/schema changes | All affected test modules |
| Auth/security changes | All affected test modules |

## Commit cadence

Commit after each green-refactor cycle. See `commits.md` for full discipline (bounded scope, Conventional Commits format, when to write a body). Reference the PRD ID and any relevant decision IDs in the commit body when the *why* isn't obvious.

If tests fail:
1. Analyze the failure — don't guess.
2. Fix the root cause, not the symptom.
3. Re-run affected tests.
4. Repeat until 0 failures.

**Never commit with failing tests.**

## Stop condition

All PRD behaviors have passing tests at the right boundary. Code passes static types. No new shallow modules introduced.

## Anti-pattern

Big-batch implementation followed by "let me add tests now." That is implementation-driven development with a test suffix.
