#!/usr/bin/env python3
"""
Sanitize a JSONL or SQLite transcript source for use as a test fixture.

Replacements applied (all via regex, in order):
  1. API keys / tokens     — patterns like sk-..., ghp_..., xoxb-..., etc.
  2. AWS access keys       — AKIA...
  3. Generic bearer tokens — Authorization: Bearer <token>
  4. Email addresses       — replaced with user@example.invalid
  5. Absolute home paths   — /Users/<name>/... and /home/<name>/...
                             replaced with /synthetic/path/N (counter-based)

Output goes to stdout. Idempotent: running on already-sanitized output
produces identical bytes because synthetic substitution values are stable
under the same replacement patterns.

Usage:
    # JSONL source
    python3 sanitizer.py transcript.jsonl > sanitized.jsonl

    # Pipe
    cat raw.jsonl | python3 sanitizer.py - > clean.jsonl

Limitations:
    - SQLite sources must be exported to JSONL first (e.g. via sqlite3 -json).
    - Only line-oriented JSONL is processed; binary blobs are not inspected.
    - This script eliminates common secret patterns but is not a security
      guarantee. Review output before committing to a public repository.
"""

import re
import sys
from typing import Iterator

# ---------------------------------------------------------------------------
# Replacement patterns (ordered — more specific first)
# ---------------------------------------------------------------------------

# Counter for synthetic path numbering (module-level so it persists across
# lines; reset per invocation via the _path_counter dict below).
_path_map: dict[str, str] = {}
_path_counter: list[int] = [0]


def _synthetic_path(match: re.Match) -> str:
    original = match.group(0)
    if original not in _path_map:
        _path_counter[0] += 1
        _path_map[original] = f"/synthetic/path/{_path_counter[0]}"
    return _path_map[original]


# Each tuple is (compiled_pattern, replacement_string_or_callable).
RULES: list[tuple[re.Pattern, object]] = [
    # OpenAI / Anthropic API keys
    (re.compile(r'sk-[A-Za-z0-9]{20,}'), "sk-REDACTED"),
    # GitHub personal access tokens (classic ghp_ and fine-grained github_pat_)
    (re.compile(r'ghp_[A-Za-z0-9]{36,}'), "ghp_REDACTED"),
    (re.compile(r'github_pat_[A-Za-z0-9_]{36,}'), "github_pat_REDACTED"),
    # Slack tokens
    (re.compile(r'xox[baprs]-[A-Za-z0-9\-]{10,}'), "xoxb-REDACTED"),
    # AWS access key IDs
    (re.compile(r'AKIA[A-Z0-9]{16}'), "AKIAXXXXXXXXXXXXXXXX"),
    # AWS secret access keys (40-char base64-ish after known prefixes)
    (re.compile(r'(?i)(aws_secret_access_key\s*[=:]\s*)[A-Za-z0-9+/]{40}'), r'\1REDACTED'),
    # Bare JWTs (eyJ<base64>.<base64>.<base64>) — appear as standalone field values, not just in bearer headers
    (re.compile(r'eyJ[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+'), "eyJ.JWT.REDACTED"),
    # Generic bearer tokens in Authorization headers
    (re.compile(r'(?i)(authorization:\s*bearer\s+)[A-Za-z0-9\-._~+/]+=*'), r'\1REDACTED'),
    # Passwords in key=value or key: value form
    (re.compile(r'(?i)(password\s*[=:]\s*)\S+'), r'\1REDACTED'),
    # Generic tokens in key=value or key: value form
    (re.compile(r'(?i)(token\s*[=:]\s*)[A-Za-z0-9\-._~+/]{8,}'), r'\1REDACTED'),
    # Email addresses — replace with the stable sentinel accepted by the secret scan
    (re.compile(r'[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}'), "user@example.invalid"),
    # Absolute paths under /home/<username>/  (Linux)
    (re.compile(r'/home/[A-Za-z0-9_.\-]+(?:/[^\s"\'\\,;>)]*)?'), _synthetic_path),
    # Absolute paths under /Users/<username>/  (macOS)
    (re.compile(r'/Users/[A-Za-z0-9_.\-]+(?:/[^\s"\'\\,;>)]*)?'), _synthetic_path),
    # Absolute paths under C:\Users\<username>\ or C:/Users/<username>/  (Windows).
    # Windows paths have TWO separators: the drive colon `:` and then a path
    # separator `\` or `/`. They are required to appear in that order.
    (re.compile(r'[Cc]:[\\\/]Users[\\\/][A-Za-z0-9_.\-]+(?:[\\\/][^\s"\'\\,;>)]*)?'), _synthetic_path),
]


# Patterns that MUST NOT survive sanitization. Used by the post-pass
# self-check to fail-loud if a regex regression lets a known-shaped secret
# slip through. These re-derive the canonical patterns from RULES so a
# typo in one is caught by the other.
_VERIFY_PATTERNS: list[re.Pattern] = [
    re.compile(r'sk-[A-Za-z0-9]{20,}'),
    re.compile(r'ghp_[A-Za-z0-9]{36,}'),
    re.compile(r'github_pat_[A-Za-z0-9_]{36,}'),
    re.compile(r'xox[baprs]-[A-Za-z0-9\-]{10,}'),
    re.compile(r'AKIA[A-Z0-9]{16}'),
    re.compile(r'eyJ[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+'),
]


def sanitize_line(line: str) -> str:
    """Apply all sanitization rules to a single line of text."""
    for pattern, replacement in RULES:
        if callable(replacement):
            line = pattern.sub(replacement, line)
        else:
            line = pattern.sub(replacement, line)
    return line


def process_lines(lines: Iterator[str]) -> str:
    """Read lines, sanitize, write to stdout, return the full sanitized output."""
    chunks: list[str] = []
    for line in lines:
        clean = sanitize_line(line)
        sys.stdout.write(clean)
        chunks.append(clean)
    return "".join(chunks)


def verify_no_known_secrets(text: str) -> None:
    """
    Post-pass self-check: re-scan the sanitized output for canonical secret
    shapes. If any survive, the corresponding RULES entry has regressed —
    fail loud rather than silently emit a leaky fixture.
    """
    for pat in _VERIFY_PATTERNS:
        match = pat.search(text)
        if match:
            sys.stderr.write(
                f"sanitizer self-check FAILED: pattern {pat.pattern!r} "
                f"survived sanitization. Update RULES so this shape is caught.\n"
            )
            sys.exit(2)


def main() -> None:
    # Reset path state per invocation (important for idempotency testing).
    _path_map.clear()
    _path_counter[0] = 0

    if len(sys.argv) < 2 or sys.argv[1] == "-":
        output = process_lines(sys.stdin)
    else:
        path = sys.argv[1]
        with open(path, "r", encoding="utf-8", errors="replace") as fh:
            output = process_lines(fh)

    verify_no_known_secrets(output)


if __name__ == "__main__":
    main()
