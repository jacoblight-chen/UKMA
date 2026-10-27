// dsh-ukma — UKMA for DSH (standalone, installable DSH plugin)
//
// Registers native model tools wrapping the `ukma` CLI
// (https://github.com/jacoblight-chen/UKMA). The agent can bootstrap a
// project's pre-indexed code knowledge (`.ukma/`), maintain it (sync),
// query it (search / symbol / AI-context / structural), and use UKMA's
// Fractal Memory (memory search / store) without grepping files or re-reading
// its own history.
//
// WHY A LONG-LIVED `ukma --mcp` CHILD (not per-call `--run-tool`):
// UKMA's code *parse* index (the assembled symbol/edge index that backs
// code.find_symbols, code.get_ai_context, ...) is built by
// code.analyze_repository and lives in the PROCESS that built it. A
// per-call `ukma --run-tool <op>` spawns a fresh process, so the index it
// would need is never present (verified empirically: analyze in process A
// does not make find_symbols work in process B). The parse *cache*
// (`.ukma/index.db`) persists the raw parses, but not the assembled index.
// So this plugin holds ONE long-lived `ukma --mcp --index-cwd <path>` child
// per project and speaks the MCP stdio protocol to it: the index is built
// once (code.analyze_repository) and reused for every subsequent call.
//
// Setup commands (`ukma init/sync/status/uninit`) are stateless one-shots
// (they never index; they write `.ukma/` and exit), so they run as a fresh
// `ukma` spawn each call.
//
// Tool surface (config `surface`, default 'core'):
//   always  — status/init/sync/uninit (setup) + capabilities + describe +
//            the `ukma_call` escape hatch (any of the 125 operations)
//   core    — + search/symbol/explore/search_pattern (code) +
//            memory_search/memory_store (memory)
//   full    — + structural pass-throughs (callers/callees/impact/...)
// `ukma_describe` wraps system.get_operation so the model can self-serve the
// exact request shape for any operation before `ukma_call`.
//
// Wording of the system-prompt guidance follows the dsh-codegraph strategy:
// imperative ("MUST use ... INSTEAD of"), explicit anti-patterns, and a hard
// stop rule for unindexed projects. A soft "Prefer ..." phrasing measurably
// gets ignored. Rendered at order 98 — before the built-in tool block
// (read=100, write=101, edit=102, glob=103, grep=104) — so the model sees
// "reach for ukma_* first".
//
// Prompt front-load (config `frontload`, default FALSE): the dsh-codegraph
// prompt-hook equivalent. Off by default because the injected context is a
// visible steer message (DSH has no invisible context channel) and dsh-
// codegraph's author disabled it locally for the same reason. Enable in
// cordis.patch.yml if you want the pre-run of ukma_search on structural
// prompts. Every failure path is a silent no-op.
//
// Execution: this plugin is a node module, so it spawns the `ukma` binary
// directly via node:child_process (resolved from PATH, or the `bin` config).
// That is what lets it hold a long-lived `--mcp` child AND run one-shot
// commands with one mechanism, with no dependence on the host's executor
// services (which are one-shot and would race their own mount at apply time).
//
// Export shape: namespace plugin (named exports, no default) — the same shape
// as @deepseek-ai/dsh-tool-cordis and the other @local/* static plugins.
// `defineTool` resolves from `@deepseek-ai/dsh-tools`, declared as a normal
// dependency and installed by pnpm alongside this package.
//
// NOTE TO INSTALLERS: the peer range below IS the compatibility gate. DSH 0.2.0
// reads `peerDependencies` for every `@deepseek-ai/dsh*` entry and rejects the
// install when the running version fails `semver.satisfies(v, range,
// {includePrerelease: true})`. A `<0.2.0-0` ceiling excludes every 0.2.x
// prerelease and hard-fails `dsh plugin add`, so the ceiling is `<0.3.0-0`.
// Keep it that way; a too-narrow range is a failed install, not a warning.

import { spawn } from 'node:child_process'
import { defineTool } from '@deepseek-ai/dsh-tools'
import { createUserMessage } from '@deepseek-ai/dsh-llm'
import z from '@deepseek-ai/schemastery'

export const name = 'dsh-ukma'

export const inject = ['tools', 'systemPrompt']

// Plugin config.
// - guideSearch (default true): when set, a high-priority system-prompt
//   section tells the model to reach for ukma_* before grep/glob/read for
//   code search, and to check ukma memory before assuming it has forgotten.
// - surface (default 'core'): which tools to register (see header).
// - bin (default ''): absolute path to the `ukma` binary. Empty = resolve
//   from PATH. Set this in cordis.patch.yml if `ukma` is not on PATH.
// - frontload (default false): pre-run ukma_search on structural prompts and
//   steer the result into the turn as <ukma_context>. Off by default (the
//   injected message is visible in the transcript).
export const Config = z.object({
  guideSearch: z.boolean().default(true),
  surface: z.union([z.const('core'), z.const('full')]).default('core'),
  bin: z.string().default(''),
  frontload: z.boolean().default(false)
})

// The repository_id this plugin binds every code revision to. UKMA keys its
// parse index by (scope, repository_id, commit_sha); the caller supplies the
// repository_id, so the plugin picks one stable value and uses it for both
// analyze_repository (build) and every query (read). `revision` is `@cwd`,
// which the server resolves to the indexed tree's content-addressed revision.
const REPOSITORY_ID = 'repo'
const CWD_REVISION = '@cwd'

