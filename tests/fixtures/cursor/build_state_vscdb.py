#!/usr/bin/env python3
"""
Build a deterministic synthetic state.vscdb for Cursor adapter tests.

All data is fabricated. No real user sessions, paths, secrets, or identifiers.
Re-running this script on the same Python + SQLite version produces an identical
file because:
  - Row insertion order is fixed.
  - All timestamps are hardcoded constants.
  - VACUUM is run at the end to normalize free-list pages.

Usage:
    python3 build_state_vscdb.py [output_path]

    output_path defaults to state.vscdb in the same directory as this script.
"""

import json
import os
import sqlite3
import sys


def build(output_path: str) -> None:
    if os.path.exists(output_path):
        os.remove(output_path)

    conn = sqlite3.connect(output_path)
    cur = conn.cursor()

    # Cursor's state.vscdb uses a single key-value table called ItemTable.
    cur.execute(
        "CREATE TABLE ItemTable ("
        "  key   TEXT PRIMARY KEY NOT NULL,"
        "  value BLOB"
        ")"
    )

    # --- aiService.generations ---
    # Synthetic generation records matching Cursor's internal shape.
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
    cur.execute(
        "INSERT INTO ItemTable (key, value) VALUES (?, ?)",
        ("aiService.generations", json.dumps(generations)),
    )

    # --- aiService.prompts ---
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
    cur.execute(
        "INSERT INTO ItemTable (key, value) VALUES (?, ?)",
        ("aiService.prompts", json.dumps(prompts)),
    )

    # --- composer.composerData ---
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
    cur.execute(
        "INSERT INTO ItemTable (key, value) VALUES (?, ?)",
        ("composer.composerData", json.dumps(composer_data)),
    )

    conn.commit()

    # VACUUM normalizes page layout for reproducibility.
    conn.execute("VACUUM")
    conn.close()
    print(f"Written: {output_path} ({os.path.getsize(output_path)} bytes)")


if __name__ == "__main__":
    script_dir = os.path.dirname(os.path.abspath(__file__))
    out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(script_dir, "1-state.vscdb")
    build(out)
