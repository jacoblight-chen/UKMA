# 發佈 runbook：`ukma-cli`（npm）與 GitHub Release

這頁把「第一次發佈 `ukma-cli@0.1.0` 實際踩到的坑」與之後每次發版的標準流程固定下來。
`npm/bin/ukma.js` 只是啟動器（下載 release 內的裸 binary 並轉發參數），真正的程式在
Rust binary 裡；所以 **npm 發佈永遠要等 GitHub Release 先產生資產**。

## 1. 兩條發佈路徑

| 路徑 | 觸發 | 認證 | provenance |
| --- | --- | --- | --- |
| CI fallback（**目前使用中**） | push `v*` tag → `.github/workflows/release.yml` 的 `npm` job，repo 設定了 `NPM_TOKEN` secret | granular access token（需勾 **Bypass 2FA**） | ❌ 無 |
| CI（OIDC，**暫未啟用**） | 同上，但 npm 頁面已設 trusted publisher（需帳號有 2FA） | trusted publishing（OIDC，無需任何長期憑證） | ✅ 有 |
| 手動 | `scripts/publish_npm.ps1` / `scripts/publish_npm.sh` | 本機 `npm whoami` 的登入憑證或 `NPM_TOKEN` | ❌ 無 |

> `npm` job 的 `needs: publish`：四個平台的資產先上傳完成，npm 才會發布。這是必要的，
> 因為 `npx ukma-cli` 一執行就會去抓 `ukma-<triple>` 資產。

## 2. 手動發佈（本機）

`npm publish` 讀的是**當前目錄**的 `package.json`，而倉庫根目錄刻意沒有 `package.json`，
所以兩支 helper 都會先 `cd npm/`，預設 dry run（只印 tarball，不上傳）：

```powershell
.\scripts\publish_npm.ps1                          # dry run
.\scripts\publish_npm.ps1 -Tag v0.1.0 -Publish      # 真正上傳
.\scripts\publish_npm.ps1 -Publish -Otp 123456      # 帳號真的開了 2FA 時
```

```shell
sh scripts/publish_npm.sh                           # dry run
sh scripts/publish_npm.sh --tag v0.1.0 --publish    # 真正上傳
```

帶 `-Tag` / `--tag` 時，helper 會先確認 tag 與 `crates/ukma-cli/Cargo.toml` 的版本一致，
並把 `npm/package.json` 的版本對齊（`npm publish` 自己不會從 tag 推版本）。

## 3. CI：trusted publishing（OIDC）

> **目前狀態（2026-10）：OIDC 尚未啟用。** 設定 trusted publisher 需要帳號有 2FA，
> 而 `jacoblightchen` 目前沒有 2FA，因此本次選擇「暫不啟用 2FA、繼續用 bypass-2FA token」。
> **現在實際走的是 `NPM_TOKEN` fallback 路徑**——要讓 CI 發佈，請先在 GitHub repo 設定
> `NPM_TOKEN` secret（Settings → Secrets and variables → Actions → New repository secret →
> `NPM_TOKEN`，值為一顆勾了 **Bypass 2FA** 的 granular token）。之後想切 OIDC 時，
> 先啟用 2FA（security key / authenticator app），再回頭做下面的設定即可。

npm 已公告 **2027-01** 起，「Bypass 2FA」的 granular token 將不能再直接發佈，官方遷移目標
就是 trusted publishing。設定一次即可，之後完全不需要 `NPM_TOKEN`：

1. npm 套件頁 → `ukma-cli` → **Settings** → **Trusted publishing** → **Add**
   （在 `/access` 頁面頂部的「Trusted Publisher」區塊，點 **Add Trusted Publisher connection
   for GitHub Actions**）。
2. 填：publisher = **GitHub Actions**、Organization or user = `jacoblight-chen`、
   Repository = `UKMA`、Workflow filename = `release.yml`、Environment 留空，
   並勾選 **Allow npm publish**（直接發佈；不勾則要求 staged publishing）。
3. 存檔。之後 push `v*` tag，`release.yml` 的 `npm` job 就會用 OIDC 發佈並附 provenance。

⚠️ **設定 trusted publisher 需要 2FA**：點「Set up connection」後，若帳號沒有 2FA，
npm 會轉到 2FA 設定精靈（`/settings/<user>/tfa`），必須先完成 2FA（security key 或
authenticator app）才能存下這筆 trusted publisher。這是設定 OIDC 的前置門檻。

workflow 這邊的必要條件（已寫在 `.github/workflows/release.yml`）：

