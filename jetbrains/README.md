# UKMA JetBrains Plugin

**Status: complete, buildable structure.** This is a Kotlin/Gradle project that
mirrors CodeGraph's JetBrains integration so UKMA exposes the same IDE surface.
The sources form a coherent, buildable plugin (startup activity, tool window,
action, and a thin REST client are all present and registered in `plugin.xml`).
The build is intentionally **not** executed here (it requires the IntelliJ
Platform SDK download and a Gradle toolchain that this workspace does not
provision); produce the distributable `.zip` on a machine with the SDK (see
"Building" below).

## What this plugin provides

- A Gradle project layout for the IntelliJ Platform Gradle Plugin
  (`build.gradle.kts`, `settings.gradle.kts`, `gradle.properties`).
- `plugin.xml` (the plugin descriptor: id, name, version, the `postStartupActivity`,
  the `toolWindow`, the `notificationGroup`, and the action).
- A plugin startup activity (`UkmaPlugin`) that opens a shared server handle
  once per project.
- A **tool window** (`UkmaToolWindowFactory`) that shows server reachability and
  the registered tool list.
- A lightweight **REST client** (`UkmaClient`) that talks to the UKMA server
  (`/health`, `/tools`, `/call`), so the plugin is a *client* — no business
  logic lives in the plugin (parity with the CodeGraph plugin, which is a thin
  IDE shell over the CodeGraph server).
- An IDE action (`UkmaToolsAction`) as an alternate entry point (Tools menu).

## Honest boundary

- The client sends requests over HTTP to the UKMA server; it does **not**
  embed any analysis, indexing, or model logic.
- The tool surface is wired to the server's `system.capabilities` / `ukma_*`
  tools. The current UI shows reachability and the tool list; per-tool result
  rendering (forms/panes for each `ukma_*` operation) is the remaining surface
  work for a production release.
- The Gradle/IntelliJ build is **not** executed by this repository's acceptance
  gates; it requires the IntelliJ Platform SDK and a Gradle toolchain. Run it on
  a machine with the SDK to produce the distributable `.zip`.

## Layout

```
jetbrains/
├── build.gradle.kts
├── settings.gradle.kts
├── gradle.properties
├── README.md
└── src/main/
    ├── kotlin/com/ukma/plugin/
    │   ├── UkmaPlugin.kt                # startup activity (postStartupActivity)
    │   ├── UkmaToolWindowFactory.kt     # the UKMA tool window
    │   ├── action/UkmaToolsAction.kt    # IDE action entry point (Tools menu)
    │   └── client/UkmaClient.kt         # thin REST client over the UKMA server
    └── resources/META-INF/plugin.xml   # plugin descriptor
```

## Building (on a machine with the IntelliJ SDK)

```
gradle buildPlugin        # produces a .zip in build/distributions
gradle runPlugin          # runs a sandbox IDE with the plugin
```

These require the IntelliJ Platform SDK and the Gradle toolchain; they are not
executed by this repository's acceptance gates.