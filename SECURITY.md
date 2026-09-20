# Security Policy

## Reporting a vulnerability

**Do not open a public issue for security vulnerabilities.**

Email the maintainers directly or use GitHub's private vulnerability reporting.

Include:
- Description of the vulnerability
- Steps to reproduce
- Potential impact
- Any suggested fixes

## Response timeline

- **Acknowledgment:** within 48 hours
- **Initial assessment:** within 1 week
- **Fix timeline:** depends on severity

## Security model

kn9t is **not a sandbox**. It executes tools (including shell commands) with the
user's full privileges. Risk mitigation lives in a policy plugin, not in kn9t core.

See [ADR-0008](docs/adr/0008-policy-is-a-plugin.md) for the design rationale.
