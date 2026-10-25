# UKMA GitHub Action (Tier 3)

CodeGraph-style: run a UKMA code-intelligence tool against a pull request and
post the result as a review comment. The action

1. builds the `ukma` release binary (or uses a prebuilt one via `binary`),
2. indexes the checkout as the `@cwd` revision (`--index-cwd`),
3. runs one tool by name (`--run-tool <name> --tool-args <json>`), and
4. posts the rendered envelope as a PR comment.

## Usage

```yaml
# .github/workflows/ukma-review.yml
name: UKMA PR review
on:
  pull_request:
    types: [opened, synchronize, reopened]

jobs:
  ukma-review:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      # For a published action: owner/UKMA/actions/github-action@v1
      - uses: ./github-action
        id: analyze
        with:
          tool: code.analyze_repository
          tool-args: '{"revision": {"repository_id": "pr-checkout", "vcs": "git", "commit_sha": "@cwd", "tree_sha": null, "dirty_policy": "snapshot"}, "parser_profile": "default", "include_globs": [], "exclude_globs": [], "asynchronous": true}'
      - uses: ./github-action
        with:
          tool: code.find_symbols
          # Use the concrete revision from the analyze result:
          tool-args: '{"revision": "<revision from the previous step>", "repository_id": "pr-checkout", "query": "main", "kinds": [], "path_glob": "", "language": "", "page": {"cursor": null, "limit": 20}}'
```

Notes:

- `code.analyze_repository` is a job submit; `code.get_analysis_job` polls it,
  and the accepted response carries the job id.
- `code.find_symbols` and the other query operations take the **concrete**
  revision sha (the 40-char hex id of the `@cwd` scan, printed on the job log
  line `ukma: indexed N files at revision <sha>`).
- The worktree is a **dirty snapshot** (`dirty_policy: "snapshot"`); the action
  makes no durability or cross-process claim.
