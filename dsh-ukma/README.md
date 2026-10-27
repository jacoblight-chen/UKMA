# dsh-ukma

A [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) (dsh)
plugin that connects a running DSH session to a
[UKMA](../README.md) knowledge base — the 65-operation, 7-domain,
content-addressed code + Fractal-Memory platform — as a set of native dsh
tools, without any change to the UKMA core.

Modelled on
[`dsh-codegraph`](https://github.com/jiangzhenguo/dsh-codegraph), it exposes
UKMA's operations (symbol search, structural code intelligence, persistent
memory, and an escape hatch to all 65 operations) as dsh tools, plus an
optional system-prompt section and an optional prompt front-load hook.

> **Status.** UKMA is a pre-1.0 platform and dsh is a developer preview that
> changes quickly. This plugin targets the dsh plugin API as of dsh `0.1.x`–
> `0.2.x` (the same range `dsh-codegraph` supports) and is gated by a
> `peerDependencies` ceiling of `<0.3.0-0` — a DSH install that resolves an
> out-of-range peer is rejected rather than half-working.

---

## Why a persistent `ukma --mcp` child (and not a per-call `--run-tool`)

This is the key architectural decision, and it differs from `dsh-codegraph`.

`dsh-codegraph` spawns a fresh `codegraph` process per tool call. That model
works for CodeGraph because its index is a **persistent on-disk SQLite**
database. UKMA is different: the **code parse index is built per-process and
is not persisted** — only the parse cache (`.ukma/index.db`) is. Empirically
verified: `code.analyze_repository` in one `ukma` process does **not** make
`code.find_symbols` work in a *different* `ukma` process (the `RevisionIndex`
is keyed by `scope + repository_id + commit_sha` and lives only in the
process that ran the analyze).

So a per-call `ukma --run-tool <op>` cannot serve code queries. Instead, this
plugin holds **one long-lived `ukma --mcp --index-cwd <project>` child per
project** and speaks MCP JSON-RPC to it:

1. `initialize` handshake (gated by a 60s timeout).
2. On the first `code.*` tool call, run `code.analyze_repository`
   (`commit_sha: "@cwd"`, `asynchronous: true`) **in that same process** and
   wait for it to complete — this builds the in-process index.
3. Subsequent `code.*` calls reuse the same child and its index.

The child is disposed when the DSH host process exits (a single module-level
`process.on('exit')` handler), so it never outlives the session. One-shot
operations (setup commands, `system.*`, and the `ukma_call` escape hatch) use
a transient `ukma --run-tool` spawn, since they don't need the persistent
index.

---

## Install

The plugin is a dsh **bundle** (`dsh.bundle.patch` in `package.json` points at
`cordis.patch.yml`). Until it is a standalone repo, install it from a local
path or the published npm package (the `github:` install form points at a repo
root, not the `dsh-ukma/` subfolder):

```sh
# From a checkout of this repo (the plugin lives in dsh-ukma/):
dsh plugin --profile web add /path/to/UKMA/dsh-ukma

# Or, once published:
dsh plugin --profile web add @jacoblight-chen/dsh-ukma
```

`dsh` resolves the `peerDependencies` and will **reject** the install if the
profile's `dsh-llm` / `dsh-tools` versions are outside the supported range.

Prerequisites:
- A working `ukma` executable on `PATH`, or set the `bin` config (below) to
  its absolute path.
- The target project initialised with `ukma init` (or the `ukma_init` tool).

---

## Configuration

The plugin reads its config from the dsh bundle config object.

| key | type | default | meaning |
| --- | --- | --- | --- |
| `bin` | `string` | `""` (→ `ukma` on `PATH`) | Absolute path to the `ukma` executable. |
| `surface` | `'core' \| 'full'` | `'core'` | `core` registers the curated 13 tools; `full` adds the 17 structural pass-throughs (30 total). |
| `guideSearch` | `boolean` | `true` | Register the `tool:ukma` system-prompt section (order 98, below the built-in 100–104 block). Set `false` to disable. |
| `frontload` | `boolean` | `false` | Listen for user prompts entering an agent's inbox, gate by a confidence keyword, pre-run `ukma_search`, and steer the result into the turn as `<ukma_context>`. Off by default (it injects a visible steer message); enable deliberately. |