// Always present (both surfaces).
const ALWAYS_TOOLS = new Set([
  'ukma_status',
  'ukma_init',
  'ukma_sync',
  'ukma_uninit',
  'ukma_capabilities',
  'ukma_describe',
  'ukma_call'
])

// The core query surface (the minimal set that bootstraps the index and
// answers most code + memory questions).
const CORE_TOOLS = new Set([
  'ukma_search',
  'ukma_symbol',
  'ukma_explore',
  'ukma_search_pattern',
  'ukma_memory_search',
  'ukma_memory_store'
])

// Structural / relational pass-throughs, registered only under surface: 'full'.
const FULL_TOOLS = new Set([
  'ukma_callers',
  'ukma_callees',
  'ukma_impact',
  'ukma_dependencies',
  'ukma_references',
  'ukma_module_summary',
  'ukma_circular_deps',
  'ukma_hot_paths',
  'ukma_dead_imports',
  'ukma_entry_points',
  'ukma_related_tests',
  'ukma_complexity',
  'ukma_find_by_signature',
  'ukma_search_error',
  'ukma_get_edit_context',
  'ukma_get_curated_context',
  'ukma_index_files'
])

// Which code-domain operations require the parse index to be built first.
function needsCodeIndex(operationId) {
  return operationId.startsWith('code.')
}

// System-prompt guidance that makes the ukma tools the preferred route.
// Rendered at order 98 (before the built-in tool block, order >= 100) so the
// model sees "reach for ukma_* first" for code search and memory recall.
const UKMA_GUIDE = `# UKMA — pre-indexed code graph + fractal memory

UKMA is a pre-computed graph of every symbol and call edge in the project, plus a persistent memory of what was already done here. When the current project is indexed (a \`.ukma/\` directory at the project root; confirm once with ukma_status), you MUST use the ukma_* tools INSTEAD of grep/glob/read to find or understand code:
- Call ukma_search BEFORE any grep/glob/read whenever you need to locate a symbol. If the project is not yet indexed (ukma_status shows it is), run ukma_init once, then ukma_search.
- To understand a flow or read the code you can name, call ukma_explore with the symbol names from ukma_search (intent "explain"). It assembles the relevant context in one call.
- After editing source files, run ukma_sync so later ukma_search/ukma_explore calls reflect the new code.
- Before assuming you have forgotten a fact about this project, call ukma_memory_search. It searches the project's persistent memory. When you learn something durable (a decision, a non-obvious fact, a procedure), store it with ukma_memory_store.
- For structural queries (callers, callees, impact, dependencies, module summary, ...) use ukma_call with the operation id; ukma_describe returns the exact request shape for any operation.

Anti-patterns — do NOT:
- grep/glob first "to find the files": that repeats work the index already did, in dozens of round-trips instead of one call.
- re-verify ukma results with grep: they come from a full AST parse.
- keep calling ukma tools after one reports the project is not indexed and you did not initialize it: stop calling them for the rest of the session and use the built-in tools instead. Indexing is the user's decision.
- assume a memory answer is stale without checking: ukma_memory_search returns the stored, trust-graded content.`

// ---- helpers ---------------------------------------------------------------

