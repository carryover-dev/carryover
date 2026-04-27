# Claude Code fixtures

## Fixtures

| File | Edge case covered |
|---|---|
| `1-simple-conversation.jsonl` | Baseline 3-turn user/assistant exchange. Single session, no `parentUuid` chain. Verifies the happy path: fields parsed, role preserved, `ts` mapped to `LedgerRow.ts`. |
| `2-parentuuid-chain-deep.jsonl` | Seven messages chained via `parentUuid` with chain depth ≥2. Verifies that `parentUuid` values are preserved as `LedgerRow.parent_id` and that the adapter does not flatten or drop intermediate chain links. |
| `3-tool-use-and-result.jsonl` | Messages where `content` is a JSON array containing `tool_use` and `tool_result` entries rather than a plain string. Verifies that array-valued `content` is handled and that `tool_use` inputs are captured in `tool_calls_json`. |
| `4-non-conversation-types.jsonl` | Interleaves `type: "file-history-snapshot"`, `type: "permission-mode"`, `type: "queue-operation"`, and `type: "summary"` rows among ordinary `type: "say"` conversation rows. Verifies that the adapter filters out all non-conversation types and that the surviving rows are the expected `say` entries only. |
| `5-partial-line-tail.jsonl` | Three complete lines followed by a fourth line that is truncated mid-JSON (no closing `}`, no trailing newline). Verifies that the adapter emits `AdapterError::PartialJsonl { offset, .. }` with the correct byte offset and does not panic. The byte offset of the partial line is 798. |

## Schema reference

Claude Code writes one JSON object per line to
`~/.claude/projects/<slug>/<uuid>.jsonl`. Relevant fields:

| Field | Type | Notes |
|---|---|---|
| `uuid` | string | Unique message identifier |
| `parentUuid` | string \| null | Links to parent message; null for root |
| `sessionId` | string | Groups messages into one session |
| `type` | string | `"say"` for conversation; others are filtered |
| `role` | string | `"user"` or `"assistant"` |
| `content` | string \| array | Plain text or array of typed content blocks |
| `ts` | number | Unix epoch milliseconds |

## Provenance

All content is synthetic — generated from fabricated input via `../sanitizer.py`.
No real user sessions, paths, or identifiers are present.
