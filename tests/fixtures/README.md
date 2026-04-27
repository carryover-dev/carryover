# Test fixture corpus

## Purpose

These fixtures drive adapter parser tests for the three v0.1 transcript readers:
Claude Code, Cursor, and Codex CLI. Each adapter PR imports fixtures via the
helpers in `tests/fixtures.rs` and writes `#[test]` functions that exercise
`parse()` and `read_new_records()` invariants against known inputs.

## Why synthetic data

All fixture content is fabricated. Zero real user sessions, secrets, email
addresses, or file paths from any developer's machine appear in this corpus.
Synthetic data is required for two reasons:

1. **Security** — this is a public repository. Real transcripts can contain
   pasted secrets, credentials, and personally identifiable information.
2. **Reproducibility** — fabricated inputs are stable; real transcripts drift
   as tool versions change and make CI non-deterministic.

## Directory layout

```
tests/fixtures/
├── README.md               ← this file
├── sanitizer.py            ← script to sanitize real transcripts before adding
├── claude/
│   ├── README.md
│   ├── 1-simple-conversation.jsonl
│   ├── 2-parentuuid-chain-deep.jsonl
│   ├── 3-tool-use-and-result.jsonl
│   ├── 4-non-conversation-types.jsonl
│   └── 5-partial-line-tail.jsonl
├── cursor/
│   ├── README.md
│   ├── build_state_vscdb.py
│   └── 1-state.vscdb
└── codex/
    ├── README.md
    ├── 1-simple-session.jsonl
    ├── 2-missing-session-meta.jsonl
    ├── 3-multiple-sessions.jsonl
    ├── 4-tool-use-event.jsonl
    └── 5-partial-line-tail.jsonl
```

## Edge cases covered per tool

### Claude Code

| Fixture | Edge case |
|---|---|
| `1-simple-conversation.jsonl` | Baseline 3-turn exchange, no parent chain |
| `2-parentuuid-chain-deep.jsonl` | `parentUuid` chain depth ≥2 across 7 messages |
| `3-tool-use-and-result.jsonl` | `content[]` array with `tool_use` + `tool_result` entries |
| `4-non-conversation-types.jsonl` | Interleaved `file-history-snapshot`, `permission-mode`, `queue-operation`, `summary` rows that must be filtered |
| `5-partial-line-tail.jsonl` | Last line truncated mid-JSON; adapter must emit `AdapterError::PartialJsonl` with the byte offset, not panic |

### Cursor

| Fixture | Edge case |
|---|---|
| `1-state.vscdb` | Synthetic SQLite with `aiService.generations`, `aiService.prompts`, `composer.composerData` rows |
| Locked-DB simulation | No separate fixture needed; the adapter test opens `1-state.vscdb` exclusively in a sibling thread to force the WAL copy-fallback path. See `cursor/README.md`. |

### Codex CLI

| Fixture | Edge case |
|---|---|
| `1-simple-session.jsonl` | `session_meta` first line followed by 3 `event_msg` rows |
| `2-missing-session-meta.jsonl` | Only `event_msg` rows; adapter must emit a structured warning, not panic |
| `3-multiple-sessions.jsonl` | Two session blocks back-to-back, each with its own `session_meta` |
| `4-tool-use-event.jsonl` | `event_msg` payloads with `tool_calls` and `role: "tool"` result rows |
| `5-partial-line-tail.jsonl` | Last line truncated mid-JSON; adapter must emit `AdapterError::PartialJsonl`, not panic |

## Running the sanitizer

To extend the corpus from a real transcript:

```sh
# From a JSONL source
python3 tests/fixtures/sanitizer.py path/to/real.jsonl > tests/fixtures/claude/N-new-fixture.jsonl

# From stdin
cat raw.jsonl | python3 tests/fixtures/sanitizer.py - > clean.jsonl
```

Review the output manually before committing. The sanitizer catches common
secret patterns but is not a security guarantee.

## Adding fixtures

1. Run `sanitizer.py` on the source.
2. Review the output for any remaining real data.
3. Place the file in the appropriate tool subdirectory.
4. Update the per-tool `README.md` with the new fixture and its edge case.
5. Add a presence assertion in `tests/fixtures.rs` if adding a new filename.
