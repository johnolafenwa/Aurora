# ADR-0038 release readiness review

## Goal and authorized scope

Review the complete loans/views implementation before pushing a candidate for
release tests. Preserve compiler coverage gates at 96.30% lines, 97.21%
functions, and 94.71% regions and add tests as explicitly requested by the user.
No tag or publication is authorized by this readiness checkpoint.

## Work completed

- Reviewed ADR-0038 contracts, place/loan semantics, returned-view and closure
  handling, shared MIR validation, backend lowering, and release metadata.
- Fixed a returned-view symbolic-analysis resource-bound gap: common validation
  expanded and memoized paths before enforcing a cumulative allocation limit.
  It now reserves path bytes before composing suffixes/products and shares
  memoized strings. The public entry points are `run_mir`, `run_serialized_mir`,
  and native object emission.
- A failing regression proved the gap before the fix. It now rejects bounded
  malicious alias chains encoded with BeginLoan, Reborrow, or BeginReturnedLoan
  through all three public boundaries; an ordinary returned view still runs.
- The security fix workflow supplied an independent read-only investigation and
  a fresh independent post-fix bypass/regression review. The latter found no
  remaining actionable scoped finding. This bounds expanded path bytes, not
  total module memory or collection overhead.
- Corrected draft release notes to use actual binding syntax and mark the
  pending compiler/extension releases Unreleased rather than already published.
- Reverted the unapproved coverage-floor reduction from the earlier draft.
- Added meaningful coverage for implicit-import diagnostic classification,
  task-group failure-wake cleanup, and direct symbolic returned-view resource
  limits. These tests restore the original compiler coverage floors.
- A complete local run exposed one CLI-only timing flake under heavy parallel
  instrumentation. Daybreak review traced it to per-process scheduler-thread
  contention rather than queue semantics; the regression now requests one Aura
  worker while retaining its 15-second product watchdog, and it passed again in
  the same fully contended coverage run.
- Inspected the final 55-file candidate, including every production change,
  release identity, generated document, diagnostic snapshot, and tracking
  update. The staged patch is whitespace-clean and excludes all protected files.

## Verification

- Complete local `npm run ci`: passed with 1,881 compiler library tests and 375
  CLI integration tests in both ordinary and instrumented runs.
- Compiler coverage: 101,387/105,274 lines (96.307730%), 6,760/6,954 functions
  (97.210239%), and 149,596/157,785 regions (94.810026%). The unchanged
  96.30%/97.21%/94.71% gates pass.
- LSP: 111 tests with 100% statement/branch/function/line coverage.
- Extension: 27 tests; build and package-contract checks passed.
- Release packaging and identity: 36 and 15 tests passed respectively.
- Reference/tutorial checks: 340 tutorial fences and 129 verified Manual Aura
  blocks passed; generated agent documentation and the production docs build
  are current.
- Dependency audit: no npm vulnerabilities; Cargo audit has only the existing
  allowed `rustls-pemfile` unmaintained advisory. Formatting, Clippy with
  warnings denied, backend parity, and repository hygiene also passed.
- The exact coverage JSON was preserved before coverage-only artifacts were
  cleaned. `target/` returned below the repository's 20 GiB hygiene limit.

## Remaining work

Commit and push the reviewed candidate for one exact-candidate hosted
Linux/macOS CI run. Do not report publication or create a release tag at this
checkpoint. Protected user files remain untouched: personal/file_ops.au, .swp,
and the untracked ADR-0022 draft. Concurrent roadmap commits are preserved.
