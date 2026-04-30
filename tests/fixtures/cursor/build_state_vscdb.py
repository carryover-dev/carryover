#!/usr/bin/env python3
"""
Build deterministic synthetic state.vscdb fixtures for Cursor adapter tests.

All data is fabricated. No real user sessions, paths, secrets, or identifiers.
Re-running this script on the same Python + SQLite version produces identical
files because row insertion order is fixed, timestamps are hardcoded constants,
and VACUUM is run at the end to normalize free-list pages.

Generates two fixture sets:
  1. Old schema (pre-migration): tests/fixtures/cursor/oldSchema/state.vscdb
     Contains aiService.generations, aiService.prompts, composer.composerData
     all in a single global DB.

  2. New schema (post-migration): tests/fixtures/cursor/globalStorage/state.vscdb
     + tests/fixtures/cursor/workspaceStorage/<ws_id>/state.vscdb
     Global DB has composer.composerHeaders; per-workspace DBs have prompts.

Usage:
    python3 build_state_vscdb.py
"""

import json
import os
import sqlite3


SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))


def create_item_table(conn: sqlite3.Connection) -> None:
    conn.execute(
        "CREATE TABLE ItemTable ("
        "  key   TEXT PRIMARY KEY NOT NULL,"
        "  value BLOB"
        ")"
    )


def vacuum_close(conn: sqlite3.Connection, path: str) -> None:
    conn.commit()
    conn.execute("VACUUM")
    conn.close()
    print(f"Written: {path} ({os.path.getsize(path)} bytes)")


# ---------------------------------------------------------------------------
# Old schema (pre-migration)
# ---------------------------------------------------------------------------

def build_old_schema(output_path: str) -> None:
    os.makedirs(os.path.dirname(output_path), exist_ok=True)
    if os.path.exists(output_path):
        os.remove(output_path)

    conn = sqlite3.connect(output_path)
    create_item_table(conn)

    generations = [
        {
            "generationId": "gen-synthetic-0001",
            "sessionId": "cursor-session-001",
            "requestTs": 1700200000000,
            "responseTs": 1700200001500,
            "model": "synthetic-model-v1",
            "promptTokens": 42,
            "completionTokens": 87,
            "userMessage": "How do I center a div in CSS?",
            "assistantMessage": (
                "Use flexbox: set the parent to `display: flex; "
                "justify-content: center; align-items: center;`."
            ),
        },
        {
            "generationId": "gen-synthetic-0002",
            "sessionId": "cursor-session-001",
            "requestTs": 1700200010000,
            "responseTs": 1700200011200,
            "model": "synthetic-model-v1",
            "promptTokens": 55,
            "completionTokens": 110,
            "userMessage": "What is CSS grid?",
            "assistantMessage": (
                "CSS Grid is a two-dimensional layout system. "
                "Define rows and columns with `grid-template-rows` and "
                "`grid-template-columns`, then place items with `grid-area`."
            ),
        },
        {
            "generationId": "gen-synthetic-0003",
            "sessionId": "cursor-session-002",
            "requestTs": 1700210000000,
            "responseTs": 1700210002000,
            "model": "synthetic-model-v1",
            "promptTokens": 30,
            "completionTokens": 60,
            "userMessage": "Explain async/await in JavaScript.",
            "assistantMessage": (
                "async/await is syntactic sugar over Promises. "
                "An async function always returns a Promise; "
                "await pauses execution until the Promise resolves."
            ),
        },
    ]
    conn.execute(
        "INSERT INTO ItemTable (key, value) VALUES (?, ?)",
        ("aiService.generations", json.dumps(generations)),
    )

    prompts = [
        {
            "promptId": "prompt-synthetic-0001",
            "sessionId": "cursor-session-001",
            "ts": 1700200000000,
            "text": "How do I center a div in CSS?",
            "files": ["/synthetic/path/8/index.html"],
        },
        {
            "promptId": "prompt-synthetic-0002",
            "sessionId": "cursor-session-001",
            "ts": 1700200010000,
            "text": "What is CSS grid?",
            "files": [],
        },
        {
            "promptId": "prompt-synthetic-0003",
            "sessionId": "cursor-session-002",
            "ts": 1700210000000,
            "text": "Explain async/await in JavaScript.",
            "files": ["/synthetic/path/9/app.js"],
        },
    ]
    conn.execute(
        "INSERT INTO ItemTable (key, value) VALUES (?, ?)",
        ("aiService.prompts", json.dumps(prompts)),
    )

    composer_data = {
        "composers": [
            {
                "composerId": "composer-synthetic-0001",
                "sessionId": "cursor-session-001",
                "createdAt": 1700200000000,
                "title": "CSS layout help",
                "messages": [
                    {"role": "user", "content": "How do I center a div in CSS?", "ts": 1700200000000},
                    {
                        "role": "assistant",
                        "content": "Use flexbox: `display: flex; justify-content: center; align-items: center;`.",
                        "ts": 1700200001500,
                    },
                ],
            },
            {
                "composerId": "composer-synthetic-0002",
                "sessionId": "cursor-session-002",
                "createdAt": 1700210000000,
                "title": "JavaScript async patterns",
                "messages": [
                    {"role": "user", "content": "Explain async/await in JavaScript.", "ts": 1700210000000},
                    {
                        "role": "assistant",
                        "content": "async/await is syntactic sugar over Promises.",
                        "ts": 1700210002000,
                    },
                ],
            },
        ]
    }
    conn.execute(
        "INSERT INTO ItemTable (key, value) VALUES (?, ?)",
        ("composer.composerData", json.dumps(composer_data)),
    )

    vacuum_close(conn, output_path)