function stripAnsi(text) {
  return String(text).replace(/\x1B\[[0-9;]*[A-Za-z]/g, '')
}

function truncate(text, max) {
  if (text.length <= max) return text
  return text.slice(0, max) + '\n... [truncated] ...'
}

function executableHint(bin) {
  return (
    '`ukma` was not found' +
    (bin ? ` at ${bin}` : ' on PATH') +
    '. Install it once (e.g. `irm https://raw.githubusercontent.com/jacoblight-chen/UKMA/main/scripts/install.ps1 | iex` on Windows, or `curl -fsSL https://raw.githubusercontent.com/jacoblight-chen/UKMA/main/scripts/install.sh | sh` elsewhere), ' +
    (bin ? 'or fix the `bin` config to point at the binary.' : 'or set the `bin` config to the binary path.')
  )
}

// Resolve the ukma binary path: the explicit `bin` config, else `ukma` on
// PATH (spawn resolves it; we only surface a clear error when spawn fails
// with ENOENT).
function ukmaBin(bin) {
  return bin && bin.length > 0 ? bin : 'ukma'
}

// One-shot command (setup): spawn `ukma <argv...>` and return the combined
// stdout (stderr is folded in on failure). A non-zero exit throws.
function runUkmaOneShot({ argv, cwd, bin, signal, timeoutMs }) {
  return new Promise((resolve, reject) => {
    const binPath = ukmaBin(bin)
    let child
    try {
      child = spawn(binPath, argv, {
        cwd: cwd || process.cwd(),
        stdio: ['ignore', 'pipe', 'pipe'],
        windowsHide: true
      })
    } catch (error) {
      reject(new Error(`dsh-ukma: cannot spawn ukma: ${error.message}`))
      return
    }
    let stdout = ''
    let stderr = ''
    let settled = false
    const timer =
      timeoutMs !== undefined
        ? setTimeout(() => {
            if (!settled) {
              settled = true
              try { child.kill() } catch { /* already gone */ }
              reject(new Error(`ukma ${argv.join(' ')} timed out after ${timeoutMs}ms`))
            }
          }, timeoutMs)
        : undefined
    if (signal) {
      if (signal.aborted) {
        if (!settled) {
          settled = true
          try { child.kill() } catch { /* already gone */ }
          reject(new Error('aborted before start'))
        }
        return
      }
      signal.addEventListener('abort', () => {
        if (!settled) {
          settled = true
          try { child.kill() } catch { /* already gone */ }
          reject(new Error('aborted'))
        }
      })
    }
    child.stdout.on('data', (d) => { stdout += d })
    child.stderr.on('data', (d) => { stderr += d })
    child.on('error', (error) => {
      if (!settled) {
        settled = true
        if (timer) clearTimeout(timer)
        if (error && error.code === 'ENOENT') reject(new Error('dsh-ukma: ' + executableHint(binPath)))
        else reject(new Error(`dsh-ukma: ${error.message}`))
      }
    })
    child.on('close', (code) => {
      if (!settled) {
        settled = true
        if (timer) clearTimeout(timer)
        const out = stripAnsi(stdout)
        const err = stripAnsi(stderr)
        if (code !== 0) {
          const detail = err && err.trim() ? err.trim() : out && out.trim()
          reject(
            new Error(
              detail ? `ukma exited ${code}: ${truncate(detail, 2000)}` : `ukma exited ${code}`
            )
          )
        } else {
          resolve(out)
        }
      }
    })
  })
}

// ---- persistent `ukma --mcp` child (the code index lives here) -------------

// One long-lived `ukma --mcp` child per project. It speaks the MCP stdio
// protocol: line-delimited JSON-RPC. `initialize` once, then `tools/call` per
// operation. The code parse index is built in THIS process (by
// code.analyze_repository) and persists across calls — the whole reason this
// is a child process rather than per-call spawns.
class McpSession {
  constructor(project, bin, initializeTimeoutMs = 60_000) {
    this.project = project
    this.bin = bin
    this.initializeTimeoutMs = initializeTimeoutMs
    this.child = null
    this.ready = null
    this.nextId = 0
    this.pending = new Map()
    this.buffer = ''
    this.closed = false
  }

  start(signal) {
    if (this.ready) return this.ready
    this.ready = new Promise((resolve, reject) => {
      const argv = ['--mcp', '--index-cwd', this.project]
      let child
      try {
        // Do NOT set cwd to the project: the child only needs --index-cwd, and
        // inheriting the parent's cwd keeps the project directory free (so a
        // host can still clean it up on Windows, where a live cwd is locked).
        child = spawn(ukmaBin(this.bin), argv, {
          stdio: ['pipe', 'pipe', 'pipe'],
          windowsHide: true
        })
      } catch (error) {
        this.ready = null
        reject(new Error(`dsh-ukma: cannot spawn ukma --mcp: ${error.message}`))
        return
      }
      this.child = child
      child.on('error', (error) => {
        this.closed = true
        if (error && error.code === 'ENOENT') {
          const msg = 'dsh-ukma: ' + executableHint(ukmaBin(this.bin))
          this.failAll(new Error(msg))
          this.ready = null
          reject(new Error(msg))
        } else {
          this.failAll(new Error(`dsh-ukma: ${error.message}`))
        }
      })
      child.on('close', (code) => {
        this.closed = true
        if (code !== 0 && code !== null) {
          this.failAll(new Error(`ukma --mcp exited ${code}`))
        } else {
          this.failAll(new Error('ukma --mcp child closed'))
        }
      })
      child.stdout.on('data', (d) => this.onData(d))
      if (signal) {
        signal.addEventListener('abort', () => this.dispose())
      }
      // The initialize handshake: the child must answer before any tools/call.
      this.request('initialize', {}, this.initializeTimeoutMs).then(
        () => resolve(),
        (error) => {
          this.ready = null
          this.dispose()
          reject(error)
        }
      )
    })
    return this.ready
  }

  onData(chunk) {
    this.buffer += chunk.toString()
    let nl
    while ((nl = this.buffer.indexOf('\n')) >= 0) {
      const line = this.buffer.slice(0, nl).trim()
      this.buffer = this.buffer.slice(nl + 1)
      if (!line) continue
      let parsed
      try {
        parsed = JSON.parse(line)
      } catch {
        continue // a malformed line is not a response to a pending id
      }
      const id = parsed.id
      if (id === undefined || id === null) continue
      const entry = this.pending.get(id)
      if (!entry) continue
      this.pending.delete(id)
      clearTimeout(entry.timer)
      if (parsed.error) entry.reject(new Error(`mcp error ${parsed.error.code}: ${parsed.error.message}`))
      else entry.resolve(parsed.result)
    }
  }

  request(method, params, timeoutMs) {
    if (this.closed) return Promise.reject(new Error('ukma --mcp child is closed'))
    const id = ++this.nextId
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id)
        reject(new Error(`mcp ${method} timed out after ${timeoutMs}ms`))
      }, timeoutMs)
      this.pending.set(id, { resolve, reject, timer })
      const message = JSON.stringify({ jsonrpc: '2.0', id, method, params })
      try {
        this.child.stdin.write(message + '\n')
      } catch (error) {
        this.pending.delete(id)
        clearTimeout(timer)
        reject(error)
      }
    })
  }

  // One tools/call. Returns the envelope text (the pretty-printed JSON body)
  // and surfaces a tool-level error (isError) as a thrown Error whose message
  // is the gateway's `<CODE>: <message>`.
  toolsCall(toolName, args, signal, timeoutMs) {
    return this.start(signal).then(() =>
      this.request('tools/call', { name: toolName, arguments: args }, timeoutMs)
    ).then((result) => {
      const content = (result && result.content) || []
      const text = content
        .filter((block) => block && block.type === 'text')
        .map((block) => block.text)
        .join('\n')
      if (result && result.isError) {
        // A tool-level failure: the message is `<CODE>: <message>`.
        throw new Error(text || 'ukma tool call failed')
      }
      return text
    })
  }

  failAll(error) {
    for (const entry of this.pending.values()) {
      clearTimeout(entry.timer)
      entry.reject(error)
    }
    this.pending.clear()
  }

  dispose() {
    if (this.child) {
      try { this.child.kill() } catch { /* already gone */ }
    }
  }
}