- job 層級 `permissions: { contents: read, id-token: write }`——沒有 id token 就沒有 OIDC。
- npm CLI **≥ 11.5.1**（Node 20 內建 npm 10，所以 job 用 Node 24，並有一步檢查版本）。
- **不要**在 `actions/setup-node` 設 `registry-url`：它會寫出一份 `_authToken` 為空的
  `.npmrc`，npm 看到有 `.npmrc` 就會跳過 OIDC 交換，最後得到 `E404`/`ENEEDAUTH`。
- 發佈指令是 `npm publish --access public --provenance`。

fallback：若 repo 仍設定了 `NPM_TOKEN` secret，job 會改走 token 路徑（並在 log 印 notice
說明沒有 provenance）。要完全切到 OIDC，把 secret 刪掉即可。

⚠️ 在 npm 頁面完成 trusted publisher 設定**之前**，若也沒有 `NPM_TOKEN`，push tag 會在
`npm` job 失敗（npm 拿不到任何憑證，會以 `ENEEDAUTH` / `E404` 收場）。這是預期行為——
寧可讓 release 紅燈，也不要靜默跳過 npm 發佈。設定完後重跑
`gh workflow run release.yml -f tag=v<version>`（或再推一次同名 tag）即可補發。

## 4. 首次發佈踩到的坑

### 坑 1：在倉庫根目錄執行 `npm publish`

```
npm error code ENOENT
npm error path F:\UKMA\package.json
npm error enoent Could not read package.json
```

`--prefix npm` **對 `publish` 無效**（它對 `install`/`ls` 之類才有效）。正解是先 `cd npm`，
或直接用 helper。

### 坑 2：granular token 需要 Bypass 2FA，但帳號沒開 2FA 就沒有碼可打

```
npm error code E403
npm error 403 Forbidden - PUT https://registry.npmjs.org/ukma-cli
npm error 403 Two-factor authentication or granular access token with bypass 2fa enabled is required
```

關鍵事實：

- `npm profile get` 可能顯示 `two-factor auth: disabled`。
- 但 **Granular Access Token 無論帳號是否開 2FA，都必須勾「Bypass 2FA」才能發佈**，
  否則就是上面那個 E403。
- 而帳號 2FA 是關閉的 → 根本不會產生 OTP → `--otp` 沒東西可填。
- `npm login --auth-type=web` 也會寫入一個 granular token，症狀相同。

正解（任一）：重新建立 token 並**勾選 Bypass 2FA**（Automation token 一律 bypass），
或直接在 npm 設定 trusted publishing（§3），或先啟用帳號 2FA 再用 `--otp`。
本次首發就是用「勾了 Bypass 2FA 的 granular token」完成的。

### 坑 3：repo 是 private → 所有 release 資產對匿名使用者 404

`npx ukma-cli` 一跑就下載 `https://github.com/jacoblight-chen/UKMA/releases/...`。
repo 未公開時，連 repo 首頁、`latest.json`、`install.ps1` 對匿名請求都是 404，
npm 套件本身雖然裝得起來，但啟動器抓不到 binary。

診斷：

```powershell
(Invoke-WebRequest https://api.github.com/repos/jacoblight-chen/UKMA -UseBasicParsing).Content
# "private": true  → 需要公開
```

正解：把 repo 設為 public，並確認 `visibility: public` 真的生效（UI 上按了不等於生效）。
倉庫公開前應先做憑證掃描：不要有被追蹤的 `.npmrc`（`.gitignore` 已忽略任何層級的
`.npmrc`）、`_authToken` 只以 `$NODE_AUTH_TOKEN` 變數形式出現在 workflow 裡。

### 坑 4：剛發佈完，registry 只看得到 `0.0.0-stage`

首發後 `npm view ukma-cli versions` 一度只回 `0.0.0-stage`
（描述是 "Temporary package placeholder for staged publishing"），約 **2 分鐘**後
`0.1.0` 才出現、`dist-tags.latest` 才指過去。`npm stage list` 在那段時間是空的。

因此：**不要**在上傳後立刻斷言版本存在。CI 的「Confirm the published version」因此改成
每 30 秒重試、最多 10 分鐘的輪詢。

### 坑 5：`install.ps1` 把 SHA-256 檔讀成位元組陣列

`Invoke-WebRequest` 在回應沒有 charset 時，`.Content` 是 `byte[]` 而不是字串；
`.sha256` 檔（內容是 `sha256sum` 格式的 `<hex>  <file>`）被 `-split ' '` 切開後比較的是
位元組值，於是出現：

```
SHA-256 mismatch: expected 49
```

