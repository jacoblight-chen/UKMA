#!/usr/bin/env bash
# UKMA GitHub Action entrypoint (CodeGraph-style PR review).
#
# 1. Locates or builds the `ukma` binary.
# 2. Indexes the checkout as the @cwd revision (Tier 2).
# 3. Runs one tool by name (Tier 0) and captures the result.
# 4. Posts the result as a pull request comment when applicable.
set -uo pipefail

# --- 1. Binary ----------------------------------------------------------------
BIN=""
if [[ -n "${UKMA_BINARY:-}" && -x "${UKMA_BINARY}" ]]; then
  BIN="${UKMA_BINARY}"
elif [[ -f "target/release/ukma" ]]; then
  BIN="target/release/ukma"
else
  cargo build -p ukma-cli --release
  BIN="target/release/ukma"
fi

# --- 2. + 3. Index and run ----------------------------------------------------
output="$(
  "$BIN" \
    --index-cwd "$GITHUB_WORKSPACE" \
    --run-tool "$UKMA_TOOL" \
    --tool-args "$UKMA_TOOL_ARGS"
)"
rc=$?

echo "## UKMA tool result (${UKMA_TOOL}, exit ${rc})"
echo '```'
echo "${output}"
echo '```'

if [[ "${rc}" -ne 0 ]]; then
  echo "::error::ukma tool ${UKMA_TOOL} exited with code ${rc}"
fi

# Expose the output to the action's `output` output.
{
  echo "output<<UKMA_EOF"
  echo "${output}"
  echo "UKMA_EOF"
} >>"${GITHUB_OUTPUT}"

# --- 4. Post a review comment --------------------------------------------------
if [[ "${UKMA_POST:-true}" == "true" && -n "${GITHUB_TOKEN:-}" && -n "${GITHUB_EVENT_PATH:-}" ]]; then
  case "${GITHUB_EVENT_NAME:-}" in
    pull_request | pull_request_target)
      number="$(jq -r '.number // empty' "${GITHUB_EVENT_PATH}")"
      if [[ -n "${number}" ]]; then
        comment="**UKMA** \`${UKMA_TOOL}\` (worktree revision \`@cwd\`):

\`\`\`
${output}
\`\`\`"
        payload="$(jq -n --arg body "${comment}" '{body: $body}')"
        if curl -s -f -X POST \
          -H "Authorization: Bearer ${GITHUB_TOKEN}" \
          -H "Accept: application/vnd.github+json" \
          "https://api.github.com/repos/${GITHUB_REPOSITORY}/issues/${number}/comments" \
          -d "${payload}" > /dev/null; then
          echo "Posted the result as a comment on PR #${number}."
        else
          echo "::warning::posting the PR comment failed; the result is in the job log"
        fi
      fi
      ;;
    *)
      echo "No pull request context (${GITHUB_EVENT_NAME:-unknown}); skipping the comment."
      ;;
  esac
fi

exit "${rc}"
