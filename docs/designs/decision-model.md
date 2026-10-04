# Decision models

The decision feature spans the stack around one currency type: `shuvarie-decision`'s `Decision`
(named, typed question) in, and `DecisionAnswer`/`DecisionOutcome` out. Decision models are **not**
generative completions (no streaming, no tools, no free text) and do **not** ride the LLM provider
path — they have their own connection and their own client.

One wire protocol covers every backend: TypeSafe's System One API (`POST /v1/systemone`), which
Ollama's Clef models and Cloudflare's Clef deployment also serve. A backend differs only in URL and
credential. Protocol limits (choice/score 2–26, 1–64 questions, 64 KiB state) are enforced locally
so a malformed definition reports its own name instead of the wire's anonymous error.
`rig-typesafeai` stays private to that crate.

- `crates/decision/` (`shuvarie-decision`): the System One codec and client. `noul` is a
  probability in `[0, 1]`, *not* a boolean, and `confidence` is distribution concentration —
  **not** the chance the answer is correct. Threshold policy belongs to the caller.
- `crates/config/src/decisions.rs` + `ranking.rs`: the `decisions { }` definitions (named and
  reusable), option ranking, and the `permissions` bindings that name them; `connections.kdl`'s
  `decision-providers` / `decision` hold the connection.
- `crates/core/src/decisions.rs`: the `Decisions` service — compiles config into requests, owns the
  shell/tool checks and option ranking, and holds the swappable connection. `build` never fails:
  a broken check is recorded, surfaced as a startup warning, and follows its own `on-error`
  policy (default `ask`). Checks are **tighten-only** and never spend a call on a command already
  denied by a rule.
- `crates/core/src/tools/decide.rs`: the LLM-initiated `decide` tool.
- `src/tui/add_decision_provider.rs`: the provider dialog (name, endpoint, optional key, free-text
  model) — the only model-selection surface, since no registry enumerates decision models.

**Off by default**: the feature reaches a network endpoint, so it is inert until
`decisions { enabled #true }` opts in.

