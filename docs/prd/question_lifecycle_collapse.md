# PRD: Question Lifecycle Collapse

## Goal

Collapse question lifecycle from 4 states to 2:

- **open** — exists in `questions/<purpose>/`, no resolution tag
- **graveyard** — exists in `questions/graveyard/<purpose>/`, junk/unanswerable, excluded from raise/qa/list

Anything else: hard delete. "Answered" = conclusion exists; the question file is gone.

Reexplore = `rm -rf questions/graveyard/`.

## Motivation

Current model has redundant state:

- Tags `answered` + `dropped` carry status
- Filesystem mirrors `questions/answered/<purpose>/` + `questions/dropped/<purpose>/`
- Conclusion docs *also* exist as the actual answer record, linked via `Answers` edge

Result: `answered`-tagged question + conclusion doc duplicate the same fact. `dropped` keeps junk indexed forever. Three places to filter, three places to migrate, three places to drift.

## Non-goals

- Preserve historical "we tried this" trail for deleted questions. (Conclusion is the only durable artifact. If user wants audit, write it into the conclusion body.)
- Preserve `answered/` mirror folder. Gone.
- Soft-delete or tombstone for answered questions. Hard delete only.

## Surface changes

### MCP tool: `mark_question`

Before: `status: "answered" | "dropped"` (tag-based).
After: `status: "deleted" | "buried"`.

- `deleted` → hard-delete file + edge cleanup. Use when question is junk, dupe, or already answered without needing further conclusion.
- `buried` → move to `questions/graveyard/<purpose>/`, no tag mutation. File preserved, excluded from raise/qa/list/search.

Idempotent: deleting non-existent = no-op true. Burying already-buried = no-op true.

### Auto-promote (learn_pass)

`learn::promote::promote_to_conclusion` currently:
1. Creates conclusion + Answers edges
2. Tags question `answered` (+ `synthesized` if support-floor path)
3. Moves question to `answered/`

After:
1. Creates conclusion + Answers edges (unchanged)
2. **Hard-delete the question file**
3. Drop `synthesized` tag entirely (was only used to mark which path; if needed, encode on conclusion instead)

Risk: question body context lost. Mitigation: conclusion body should restate the question; existing `synthesize_conclusion_body` already does this. Verify before merge.

### `mark_question_answered` helper (`tools.rs:72`)

Used when an `Answers` reason is ingested mid-flow (e.g. `[[<qid>]]` body-start). Currently flips tag + moves to `answered/`. After: hard-delete the question file.

Rename → `delete_question_on_answer`. Same call sites.

### Filters

Drop everywhere — not needed since deleted files don't exist:

- `tools.rs:970` — `list_open_questions` filter
- `tools.rs:1624` — list helper filter
- `learn/qa.rs:124, 339-341, 386` — open-set filters
- `learn/raise.rs:20-22, 190-192` — raise dedup against answered set

Keep: filter for `graveyard/` path prefix in raise + qa + list + search.

### Filesystem

- `questions/answered/` — migration removes. New code never writes.
- `questions/dropped/` — migration removes (delete contents per user decision).
- `questions/graveyard/` — new. `move_question_to(root, qid, "graveyard")` replaces `move_to_dropped`.
- `move_to_answered` — delete function entirely.

### Cache / tag index

Drop `tag_index_lookup(root, "answered")` + `"dropped"` callsites.

## Migration (one-shot, on next ingest or via `author` tool)

1. For every question file under `questions/answered/**` → **hard delete**. (Conclusion already exists with edge; question is redundant.)
2. For every question file under `questions/dropped/**` → **hard delete**. (Per user: existing dropped not worth preserving.)
3. For every question file with `answered` tag still in main tree (shouldn't happen post-promote, but defensive) → **hard delete**.
4. For every question file with `dropped` tag still in main tree → **hard delete**.
5. Rebuild tag index + cache.

Migration runs once, gated by version field in `.wiki/migrations.json`. Idempotent.

## Schema

No frontmatter changes. State entirely encoded by:
- file existence (open vs deleted)
- file path prefix (`questions/graveyard/...` vs main tree)

Drop tag values `answered`, `dropped`, `synthesized` from any question doc on ingest validation.

## Edge cases

1. **Promote runs, conclusion creation succeeds, question delete fails (FS error).** Order: create conclusion → delete question. If delete fails, log + leave question. Next learn_pass will see question + conclusion edge → re-attempt delete (new helper: `cleanup_resolved_questions`).
2. **Inbound wikilinks to deleted question.** Currently `rewrite_inbound_links` repoints. After delete, links become dangling. Decision: rewrite to point at the resulting conclusion id (already linked via Answers edge). Add to `promote_to_conclusion` post-step.
3. **Buried question gets new evidence.** User unburies manually (move file out of `graveyard/`). No API for this — explicit FS action signals intent.
4. **Concurrent ingest + migration.** Migration takes file lock on `.wiki/`. Same pattern as existing cache rebuild.

## Out-of-scope follow-ups

- Auto-suggest burial for low-signal old questions (heuristic).
- Conclusion-to-question reverse navigation UI.
- Graveyard search command (`include_graveyard: true` in search params).

## Test plan

1. **Unit**: `mark_question({status: "deleted"})` removes file + edge.
2. **Unit**: `mark_question({status: "buried"})` moves to `graveyard/<purpose>/`.
3. **Unit**: `promote_to_conclusion` creates conclusion → deletes question → repoints inbound links.
4. **Unit**: `learn_pass` open-question filter excludes graveyard, includes everything else.
5. **Integration**: full migration on fixture with mixed `answered/`, `dropped/`, tagged-but-not-moved questions → all gone, conclusions intact.
6. **Regression**: existing `body_start_wikilink_to_question_emits_supports_no_automark` test must pass — body-start wikilink to question still creates Supports edge (no auto-delete on Supports, only on Answers).

## Rollout

Single PR, single migration. No feature flag — versioned migration is enough. Bump minor: `1.23.0`.

## Risk

- **Data loss**: deleting `dropped/` content per user request — if any contained signal, gone forever. Accepted.
- **Conclusion-without-context**: if `synthesize_conclusion_body` doesn't restate question text adequately, deleted question = lost rationale. Audit `promote.rs::synthesize_conclusion_body` before merge.
- **Inbound link repointing**: must run before delete, in same transaction-ish flow, else dangling links.

## Decision points (locked)

| # | Question | Answer |
|---|----------|--------|
| 1 | Audit trail on delete? | No. `graveyard/` for unanswerable, hard delete for answered. |
| 2 | Keep `answered/` mirror? | No. Drop. |
| 3 | Migrate existing `answered`-tagged? | Hard delete. |
| 4 | Migrate existing `dropped/`? | Hard delete. |
