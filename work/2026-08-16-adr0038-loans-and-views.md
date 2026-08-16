# ADR-0038 loans and views

## Goal

Implement ADR-0038 end to end: place identity, shared and mutable views,
inferred lifetimes, returned views, reborrowing, closure interaction, cleanup,
diagnostics, backend parity, tooling, examples, tutorials, and maintained
reference material.

## Work completed

- Added contextual local and returned `view` grammar plus exhaustive lambda
  capture lists.
- Added semantic place/projection identity, overlap checking, last-use regions,
  reborrowing, returned-origin contracts, trait origin-slot conformance,
  loan-closure call kinds, escape checks, and `AU3010`.
- Added explicit MIR loan operations and scope/early-exit cleanup. MIR execution
  resolves live aliases for roots, fields, tuple positions, returned views, and
  mutable closure captures.
- Added direct-backend loan aliases, exact control-flow-selected returned-view
  projection handoff, tuple-place traversal, and mutable/consuming closure
  environment writeback parity.
- Updated compiler analysis, LSP bridge schema 6, hover/definition metadata,
  TextMate syntax, snippets, editor protocol expectations, ADR status, Manual,
  tutorials, and `examples/basics/views.au`.

## Verification

- All 38 focused parser, semantic, analysis, MIR, MIR-runtime, native-codegen,
  and native-runtime ADR-0038 unit tests pass. The public compiler/backend
  integration probe and all new parse/check/run fixtures pass.
- The complete Rust matrix passes: 365 CLI tests, 1,707 compiler library tests,
  and all integration suites. The forced MIR/direct fixture matrix passes with
  one aggregate, zero mismatches, in 1,082.98 seconds.
- Compiler coverage passes the unchanged floors at 94.78% regions
  (137,887/145,480), 97.23% functions (6,168/6,344), and 96.37% lines
  (93,552/97,071). LSP coverage is 100% for statements, branches, functions,
  and lines.
- LSP tests pass 110/110 and extension tests pass 25/25. Reference integrity,
  all 339 tutorial fences, generated LLM documentation freshness, the
  production docs build, formatting, Clippy with warnings denied, npm/Cargo
  audit, and repository hygiene pass.
- The first monolithic CI attempt passed every stage through the Rust matrix,
  then exhausted local disk while linking a late parity fixture. After cleaning
  only disposable build artifacts, the parity gate and every remaining CI gate
  passed on the same source. The exact-source compiler coverage rerun required
  normal `/dev/fd` access for native diagnostic-channel tests and passed outside
  the filesystem sandbox.
- The final live audit identified the newly published `nanoid <3.3.18`
  advisory. The transitive lock entry was upgraded to compatible version
  3.3.18; npm now reports zero vulnerabilities, and Cargo audit retains only
  its existing allowed `rustls-pemfile` maintenance warning.

## Follow-up

Indexed/keyed places, view-bearing aggregates, multi-origin returned views,
returned loan closures, and lifetime-bearing callable types remain
intentionally deferred by ADR-0038.