---

## Tool surface

`ukma_init` / `ukma_sync` / `ukma_uninit` / `ukma_status` and the
`ukma_capabilities` / `ukma_describe` / `ukma_call` trio are always
registered. `surface: 'core'` (default) adds the curated memory + code tools;
`surface: 'full'` adds the 17 structural pass-throughs.

| tool | operation | purpose |
| --- | --- | --- |
| `ukma_init` | — (one-shot `ukma init`) | Initialise `.ukma/` for a project (idempotent). |
| `ukma_sync` | — (one-shot `ukma sync`) | Re-scan / re-index the project. |
| `ukma_uninit` | — (one-shot `ukma uninit`) | Remove the `.ukma/` state. |
| `ukma_status` | `system.capabilities`-style | Report the current revision + manifest state. |
| `ukma_capabilities` | `system.capabilities` | Domain + operation counts (the "what can I do" entry point). |
| `ukma_describe` | `system.get_operation` | Self-serve the exact request schema of any operation (by `operation_id`). |
| `ukma_call` | any of the 65 | Escape hatch: dispatch any operation with a free-form `args` object. |
| `ukma_search` | `code.find_symbols` | Search symbols by name (auto-indexes on first use). |
| `ukma_symbol` | `code.find_symbols` | Look up a specific symbol by name + kind. |
| `ukma_explore` | `code.get_ai_context` | Assemble AI context around a set of target refs. |
| `ukma_search_pattern` | `code.search_by_pattern` | Text/pattern search across the codebase. |
| `ukma_memory_search` | `memory.search` | Search persistent Fractal Memory (trust-graded). |
| `ukma_memory_store` | `memory.store_episode` | Store a durable episode (decision / fact / procedure). |

`surface: 'full'` additionally registers 17 structural pass-throughs
(`ukma_callers`, `ukma_callees`, `ukma_impact`, `ukma_dependencies`,
`ukma_references`, `ukma_module_summary`, `ukma_circular_dependencies`,
`ukma_hot_paths`, `ukma_dead_imports`, `ukma_entry_points`,
`ukma_related_tests`, `ukma_complexity`, `ukma_find_by_signature`,
`ukma_search_error`, `ukma_get_edit_context`, `ukma_get_curated_context`,
`ukma_index_files`). Each takes a free-form `args` object; for `code.*`
operations the plugin auto-fills `repository_id` / `revision` (the `@cwd`
alias). Use `ukma_describe` for the exact request shape of any of them.

---

## How it works (the guidance section)

When `guideSearch` is on, the plugin registers a `tool:ukma` system-prompt
section at **order 98** (below the built-in 100–104 block, so it supplements
rather than overrides). It tells the agent, imperatively:

- Prefer the `ukma_*` tools over re-deriving what the index already knows.
- Call `ukma_search` before assuming a symbol is absent.
- Call `ukma_memory_search` before assuming a project fact is forgotten; store
  durable learnings with `ukma_memory_store`.
- Hard-stop: if the project is not indexed, run `ukma_init` / `ukma_sync`
  first — do not fabricate results.

---

## Tests

```sh
node test/run-plugin-test.mjs
UKMA_BIN=/path/to/ukma node test/run-plugin-test.mjs
```

The harness loads the real `lib/index.js`, mounts a `ctx` stub
(`tools.register` / `systemPrompt.section` / `get` / `on`), calls `apply()`,
and exercises the real `execute` paths **against a real `ukma` binary**.
When the dsh peer packages are not resolvable (a standalone checkout), the
harness self-stubs them into the plugin's `node_modules` for the duration of
the run and removes them on exit — the full execute path is verified either
way, so it works in a DSH profile, in CI, or standalone. The 12 sections
(58 checks):

