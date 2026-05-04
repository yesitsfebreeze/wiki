# Phase 6 — Adversarial pre-commit review

**Goal:** final quality gate before commit.

## Self-review checklist

- [ ] All acceptance criteria addressed.
- [ ] No hard-coded values that should be constants.
- [ ] No assumptions made without verification.
- [ ] All edge cases handled.
- [ ] Error handling complete (only at boundaries — see KISS principles).
- [ ] No security vulnerabilities (injection, XSS, SQL injection, OWASP top 10).
- [ ] Tests cover new functionality.
- [ ] Appropriate test suite passes.
- [ ] Documentation updated.
- [ ] Code follows existing patterns.
- [ ] No half-finished implementations.
- [ ] No speculative abstractions or feature flags.
- [ ] No comments narrating *what* the code does (only *why* when non-obvious).

## Final adversarial questions

- What happens if this runs twice concurrently?
- What if input is null/empty/negative/huge?
- Did I check for race conditions?
- What assumptions am I making that could be wrong?
- If I were trying to break this, how would I?
- Would I be embarrassed if this broke in production?

## TOCTOU prevention (Time-of-Check to Time-of-Use)

```
WRONG: state can change between check and use
read state → [gap where another process can modify] → act on stale state

CORRECT: atomic check-and-act
lock → read state → act → unlock
```

Applies to any shared mutable state: databases, files, caches, APIs.

## Transaction side-effect awareness

When code throws inside a transaction, ALL changes in that transaction are rolled back. If error-handling state (marking something as failed, creating audit records) must persist despite the exception, it must happen **outside** the transaction.

## Shared-state documentation

Before implementing changes to shared mutable state:

1. All actors/methods that can modify this data.
2. All concurrent scenarios.
3. Invariants that must ALWAYS hold.
4. Locking/coordination strategy.

## Implementation rules

- Use existing abstractions — don't reinvent what the codebase provides.
- Use existing constants/enums/configuration — never hard-code values.
- Never skip input validation at boundaries.
- Use the project's established patterns for logging, error handling, state management.
- Trust internal callers — don't re-validate inside the system.

## Stop condition

All checks pass. Ready to commit.

## Anti-pattern

Treating bugs as one-off symptoms. Every bug is a symptom — find the disease. What systemic issue allowed it? Where else does this pattern appear?