# ---------------------------------------------------------------------------
# New schema (post-migration)
# ---------------------------------------------------------------------------

WS1_ID = "wsaaa111bbb222cc"
WS2_ID = "wsccc333ddd444ee"

WS1_FSPATH = "/synthetic/project-alpha"
WS2_FSPATH = "/synthetic/project-beta"

COMPOSER1_ID = "composer-new-0001"
COMPOSER2_ID = "composer-new-0002"


def build_new_schema_global(output_path: str) -> None:
    os.makedirs(os.path.dirname(output_path), exist_ok=True)
    if os.path.exists(output_path):
        os.remove(output_path)

    conn = sqlite3.connect(output_path)
    create_item_table(conn)

    headers = {
        "allComposers": [
            {
                "composerId": COMPOSER1_ID,
                "lastUpdatedAt": 1700300010000,
                "workspaceIdentifier": {
                    "id": WS1_ID,
                    "uri": {
                        "fsPath": WS1_FSPATH,
                        "scheme": "file",
                    },
                },
            },
            {
                "composerId": COMPOSER2_ID,
                "lastUpdatedAt": 1700300000000,
                "workspaceIdentifier": {
                    "id": WS2_ID,
                    "uri": {
                        "fsPath": WS2_FSPATH,
                        "scheme": "file",
                    },
                },
            },
        ]
    }
    conn.execute(
        "INSERT INTO ItemTable (key, value) VALUES (?, ?)",
        ("composer.composerHeaders", json.dumps(headers)),
    )

    vacuum_close(conn, output_path)


def build_new_schema_workspace1(output_path: str) -> None:
    os.makedirs(os.path.dirname(output_path), exist_ok=True)
    if os.path.exists(output_path):
        os.remove(output_path)

    conn = sqlite3.connect(output_path)
    create_item_table(conn)

    prompts = [
        {"text": "How do I implement binary search?", "commandType": 4},
        {"text": "Can you add unit tests for that?", "commandType": 4},
    ]
    conn.execute(
        "INSERT INTO ItemTable (key, value) VALUES (?, ?)",
        ("aiService.prompts", json.dumps(prompts)),
    )

    generations = [
        {"unixMs": 1700300001000, "generationUUID": "gen-new-0001", "type": 1, "textDescription": "binary search impl"},
        {"unixMs": 1700300010000, "generationUUID": "gen-new-0002", "type": 1, "textDescription": "unit tests"},
    ]
    conn.execute(
        "INSERT INTO ItemTable (key, value) VALUES (?, ?)",
        ("aiService.generations", json.dumps(generations)),
    )

    vacuum_close(conn, output_path)


def build_new_schema_workspace2(output_path: str) -> None:
    os.makedirs(os.path.dirname(output_path), exist_ok=True)
    if os.path.exists(output_path):
        os.remove(output_path)

    conn = sqlite3.connect(output_path)
    create_item_table(conn)

    prompts = [
        {"text": "Explain Docker networking", "commandType": 4},
    ]
    conn.execute(
        "INSERT INTO ItemTable (key, value) VALUES (?, ?)",
        ("aiService.prompts", json.dumps(prompts)),
    )

    generations = [
        {"unixMs": 1700300005000, "generationUUID": "gen-new-0003", "type": 1, "textDescription": "docker networking"},
    ]
    conn.execute(
        "INSERT INTO ItemTable (key, value) VALUES (?, ?)",
        ("aiService.generations", json.dumps(generations)),
    )

    vacuum_close(conn, output_path)


if __name__ == "__main__":
    # Old schema (two paths: legacy root for fixtures.rs test + oldSchema/ for adapter tests)
    build_old_schema(os.path.join(SCRIPT_DIR, "1-state.vscdb"))
    build_old_schema(os.path.join(SCRIPT_DIR, "oldSchema", "state.vscdb"))

    # New schema: global + two workspaces
    build_new_schema_global(os.path.join(SCRIPT_DIR, "globalStorage", "state.vscdb"))
    build_new_schema_workspace1(
        os.path.join(SCRIPT_DIR, "workspaceStorage", WS1_ID, "state.vscdb")
    )
    build_new_schema_workspace2(
        os.path.join(SCRIPT_DIR, "workspaceStorage", WS2_ID, "state.vscdb")
    )

    print("All fixtures built.")
