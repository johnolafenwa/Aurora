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
- Implementation and local gates are complete; final-head size evidence and
  hosted integration remain in progress.

## Remaining work

Complete final-head size measurements and publication, final documentation
checks, branch hosted CI, merge-commit PR, main hosted CI, handoff, and cleanup.

## Verification notes

- All nine Rust workloads pass one exact protocol/checksum smoke run; no timing
  is published. Standalone `cargo audit` found no vulnerabilities in 10 packages.
- Python runner/size/provenance tests pass (44). Release packaging tests pass (36).
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

## Executable-size evidence

The initial clean-ref measurement uses v0.3.3-preview and implementation commit
`11ce755dd97386c2aa230ba16b5f7e26a675e2cf`. The final branch head is confirmed
separately before integration. The canonical raw report is
`work/2026-09-07-pre-batch-1-executable-sizes.json`.

| Subject | Before bytes | After default bytes | After tuned bytes | Reduction |
| --- | ---: | ---: | ---: | ---: |
| `aura` compiler | 15,424,032 | 15,866,008 | 10,897,392 | 29.35% |
| Native hello world | 23,646,792 | 1,702,088 | 1,586,968 | 93.29% |
| Retrying-worker stand-in | 23,715,816 | 4,049,240 | 3,666,600 | 84.54% |

All three clean builds passed standalone hello/worker execution with identical
source bytes and full output, and Cargo unavailable. The tuned profile uses fat
LTO; no fallback to thin was needed locally. All size worktrees and target trees
were removed after hashing. Timing measurements remain pending after reboot.

- Final documentation gates passed after registering the new size-command fence:
  Manual reference replay (129 verified Aura blocks), 340 tutorial fences,
  generated LLM check, production docs build, 15 identity tests, 36 packaging
  tests, and repository hygiene. The fence is orchestration metadata; no
  language fixture expectation was changed.

## Hosted verification

- PR: https://github.com/johnolafenwa/Aura/pull/6.
- Initial hosted CI at `9f24820` failed on both hosts in the existing scalable
  runner test: its integer-helper mock returned empty output and expected the
  old label. The stricter integer runner correctly requires `10000000` plus a
  newline. This same failure was present in an earlier local log and was missed
  when the surrounding shell command's final status was inspected.
- Updated only that mock and its current label, preserving checksum validation.
  All 100 benchmark-tooling tests now pass together. No language fixture or
  runtime behavior changed. The final CI head's sizes will be reconfirmed before
  merge; the current table was confirmed at `9f24820`.
