# Codex CLI fixtures

## Fixtures

| File | Edge case covered |
|---|---|
| `1-simple-session.jsonl` | `session_meta` first line followed by 3 `event_msg` rows. Verifies the happy path: session metadata extracted, event messages mapped to `LedgerRow`, roles preserved. |
| `2-missing-session-meta.jsonl` | Only `event_msg` rows with no `session_meta` first line. Verifies that the adapter emits a structured warning event and continues parsing rather than panicking or aborting. |
| `3-multiple-sessions.jsonl` | Two session blocks back-to-back, each starting with its own `session_meta`. Verifies that the adapter correctly resets session context at each `session_meta` boundary and does not mix rows between sessions. |
| `4-tool-use-event.jsonl` | `event_msg` payloads containing `tool_calls` arrays and `role: "tool"` result rows. Verifies that tool call data is captured in `tool_calls_json` and that tool result rows are handled without error. |
| `5-partial-line-tail.jsonl` | Four complete lines followed by a fifth line truncated mid-JSON (no closing `}`, no trailing newline). Verifies that the adapter emits `AdapterError::PartialJsonl { offset, .. }` with the correct byte offset (785) and does not panic. |

## Schema reference

Codex CLI writes JSONL to `~/.codex/sessions/YYYY/MM/DD/rollout-<id>.jsonl`.
The file format uses two record types:

### `session_meta` (first line of each session block)

| Field | Type | Notes |
|---|---|---|
| `type` | string | Always `"session_meta"` |
| `session_id` | string | Unique session identifier |
| `started_at` | number | Unix epoch milliseconds |
| `model` | string | Model name used for the session |
| `cwd` | string | Working directory at session start |

### `event_msg`

| Field | Type | Notes |
|---|---|---|
| `type` | string | Always `"event_msg"` |
| `session_id` | string | References the enclosing session |
| `seq` | number | Monotonically increasing sequence number |
| `role` | string | `"user"`, `"assistant"`, or `"tool"` |
| `content` | string | Message text or tool output |
| `ts` | number | Unix epoch milliseconds |
| `tool_calls` | array \| absent | Present on assistant turns that invoke tools |
| `tool_call_id` | string \| absent | Present on `role: "tool"` result rows |

## Provenance

All content is synthetic — generated from fabricated input via `../sanitizer.py`.
No real user sessions, paths, or identifiers are present.