// Track every session so the plugin can dispose them on process exit (the
// children would otherwise outlive the DSH host).
const activeSessions = new Map() // project -> McpSession
const trackedChildren = []

// Dispose all persistent --mcp children when the host process exits. Registered
// once at module load (not per apply()), so repeated apply() — e.g. in a test
// harness — does not stack exit listeners and trip the MaxListeners warning.
if (typeof process !== 'undefined' && typeof process.on === 'function') {
  process.on('exit', () => {
    for (const session of trackedChildren) session.dispose()
  })
}

function sessionFor(project, bin, signal) {
  let session = activeSessions.get(project)
  if (!session) {
    session = new McpSession(project, bin)
    activeSessions.set(project, session)
    trackedChildren.push(session)
  }
  return session
}

// Build the `code.analyze_repository` input for the cwd revision. `asynchronous:
// true` is required by the schema; in the in-process reference deployment the
// analyze runs to completion before the call returns, so the index is ready.
const analyzeInput = () => ({
  revision: {
    repository_id: REPOSITORY_ID,
    vcs: 'git',
    commit_sha: CWD_REVISION,
    tree_sha: null,
    dirty_policy: 'snapshot'
  },
  parser_profile: 'default',
  include_globs: [],
  exclude_globs: [],
  asynchronous: true
})

// The code index, built once per session. Idempotent: a second call reuses
// the parse cache and is cheap.
const codeIndexEnsured = new Set() // session identity -> true
function ensureCodeIndex(session, signal) {
  if (codeIndexEnsured.has(session)) return Promise.resolve()
  return session
    .toolsCall('ukma_code_analyze_repository_v1', analyzeInput(), signal, 300000)
    .then(() => {
      codeIndexEnsured.add(session)
    })
}

// ---- plugin ---------------------------------------------------------------

