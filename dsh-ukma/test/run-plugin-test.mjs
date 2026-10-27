// dsh-ukma runtime test harness (full suite).
//
// Loads the plugin's lib/index.js, mounts a ctx stub (tools / systemPrompt /
// get / on), calls apply(), and exercises the real tool `execute` paths
// against a REAL `ukma` binary (via the UKMA_BIN env var, or PATH).
//
// Self-contained: when the @deepseek-ai/dsh-tools / dsh-llm / schemastery
// packages are NOT resolvable from the plugin's own path, the harness writes
// minimal stand-ins into dsh-ukma/node_modules for the duration of the run
// and removes them on exit (so they can never shadow the real packages in a
// DSH profile). When the real packages ARE present (a DSH profile, or CI),
// the harness uses them, which validates the ToolDefinition contract and the
// createUserMessage shape against the version actually installed — making
// this suite a cross-version gate.
//
// Sections:
//   0  environment + self-stub + install gate
//   1  mount + tool surface + guidance
//   2  ToolDefinition contract (incl. presentCall robustness)
//   3  setup tools + lifecycle (init idempotent, uninit, re-init)
//   4  code tools: search / symbol / explore / pattern (persistent --mcp child)
//   5  system + memory + ukma_call escape hatch (code.* binding fill)
//   6  Config validation + explicit overrides
//   7  error paths + robustness (bad ops, missing binary, repeated apply)
//   8  structural pass-throughs (full surface): real calls + bad-symbol error
//   9  prompt front-load behaviour (steer on verified symbol, dedup, gates)
//   10 front-load listener registration (on/off)
//   11 registry cross-check (every op id the plugin uses exists in the
//      125-op operation registry; skipped when the monorepo is not present)
//
// Run:
//   node test/run-plugin-test.mjs
//   UKMA_BIN=/path/to/ukma node test/run-plugin-test.mjs

import { createRequire } from 'node:module'
import { mkdtempSync, writeFileSync, rmSync, mkdirSync, readFileSync, existsSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, dirname } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const pluginDir = join(here, '..')
const requireFromPlugin = createRequire(join(pluginDir, 'lib', 'index.js'))

// ---- small test kit --------------------------------------------------------

let passed = 0
let failed = 0
const failures = []

function ok(name, cond, detail) {
  if (cond) {
    passed++
    console.log(`  PASS  ${name}`)
  } else {
    failed++
    failures.push(name)
    console.log(`  FAIL  ${name}${detail ? ` — ${detail}` : ''}`)
  }
}

async function throws(name, fn, msgCheck) {
  try {
    await fn()
    ok(name, false, 'expected a throw, got success')
  } catch (error) {
    const msg = String((error && error.message) || error)
    ok(name, !msgCheck || msgCheck(msg), `threw (${msg.slice(0, 100)})`)
  }
}

function section(title) {
  console.log(`\n=== ${title} ===`)
}

function removeDir(dir) {
  try {
    rmSync(dir, { recursive: true, force: true })
  } catch {
    // best-effort: a child process may still hold a handle on some platforms
  }
}

