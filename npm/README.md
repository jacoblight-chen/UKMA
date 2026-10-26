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

Publishing must happen **inside this directory** (`npm/`): `npm publish` reads
`package.json` from the current directory, and the repository root deliberately
has none. Running it there fails with:

```
npm error code ENOENT
npm error path .../package.json
npm error enoent Could not read package.json
```

`--prefix npm` does **not** help for `publish`; use the helper, which `cd`s for
you, shows the tarball first, and stays a dry run unless you ask it to upload:

```powershell
.\scripts\publish_npm.ps1                                  # dry run
.\scripts\publish_npm.ps1 -Tag v0.1.0 -Publish             # upload
.\scripts\publish_npm.ps1 -Publish -Otp 123456             # account without a bypass-2FA token
```

```shell
sh scripts/publish_npm.sh                                  # dry run
sh scripts/publish_npm.sh --tag v0.1.0 --publish           # upload
sh scripts/publish_npm.sh --publish --otp 123456
```

### Common failures

| Error | Cause | Fix |
| --- | --- | --- |
| `ENOENT … package.json` | ran outside `npm/` | `cd npm` first, or use the helper |
| `E403 … Two-factor authentication or granular access token with bypass 2fa enabled is required` | the credential cannot satisfy npm's 2FA check. **A Granular Access Token needs "Bypass 2FA" even when the account has 2FA disabled** (`npm profile get` → `two-factor auth: disabled`), and then there is no code to type | recreate the token with **Bypass 2FA** checked (Automation tokens always bypass), or `npm logout` + `npm login` and publish with the web-session credential. If the account really has 2FA on, pass `--otp <code>` |
| `E402 Payment Required` | npm treated the package as private | keep `--access public` |
| `E403 Forbidden - PUT … you do not have permission` | the name belongs to someone else, or the scope is not yours | pick another name in `package.json` (e.g. `@<your-scope>/ukma`) |
| `npx ukma-cli` downloads and gets HTTP 404 | the GitHub release (with the bare `ukma-<triple>` assets) does not exist yet, **or the repository is still private** — a private repo 404s every asset for anonymous users | publish the release first (`git tag v<version> && git push origin v<version>`), and make the repository public |
| `npm view` still shows `0.0.0-stage` | that is npm's placeholder while the real version propagates; it takes ~2 minutes | wait and re-check instead of re-publishing |

`.github/workflows/release.yml` publishes this package from `npm/` on every `v*`
tag. It prefers **trusted publishing (OIDC)** — no secret, provenance attached —
and falls back to an `NPM_TOKEN` secret (granular token with **Bypass 2FA**) when
one is configured. Configure the trusted publisher on the package's npm page:
owner `jacoblight-chen`, repository `UKMA`, workflow filename `release.yml`.

Full runbook, including the setup for trusted publishing and the rollback rules:
[`docs/publishing.md`](../docs/publishing.md).
