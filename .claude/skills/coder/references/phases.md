# Phases 0-3, 5

Phase 4 (TDD) → `tdd.md`. Phase 6 (adversarial) → `adversarial.md`. Phase 7 (PR gate) → `pr-gate.md`.

Tool-agnostic. If the project has a persistent knowledge store (wiki MCP, knids, project notes), use it. If not, the codebase + project docs + conversation are the substrate. Adapt query/ingest steps to whatever persistence the project provides.

---

## Phase 0 — Orient

**Goal:** establish what's already known about the topic before drafting anything.

**Steps:**

1. Identify the persistence layer the project uses (knowledge store MCP, `docs/` tree, design notes, ADRs, prior PRs/issues, none).
2. Query it with 2-3 phrasings of the user's intent. Read the highest-confidence and most-recent items in full.
3. Search the codebase for prior implementations of the same concept.
4. Identify which prior decisions are load-bearing for this work.
5. Surface the orientation summary inline: "Project knows X about this. Highest-signal sources: A, B, C. Open questions: D, E."

**Stop condition:** you can describe what's already known and what isn't. User confirms or corrects framing.

If the project has nothing on the topic, say so. Do not fabricate context.

---

## Phase 1 — Align: design concept

**Symptom prevented:** AI builds the wrong thing.
**Cause:** no shared *design concept* (Brooks). The invisible theory of what is being built.

**Steps:**

1. From phase 0, draft a candidate design concept: what's being built, why, scope, non-goals, load-bearing constraints. Cite sources for each claim.
2. Walk down the design tree (Brooks): list each major decision and its dependencies. Propose a recommended answer for each, citing prior decisions where one exists.
3. Surface to user: **"Is this the plan? Is this how you thought it would lay out?"**
4. Resolve disagreements one branch at a time. Don't move on with unresolved branches.
5. If a question can be answered by reading the codebase, read it instead of asking.
6. On convergence, record the design concept (commit a design note, ingest into knowledge store, update issue, whichever the project uses).

**Stop condition:** user and AI describe the same thing in different words, including boundaries and non-goals. Decision tree resolved. Design concept recorded.

**Anti-pattern:** jumping to PRD before user confirms the design concept matches their mental model.

---

## Phase 2 — Glossary: ubiquitous language

**Symptom prevented:** AI is verbose, terms drift, same word means different things.
**Cause:** no shared vocabulary (Evans, *DDD*).

**Steps:**

1. Query the project for prior glossary or vocabulary documents.
2. Scan phase-0 + phase-1 conversation for domain nouns, verbs, concepts.
3. Identify problems:
   - Same word for different concepts (ambiguity) — flag.
   - Different words for same concept (synonyms) — pick canonical.
   - Vague or overloaded terms — sharpen.
4. Draft `UBIQUITOUS_LANGUAGE.md` (or update existing) with markdown tables grouped by topic:

   ```md
   ## <topic>
   | Term | Definition | Aliases to avoid |
   |------|------------|------------------|
   | **CanonicalTerm** | one-line definition | synonym1, synonym2 |
   ```

5. Surface to user: **"These are the canonical terms. Anything missing or wrong?"**
6. On confirmation, write the file and record each term in the project's persistence layer.
7. Reference the glossary from phase 3 onward — PRDs, prompts, code review.

**Stop condition:** every load-bearing term has one canonical definition. File written.

**Anti-pattern:** glossary as afterthought. If deferred past phase 3, wrong vocabulary cements into code.

---

## Phase 3 — PRD: modules, interfaces, tests

**Symptom prevented:** code ships but doesn't match what was discussed.
**Cause:** no module-level plan; AI invents structure that drifts from design concept.

**Steps:**

1. Query the project for related architecture notes.
2. Read the relevant code paths — confirm current state, do not assume.
3. Draft a PRD with:
   - **Goal** — one sentence.
   - **Non-goals** — what stays out.
   - **Modules touched** — for each: name, current interface, proposed change, rationale (using glossary terms).
   - **New modules** — for each: name, interface signature, behaviors, where it sits in the deep-module hierarchy.
   - **Test plan** — for each behavior: which interface boundary you test from, what the assertion is.
   - **Open questions** — anything you cannot resolve from prior context or code.
4. Use only canonical glossary terms. No synonyms, no inventions.
5. Surface to user: **"Here is the PRD. Modules and interfaces match your mental model?"**
6. Resolve open questions one at a time. Cycle back to phase 1 or 2 if PRD reveals a missing decision or term.
7. On confirmation, write `PRD-<topic>.md` and record summary in persistence layer.
8. Small changes: skip the file, write GitHub issues with same module + interface + test specificity.

**Stop condition:** every module change names module, interface, behavior added/changed, test that proves it. User confirmed.

**Anti-pattern:** PRD that names files but not interfaces. Interfaces are the contract; files are scaffolding.

---

## Phase 5 — Design interfaces, delegate implementation

**Symptom prevented:** brain fatigue, code shipping faster than you can hold in your head.
**Cause:** reviewing implementations doesn't scale. Designing interfaces does.

**Steps:**

1. For new modules introduced in phase 3-4, design the interface deliberately before delegating implementation:
   - What does it accept?
   - What does it return?
   - What invariants does it guarantee?
   - What does it explicitly *not* do?
2. Treat the implementation as a **gray box**: trust the interface contract, test from outside. Only review the inside for high-stakes modules (auth, payment, security, financial).
3. For existing shallow-module thickets that obstructed phase 4:
   - Identify clusters of related functions/files.
   - Wrap behind a single deep-module interface.
   - Move tests to the new boundary.
   - Delete now-obsolete inner interfaces.
4. Per Kent Beck: **invest in the design every day.** Every commit either invests or divests.
5. Update the project's persistence layer after non-trivial architectural changes:
   - Record new module's purpose + interface.
   - Link to PRD and design concept.
   - Forget stale architecture notes that no longer match code.

**Stop condition:** new modules expose simple interfaces, hide implementation, are testable from outside, compose without leaking dependencies.

**Anti-pattern:** reviewing line-by-line implementation of a deep module you trust. Time sink.

---

## The loop — when to revisit

| symptom | phase to revisit |
|---|---|
| AI built wrong thing | 1 align |
| Terms drift, AI verbose | 2 glossary |
| Ships code that doesn't match plan | 3 PRD |
| Big diffs, late tests, type errors | 4 TDD (`tdd.md`) |
| Brain fatigue, can't hold system | 5 design + delegate |
| AI confidently contradicts prior decisions | 0 orient (context drift) |

After every phase: record the decision so the next session starts with more context, not less.

## Greenfield checklist

| phase | query | record |
|---|---|---|
| 0 orient | 2-3 phrasings of intent, fetch full text for top-3 prior items | — |
| 1 align | prior design concepts, related architecture | design concept |
| 2 glossary | prior glossary | each term |
| 3 PRD | architecture for affected modules | PRD summary |
| 4 TDD | (commit messages cite decision IDs in body) | — |
| 5 design + delegate | stale architecture | new module interface; forget stale |