export function apply(ctx, config = {}) {
  const tools = ctx.tools
  const {
    guideSearch = true,
    surface = 'core',
    bin = '',
    frontload = false
  } = config

  const toolEnabled = (toolName) =>
    surface === 'full' || ALWAYS_TOOLS.has(toolName) || CORE_TOOLS.has(toolName)

  // Make the ukma_* tools the preferred route for code search + memory recall:
  // instruct the model BEFORE the built-in grep/glob/read guidance (order < 100)
  // to reach for the indexed graph first. Adjustable via `guideSearch`.
  if (guideSearch !== false) {
    ctx.systemPrompt.section({
      name: 'tool:ukma',
      order: 98,
      text: UKMA_GUIDE
    })
  }

  const cwdOf = (exec) => {
    const agent = exec && exec.agent
    const header = agent && agent.session && agent.session.header
    return header && header.cwd ? header.cwd : undefined
  }

  // A tool's project root: the explicit `path` arg, else the session cwd.
  const projectOf = (args, exec) => {
    const explicit = typeof args.path === 'string' && args.path.length > 0
    if (explicit) return args.path
    const base = cwdOf(exec)
    if (!base) {
      throw new Error(
        'ukma: no session workspace (cwd) available; pass the project path explicitly via the `path` argument.'
      )
    }
    return base
  }

  // The `ukma` operation id (dot form) -> MCP tool name (underscore form).
  const toolNameFor = (operationId) =>
    'ukma_' + operationId.replace(/\./g, '_') + '_v1'

  // Fill the @cwd binding for a code.* request input. Two shapes exist:
  //   - simple (find_symbols, get_ai_context, search_by_pattern, ...): a
  //     top-level `revision` alias (the string "@cwd") + a top-level
  //     `repository_id`.
  //   - nested (analyze_repository): a `revision` object that carries its own
  //     repository_id / commit_sha — and has NO top-level repository_id, which
  //     the `additionalProperties: false` schema would reject if we added one.
  // Only fills fields the caller omitted; never overwrites provided values.
  const fillCodeBinding = (input) => {
    const nested = input.revision && typeof input.revision === 'object'
    if (nested) {
      if (input.revision.commit_sha === undefined) input.revision.commit_sha = CWD_REVISION
      if (input.revision.repository_id === undefined) input.revision.repository_id = REPOSITORY_ID
    } else {
      if (input.revision === undefined) input.revision = CWD_REVISION
      if (input.repository_id === undefined) input.repository_id = REPOSITORY_ID
    }
  }

  // Run one operation through the persistent MCP child. `ensure` controls
  // whether the code index is built first (code.* operations need it).
  function callOperation(project, operationId, input, exec, timeoutMs) {
    const session = sessionFor(project, bin, exec.signal)
    const ensure = needsCodeIndex(operationId)
      ? ensureCodeIndex(session, exec.signal)
      : Promise.resolve()
    return ensure
      .then(() => session.toolsCall(toolNameFor(operationId), input, exec.signal, timeoutMs))
      .catch((error) => {
        // Self-heal: some operations (e.g. code.analyze_impact) compare the
        // request's revision against the index's LITERAL commit id and do not
        // resolve the '@cwd' alias. When the gateway reports the mismatch,
        // retry once with the revision the index was actually built at.
        const msg = String((error && error.message) || '')
        const m = msg.match(/the index is ([0-9a-f]{16,})/)
        if (m && input && typeof input === 'object' && input.revision === CWD_REVISION) {
          return session.toolsCall(toolNameFor(operationId), { ...input, revision: m[1] }, exec.signal, timeoutMs)
        }
        throw error
      })
  }

  function registerTool(name, description, parameters, executeFn, opts) {
    if (!toolEnabled(name)) return
    opts = opts || {}
    const tool = defineTool({
      name,
      description,
      parameters,
      output: {
        schema: { type: 'string' },
        render: (_args, value) => [{ type: 'text', text: String(value) }]
      },
      ...(opts.timeoutMs !== undefined ? { timeoutMs: opts.timeoutMs } : {}),
      async execute(args, exec) {
        exec.signal.throwIfAborted()
        return executeFn(args, exec)
      },
      presentCall: (args) => ({
        card: 'terminal',
        title: opts.callTitle ? opts.callTitle(args) : 'ukma',
        description: opts.callDescription
      })
    })
    tools.register(tool)
  }

  const PATH_PARAM = {
    path: { type: 'string', description: 'Project directory (default: the current session workspace).' }
  }

  // ---- setup (one-shot `ukma` spawns) --------------------------------------

  registerTool(
    'ukma_status',
    'Show the UKMA index state of a project: the manifest, the snapshot revision, the parse entry count, pending (unsynced) files, and the daemon status. Run this first to confirm the project is initialized before querying. Read-only; does not rebuild the index.',
    { ...PATH_PARAM },
    (args, exec) =>
      runUkmaOneShot({ argv: ['status', projectOf(args, exec)], bin, signal: exec.signal, timeoutMs: 60000 }),
    { callDescription: 'Show the UKMA index status' }
  )

  registerTool(
    'ukma_init',
    'Initialize UKMA in a project directory: create .ukma/project.json and .ukma/index.db and build the initial snapshot. Run once per project before querying with ukma_search/explore. Requires the ukma binary on PATH (or the `bin` config).',
    { ...PATH_PARAM },
    (args, exec) =>
      runUkmaOneShot({ argv: ['init', projectOf(args, exec)], bin, signal: exec.signal, timeoutMs: 600000 }),
    { callDescription: 'Initialize the UKMA index' }
  )

  registerTool(
    'ukma_sync',
    'Incrementally refresh .ukma/index.db with files whose (size, mtime) changed. Run after editing source files so ukma_search/ukma_explore reflect the new code. An optional `force` flag discards the cache and rescans the whole tree.',
    {
      ...PATH_PARAM,
      force: { type: 'boolean', description: 'Discard the cache and rescan the whole tree.' }
    },
    (args, exec) => {
      const argv = ['sync', projectOf(args, exec)]
      if (args.force) argv.push('--force')
      return runUkmaOneShot({ argv, bin, signal: exec.signal, timeoutMs: 600000 })
    },
    { callDescription: 'Sync the UKMA index' }
  )

  registerTool(
    'ukma_uninit',
    'Remove UKMA from a project by deleting its .ukma/ directory. Use only when the index is no longer needed.',
    { ...PATH_PARAM },
    (args, exec) =>
      runUkmaOneShot({ argv: ['uninit', projectOf(args, exec), '--yes'], bin, signal: exec.signal, timeoutMs: 120000 }),
    { callDescription: 'Remove the UKMA index' }
  )

  // ---- system ------------------------------------------------------------

  registerTool(
    'ukma_capabilities',
    'Report the UKMA system capabilities: the domain operation counts (code/document/memory/traceability/pro), the API and spec versions, and the schema status. No index required.',
    {},
    (args, exec) => {
      const project = cwdOf(exec)
      if (!project) throw new Error('ukma: no session workspace (cwd) available.')
      return callOperation(project, 'system.capabilities', {}, exec, 60000)
    },
    { callDescription: 'Show the UKMA capabilities' }
  )

  registerTool(
    'ukma_describe',
    'Return the full registry spec for one operation (its request and response schema, scopes, preconditions, state machine, error codes). Use this to learn the exact request shape before calling ukma_call. Accepts an operation id (e.g. "code.query_callers").',
    {
      operation: { type: 'string', required: true, description: 'The operation id, e.g. "code.query_callers" or "memory.search".' }
    },
    (args, exec) => {
      const project = cwdOf(exec)
      if (!project) throw new Error('ukma: no session workspace (cwd) available.')
      return callOperation(project, 'system.get_operation', { operation_id: args.operation }, exec, 60000)
    },
    { callDescription: 'Describe one operation (schema self-serve)' }
  )

  // ---- the escape hatch: any of the 125 operations ------------------------

  registerTool(
    'ukma_call',
    'Call any registered UKMA operation by id with an explicit request input. This is the general path for operations not exposed as a named tool (structural analysis, traceability, document, pro security, ...). Use ukma_describe to learn the request shape first. For code.* operations the index is built automatically on first use.',
    {
      ...PATH_PARAM,
      operation: { type: 'string', required: true, description: 'The operation id, e.g. "code.analyze_impact" or "memory.search".' },
      args: {
        type: 'object',
        description:
          'The request input object for the operation. For code.* operations, repository_id and revision are filled in for you (repository_id is fixed, revision is @cwd) when omitted.'
      }
    },
    (args, exec) => {
      const project = projectOf(args, exec)
      const operation = args.operation
      let input = args.args && typeof args.args === 'object' ? { ...args.args } : {}
      // Fill the cwd binding for code.* operations when the caller omits them.
      if (operation.startsWith('code.')) fillCodeBinding(input)
      const heavy = /analyze|consolidate|scan|orchestrated/.test(operation)
      return callOperation(project, operation, input, exec, heavy ? 300000 : 120000)
    },
    {
      callDescription: 'Call any UKMA operation',
      callTitle: (args) => `ukma ${(args && args.operation) || 'call'}`
    }
  )

  // ---- core code surface -------------------------------------------------

  registerTool(
    'ukma_search',
    "Search symbols in the codebase by name/substring and return matching symbols (name, qualified name, kind, definition location, language). Use to locate a symbol before reading its source with ukma_explore. Requires an initialized project (run ukma_init first); the index is built automatically on first use.",
    {
      ...PATH_PARAM,
      query: { type: 'string', required: true, description: 'Symbol name or partial name to search for (e.g. "greet", "fetchUser").' },
      kinds: { type: 'array', items: { type: 'string' }, description: 'Restrict to these symbol kinds (empty/omitted = all).' },
      limit: { type: 'integer', description: 'Maximum results (default 20).' }
    },
    (args, exec) => {
      const project = projectOf(args, exec)
      const input = {
        repository_id: REPOSITORY_ID,
        revision: CWD_REVISION,
        query: args.query,
        kinds: Array.isArray(args.kinds) ? args.kinds : [],
        path_glob: null,
        language: null,
        page: { cursor: null, limit: args.limit || 20 }
      }
      return callOperation(project, 'code.find_symbols', input, exec, 120000)
    },
    { callDescription: 'Search symbols in the codebase' }
  )

  registerTool(
    'ukma_symbol',
    "Get one symbol's full record by its symbol_id (from ukma_search): definition location, signature, and relations. Requires an initialized project.",
    {
      ...PATH_PARAM,
      symbol_id: { type: 'string', required: true, description: 'The symbol id (from a ukma_search result).' }
    },
    (args, exec) => {
      const project = projectOf(args, exec)
      return callOperation(project, 'code.get_symbol', { symbol_id: args.symbol_id }, exec, 120000)
    },
    { callDescription: 'Show one symbol by id' }
  )

  registerTool(
    'ukma_explore',
    "Assemble intent-aware context for named symbols in one call (the UKMA equivalent of an 'explore'): returns the relevant sections bounded by a token budget. Pass symbol names (resolved to their indexed symbol ids) or symbol ids from ukma_search, plus an intent ('explain', 'edit', or 'curated'). Requires an initialized project.",
    {
      ...PATH_PARAM,
      target_refs: { type: 'array', items: { type: 'string' }, required: true, description: 'Symbol names or ids to assemble context for (from ukma_search).' },
      intent: { type: 'string', description: "The intent: 'explain' (default), 'edit', or 'curated'." },
      max_tokens: { type: 'integer', description: 'Token budget for the assembled context (default 4000).' }
    },
    (args, exec) => {
      const project = projectOf(args, exec)
      const rawRefs = (Array.isArray(args.target_refs) ? args.target_refs : [])
        .filter((ref) => typeof ref === 'string' && ref.length > 0)
      if (rawRefs.length === 0) {
        return Promise.reject(
          new Error('ukma_explore: target_refs must name at least one symbol (a symbol name or a symbol id from ukma_search)')
        )
      }
      // code.get_ai_context resolves target_refs against LITERAL symbol ids,
      // so a bare name is resolved first via code.find_symbols (whose first
      // call also builds the index through callOperation).
      const isSymbolId = (ref) => /^[a-z0-9]+:v\d+:[0-9a-f]{8,}$/i.test(ref)
      const resolveAll = rawRefs.reduce(
        (acc, ref) =>
          acc.then(async (refs) => {
            if (isSymbolId(ref)) {
              refs.push(ref)
              return refs
            }
            const out = await callOperation(
              project,
              'code.find_symbols',
              {
                repository_id: REPOSITORY_ID,
                revision: CWD_REVISION,
                query: ref,
                kinds: [],
                path_glob: null,
                language: null,
                page: { cursor: null, limit: 5 }
              },
              exec,
              120000
            )
            const m = out.match(/sym:v1:[0-9a-f]+/)
            if (!m) {
              throw new Error(`ukma_explore: no symbol matching "${ref}" (run ukma_search to confirm the name)`)
            }
            refs.push(m[0])
            return refs
          }),
        Promise.resolve([])
      )
      return resolveAll.then((refs) =>
        callOperation(
          project,
          'code.get_ai_context',
          {
            repository_id: REPOSITORY_ID,
            revision: CWD_REVISION,
            intent: args.intent || 'explain',
            max_tokens: args.max_tokens || 4000,
            target_refs: refs
          },
          exec,
          120000
        )
      )
    },
    { callDescription: 'Assemble context for named symbols (explore)' }
  )

  registerTool(
    'ukma_search_pattern',
    'Search the codebase by a pattern (e.g. a regex or a name pattern) and return matching locations. The `scope` selects name vs body matching. Requires an initialized project.',
    {
      ...PATH_PARAM,
      pattern: { type: 'string', required: true, description: 'The pattern to search for.' },
      scope: { type: 'string', description: "Match scope: 'name' (default) or 'body'." },
      limit: { type: 'integer', description: 'Maximum results (default 20).' }
    },
    (args, exec) => {
      const project = projectOf(args, exec)
      const input = {
        repository_id: REPOSITORY_ID,
        revision: CWD_REVISION,
        pattern: args.pattern,
        scope: args.scope || 'name',
        page: { limit: args.limit || 20 }
      }
      return callOperation(project, 'code.search_by_pattern', input, exec, 120000)
    },
    { callDescription: 'Search the codebase by pattern' }
  )

  // ---- core memory surface -----------------------------------------------

  registerTool(
    'ukma_memory_search',
    "Search the project's persistent Fractal Memory (decisions, facts, procedures, past work) and return trust-graded results. Call this before assuming you have forgotten something about the project.",
    {
      ...PATH_PARAM,
      query: { type: 'string', required: true, description: 'The natural-language query.' },
      max_results: { type: 'integer', description: 'Maximum results (default 10).' },
      max_tokens: { type: 'integer', description: 'Token budget for the response (default 4000; must be > 0).' },
      detail: { type: 'string', description: "Detail level: 'title' | 'facts' | 'summary' | 'full' (default 'summary')." }
    },
    (args, exec) => {
      const project = projectOf(args, exec)
      const input = {
        query: {
          query: args.query,
          // Empty kinds searches every store.
          kinds: [],
          max_results: args.max_results || 10,
          // The domain requires max_tokens > 0 (a zero budget is a validation
          // error), so default to a positive budget.
          max_tokens: args.max_tokens || 4000,
          detail: args.detail || 'summary'
        }
      }
      return callOperation(project, 'memory.search', input, exec, 120000)
    },
    { callDescription: 'Search the project memory' }
  )

  registerTool(
    'ukma_memory_store',
    "Store a durable episode in the project's Fractal Memory: a decision, a non-obvious fact, or a procedure the agent should remember. Provide a summary (and optionally key facts / source references). The episode is committed under this session's sandbox scope (session/agent identity); memory visibility is sandbox-scoped by design, so a later ukma_memory_search only sees items visible to the current context (in a single-context local deployment that is the semantic store; in a multi-agent deployment with matching identity, this sandbox's episodes are visible).",
    {
      ...PATH_PARAM,
      summary: { type: 'string', required: true, description: 'A concise summary of what to remember.' },
      key_facts: { type: 'array', items: { type: 'string' }, description: 'Discrete key facts (optional).' },
      episode_type: { type: 'string', description: "The episode type (default 'conversation_turn')." }
    },
    (args, exec) => {
      const project = projectOf(args, exec)
      const input = {
        input: {
          agent_id: 'dsh-ukma',
          session_id: 'dsh-ukma',
          episode_type: args.episode_type || 'conversation_turn',
          summary: args.summary,
          key_facts: Array.isArray(args.key_facts) ? args.key_facts : [],
          source_refs: [],
          evidence: [],
          language: '',
          outcome: 'success',
          agent_depth: 0,
          importance_score: 1
        }
      }
      return callOperation(project, 'memory.store_episode', input, exec, 120000)
    },
    { callDescription: 'Store a durable memory episode' }
  )

  // ---- full surface: structural pass-throughs ----------------------------
  // These forward a free-form `args` object to the operation (filling the cwd
  // binding for code.* operations). The model learns the exact shape via
  // ukma_describe. Uniform + low-risk: no per-operation parameter guessing.

  const passthrough = (toolName, operationId, description, callDescription) => {
    registerTool(
      toolName,
      description,
      {
        ...PATH_PARAM,
        args: {
          type: 'object',
          description:
            'The request input for ' +
            operationId +
            '. For code.* operations, repository_id and revision are filled in (repository_id fixed, revision @cwd) when omitted. Use ukma_describe to see the exact shape.'
        }
      },
      (args, exec) => {
        const project = projectOf(args, exec)
        const input = args.args && typeof args.args === 'object' ? { ...args.args } : {}
        if (operationId.startsWith('code.')) fillCodeBinding(input)
        const heavy = /analyze|scan|orchestrated/.test(operationId)
        return callOperation(project, operationId, input, exec, heavy ? 300000 : 120000)
      },
      {
        callDescription,
        callTitle: () => `ukma ${operationId}`
      }
    )
  }

  passthrough('ukma_callers', 'code.query_callers', 'Find all functions/methods that call a symbol (structural). Use before refactoring. Use ukma_describe for the exact request shape.', 'Find callers of a symbol')
  passthrough('ukma_callees', 'code.query_callees', 'Find all functions/methods a symbol calls (structural). Use ukma_describe for the exact request shape.', 'Find callees of a symbol')
  passthrough('ukma_impact', 'code.analyze_impact', 'Analyze what code is affected by changing a symbol (structural). Use before refactoring or renaming. Use ukma_describe for the exact request shape.', 'Analyze impact of a change')
  passthrough('ukma_dependencies', 'code.query_dependencies', 'Query the dependency relationships for a symbol/module (structural). Use ukma_describe for the exact request shape.', 'Query dependencies')
  passthrough('ukma_references', 'code.find_references', 'Find all reference locations for a symbol (structural). Use ukma_describe for the exact request shape.', 'Find references')
  passthrough('ukma_module_summary', 'code.get_module_summary', 'Summarize a module (its symbols, entry points, hot paths) (structural). Use ukma_describe for the exact request shape.', 'Module summary')
  passthrough('ukma_circular_deps', 'code.find_circular_deps', 'Find circular dependencies in the codebase (structural). Use ukma_describe for the exact request shape.', 'Find circular dependencies')
  passthrough('ukma_hot_paths', 'code.find_hot_paths', 'Find the hottest call paths in the codebase (structural). Use ukma_describe for the exact request shape.', 'Find hot paths')
  passthrough('ukma_dead_imports', 'code.find_dead_imports', 'Find unused imports in the codebase (structural). Use ukma_describe for the exact request shape.', 'Find dead imports')
  passthrough('ukma_entry_points', 'code.find_entry_points', 'Find the entry points of the codebase (structural). Use ukma_describe for the exact request shape.', 'Find entry points')
  passthrough('ukma_related_tests', 'code.find_related_tests', 'Find test files related to changed source files (structural). Use ukma_describe for the exact request shape.', 'Find related tests')
  passthrough('ukma_complexity', 'code.analyze_complexity', 'Analyze complexity hotspots in the codebase (structural). Use ukma_describe for the exact request shape.', 'Analyze complexity hotspots')
  passthrough('ukma_find_by_signature', 'code.find_by_signature', 'Find symbols by signature (structural). Use ukma_describe for the exact request shape.', 'Find by signature')
  passthrough('ukma_search_error', 'code.search_by_error', 'Search the codebase by an error message (structural). Use ukma_describe for the exact request shape.', 'Search by error message')
  passthrough('ukma_get_edit_context', 'code.get_edit_context', 'Assemble edit-oriented context for a change (structural). Use ukma_describe for the exact request shape.', 'Assemble edit context')
  passthrough('ukma_get_curated_context', 'code.get_curated_context', 'Assemble curated context for a target (structural). Use ukma_describe for the exact request shape.', 'Assemble curated context')
  passthrough('ukma_index_files', 'code.index_files', 'Index (or re-index) a set of files into the current revision (structural). Use ukma_describe for the exact request shape.', 'Index files')

  // ---- prompt front-load (off by default) --------------------------------
  // The dsh-codegraph prompt-hook equivalent: listen for user prompts entering
  // an agent's inbox, gate by confidence, pre-run ukma_search, and steer the
  // result into the turn as <ukma_context>. Every failure path is a silent
  // no-op; it never breaks or blocks the user's prompt.
  if (frontload !== false) {
    const FRONTLOAD_MARKER = '<ukma_context'
    const FRONTLOAD_SOURCE_KIND = `plugin:${name}`
    const FRONTLOAD_DEDUP_MS = 10 * 60 * 1000
    const frontloadedByAgent = new Map()
    const alreadyFrontloaded = (agentId, text) => {
      let seen = frontloadedByAgent.get(agentId)
      if (!seen) {
        seen = new Map()
        frontloadedByAgent.set(agentId, seen)
      }
      const now = Date.now()
      for (const [t, ts] of seen) if (now - ts > FRONTLOAD_DEDUP_MS) seen.delete(t)
      if (seen.has(text)) return true
      seen.set(text, now)
      if (seen.size > 20) seen.delete(seen.keys().next().value)
      return false
    }

    const maybeFrontload = async (agent, message) => {
      if (!message || message.role !== 'user') return
      if (message.source && message.source.kind !== 'user') return
      if (!agent || !agent.inbox || !Array.isArray(agent.inbox.nextTurn)) return
      if (!agent.inbox.nextTurn.some((c) => c && c.id === message.id)) return

      const text = (Array.isArray(message.content) ? message.content : [])
        .filter((b) => b && b.type === 'text')
        .map((b) => b.text)
        .join('\n')
        .trim()
      if (text.length < 8 || text.includes(FRONTLOAD_MARKER)) return
      if (alreadyFrontloaded(String(agent.id || ''), text)) return

      const header = agent.session && agent.session.header
      const cwd = header && header.cwd
      if (!cwd) return

      // Gate: only fire for prompts that name a code-shaped token (a symbol
      // that exists in the index). Prose without a verified symbol is a no-op.
      const tokens = (text.match(/[a-z][a-z0-9]*(?:[A-Z][a-zA-Z0-9]*)+|[A-Z][a-zA-Z0-9]*[a-z][a-zA-Z0-9]*|[a-z][a-z0-9]*(?:_[a-z0-9]+)+/g) || []).slice(0, 5)
      if (tokens.length === 0) return
      let verified = false
      let body = ''
      const session = sessionFor(cwd, bin, AbortSignal.timeout(30000))
      try {
        await ensureCodeIndex(session, AbortSignal.timeout(30000))
        for (const token of tokens) {
          const out = await session.toolsCall(
            'ukma_code_find_symbols_v1',
            { repository_id: REPOSITORY_ID, revision: CWD_REVISION, query: token, kinds: [], path_glob: null, language: null, page: { cursor: null, limit: 1 } },
            AbortSignal.timeout(15000),
            15000
          )
          if (out && out.includes('"items"') && !out.includes('"items": []')) {
            verified = true
            body = out
            break
          }
        }
      } catch {
        return // not indexed / not verified — silent no-op
      }
      if (!verified || !body) return
      const MAX = 12000
      const capped =
        body.length > MAX ? body.slice(0, MAX) + '\n…(truncated; call ukma_search for the rest)' : body
      agent.steer(
        createUserMessage({
          content: [
            {
              type: 'text',
              text: `${FRONTLOAD_MARKER} note="Code context from UKMA for this prompt — treat the returned symbols as already located; call ukma_explore for full context.">\n${capped}\n</ukma_context>`
            }
          ],
          source: {
            kind: FRONTLOAD_SOURCE_KIND,
            plugin: name,
            form: 'notice',
            summary: 'ukma: pre-located symbols for this prompt'
          }
        })
      )
    }

    ctx.on('agent/inbox/inserted', ({ agent, message }) => {
      void maybeFrontload(agent, message).catch(() => {})
    })
  }
}
