# Plugin compatibility findings

GitHub API heads observed during this task:
- Hermes: 47685348eaca9d673719003b9e03a71becfa6423
- Pi: 509ee2bd0ba9fc3d31fb96fe8f5a6ef73b51833c

Read current `plugins/AGENTS.md` and Discord plugin manifest from Hermes,
and `packages/coding-agent/docs/extensions.md` / `packages.md` from Pi via
the GitHub API. This is a focused document study, not a full source audit.

Hermes provides plugin CLI registration through `ctx.register_cli_command`,
lifecycle hooks, tools, out-of-tree discovery and a SHA-pinned plugin catalog.
Its policy explicitly forbids platform-specific modifications to core: add
a generic capability with a concrete consumer instead. Memory plugins have
a separate provider interface and setup lifecycle.

Pi exposes `registerTool`, `registerCommand`, lifecycle events, UI prompts,
and session-persistent `appendEntry`. Packages declare extensions and other
resources, and can be installed from Git or npm. Extensions have full OS
access; installation is not sandboxing.

Gray's current host does not provide either runtime or generic plugin CLI
subcommand dispatch. A Python Discord adapter cannot claim to be a Pi/Hermes
runtime extension or make `gray discord setup` exist simply by returning a
manifest. This release therefore supplies `gray-discord setup` itself and
registers only the existing gray protocol-1.1 outgoing tool. A future generic
host dispatch/setup capability is separate work, not silently emulated here.

The attempt to update the local Hermes reference with git timed out; its
checkout is NOT verified to equal the GitHub head above. The installed Hermes
was not upgraded. No claim of a completed Hermes update is made.
