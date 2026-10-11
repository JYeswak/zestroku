# Goal

Working, verified Roku control from one binary: discovery, read verbs, keypress, gated launch/install, a serve daemon (HTTP API + MCP), a loopback sim for CI-verifiable e2e, and the m8 parity set (doctor, error codes, undo, policy, aliases).

Done means: origin/main CI is green on all gates (fmt, check, clippy, test, one-asupersync, tokio-ban, identifier-scrub), each M7 bead's acceptance is re-run by someone other than its implementer, and the live darwin leg shows both owner devices discovered and stored.
