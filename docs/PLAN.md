# Standalone Discord plugin for gray

Approved goal: port Hermes Discord integration to a separate public repository,
with plugin-owned setup and service lifecycle. No tokens in source or model input.

Implementation:
1. Pin Hermes provenance and preserve MIT notices. Port Unicode helpers and
   safe-mentions policy; use discord.py for protocol/reconnect/rate limits.
2. Private atomic configuration; hidden token prompt, API validation, invitation,
   expiring DM pairing with local confirmation; no open-to-all fallback.
3. Isolated gray homes per conversation, explicit resume IDs, bounded subprocess
   lifecycle, assistant-only output. Never select the latest shared session.
4. Gateway for owner DMs/mentions; outgoing-only stdio plugin, safe destinations,
   no duplicate gateway on agent construction. Plugin-owned periodic schedules.
5. Setup/install/status/restart/stop/doctor/uninstall commands; user systemd service.
6. Offline tests, real gray conformance, wheel installation and CLI smoke tests,
   license/secret scan, then publish a new public repository.

Compatibility: Python >=3.11, Linux systemd for managed service; foreground run
on Unix. Existing gray plugin wire 1.1, CLI print/session contract. CLI is
`gray-discord`; `gray discord setup` is not supported by the current gray host.
Release installation uses pip + plugin-owned registration, not the host's
Git skills-only importer. No Rust changes, no copying existing private repo history.

Limits: text only; periodic schedules (not calendar cron), no automatic replay
of interrupted actions, no separate semantic-memory backend, no full Hermes parity.
