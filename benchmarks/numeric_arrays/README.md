# Numeric-array release evidence

This benchmark measures two exact one-million-element `float64` workloads:

- fresh owned elementwise addition of two contiguous arrays
- reduction of one existing contiguous array with `sum()`

It compares Aura's direct native backend with NumPy and plain Rust under the
same explicit single-thread environment and records release evidence.

The add lane allocates and releases a fresh result on every measured
operation. All implementations prepare their two inputs before the clock
starts. The sum lane reuses one prepared input. Every process performs one
unmeasured kernel warmup, emits `READY`, waits for the host's exact `GO` line,
then reports a checksum in `DONE`. The host owns and verifies the whole
process group.

Run the benchmark only from a clean detached checkout on the maintained,
post-reboot Mac14,9 baseline:

```bash
python3 scripts/bench-numeric-arrays.py \
  --label phase73-post-reboot \
  --aura target/release/aura \
  --python /Applications/Xcode.app/Contents/Developer/usr/bin/python3 \
  --pairs 11 \
  --raw-json /private/tmp/aura-phase73-arrays-post-reboot-raw.json \
  --summary-json /private/tmp/aura-phase73-arrays-post-reboot-summary.json
```

The raw schema records all warmups and paired observations, exact commands,
source and binary hashes, repository/host/boot identity, dependency identity,
quiet-process inventories before build, before timing, and after timing,
parameters, checksums, and derived statistics.
Input provenance includes SHA-256 identities for the benchmark runner, NumPy
reference, and `scripts/benchmark_process.py`, which owns process-group launch,
timeout, and cleanup behavior.
The smaller summary repeats the release-relevant provenance and links back to
the raw report by SHA-256.

The six-lane order reverses every repetition. Each lane uses
`AURA_WORKERS=1` plus the common BLAS/OpenMP single-thread environment.
There are 512 add operations and 1,024 reductions per timed observation.
Reported values include raw samples, median, median absolute deviation, p95,
best, paired Aura/NumPy and Aura/Rust ratios, their medians, and ratios of medians.

No speed threshold is enforced against either reference implementation. A report is contractual only when
the checkout is clean and detached, the host is Mac14,9, every
protocol/checksum validates, the competing-process override is absent, and
all three host inventories are quiet. An inventory rejects an Aura-checkout
`cargo`, `rustc`, or `aura` process at any CPU level. It also rejects any other
process that remains at or above 50% CPU in two snapshots 0.25 seconds apart,
so a canonical CPU burner such as `yes` is recorded even outside the checkout.
The runner PID, its descendants, its direct parent, and the short-lived
`ps`/`lsof` inventory helpers are excluded from classification.

## Phase 7.3 measured result

The contractual run completed on the post-reboot Mac14,9 M2 Pro / 16 GiB host
at commit `0511adf61931953df096dc1b6721a543d856be25`. The recorded boot time
was `Thu Jul 30 23:02:25 2026`; the checkout was clean and detached, all three
quiet-host inventories were empty, and no override was used. Xcode Python
3.9.6 supplied NumPy 2.0.2 with Accelerate.

| workload | Aura median per operation | NumPy median per operation | ratio of medians |
| --- | ---: | ---: | ---: |
| fresh owned one-million-element `float64` add | 1.142461 ms | 0.251602 ms | 4.540751× |
| existing one-million-element `float64` sum | 1.150392 ms | 0.174065 ms | 6.608975× |

The 11-pair raw report is retained at
`/private/tmp/aura-phase73-arrays-post-reboot-raw.json`, SHA-256
`f51b979977519b5cbca9be4119a77bb3aff1d1a2874e1cdd4269f315bc1f9e7d`.
The summary is retained at
`/private/tmp/aura-phase73-arrays-post-reboot-summary.json`, SHA-256
`f6fc84c1f0fadfb4b93a5f07befb5a33cbaa6926d54ef88a795e103106b410ab`.
The measured release `aura` binary hash is
`a717e19d2f634087ae51c601632b428ed8cc5c98ed6745039d7f036b189ca035`.

Release disassembly of the float64 add kernel emitted scalar `fadd d`
instructions, and the deterministic floating reductions likewise remained
scalar. Aura's Array API and NumPy cover different surfaces; the workloads
above are the operations compared by this benchmark.

## Rust comparison lane (0.3.4 foundations)

The runner builds pinned Rust 1.95.0 references from `benchmarks/rust_baselines/`
with `--release --locked`, fat LTO, and one codegen unit. Report schema is now 2.
It records source/lockfile and binary SHA-256 identities and paired Aura/Rust
samples. Rust timing results are pending the post-reboot measurement session;
protocol smoke checks are not published as performance evidence.

See [the Rust baseline contract](../rust_baselines/README.md) for exact workload,
allocation, arithmetic, scheduling, and protocol equivalence. The integer-loop
lane retains its whole-process checksum protocol; other lanes use READY/GO/DONE.