- **Section 0** — environment + install gate: the `ukma` binary is resolvable
  (or `UKMA_BIN` is set) and the plugin loads.
- **Section 1** — mount + tool surface: `core` registers exactly the 13
  expected tools; `full` registers 30; `guideSearch=false` omits the guidance;
  the `tool:ukma` section is registered at order 98; the guidance text uses
  imperative wording and enumerates anti-patterns.
- **Section 2** — every tool is a well-formed `ToolDefinition`
  (`name` / `description` / `parameters` / `output.render` / `execute` /
  `presentCall`, with `presentCall` returning a tagged `card` view) across all
  30 tools, including `presentCall` on an empty `{}` and `undefined` args.
- **Section 3** — setup + lifecycle: `ukma_init` (fresh + idempotent),
  `ukma_status` (revision), `ukma_sync`, `ukma_uninit` → re-`init`, and a tool
  with no `cwd` and no `path` throws a clear error.
- **Section 4** — code tools: `ukma_search` (auto-index via the persistent
  `--mcp` child, finds a seeded symbol, output carries a `symbol_id`),
  `ukma_search_pattern` (session reuse), `ukma_symbol`, and `ukma_explore`
  (name→symbol-id resolution, then intent-aware context).
- **Section 5** — system + memory + escape hatch: `ukma_capabilities`,
  `ukma_describe`, `ukma_memory_store` (change receipt), `ukma_memory_search`
  (valid page), and `ukma_call` on a `system.*` op, a `code.*` op (binding
  fill + auto-index), and a no-binding op.
- **Section 6** — `Config` validation (rejects an invalid `surface` value) and
  explicit overrides end-to-end (`full` + `guideSearch: false` + `frontload:
  true`).
- **Section 7** — error paths + robustness: unknown op via `ukma_describe` /
  `ukma_call`, an invalid `detail` level (schema error), a missing binary
  (actionable install hint), and repeated `apply()` (×3) does not throw.
- **Section 8** — the structural pass-throughs on a real project:
  `ukma_callers` (TraversalQuery), `ukma_references` (ReferenceQuery),
  `ukma_impact` (ImpactQuery with `@cwd` revision self-heal), and a
  non-existent symbol id errors cleanly.
- **Section 9** — prompt front-load behaviour: steers `<ukma_context>` for a
  prompt naming a verified symbol (marker + evidence in the steered message),
  dedups an identical prompt, and is a no-op for token-less prompts and for
  non-user messages.
- **Section 10** — the front-load inbox listener is registered only when
  `frontload: true` (off vs on, on two independent mounts).
- **Section 11** — the 25 op ids the plugin uses are all present in the 125-op
  `operation-registry.yaml` (skipped in a standalone checkout without the repo).

**Verified green (58/58)** against a release `ukma` build.

**Memory visibility boundary.** `ukma_memory_store` commits an episode under
the session's **sandbox scope** (session/agent identity). Memory visibility
is sandbox-scoped by design: `ukma_memory_search` returns only items visible
to the current context — in a single-context local deployment that is the
semantic store (the plugin's episodes are stored, durably, but not visible to
the dev context, which carries no session/agent identity); in a multi-agent
deployment with a matching identity, the sandbox's episodes are visible. The
harness therefore asserts the write path (change receipt) and a valid search
page, and does not assert cross-sandbox visibility.

---

## Honest boundaries

- dsh is a **developer preview**; the plugin API has changed across 0.1.x and
  0.2.x. This plugin targets the same API surface `dsh-codegraph` supports and
  is peer-gated to `<0.3.0-0`.
- The `github:` install form points at a **repo root**, not the `dsh-ukma/`
  subfolder — use a local path or the published npm package.
- The code index is **per-process**; the persistent `--mcp` child is what makes
  code queries work across tool calls. A fresh DSH session re-analyzes on the
  first `code.*` call (the parse cache makes this cheap on re-runs).
- The `ukma` JSON file adapter is development-only: it makes **no
  cross-process safety or durability guarantee**. Run one DSH session per
  project when the memory/revision state matters.
