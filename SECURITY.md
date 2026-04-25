# Security Policy

Thank you for helping keep Carryover and its users safe.

## Reporting a vulnerability

Please report suspected vulnerabilities privately, not in public GitHub issues.

- **Email:** `contact@voura.app`
- Include a clear description, reproduction steps, the affected version or commit, and any proof-of-concept material.
- If you would like an encrypted channel, ask in the first message and one will be arranged.

The maintainer will acknowledge your report within **seven days** and will keep you updated as triage and remediation progress. Coordinated disclosure timelines are negotiated per report and we aim to credit reporters who wish to be named.

## Supported versions

Carryover is pre-launch. While the project is in alpha, only the **latest published release** is supported with security fixes. Once v0.1.0 ships, this section will be updated with a concrete supported-version table.

| Version | Supported |
|---------|-----------|
| latest  | Yes       |
| older   | No        |

## Scope

In scope:

- The `carryoverd` daemon, its local hook endpoint on `localhost:47823`, the SQLite ledger, and any official packaging artifacts (Homebrew formula, npm binary).
- Hook stubs and integration scripts published by this project.

Out of scope (please report upstream):

- Vulnerabilities in third-party AI agents (Claude Code, Cursor, Codex, Copilot, Windsurf, Aider).
- Vulnerabilities in language runtimes, operating systems, or unrelated dependencies.

## Safe harbor

Good-faith research conducted under this policy will not be pursued legally by the project. Please do not access user data you do not own, do not exfiltrate data, and do not degrade service for other users.
