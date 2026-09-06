# Pre-Batch-1 foundations, items 1–4

## Goal and authorized scope

Deliver Cranelift speed optimization (subject to parity), a backend boundary
inventory, equivalent Rust benchmark lanes, release-profile/link improvements,
and deterministic executable-size evidence for the 0.3.4 update. No release,
version bump, tag, reboot, or publication-grade timing measurement is authorized.
Branch: `codex/pre-batch-1-foundations`. Preserve the existing personal file,
`.swp`, and the untracked ADR-0022 draft.

## Progress and verification

- Baseline recorded release coverage: 96.307730% lines, 97.210239% functions,
  94.810026% regions (prior release evidence, not a new measurement).
- Enforced floors remain 96.30/97.21/94.71.
- Test-first flag regression failed with `None` versus `Speed` as expected.
- Test-first platform-link regression failed because the helper was absent.
- Implementation and all required gates are in progress.

## Remaining work

Complete items 1–4, local gates, clean-ref size measurements, documentation,
branch hosted CI, merge-commit PR, main hosted CI, measurement handoff, cleanup.

## Verification notes

- All nine Rust workloads pass one exact protocol/checksum smoke run; no timing
  is published. Standalone `cargo audit` found no vulnerabilities in 10 packages.
- Python runner/size/provenance tests pass (43). Release packaging tests pass (36).
- Reference gate exposed a pre-existing post-publication metadata test that still
  required an unreleased 0.3.3 changelog. Updated it to require the published
  2026-09-06 entry and new 0.3.4 unreleased section; manifest versions stay 0.3.3.
- Plain `git diff --check` reports existing whitespace in protected
  `personal/file_ops.au`; the repository hygiene gate explicitly excludes it
  and passes. The file remains untouched.

- The first full Rust run passed 374/375 CLI tests but failed the cache entry-id
  recovery assertion during concurrent runtime-archive refreshes from other
  compiler gates. Daybreak's read-only investigation identified content-key
  drift; the exact isolated cache regression passed (1/1). The isolated
  direct/MIR stripped source-frame test also passed (1/1). No link reversion
  is indicated. Subsequent compiler gates run without competing builds.
- Cache format v6 invalidates the prior artifact construction pipeline, keeping
  product and semantic-schema versions unchanged. Its regression failed first
  with v5, then passed after the single constant change.

- The uncontended outer Rust run passed 374/375 CLI tests, including cache
  recovery. Its one failure was the manual CFG/view object's linker reporting
  `target/debug/libaura_compiler.a` absent during another test's archive rebuild
  (cli.rs:1291), before execution. This is shared parallel-test build state,
  not a generated-code mismatch; an isolated rerun is recorded below.

- The isolated CFG/view regression passed (1/1). All compiler suites passed:
  1,882 compiler unit/native-codegen tests, all fixture suites, and the remaining
  package, FFI, semantic-interface, runtime, and diagnostic integration suites.
  No fixture expectation or codegen workaround was changed.

- Forced MIR/direct parity passed all 385 runtime fixtures (291 run-pass and
  94 run-fail), fallback disabled and loopback available. Exact stdout and
  diagnostics matched the unchanged fixture oracles. Cranelift speed and both
  link steps remain adopted, pending final coverage/hosted confirmation.

- Clippy passed with warnings denied. After parity, `cargo clean --profile dev`
  removed obsolete debug outputs: physical `target/` usage fell from about
  20 GiB to 6.7 GiB, with 64 GiB free before coverage. Coverage retains the
  exact existing 96.30/97.21/94.71 floors.

- The coverage workspace run passed all 375 CLI integration tests serially,
  including both regressions affected by archive replacement during parallel
  runs. The CLI unit suite passed 34/34; native-codegen, FFI, backtrace, and
  package acceptance suites also passed.

- Full serial workspace coverage passed: 101,387/105,275 lines (96.306815%),
  6,760/6,954 functions (97.210239%), and 149,598/157,790 regions (94.808289%).
  All three unchanged floors are met. The complete workspace test run was green.
- Coverage outputs and the now-obsolete coverage-only uninstrumented runtime
  target are cleaned before the separate release-profile diagnostic check.

- Added a test-first regression requiring Rust baseline builds to use the
  shared owned-process-group helper. It failed against the original subprocess
  call, then passed after using the existing timeout/interrupt cleanup path.
  All 43 benchmark tooling tests remain green; workload protocols are unchanged.

- Actual fat-LTO release-profile direct/MIR launcher regression passed, retaining
  source frames with absent source files and unavailable Cargo in child execution.
  Rust 1.95.0's matching `llvm-nm` confirms the runtime archive retains
  `aura_native_run` and 356 direct runtime exports. Apple's older `nm` cannot
  parse the newer embedded LLVM attributes; this is a reader-version mismatch,
  not a link failure. The archive is not post-link stripped.
- Size-command cleanup also has a red/green regression for the shared owned
  process-group helper. All 44 runner/size/provenance tests now pass.
- Full local gates are green (parallel build-state failures classified above);
  remaining work is clean-ref size publication, final docs checks, hosted branch
  CI, merge, hosted main CI, handoff, and final cleanup.
