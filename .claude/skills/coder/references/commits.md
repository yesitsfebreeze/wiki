# Commits — incremental, bounded, well-described

Commits are the unit of review and rollback. Make them small, self-contained, and explained.

## Bounded

One commit = one logical change. If the description needs the word "and", split it.

- Test + implementation for one behavior → one commit.
- Refactor that enables a feature → separate commit before the feature.
- Formatting / rename sweeps → their own commit, never mixed with logic.
- Dependency bumps → isolated.

A commit a reviewer cannot understand on its own is too big.

## Incremental

Commit after each green-refactor cycle. Don't batch a day's work into one diff.

Benefits:
- Bisect finds the exact breaking change.
- Revert is surgical — one commit, not a week.
- Review fits in working memory.
- You stay inside the feedback loop instead of outrunning headlights.

Rule: if you can't explain what changed in one sentence, the commit is too big.

## Well-described

Format: Conventional Commits.

```
<type>(<scope>): <subject>

<body — why, not what>

<footer — refs, breaking changes>
```

- **Subject** ≤50 chars. Imperative mood ("add", "fix", not "added", "fixes"). No trailing period.
- **Body** only when *why* isn't obvious from the diff. Explain motivation, constraint, or non-obvious tradeoff. Wrap at 72 chars.
- **Footer** for issue refs (`Refs: #123`), breaking changes (`BREAKING CHANGE: ...`), or co-authors.

Types: `feat`, `fix`, `refactor`, `perf`, `test`, `docs`, `chore`, `build`, `ci`, `style`.

## Subject discipline

Bad → good:

- `update stuff` → `fix(auth): reject expired tokens at boundary`
- `wip` → `refactor(walk): extract path-resolver into deep module`
- `more tests` → `test(walk): cover symlink loop edge case`
- `fixed bug` → `fix(cache): clear entry on TTL exceed, not on access`

## Body discipline

Skip when the subject + diff are self-explanatory (renames, obvious fixes, additive tests).

Write when:
- The *why* isn't in the code.
- A non-obvious tradeoff was made.
- A workaround for a specific bug exists.
- A constraint from outside the repo (RFC, vendor quirk, deadline) shaped the choice.

Don't write:
- Restating what the diff already shows.
- Narrating the session ("I tried X, then Y, then settled on Z").
- References to the current task that will rot ("for ticket ABC-42 demo on Tuesday").

## Anti-patterns

- **Mega-commit at end of day.** Loses bisect granularity, blocks review.
- **Mixed concerns.** "fix + reformat + rename" hides the fix.
- **Refactor inside feature commit.** Reviewer can't tell what's behavior change vs. shape change.
- **Empty or vague subjects.** `wip`, `update`, `fix stuff`, `changes` — useless in `git log`.
- **Body that paraphrases the diff.** Describe the *why*, the diff already shows *what*.
- **Skipping commit between green and refactor.** Lose the "known-good" checkpoint.

## When to amend vs. new commit

Amend only the most recent commit, only before pushing, only when the change belongs to the same logical unit (typo, missed file). Never amend pushed commits unless the user explicitly asks.

Otherwise: new commit. `fix:` after `feat:` is honest history.
