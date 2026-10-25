# UKMA Phase 7 Migration PR — Pre-merge Checklist

> Per `docs/PHASE7_CODEGRAPH_MIGRATION_PLAN.md` §5: S1+ pull requests must include a
> migration ledger entry, license preservation, and tests. **Reject PRs missing any
> of the items below** (reviewer-enforced; see S0 report §6/§7).

## Slice identification

- [ ] `slice_id` (e.g. `S1-graph-minimal`) matches a plan slice (S1–S7)
- [ ] Single verifiable slice only (no multi-slice PRs)

## Migration ledger (required)

- [ ] `docs/migration/ledger/<slice_id>.yaml` added/updated:
  - [ ] upstream commit SHA + paths (from `upstream-lock.yaml`)
  - [ ] license: identified + notices_preserved
  - [ ] `changes` / `behavior` (preserved + intentional_breaking) listed
  - [ ] `tests`: golden/contract/integration status with **executed** results
  - [ ] review owner + status

## License (required)

- [ ] Upstream `LICENSE` (Apache-2.0) / `NOTICE` attribution preserved or recorded
- [ ] Per-file SPDX headers preserved on migrated files
- [ ] No proprietary/unknown-license content introduced

## Tests (required — actual execution, not claims)

- [ ] `cargo fmt --all -- --check` — exit 0
- [ ] `cargo check --workspace --all-targets` — exit 0
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` — exit 0, no warnings
- [ ] `cargo test --workspace --all-targets` — exit 0 (positive + negative + authorization + version + failure paths for new behavior)
- [ ] Golden/contract tests reference `tests/golden/` fixtures with source SHA recorded

## Architecture guardrails (required)

- [ ] No direct backend coupling (Graph/Storage go through ports/traits only)
- [ ] `ukma-contracts` gains no database-specific dependency
- [ ] No UKMA v1.2.3 wire-level API semantics changed without explicit architecture approval
- [ ] No upstream API signatures invented (all mapped signatures observed at the locked SHA)

## Change summary

| Item | Detail |
|---|---|
| Slice | |
| Upstream SHA | (from upstream-lock.yaml) |
| Files changed (count) | |
| New tests added | |
| Risks / breaking changes | |
