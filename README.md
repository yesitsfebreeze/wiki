# wiki |

MCP server that maintains a persistent Obsidian-vault knowledge base.

Instead of answering from memory or re-reading the same files, you ingest once and query forever. Instead of searching across flat notes, you traverse a typed knowledge graph — thoughts, entities, reasons, questions, conclusions — all auto-classified by topic via OpenAI embeddings.

## 💡 Why

Every time an AI answers a question, it starts from scratch — re-reading files, re-summarizing context, re-deriving the same conclusions. This wastes tokens and loses compounding knowledge.

`wiki` fixes this by maintaining a single vault per project. Ingest a document once; the knowledge is chunked, embedded, purpose-classified, and indexed. Future queries hit the vault instead of raw files. Entity linking deduplicates overlapping content automatically.

**Source = truth. `.wiki/` = derived knowledge.** Blow it away anytime; rebuild from source via `/reindex`.

## ⚡ Token savings

| Operation | Without wiki | With wiki |
|---|---|---|
| Answer a recurring question | Re-read files every time | Single `search_fulltext` call |
| Cross-topic synthesis | Manual grep + summarize | `search_by_tag` + reason traversal |
| Code file read | Full file in context | Structure map + targeted body load |

## ⚙️ How it works

```
source doc  →  ingest_thought / ingest_entity
                  ↓ OpenAI embed → purpose classification
            →  .wiki/thoughts/<uuid>.md   (type: thought, purpose: <tag>)
            →  .wiki/entities/<uuid>.md   (type: entity, purpose: <tag>)
            →  .wiki/reasons/<uuid>.md    (PartOf / Supports / ...)

.wiki/
  purposes/   — topical buckets (each has an embedding)
  thoughts/   — atomic facts from sources
  entities/   — recurring concepts
  reasons/    — directed edges between nodes
  questions/  — open questions
  conclusions/— synthesized knowledge
  ingest_log/ — append-only JSONL audit trail (ingest.jsonl, rotates at 265 lines)
  .search/    — Tantivy full-text index
```

- **Purpose classification** — every doc is embedded and matched (cosine) against purpose embeddings. Top-1 above `wiki_similarity_threshold` wins; below threshold → `general`.
- **Chunking** — multi-topic content splits into a parent container + child chunk docs linked via `PartOf` reasons.
- **Entity linking** — `learn_pass` / `link_doc` rewrites bare entity mentions as `[[wikilinks]]` and folds near-duplicate paragraphs into the canonical entity.

## 🛠️ Tools

All multi-doc tools are **batch-only** — wrap every payload in `{items: [...]}`, even for one record.

| Tool | What it does |
|---|---|
| `ingest` | 📥 Batch-write docs — `kind`: thought \| entity \| question \| reason \| conclusion. Auto-links, body-start `[[<qid>]]` → `Supports` (synthesis-fed), explicit `reason_kind:"Answers"` for direct answers |
| `search` | 🔍 Batch hybrid search — `mode`: smart (conclusions-first) \| fts (BM25) \| tag \| qa \| list. `raise_on_miss` opt-in |
| `get` | 📖 Batch-fetch docs with split `inbound`/`outbound` reasons + edge walk |
| `update` | ✏️ Batch-update content/tags. Re-embeds + re-links on body change |
| `retag` | 🏷️ Bulk add/remove tags + bulk-purpose-move without touching content |
| `delete_doc` | 🗑️ Batch-delete by `id`/`ids`; cascades edge cleanup |
| `learn_pass` | 🔁 Sensemaker — link/dedupe → connect → raise/answer → promote conclusions. `limit:0` = scan whole vault. Returns `invariant_reason` when no progress |
| `list_open_questions` | ❓ Paginate unresolved questions, filter by purpose |
| `mark_question` | ✅ Batch-resolve questions (deleted \| buried) |
| `purpose` | 🏷️ Manage purposes — `action`: list \| create \| delete \| reembed |
| `author` | 🧹 Vault maintenance — `action`: reindex \| sanitize \| migrate \| migrate_lifecycle \| feedback \| retitle_questions |
| `code` | 💻 Code index ops — `action`: index \| search \| read \| refs \| validate |
| `docs` | 📚 Fetch tool / concept markdown docs (no arg → list) |

## ❓ Question lifecycle

Two states only:

- **open** — `questions/<purpose>/...`. Default for any new question.
- **graveyard** — `questions/graveyard/<purpose>/...`. Junk or unanswerable. Excluded from `list_open_questions`, `learn_pass`, and conclusions-first search.

Anything else is hard-deleted. An *answered* question is one that was promoted: a conclusion doc now exists with an `Answers` edge to the (former) question, and the question file itself is gone. The conclusion body preserves the original question text as a preamble so context survives the delete.

Resolve open questions via `mark_question`:

| Status | Effect |
|---|---|
| `deleted` | Hard delete + cascade-delete every reason touching it. Use for already-answered or junk questions. |
| `buried` | Move to `questions/graveyard/`, tag `graveyard`, rewrite inbound wikilinks. Reversible — move the file back into the open tree to resurrect. |

To re-explore everything in the graveyard: delete `questions/graveyard/`.

`learn_pass` auto-promotes when `support_promote_floor` (default 3) supporting thoughts accumulate or one candidate clears `answer_threshold` (default 0.6). Manual fallback only if the pass can't find enough evidence.

## 💿 Install

### Terminal

```bash
claude marketplace add yesitsfebreeze/wiki
claude plugin install wiki@yesitsfebreeze
```

### Inside Claude

```
/plugin marketplace add yesitsfebreeze/wiki
/plugin install wiki@yesitsfebreeze
```

Done. MCP server + skills + hooks installed automatically.

## 🏗️ Building

Requires Rust and the WASM target:
```bash
rustup target add wasm32-wasip1
cargo install --git https://github.com/yesitsfebreeze/wiki
```

Installs `wiki` (or `wiki.exe`) into `~/.cargo/bin/`.

### Local install from a checkout

```bash
git clone https://github.com/yesitsfebreeze/wiki
cd wiki
cargo install --path . --force
```

### One-command rebuild + reinstall (`just update`)

If you have [`just`](https://github.com/casey/just) installed, the bundled `Justfile` ships an end-to-end refresh recipe — kills running `wiki.exe` instances, rebuilds, reinstalls into `~/.cargo/bin`, and best-effort refreshes the Claude plugin:

```bash
just update          # kill → build → install → claude plugin update
just kill            # just kill running wiki.exe
just install         # build release + cargo install --path . --force
just update-plugin   # claude plugin update wiki@yesitsfebreeze
just test            # cargo test --test-threads=1
```

Restart any MCP clients (Claude Code / Cursor / etc.) after `just update` so they pick up the new binary.

## ⚙️ Config

Create `~/.config/wiki/config.toml`:

```toml
openai_api_key = "sk-..."

# wiki_rerank_model = "gpt-4o-mini"        # model for query reranking
# wiki_similarity_threshold = 0.35
# wiki_dedupe_threshold = 0.85

# Code indexing (optional)
# split_src_dir = "src"
# split_ext = "rs"
# split_index_dir = ".wiki/code"
# split_max_loc = "256"
```

Environment variables override config file values.

## 🌐 Languages

`wiki` includes a WASM language system for code indexing. Each language is a `.wasm` module that teaches the parser how to decompose a source file into per-function bodies.

Language modules live in:
- `.wiki/code/languages/{ext}.wasm` — project-level
- `~/.config/split/languages/{ext}.wasm` — user-level
- embedded — built-in (`rs`, `py`)

Use the `list_languages` MCP tool to see what is installed in the current environment.