（`49` 是字元 `'1'` 的 ASCII 值。）已修正為先以 UTF-8 解碼、驗證 64 位十六進位再比對。

### 坑 6：設定 trusted publisher 需要先有 2FA

在 npm 頁面填好 trusted publisher 表單、點「Set up connection」後，若帳號**沒有** 2FA，
npm 不會直接存檔，而是轉到 2FA 設定精靈（`/settings/<user>/tfa`），要求先完成 2FA
（security key 或 authenticator app）才能存下這筆 trusted publisher。

```
（點擊 Set up connection 後）
→ Confirm Password to Continue
→ Two-Factor Authentication 精靈（Login → 2FA Method → Configure）
```

正解（本次選擇）：暫不啟用 2FA，改走 `NPM_TOKEN` fallback（§3 的「目前狀態」）。
要切 OIDC 時，先啟用 2FA 再回來設定即可。

## 5. 驗證

```shell
npm view ukma-cli dist-tags                 # latest 應為新版本
npm view ukma-cli@<version> version
npx --yes ukma-cli@<version> --version      # 應印出 ukma <version>（會下載 binary）
npm view ukma-cli@<version> dist.attestations   # OIDC 發佈才有
```

安裝器（與 `npx` 走不同路徑，兩邊都要驗）：

```powershell
.\scripts\install.ps1 -Dir $env:TEMP\ukma-check -NoPath
& "$env:TEMP\ukma-check\ukma.exe" upgrade --check
```

release 資產對匿名使用者可讀（`install.ps1` / `install.sh` 不是 release 資產，在 `scripts/` 內）：

```powershell
$base = 'https://github.com/jacoblight-chen/UKMA/releases/latest/download'
foreach ($f in 'latest.json', 'ukma-x86_64-pc-windows-msvc.exe', 'ukma-x86_64-pc-windows-msvc.exe.sha256') {
  $code = (Invoke-WebRequest "$base/$f" -Method Head -UseBasicParsing).StatusCode
  "$f -> $code"
}
(Invoke-WebRequest 'https://raw.githubusercontent.com/jacoblight-chen/UKMA/main/scripts/install.ps1' -Method Head -UseBasicParsing).StatusCode
```

## 6. 回滾

| 情況 | 做法 |
| --- | --- |
| 發佈後 72 小時內、無人依賴 | `npm unpublish ukma-cli@<version>`（版本號視為已消耗，請改發新版本，不要重用） |
| 超過 72 小時，或已有人安裝 | `npm deprecate ukma-cli@<version> "…"`，再發一個修好的版本；不要刪除 |
| 只有 GitHub 資產壞掉 | 重跑 `release` workflow（`workflow_dispatch` + `tag`）覆蓋資產；npm 版本不需動 |
| `dist-tags.latest` 指錯 | `npm dist-tag add ukma-cli@<good> latest` |

`ukma upgrade` 讀的是 `latest.json`，所以只要重跑 release workflow 就能修正已安裝使用者的升級來源。

## 7. 版本升級與 tag（之後每次發版）

```shell
# 1. 升版本（單一來源：CLI crate）
#    crates/ukma-cli/Cargo.toml  version = "0.1.1"
cargo check --workspace --all-targets          # 確認 lockfile / 版本一致

# 2. 驗收（與 CI 相同）
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo run -q -p ukma-release-evidence -- check

# 3. 提交 + 打 tag（tag 是唯一觸發點）
git commit -am "chore(cli): bump ukma to 0.1.1"
git tag v0.1.1
git push origin main v0.1.1
```

push tag 後自動發生：

1. `release` workflow 建四個平台的 zip、裸 binary、`.sha256`、`latest.json` → GitHub Release。
2. `npm` job 用 OIDC（或 `NPM_TOKEN` fallback）把 `npm/package.json` 版本設成 `0.1.1` 後發佈，
   再輪詢確認 registry 真的解析得到。

**不需要**手動改 `npm/package.json` 的版本——CI 與 helper 都會從 tag 對齊。手動發佈時才需要
`-Tag` / `--tag`。

## 8. 相關檔案

- `.github/workflows/release.yml` — 打包、release、npm 發佈（OIDC + token fallback）
- `npm/package.json`、`npm/bin/ukma.js`、`npm/README.md` — npm 套件與啟動器
- `scripts/publish_npm.ps1`、`scripts/publish_npm.sh` — 手動發佈 helper
- `scripts/install.ps1`、`scripts/install.sh` — 安裝器（含 SHA-256 驗證）
- `crates/ukma-cli/tests/npm_launcher.rs` — 啟動器必須保持「薄轉發」的測試
