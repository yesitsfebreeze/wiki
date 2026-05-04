# Phase 7 — Incremental pushes

**Goal:** keep remote in lockstep with local. Don't hoard commits.

Push is the upstream counterpart to bounded commits (`commits.md`). Same discipline: small, frequent, traceable.

## Cadence

Push after every meaningful checkpoint:

- Each green-refactor cycle that completes a coherent slice.
- Before stepping away (lunch, end of day, context switch).
- Before any operation that could lose work (rebase, branch surgery, machine reboot).

Rule: **if your local is more than ~5 commits ahead of origin, push.**

## Why incremental

- **Backup.** Local-only work is one disk failure from gone.
- **CI early signal.** Each push exercises the pipeline — catch breakage on the commit that caused it, not five commits later.
- **Visible progress.** Reviewers and collaborators see direction without waiting for a "done" moment.
- **Bisect actually works on origin.** Mega-pushes collapse history reviewers care about into one merge.
- **Rollback granularity.** Reverting one pushed commit is cheap; reverting a 30-commit dump is surgery.

## What not to do

- **Hoard until "feature done"** — defeats backup, CI, and visibility.
- **Push and force-push casually** — destructive on shared branches.
- **Push broken/half-typed code without marking it** — others may pull. If WIP, push to a clearly-named branch (`wip/<topic>` or feature branch with `[WIP]` in PR title).
- **Mix branches in one push session** — one branch, one push, one outcome to verify.

## Force-push rules

Never force-push to `main`/`master` or any shared branch. Confirm with user before any force-push.

Force-push acceptable on your own feature branch when:
- Rewriting history before review (squashing typos, rewording subjects).
- Rebasing onto updated base.

Use `--force-with-lease`, never `--force` blind. Lease aborts if someone else pushed in between, preventing silent overwrites.

## After each push

- Watch CI status. If a push breaks the pipeline, fix forward in a new commit, don't amend-and-force.
- If a finding appears in code review, address it as a fresh commit on the same branch.

## Stop condition

Local and remote agree. CI green on the latest pushed commit. No work older than the last push exists only on disk.

## Anti-patterns

- **Push-once-at-end.** Defeats every purpose of remote.
- **Silent force-push to shared branch.** Overwrites others' work.
- **Pushing without watching CI.** Pipeline failures compound silently.
- **Pushing broken `main`.** If you must push WIP, use a topic branch.