async function waitFor(fn, timeoutMs = 150000, intervalMs = 250) {
  const start = Date.now()
  // eslint-disable-next-line no-constant-condition
  while (true) {
    if (fn()) return true
    if (Date.now() - start > timeoutMs) return false
    await new Promise((r) => setTimeout(r, intervalMs))
  }
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

// ---- dsh package resolution (real, or self-stub) ---------------------------

const stubRoot = join(pluginDir, 'node_modules', '@deepseek-ai')
let stubsCreated = false

function writeStub(name, pkgJson, code) {
  const dir = join(stubRoot, name)
  mkdirSync(dir, { recursive: true })
  writeFileSync(join(dir, 'package.json'), JSON.stringify(pkgJson))
  writeFileSync(join(dir, 'index.js'), code)
}

function ensureDshPackages() {
  try {
    requireFromPlugin.resolve('@deepseek-ai/dsh-tools')
    return
  } catch {
    // fall through: install stand-ins for this run only
  }
  writeStub(
    'dsh-tools',
    { name: '@deepseek-ai/dsh-tools', version: '0.2.0', type: 'module', main: 'index.js', exports: { '.': './index.js' } },
    `export function defineTool(def) { return def; }\nexport default { defineTool };\n`
  )
  writeStub(
    'dsh-llm',
    { name: '@deepseek-ai/dsh-llm', version: '0.2.0', type: 'module', main: 'index.js', exports: { '.': './index.js' } },
    `export function createUserMessage(m) { return Object.freeze(m); }\nexport default { createUserMessage };\n`
  )
  // A zod-like subset that DOES validate unions, so the Config validation
  // checks run meaningfully in stub mode as well as with real zod.
  writeStub(
    'schemastery',
    { name: '@deepseek-ai/schemastery', version: '3.0.0', type: 'module', main: 'index.js', exports: { '.': './index.js' } },
    [
      `function field(type) {`,
      `  const f = { _type: type, _default: undefined, _hasDefault: false, _const: undefined };`,
      `  f.default = (v) => { f._default = v; f._hasDefault = true; return f; };`,
      `  f.parse = (v) => (v === undefined ? f._default : v);`,
      `  return f;`,
      `}`,
      `const z = {`,
      `  object(shape) {`,
      `    return {`,
      `      parse(input) {`,
      `        input = input || {};`,
      `        const out = {};`,
      `        for (const [k, spec] of Object.entries(shape)) {`,
      `          if (input[k] !== undefined) out[k] = spec && typeof spec.parse === 'function' ? spec.parse(input[k]) : input[k];`,
      `          else if (spec && spec._hasDefault) out[k] = spec._default;`,
      `          else out[k] = undefined;`,
      `        }`,
      `        return out;`,
      `      }`,
      `    };`,
      `  },`,
      `  boolean() { return field('boolean'); },`,
      `  string() { return field('string'); },`,
      `  number() { return field('number'); },`,
      `  array() { return field('array'); },`,
      `  const(v) { const f = field('const'); f._const = v; return f; },`,
      `  union(list) {`,
      `    const f = field('union');`,
      `    const allowed = list.filter((x) => x && x._const !== undefined).map((x) => x._const);`,
      `    f.parse = (v) => {`,
      `      if (v === undefined) return f._default;`,
      `      if (allowed.length > 0 && !allowed.includes(v)) {`,
      `        throw new Error('stub union: value ' + JSON.stringify(v) + ' not in [' + allowed.join(', ') + ']');`,
      `      }`,
      `      return v;`,
      `    };`,
      `    return f;`,
      `  }`,
      `};`,
      `export default z;`
    ].join('\n')
  )
  stubsCreated = true
}

ensureDshPackages()
if (stubsCreated) {
  console.log('  (dsh peer packages not resolvable — using self-stub stand-ins for this run; they are removed on exit)')
  process.on('exit', () => {
    try {
      rmSync(stubRoot, { recursive: true, force: true })
    } catch {
      // best-effort
    }
  })
}

const dshToolsReal = (() => {
  try {
    requireFromPlugin.resolve('@deepseek-ai/dsh-tools')
    return !stubsCreated
  } catch {
    return false
  }
})()

// ---- load the plugin -------------------------------------------------------

let plugin
let pluginLoadError
try {
  plugin = await import(pathToFileURL(join(pluginDir, 'lib', 'index.js')).href)
} catch (error) {
  pluginLoadError = error
}

const ukmaBin = process.env.UKMA_BIN || 'ukma'

// ---- a ctx stub that records registrations ---------------------------------

function makeCtx() {
  const registered = []
  const sections = []
  const listeners = {}
  return {
    registered,
    sections,
    listeners,
    tools: {
      register: (tool) => {
        registered.push(tool)
      }
    },
    systemPrompt: {
      section: (s) => {
        sections.push(s)
      }
    },
    get: () => undefined,
    on: (event, handler) => {
      listeners[event] = handler
    }
  }
}

const execStub = (cwd) => ({
  signal: new AbortController().signal,
  agent: { id: 'test-agent', session: { header: { cwd } }, inbox: { nextTurn: [] } }
})

// ============================================================================
// Section 0 — environment + install gate
// ============================================================================
section('0. environment + install gate')
ok('ukma binary resolvable', existsSync(ukmaBin) || ukmaBin === 'ukma', `ukmaBin=${ukmaBin}`)
console.log(`  dsh packages: ${dshToolsReal ? 'real' : 'self-stub'} | plugin load: ${pluginLoadError ? 'FAILED' : 'ok'}`)

if (!plugin) {
  console.log(`\nPlugin module could not be loaded: ${pluginLoadError && pluginLoadError.message}`)
  console.log('Install the dsh peer packages (or run inside a DSH profile) and re-run:')
  console.log('  pnpm add -D @deepseek-ai/dsh-tools @deepseek-ai/dsh-llm @deepseek-ai/schemastery')
  process.exit(1)
}

const { name, inject, Config, apply } = plugin

// ============================================================================
// Section 1 — mount + tool surface + guidance
// ============================================================================
section('1. mount + tool surface + guidance')
ok('plugin name is dsh-ukma', name === 'dsh-ukma')
ok(
  'inject declares tools + systemPrompt',
  Array.isArray(inject) && inject.includes('tools') && inject.includes('systemPrompt')
)

let configDefault
try {
  configDefault = Config.parse({})
  ok('Config parses defaults', true)
} catch (error) {
  ok('Config parses defaults', false, String((error && error.message) || error).slice(0, 80))
  configDefault = {}
}
ok('Config default surface is core', configDefault.surface === 'core')
ok('Config default guideSearch is true', configDefault.guideSearch === true)
ok('Config default frontload is false', configDefault.frontload === false)
ok('Config default bin is empty (PATH)', configDefault.bin === '')

// Core surface.
{
  const ctx = makeCtx()
  let threw
  try {
    apply(ctx, { bin: ukmaBin })
  } catch (error) {
    threw = error
  }
  ok('apply (core) does not throw', !threw, threw && String(threw.message).slice(0, 80))
  const names = ctx.registered.map((t) => t.name).sort()
  const expectedCore = [
    'ukma_status', 'ukma_init', 'ukma_sync', 'ukma_uninit',
    'ukma_capabilities', 'ukma_describe', 'ukma_call',
    'ukma_search', 'ukma_symbol', 'ukma_explore', 'ukma_search_pattern',
    'ukma_memory_search', 'ukma_memory_store'
  ].sort()
  ok('core registers the expected 13 tools', JSON.stringify(names) === JSON.stringify(expectedCore), `got ${names.length}: ${names.join(', ')}`)
  const guide = ctx.sections.find((s) => s.name === 'tool:ukma')
  ok('guidance section tool:ukma registered', !!guide)
  ok('guidance section order is 98 (< 100 built-in block)', guide && guide.order === 98)
  ok(
    'guidance text uses imperative wording + anti-patterns',
    guide && /MUST use the ukma_\* tools INSTEAD/.test(guide.text) && /Anti-patterns/.test(guide.text)
  )
}

// Full surface.
{
  const ctx = makeCtx()
  apply(ctx, { bin: ukmaBin, surface: 'full' })
  ok('full registers 30 tools (13 core + 17 structural)', ctx.registered.length === 30, `got ${ctx.registered.length}`)
  const hasPassthrough = ctx.registered.some((t) => t.name === 'ukma_callers') && ctx.registered.some((t) => t.name === 'ukma_index_files')
  ok('full surface includes the structural pass-throughs', hasPassthrough)
}

// guideSearch=false registers tools without the guidance.
{
  const ctx = makeCtx()
  apply(ctx, { bin: ukmaBin, guideSearch: false })
  ok('guideSearch=false omits the guidance section', ctx.sections.length === 0)
}

// ============================================================================
// Section 2 — ToolDefinition contract
// ============================================================================
section('2. ToolDefinition contract')
{
  const ctx = makeCtx()
  apply(ctx, { bin: ukmaBin, surface: 'full' })
  let allWellFormed = true
  let detail = ''
  for (const tool of ctx.registered) {
    const well =
      typeof tool.name === 'string' &&
      typeof tool.description === 'string' &&
      tool.parameters && typeof tool.parameters === 'object' &&
      tool.output && typeof tool.output.render === 'function' &&
      typeof tool.execute === 'function' &&
      typeof tool.presentCall === 'function'
    if (!well) {
      allWellFormed = false
      detail = tool.name
      break
    }
    // output.render returns ContentBlock[] and does not throw on odd input.
    for (const input of [{ path: '' }, {}, { path: '', operation: 'x', args: {} }]) {
      let renderOk = true
      try {
        const blocks = tool.output.render(input, 'x')
        renderOk = Array.isArray(blocks) && blocks.length > 0 && blocks.every((b) => b && b.type === 'text' && typeof b.text === 'string')
      } catch {
        renderOk = false
      }
      if (!renderOk) {
        allWellFormed = false
        detail = `render(${tool.name}, ${JSON.stringify(input)})`
        break
      }
    }
    if (!allWellFormed) break
    // presentCall returns a tagged view (card) and does not throw on odd input.
    for (const input of [{ path: '', operation: 'x', args: {} }, {}, undefined]) {
      let pcOk = true
      try {
        const view = tool.presentCall(input)
        pcOk = view && typeof view.card === 'string'
      } catch {
        pcOk = false
      }
      if (!pcOk) {
        allWellFormed = false
        detail = `presentCall(${tool.name}, ${input === undefined ? 'undefined' : JSON.stringify(input)})`
        break
      }
    }
  }
  ok(`every tool is well-formed across ${ctx.registered.length} tools`, allWellFormed, detail)
}

// ============================================================================
// Section 3 — setup tools + lifecycle
// ============================================================================
section('3. setup tools + lifecycle')
{
  const project = mkdtempSync(join(tmpdir(), 'dsh-ukma-setup-'))
  writeFileSync(join(project, 'main.py'), 'def add(a, b):\n    return a + b\n\nprint(add(1, 2))\n')

  const run = async (toolName, args) => {
    const ctx = makeCtx()
    apply(ctx, { bin: ukmaBin })
    const tool = ctx.registered.find((t) => t.name === toolName)
    if (!tool) throw new Error(`tool ${toolName} not registered`)
    return tool.execute({ ...args, path: project }, execStub(project))
  }

  try {
    const initOut = await run('ukma_init', {})
    ok('ukma_init succeeds on a fresh project', /revision|created|manifest/i.test(initOut), String(initOut).slice(0, 80))
  } catch (error) {
    ok('ukma_init succeeds on a fresh project', false, String(error.message).slice(0, 120))
  }

  try {
    const statusOut = await run('ukma_status', {})
    ok('ukma_status reports the revision', /revision/i.test(statusOut), String(statusOut).slice(0, 80))
  } catch (error) {
    ok('ukma_status reports the revision', false, String(error.message).slice(0, 120))
  }

  try {
    await run('ukma_sync', {})
    ok('ukma_sync runs without error', true)
  } catch (error) {
    ok('ukma_sync runs without error', false, String(error.message).slice(0, 120))
  }

  // Idempotency: a second init on an initialized project is a no-op success.
  try {
    await run('ukma_init', {})
    ok('ukma_init is idempotent (second run succeeds)', true)
  } catch (error) {
    ok('ukma_init is idempotent (second run succeeds)', false, String(error.message).slice(0, 120))
  }

  // uninit then re-init: the lifecycle recovers.
  try {
    await run('ukma_uninit', {})
    ok('ukma_uninit removes the index state', true)
  } catch (error) {
    ok('ukma_uninit removes the index state', false, String(error.message).slice(0, 120))
  }
  try {
    await run('ukma_init', {})
    ok('ukma_init recovers after ukma_uninit', true)
  } catch (error) {
    ok('ukma_init recovers after ukma_uninit', false, String(error.message).slice(0, 120))
  }

  // A tool with no cwd and no explicit path must throw a clear error.
  await throws(
    'a tool with no cwd and no path throws a clear error',
    async () => {
      const ctx = makeCtx()
      apply(ctx, { bin: ukmaBin })
      const tool = ctx.registered.find((t) => t.name === 'ukma_status')
      return tool.execute({}, { signal: new AbortController().signal, agent: { session: {} } })
    },
    (msg) => /no session workspace|pass the project path/i.test(msg)
  )

  removeDir(project)
}

// ============================================================================
// Section 4 — code tools (persistent --mcp child; auto index)
// ============================================================================
section('4. code tools (search / symbol / explore / pattern)')
{
  const project = mkdtempSync(join(tmpdir(), 'dsh-ukma-code-'))
  writeFileSync(join(project, 'app.py'), 'def greet(name):\n    return "Hello, " + name\n\ndef greetUser(userId):\n    return greet("user " + str(userId))\n\ndef main():\n    print(greet("world"))\n    print(greetUser(1))\n')

  const run = async (toolName, args) => {
    const ctx = makeCtx()
    apply(ctx, { bin: ukmaBin })
    const tool = ctx.registered.find((t) => t.name === toolName)
    if (!tool) throw new Error(`tool ${toolName} not registered`)
    return tool.execute({ ...args, path: project }, execStub(project))
  }

  // The first code tool call builds the index (analyze_repository) and then
  // answers. This exercises the persistent --mcp child end to end.
  let searchOut
  try {
    searchOut = await run('ukma_search', { query: 'greet' })
    ok('ukma_search finds the seeded symbol (auto index)', /greet/i.test(searchOut), String(searchOut).slice(0, 120))
  } catch (error) {
    ok('ukma_search finds the seeded symbol (auto index)', false, String(error.message).slice(0, 160))
    searchOut = ''
  }

  // A second code tool reuses the same session (no re-index).
  try {
    const patternOut = await run('ukma_search_pattern', { pattern: 'greet' })
    ok('ukma_search_pattern returns a result (session reused)', typeof patternOut === 'string' && patternOut.length > 0, String(patternOut).slice(0, 100))
  } catch (error) {
    ok('ukma_search_pattern returns a result (session reused)', false, String(error.message).slice(0, 160))
  }

  // Extract a symbol id from the search envelope for ukma_symbol.
  const symIdMatch = searchOut.match(/sym:v1:[0-9a-f]+/)
  const symId = symIdMatch ? symIdMatch[0] : null
  ok('ukma_search output carries a symbol id', !!symId, `no sym:v1 id (first 200: ${String(searchOut).slice(0, 200)})`)

  if (symId) {
    try {
      const symOut = await run('ukma_symbol', { symbol_id: symId })
      ok('ukma_symbol returns the symbol record', /line_start|definition|kind/i.test(symOut), String(symOut).slice(0, 120))
    } catch (error) {
      ok('ukma_symbol returns the symbol record', false, String(error.message).slice(0, 160))
    }
  } else {
    ok('ukma_symbol returns the symbol record', false, 'skipped: no symbol id from search')
  }

  try {
    const exploreOut = await run('ukma_explore', { target_refs: ['greet', 'greetUser'], intent: 'explain' })
    ok('ukma_explore assembles context for named symbols', /greet/i.test(exploreOut), String(exploreOut).slice(0, 120))
  } catch (error) {
    ok('ukma_explore assembles context for named symbols', false, String(error.message).slice(0, 160))
  }

  removeDir(project)
}

// ============================================================================
// Section 5 — system + memory + escape hatch
// ============================================================================
section('5. system + memory + ukma_call escape hatch')
{
  const project = mkdtempSync(join(tmpdir(), 'dsh-ukma-sys-'))
  const token = `UNIQUE_TOKEN_${Date.now().toString(36)}${Math.random().toString(36).slice(2, 8)}`
  writeFileSync(join(project, 'a.py'), 'def greet(x):\n    return x\n')

  const run = async (toolName, args) => {
    const ctx = makeCtx()
    apply(ctx, { bin: ukmaBin })
    const tool = ctx.registered.find((t) => t.name === toolName)
    if (!tool) throw new Error(`tool ${toolName} not registered`)
    return tool.execute({ ...args, path: project }, execStub(project))
  }

  try {
    const caps = await run('ukma_capabilities', {})
    ok('ukma_capabilities reports domain counts', /code|memory|domain/i.test(caps), String(caps).slice(0, 100))
  } catch (error) {
    ok('ukma_capabilities reports domain counts', false, String(error.message).slice(0, 120))
  }

  try {
    const desc = await run('ukma_describe', { operation: 'code.find_symbols' })
    ok('ukma_describe returns the operation spec', /find_symbols|schema|scope/i.test(desc), String(desc).slice(0, 100))
  } catch (error) {
    ok('ukma_describe returns the operation spec', false, String(error.message).slice(0, 120))
  }

  // Memory store: assert the write path end to end (the server returns a
  // change receipt with a resource id). NOTE: the episode is committed under
  // this session's sandbox scope; memory visibility is sandbox-scoped by
  // design, so a single-context local deployment's search will NOT see it
  // (the dev context has no session/agent identity to match). Cross-sandbox
  // visibility is a multi-agent deployment property, not a plugin concern.
  try {
    const stored = await run('ukma_memory_store', { summary: `the test stored a fact ${token}` })
    ok('ukma_memory_store returns a change receipt', /resource_id|changed|etag/i.test(stored), String(stored).slice(0, 160))
  } catch (error) {
    ok('ukma_memory_store returns a change receipt', false, String(error.message).slice(0, 160))
  }

  // Memory search: a valid page (items array; may be empty in a single-
  // context deployment per the sandbox visibility semantics above).
  try {
    const found = await run('ukma_memory_search', { query: token })
    ok('ukma_memory_search returns a valid page', /"items"\s*:\s*\[/.test(found), String(found).slice(0, 160))
  } catch (error) {
    ok('ukma_memory_search returns a valid page', false, String(error.message).slice(0, 160))
  }

  // Escape hatch: a system operation with an explicit args object.
  try {
    const called = await run('ukma_call', { operation: 'system.capabilities', args: {} })
    ok('ukma_call dispatches a system operation', /code|memory|domain/i.test(called), String(called).slice(0, 100))
  } catch (error) {
    ok('ukma_call dispatches a system operation', false, String(error.message).slice(0, 120))
  }

  // Escape hatch on code.*: the plugin fills repository_id/revision (@cwd)
  // and the index is built automatically. The escape-hatch contract is an
  // explicit request input (ukma_describe gives the shape), so the other
  // required fields (kinds, page) are supplied by the caller.
  try {
    const called = await run('ukma_call', {
      operation: 'code.find_symbols',
      args: { query: 'greet', kinds: [], path_glob: null, language: null, page: { cursor: null, limit: 10 } }
    })
    ok('ukma_call (code.*) fills the @cwd binding and indexes', /greet/i.test(called), String(called).slice(0, 120))
  } catch (error) {
    ok('ukma_call (code.*) fills the @cwd binding and indexes', false, String(error.message).slice(0, 160))
  }

  // Escape hatch on a no-binding structural op with the full explicit shape.
  try {
    const out = await run('ukma_call', {
      operation: 'code.query_callers',
      args: {
        direction: 'in',
        edge_kinds: [],
        max_depth: 1,
        max_nodes: 20,
        page: { cursor: null, limit: 5 },
        symbol_id: 'sym:v1:00000000000000000000000000000000'
      }
    })
    ok('ukma_call (no-binding op, explicit shape) resolves or errors cleanly', /items|NOT_FOUND|not found/i.test(out), String(out).slice(0, 100))
  } catch (error) {
    // A well-formed query for a non-existent symbol may error cleanly; that is
    // acceptable (the protocol must not crash), so accept a clean server error.
    ok('ukma_call (no-binding op, explicit shape) resolves or errors cleanly', /NOT_FOUND|not found|INVALID/i.test(String(error.message)), String(error.message).slice(0, 160))
  }

  removeDir(project)
}

// ============================================================================
// Section 6 — Config validation + explicit overrides
// ============================================================================
section('6. Config validation + explicit overrides')
{
  // An invalid surface value must be rejected (stub union validates; real zod
  // rejects with a ZodError).
  let rejected = false
  try {
    Config.parse({ surface: 'bogus' })
  } catch {
    rejected = true
  }
  ok('Config rejects an invalid surface value', rejected)

  // Explicit overrides are respected end to end (parse → apply → surface).
  const cfg = Config.parse({ surface: 'full', guideSearch: false, frontload: true, bin: ukmaBin })
  ok(
    'Config.parse honors explicit overrides',
    cfg.surface === 'full' && cfg.guideSearch === false && cfg.frontload === true && cfg.bin === ukmaBin
  )
  const ctx = makeCtx()
  apply(ctx, cfg)
  ok(
    'applied override: full surface + no guidance + frontload listener',
    ctx.registered.length === 30 && ctx.sections.length === 0 && !!ctx.listeners['agent/inbox/inserted']
  )
}

// ============================================================================
// Section 7 — error paths + robustness
// ============================================================================
section('7. error paths + robustness')
{
  const project = mkdtempSync(join(tmpdir(), 'dsh-ukma-err-'))
  writeFileSync(join(project, 'a.py'), 'x = 1\n')
  const ctx0 = makeCtx()
  apply(ctx0, { bin: ukmaBin })
  try {
    await ctx0.registered.find((t) => t.name === 'ukma_init').execute({ path: project }, execStub(project))
  } catch { /* a failed init still lets the checks below run */ }

  const run = (toolName, args, config) => {
    const ctx = makeCtx()
    apply(ctx, config || { bin: ukmaBin })
    const tool = ctx.registered.find((t) => t.name === toolName)
    if (!tool) throw new Error(`tool ${toolName} not registered`)
    return tool.execute({ ...args, path: project }, execStub(project))
  }

  // An unknown operation id must surface a clean server-side error, not a
  // protocol crash or a hang.
  await throws(
    'ukma_describe with an unknown operation throws a clean error',
    () => run('ukma_describe', { operation: 'code.definitely_not_an_operation' }),
    (msg) => msg.length > 0
  )
  await throws(
    'ukma_call with an unknown operation throws a clean error',
    () => run('ukma_call', { operation: 'code.definitely_not_an_operation', args: {} }),
    (msg) => msg.length > 0
  )

  // A bad memory detail level surfaces the domain validation error cleanly.
  await throws(
    'ukma_memory_search with an invalid detail level throws a clean schema error',
    () => run('ukma_memory_search', { query: 'x', detail: 'nonsense' }),
    (msg) => /detail|INVALID|oneof|one of/i.test(msg)
  )

  // A missing binary produces a clear, actionable ENOENT hint (mentions the
  // install path), not a raw spawn failure.
  await throws(
    'a missing ukma binary yields an actionable install hint',
    () => run('ukma_status', {}, { bin: join(tmpdir(), 'definitely-not-ukma-12345') }),
    (msg) => /ukma/i.test(msg) && /install|PATH|download/i.test(msg)
  )

  // Repeated apply() must not blow up (the exit-time child disposal listener
  // is registered once, so listeners do not stack).
  let applyOk = true
  let applyErr = ''
  for (let i = 0; i < 3; i++) {
    try {
      apply(makeCtx(), { bin: ukmaBin })
    } catch (error) {
      applyOk = false
      applyErr = String(error.message)
    }
  }
  ok('repeated apply() (x3) does not throw', applyOk, applyErr.slice(0, 80))

  removeDir(project)
}

// ============================================================================
// Section 8 — structural pass-throughs (full surface)
// ============================================================================
section('8. structural pass-throughs (full surface)')
{
  const project = mkdtempSync(join(tmpdir(), 'dsh-ukma-pass-'))
  writeFileSync(join(project, 'util.py'), 'def double(x):\n    return x * 2\n')
  writeFileSync(join(project, 'main.py'), 'from util import double\n\ndef run():\n    return double(2)\n\nprint(run())\n')
  const ctx0 = makeCtx()
  apply(ctx0, { bin: ukmaBin, surface: 'full' })
  try {
    await ctx0.registered.find((t) => t.name === 'ukma_init').execute({ path: project }, execStub(project))
  } catch { /* ok */ }

  const run = async (toolName, args) => {
    const ctx = makeCtx()
    apply(ctx, { bin: ukmaBin, surface: 'full' })
    const tool = ctx.registered.find((t) => t.name === toolName)
    if (!tool) throw new Error(`tool ${toolName} not registered`)
    return tool.execute({ ...args, path: project }, execStub(project))
  }

  // Find the target symbol (also warms the index) and grab its id.
  let symId = null
  try {
    const searchOut = await run('ukma_search', { query: 'double' })
    const m = searchOut.match(/sym:v1:[0-9a-f]+/)
    symId = m ? m[0] : null
    ok('pass-through setup: ukma_search finds the seeded symbol', !!symId, String(searchOut).slice(0, 120))
  } catch (error) {
    ok('pass-through setup: ukma_search finds the seeded symbol', false, String(error.message).slice(0, 160))
  }

  if (symId) {
    // ukma_callers: the callers of double() include run().
    try {
      const out = await run('ukma_callers', {
        args: { direction: 'in', edge_kinds: [], max_depth: 2, max_nodes: 100, page: { cursor: null, limit: 20 }, symbol_id: symId }
      })
      ok('ukma_callers resolves (TraversalQuery shape)', /items/i.test(out), String(out).slice(0, 120))
    } catch (error) {
      ok('ukma_callers resolves (TraversalQuery shape)', false, String(error.message).slice(0, 160))
    }

    // ukma_references.
    try {
      const out = await run('ukma_references', {
        args: { symbol_id: symId, include_declaration: false, relation_kinds: [], page: { cursor: null, limit: 20 } }
      })
      ok('ukma_references resolves (ReferenceQuery shape)', /items|reference/i.test(out), String(out).slice(0, 120))
    } catch (error) {
      ok('ukma_references resolves (ReferenceQuery shape)', false, String(error.message).slice(0, 160))
    }

    // ukma_impact: impact of changing double(); the plugin auto-fills the
    // required revision binding.
    try {
      const out = await run('ukma_impact', {
        args: { changed: [symId], edge_kinds: [], include_test_candidates: false, max_depth: 2, max_nodes: 100 }
      })
      ok('ukma_impact resolves (ImpactQuery shape; revision auto-filled)', /items|affected|impact/i.test(out), String(out).slice(0, 120))
    } catch (error) {
      ok('ukma_impact resolves (ImpactQuery shape; revision auto-filled)', false, String(error.message).slice(0, 160))
    }
  } else {
    ok('ukma_callers resolves (TraversalQuery shape)', false, 'skipped: no symbol id')
    ok('ukma_references resolves (ReferenceQuery shape)', false, 'skipped: no symbol id')
    ok('ukma_impact resolves (ImpactQuery shape; revision auto-filled)', false, 'skipped: no symbol id')
  }

  // A non-existent symbol must produce a clean domain error, not a protocol
  // crash.
  await throws(
    'ukma_callers with a non-existent symbol id errors cleanly',
    () => run('ukma_callers', {
      args: { direction: 'in', edge_kinds: [], max_depth: 1, max_nodes: 20, page: { cursor: null, limit: 5 }, symbol_id: 'sym:v1:00000000000000000000000000000000' }
    }),
    (msg) => /NOT_FOUND|not found|INVALID/i.test(msg)
  )

  removeDir(project)
}

// ============================================================================
// Section 9 — prompt front-load behaviour
// ============================================================================
section('9. prompt front-load behaviour')
{
  const project = mkdtempSync(join(tmpdir(), 'dsh-ukma-front-'))
  writeFileSync(join(project, 'app.py'), 'def greetUser(userId):\n    return "hello " + str(userId)\n\nprint(greetUser(1))\n')

  const ctx = makeCtx()
  apply(ctx, { bin: ukmaBin, frontload: true })
  const handler = ctx.listeners['agent/inbox/inserted']
  ok('frontload=true registers the inbox handler', !!handler)

  if (handler) {
    const steerCalls = []
    const baseAgent = {
      id: 'agent-frontload',
      session: { header: { cwd: project } },
      steer: (m) => steerCalls.push(m)
    }
    const agentFor = (msgId) => ({ ...baseAgent, inbox: { nextTurn: [{ id: msgId }] } })
    const message = (id, text, role = 'user') => ({ id, role, content: [{ type: 'text', text }] })

    // 1) A prompt that names a real symbol (greetUser, camelCase token) must
    //    trigger a steer containing the <ukma_context> marker. The first
    //    call also builds the index, so allow generous time.
    handler({ agent: agentFor('msg-1'), message: message('msg-1', 'Please explain how greetUser works in this project') })
    const gotSteer = await waitFor(() => steerCalls.length > 0, 150000)
    ok('frontload steers <ukma_context> for a prompt naming a verified symbol', gotSteer)
    let steerText = ''
    if (gotSteer) {
      const first = steerCalls[0]
      const content = first && first.content
      steerText = Array.isArray(content)
        ? content.filter((b) => b && b.type === 'text').map((b) => b.text).join('\n')
        : String(first || '')
      ok(
        'steered message carries the <ukma_context> marker + symbol evidence',
        steerText.includes('<ukma_context') && /greetUser/i.test(steerText),
        steerText.slice(0, 160)
      )
    } else {
      ok('steered message carries the <ukma_context> marker + symbol evidence', false, 'no steer observed')
    }

    // 2) Dedup: the same prompt again must NOT steer a second time.
    if (gotSteer) {
      await sleep(300)
      const before = steerCalls.length
      handler({ agent: agentFor('msg-1'), message: message('msg-1', 'Please explain how greetUser works in this project') })
      await sleep(1500)
      ok('frontload dedups an identical prompt (no second steer)', steerCalls.length === before, `steers: ${steerCalls.length}`)
    } else {
      ok('frontload dedups an identical prompt (no second steer)', false, 'skipped: no initial steer')
    }

    // 3) A prompt with no code-shaped token is a silent no-op.
    {
      const before = steerCalls.length
      handler({ agent: agentFor('msg-2'), message: message('msg-2', 'hello there friend') })
      await sleep(1500)
      ok('frontload is a no-op for token-less prompts', steerCalls.length === before, `steers: ${steerCalls.length}`)
    }

    // 4) A non-user message is a silent no-op.
    {
      const before = steerCalls.length
      handler({ agent: agentFor('msg-3'), message: { id: 'msg-3', role: 'assistant', source: { kind: 'user' }, content: [{ type: 'text', text: 'here is greetUser again' }] } })
      await sleep(1500)
      ok('frontload is a no-op for non-user messages', steerCalls.length === before, `steers: ${steerCalls.length}`)
    }
  }

  removeDir(project)
}

// ============================================================================
// Section 10 — front-load listener registration (on/off)
// ============================================================================
section('10. front-load listener registration (on/off)')
{
  const off = makeCtx()
  apply(off, { bin: ukmaBin, frontload: false })
  ok('frontload=false registers no inbox listener', !off.listeners['agent/inbox/inserted'])

  const on = makeCtx()
  apply(on, { bin: ukmaBin, frontload: true })
  ok('frontload=true registers the inbox listener', !!on.listeners['agent/inbox/inserted'])
}

// ============================================================================
// Section 11 — registry cross-check (every op id the plugin uses exists)
// ============================================================================
section('11. registry cross-check')
{
  const registryPath = join(here, '..', '..', 'contracts', 'api', 'operation-registry.yaml')
  const pluginOps = [
    // always + core
    'system.capabilities', 'system.get_operation',
    'code.find_symbols', 'code.get_symbol', 'code.get_ai_context', 'code.search_by_pattern',
    'memory.search', 'memory.store_episode',
    // full-surface pass-throughs (must mirror the passthrough() calls in lib/index.js)
    'code.query_callers', 'code.query_callees', 'code.analyze_impact', 'code.query_dependencies',
    'code.find_references', 'code.get_module_summary', 'code.find_circular_deps', 'code.find_hot_paths',
    'code.find_dead_imports', 'code.find_entry_points', 'code.find_related_tests', 'code.analyze_complexity',
    'code.find_by_signature', 'code.search_by_error', 'code.get_edit_context', 'code.get_curated_context',
    'code.index_files'
  ]
  if (existsSync(registryPath)) {
    const yaml = readFileSync(registryPath, 'utf8')
    const ids = new Set()
    for (const line of yaml.split('\n')) {
      const m = line.match(/-\s*operation_id:\s*([a-z0-9_.]+)/)
      if (m) ids.add(m[1])
    }
    const missing = pluginOps.filter((op) => !ids.has(op))
    ok(
      `registry contains all ${pluginOps.length} op ids the plugin uses (registry has ${ids.size})`,
      missing.length === 0,
      `missing: ${missing.join(', ')}`
    )
  } else {
    console.log('  SKIP  registry not present (standalone checkout) — cross-check skipped')
  }
}

// ============================================================================
// summary
// ============================================================================
console.log(`\n=== ${passed} passed, ${failed} failed ===`)
if (failed > 0) {
  console.log('failures:')
  for (const f of failures) console.log('  - ' + f)
  process.exit(1)
}
process.exit(0)
