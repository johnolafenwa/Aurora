# ADR-0064: Native backend strategy and codegen boundary

- Status: Accepted direction; detailed design pending
- Date: 2026-09-06
- Implementation: Not started
- Roadmap: Pre-Batch-1 foundations; incremental boundary work from Batch 1;
  release-backend decision in Batch 7
- Related: ADR-0031, ADR-0038, ADR-0041, and ADR-0058

## Authority and scope

The user approved this amendment to the
[priority roadmap](../14-priority-roadmap.md) on 2026-09-06. Cranelift remains
the native backend through Batch 6. MIR execution remains the other maintained
backend. This design record schedules work; no flag, benchmark, release, or
backend implementation changes are delivered by the documentation amendment.

## Pre-Batch-1 measurements

The current direct flag setup in
[`native_codegen.rs`](../../crates/aura-compiler/src/native_codegen.rs) sets
`is_pic`, `unwind_info`, and `enable_multi_ret_implicit_sret`, but does not set
`opt_level`. Native code therefore uses Cranelift's default optimization level.
Measure `opt_level=speed` with the release-performance, integer-loop, and
numeric-Array harnesses. Publish the effect, hardware/toolchain/commit
provenance, and paired measurements in the
[Performance chapter](../../docs/manual/performance.md) by the v0.3.3-preview
update. Adopt the flag when the forced MIR/direct parity matrix passes;
otherwise revert it and record the failing fixtures as Batch 7 input.

Add Rust counterparts for fib30, 10,000 tasks, TCP fan-out, the retrying worker,
integer loops, and Array addition/sum kernels. Use tokio for the three concurrent
workloads and plain Rust elsewhere, matching the READY/GO/DONE measurement
protocol, work, and provenance rules. Publish paired Aura/Rust medians before
making a backend decision. These baselines ratify no numeric speed target.

The workspace currently has no `[profile.release]`, and user programs link
against the compiler/runtime `libaura_compiler.a`. The pre-Batch-1 update
configures release optimization, LTO, codegen units, symbol stripping and debug
data separation, plus dead-stripping flags for user executables. Preserve the
runtime's unwinding panic strategy. Record sizes for `aura`, a native hello
world, and the reference agent while preserving standalone execution and
source diagnostics. Runtime/compiler crate separation stays in Batch 5.

## Thin backend boundary

Before Batch 1 design, inventory semantic decisions currently implemented in
`native_codegen.rs` rather than MIR. Record that inventory and the proposed
boundary in `architecture_docs/15-backend-boundary.md`. The note is a future
foundation deliverable, not an artifact claimed complete by this ADR.

Push semantic decisions into checked MIR; make native emission a mechanical
translation behind a small builder interface. The interface's exact shape is
to be designed in Batch 1. A later second native backend should implement
this interface instead of duplicating ownership, cleanup, typing, or failure
semantics. Adopt the boundary incrementally from Batch 1 under parity and
coverage gates. All Batch 1–4 and 6 features must run on both maintained
backends through this boundary, with the forced parity matrix as the gate.

The structural split of `sema.rs` in Batch 1 separates type representation,
capability checking, and place/loan analysis while the type/callable surface
changes. It supports this semantic boundary without switching native backend.

## Batch 7 decision

With Rust baselines and the thin boundary in place, select the release-native
strategy among:

- LLVM through inkwell;
- C emission through the host C compiler already required by native builds;
- continued improvement of Cranelift code generation.

C emission is a serious candidate because it avoids a separate LLVM build
dependency and makes the freestanding targets of Batches 12–13 cheaper to
support. Decide using measured runtime, compilation, code-size, diagnostic,
and toolchain results. No backend switch occurs before Batch 7.

## Why the switch is not first

The measured Array and task gaps involve Rust runtime code already compiled
by LLVM. A new native emitter alone does not resolve those runtime costs.
The language surface is also about to change substantially: duplicating it
across a new backend before the semantic boundary is settled increases work
and parity risk. Cranelift's compilation speed remains valuable: it could
eventually let `aura run` compile natively and retire MIR interpretation.
That possibility is a later measured decision, not a CLI-default change here.

## Completion evidence and locations

| Evidence | Location and criterion |
| --- | --- |
| Release-performance and Rust workloads | `scripts/bench-release-performance.py` and `scripts/test_bench_release_performance.py`; paired fib/task/TCP/retry medians and matching protocol results |
| Integer loops | `scripts/bench-direct-integer-loops.py`; paired equivalent Rust loops and before/after optimization results |
| Numeric kernels | `scripts/bench-numeric-arrays.py` and `scripts/test_bench_numeric_arrays.py`; paired add/sum results with equal workload and allocation accounting |
| Semantic parity | `crates/aura/tests/backend_parity.rs` via `npm run test:backend-parity`; forced MIR/direct matrix green, failing fixtures retained if the optimization flag is reverted |
| Boundary regression coverage | `crates/aura-compiler/src/mir_tests.rs` and `crates/aura-compiler/src/native_codegen_tests.rs`; moved semantic decisions retain observable results and diagnostics |
| Coverage | `scripts/coverage-compiler.sh` and the existing compiler gates; incremental refactors retain required coverage |
| Standalone artifacts and diagnostics | `scripts/test_release_packaging.py` and the existing CLI tests; optimized/stripped archives still run and retain source diagnostics |
| Published measurements | `docs/manual/performance.md`; provenance, paired medians, and executable-size table |
| Boundary design | `architecture_docs/15-backend-boundary.md`; semantic-decision inventory and small builder-interface design |
| Usability oracle | One current-surface package under 400 lines in `examples/agents/`, included in example smoke tests and run on both backends; before/after diffs at Batch 1–4 and 6 checkpoints |

The reference agent contains a tool registry, typed tool schemas, retries, a
streaming loop, and structured cleanup using only current language/library
facilities. Upcoming generated schemas or generators must not be prerequisites
for its initial version.

## Remaining design

The builder API, exact semantic moves, Rust benchmark source placement,
release-profile values, and final backend choice are implementation/design
work for the scheduled batches. New language or protocol spellings are to be
designed in their owning batch. The ten Approved Decisions remain unchanged.
