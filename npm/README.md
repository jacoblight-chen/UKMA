# `npx ukma-cli`

The npm entry point for UKMA. It is a **launcher, not a second implementation**:
`bin/ukma.js` finds the platform binary for this machine (downloading it from the
GitHub release and verifying its published SHA-256 the first time) and then
execs it, forwarding the exit code. All code-graph behaviour lives in the Rust
binary.

```shell
npx ukma-cli                 # download + run `ukma install` (wire your agents)
npx ukma-cli install --dry-run
npx ukma-cli init            # inside a project
npx ukma-cli --version
```

## Where it puts the binary

- `$UKMA_BIN` — use this executable and do not download anything.
- `$UKMA_BIN_DIR` (default `~/.ukma/bin`) — where a downloaded binary is cached.
  The cache is per version: a downloaded `ukma` stays until you delete it, so
  `npx ukma-cli@<version>` and a locally installed CLI can coexist.

## What it does not do

- It does not implement any CLI command; every argument is forwarded.
- It does not modify your PATH: `npx` resolves it per invocation. For a
  permanent install use `scripts/install.ps1` / `scripts/install.sh` or
  `cargo install --path crates/ukma-cli`.
- It does not run on Node < 18.

## Publishing

`.github/workflows/release.yml` publishes this package from `npm/` on every `v*`
tag when the repository has an `NPM_TOKEN` secret; without the secret the release
still produces the installers and `ukma upgrade` assets, and the npm step is
skipped with a notice.
