# S2 language migration status

Upstream commit: `590c2ca7dd7b37bd1d41caf45fac535b6522f969`  ·  languages: 38  ·  upstream `.rs` files: 277

**A language is `Ported` only with `grammar_ported: true` *and* a golden parity report.** One language (`rust`) has landed; the other 37 rows are `not_run`.

| Language | Upstream files | Grammar ported | Parity | Ledger |
|---|---:|---|---|---|
| bash | 5 | no | not_run | [s2-bash.yaml](ledger/s2-bash.yaml) |
| c | 14 | no | not_run | [s2-c.yaml](ledger/s2-c.yaml) |
| clojure | 6 | no | not_run | [s2-clojure.yaml](ledger/s2-clojure.yaml) |
| cobol | 9 | no | not_run | [s2-cobol.yaml](ledger/s2-cobol.yaml) |
| cpp | 7 | no | not_run | [s2-cpp.yaml](ledger/s2-cpp.yaml) |
| csharp | 8 | no | not_run | [s2-csharp.yaml](ledger/s2-csharp.yaml) |
| css | 6 | no | not_run | [s2-css.yaml](ledger/s2-css.yaml) |
| dart | 8 | no | not_run | [s2-dart.yaml](ledger/s2-dart.yaml) |
| dockerfile | 9 | no | not_run | [s2-dockerfile.yaml](ledger/s2-dockerfile.yaml) |
| elixir | 5 | no | not_run | [s2-elixir.yaml](ledger/s2-elixir.yaml) |
| elm | 6 | no | not_run | [s2-elm.yaml](ledger/s2-elm.yaml) |
| erlang | 6 | no | not_run | [s2-erlang.yaml](ledger/s2-erlang.yaml) |
| fortran | 9 | no | not_run | [s2-fortran.yaml](ledger/s2-fortran.yaml) |
| go | 7 | no | not_run | [s2-go.yaml](ledger/s2-go.yaml) |
| groovy | 5 | no | not_run | [s2-groovy.yaml](ledger/s2-groovy.yaml) |
| haskell | 5 | no | not_run | [s2-haskell.yaml](ledger/s2-haskell.yaml) |
| hcl | 5 | no | not_run | [s2-hcl.yaml](ledger/s2-hcl.yaml) |
| java | 7 | no | not_run | [s2-java.yaml](ledger/s2-java.yaml) |
| julia | 5 | no | not_run | [s2-julia.yaml](ledger/s2-julia.yaml) |
| kotlin | 9 | no | not_run | [s2-kotlin.yaml](ledger/s2-kotlin.yaml) |
| lua | 6 | no | not_run | [s2-lua.yaml](ledger/s2-lua.yaml) |
| objc | 5 | no | not_run | [s2-objc.yaml](ledger/s2-objc.yaml) |
| ocaml | 5 | no | not_run | [s2-ocaml.yaml](ledger/s2-ocaml.yaml) |
| perl | 7 | no | not_run | [s2-perl.yaml](ledger/s2-perl.yaml) |
| php | 8 | no | not_run | [s2-php.yaml](ledger/s2-php.yaml) |
| python | 17 | no | not_run | [s2-python.yaml](ledger/s2-python.yaml) |
| r | 8 | no | not_run | [s2-r.yaml](ledger/s2-r.yaml) |
| ruby | 7 | no | not_run | [s2-ruby.yaml](ledger/s2-ruby.yaml) |
| rust | 7 | **yes** | **passed** (10 tests) | [s2-rust.yaml](ledger/s2-rust.yaml) |
| scala | 6 | no | not_run | [s2-scala.yaml](ledger/s2-scala.yaml) |
| solidity | 6 | no | not_run | [s2-solidity.yaml](ledger/s2-solidity.yaml) |
| swift | 7 | no | not_run | [s2-swift.yaml](ledger/s2-swift.yaml) |
| tcl | 11 | no | not_run | [s2-tcl.yaml](ledger/s2-tcl.yaml) |
| toml | 7 | no | not_run | [s2-toml.yaml](ledger/s2-toml.yaml) |
| typescript | 7 | no | not_run | [s2-typescript.yaml](ledger/s2-typescript.yaml) |
| verilog | 9 | no | not_run | [s2-verilog.yaml](ledger/s2-verilog.yaml) |
| yaml | 5 | no | not_run | [s2-yaml.yaml](ledger/s2-yaml.yaml) |
| zig | 8 | no | not_run | [s2-zig.yaml](ledger/s2-zig.yaml) |

## Landed slice: `rust`

| Evidence | Where |
|---|---|
| Grammar (pinned to the locked upstream `Cargo.lock`) | `tree-sitter 0.25.10`, `tree-sitter-rust 0.24.2` |
| Extractor | `crates/code-intelligence/src/parsers/rust.rs` |
| Golden fixture | `crates/code-intelligence/tests/golden/s2-rust/sample.rs` |
| Golden parity test (10 tests) | `crates/code-intelligence/tests/s2_rust_parity.rs` |
| Coverage gate | `crates/code-intelligence/tests/s2_coverage.rs` |
| Ledger slice | [s2-rust.yaml](ledger/s2-rust.yaml) |

Parity scope: symbol identity, 1-based spans, containment/import/call edge kinds and
resolutions. Cross-file name resolution, macro expansion and type/lifetime inference
are **not** covered, so call edges stay `static_possible` or `unresolved` and are
never `static_exact`.

## What must happen before a row flips to `ported`

1. Add the grammar dependency (or a native extractor) and record the decision in the
   language's ledger slice under `changes`.
2. Implement symbol/span/edge extraction that satisfies the ledger's
   `required_behaviour` list.
3. Add a golden fixture plus a parity report comparing against the locked upstream
   behaviour; store it next to the slice.
4. Flip `grammar_ported: true` and `parity: passed` **in the same PR** that adds the
   tests — a status without a test is rejected by `s2_coverage`.
