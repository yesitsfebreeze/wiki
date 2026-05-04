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
  ingest_log/ — audit trail
  .search/    — Tantivy full-text index
```

- **Purpose classification** — every doc is embedded and matched (cosine) against purpose embeddings. Top-1 above `wiki_similarity_threshold` wins; below threshold → `general`.
- **Chunking** — multi-topic content splits into a parent container + child chunk docs linked via `PartOf` reasons.
- **Entity linking** — `learn_pass` / `link_doc` rewrites bare entity mentions as `[[wikilinks]]` and folds near-duplicate paragraphs into the canonical entity.

## 🛠️ Tools

| Tool | What it does |
|---|---|
| `ingest` | 📥 Write doc — `kind`: thought \| entity \| question \| reason \| conclusion. Auto-links + auto-marks-answered |
| `search` | 🔍 Hybrid search — `mode`: smart (conclusions-first) \| fts (BM25) \| tag \| qa \| list |
| `get` | 📖 Fetch doc by id with reasons + edges (depth configurable) |
| `update` | ✏️ Update body / title / tags. Re-embeds + re-links |
| `delete_doc` | 🗑️ Delete a doc; cascades edge cleanup |
| `learn_pass` | 🔁 Sensemaker — link/dedupe → connect → raise/answer → promote conclusions |
| `list_open_questions` | ❓ Paginate unresolved questions, filter by purpose |
| `mark_question` | ✅ Manually set question state (answered \| dropped) |
| `purpose` | 🏷️ Manage purposes — `action`: list \| create \| delete \| reembed |
| `admin` | 🧹 Vault maintenance — `action`: recompute \| sanitize \| migrate \| feedback |
| `code` | 💻 Code index ops — `action`: index \| search \| read \| refs \| validate |
| `docs` | 📚 Fetch tool / concept markdown docs (no arg → list) |

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
