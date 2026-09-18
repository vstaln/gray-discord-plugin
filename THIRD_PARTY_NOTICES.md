# Hermes attribution and scope

Source: https://github.com/NousResearch/hermes-agent
Inspected installed checkout: `5a8e8a6b87487c0e0785cd9eb561cc6a96c64f5e`.
This records the inspected source, not a claim that it is the latest upstream.

Copyright (c) 2025 Nous Research. MIT license reproduced in LICENSE.

`src/text.rs`: UTF-16 chunk boundary logic ported from `gateway/platforms/base.py`
in that checkout. Their original comment credits nearai/ironclaw#2304 for the
UTF-16 discrepancy.

The Discord lifecycle and safe-mentions policy were studied in
`plugins/platforms/discord/adapter.py`; owner authorization and expiring
pairing were studied in `gateway/pairing.py`. These are independently adapted
in this standalone Rust binary (`gray-discord`), not a verbatim copy of Hermes' large adapter.
The binary uses twilight rather than bundling Hermes' gateway or agent runtime.

A focused current Hermes/Pi plugin-document study is recorded in docs/PLUGIN_RESEARCH.md; neither runtime was fully audited.
This is a text-only port of selected behavior, not full Hermes compatibility.
