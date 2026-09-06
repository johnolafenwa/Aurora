# Performance

Aura tracks performance with reproducible programs, named hardware, pinned
source commits, raw observations, and content hashes. The current measurements
show where the compiler and runtime are already competitive and where later
releases need focused optimization.

This page is the performance record for the Aura 0.3 technical preview. It is
separate from the language's semantic guarantees.

## Current Measurements

The tables below were collected from exact programs in a clean detached
checkout at commit `18c45ac` on one post-reboot Mac14,9 with an Apple M2 Pro
(10 cores) and 16 GiB of memory. The recorded boot was 30 July 2026 at
23:02:25. The comparison interpreter was Xcode CPython 3.9.6.

### Control-Plane Workloads

For the four protocol workloads, the harness validates an exact `READY`
record, starts the clock when it sends `GO`, and stops at the exact `DONE`
record. Lower is faster. “Aura / CPython” is the ratio of medians.

| exact protocol workload | Aura median | CPython median | Aura / CPython |
| --- | ---: | ---: | ---: |
| naive recursive `fib(30)` | 93.875250 ms | 158.491666 ms | 0.592304 |
| create and join 10,000 tasks | 101.743042 ms | 51.950667 ms | 1.958455 |
| 20-client delayed loopback TCP fan-out | 104.505375 ms | 108.605459 ms | 0.962248 |
| 16-cycle retrying HTTP worker | 429.291292 ms | 520.447791 ms | 0.824850 |

The TCP shape uses 20 pre-bound loopback listeners. Aura 0.3 rejects transfer
of an accepted `TcpStream` into a handler task (`AU3008`), and a single listener
would serialize the handlers. The task measurement includes creation and join
of all 10,000 tasks after `GO`. The retry measurement executes the same status
and delay schedule in both programs.

### Integer Loops

The V6 integer loops are whole-process measurements. Startup-adjusted values
subtract a same-repetition startup control and estimate the loop cost.

| exact 10,000,000-iteration comparison | Aura whole process | CPython whole process | Aura startup-adjusted | CPython startup-adjusted |
| --- | ---: | ---: | ---: | ---: |
| Aura `int32` / CPython integer | 36.620333 ms | 321.096625 ms | 31.037083 ms | 295.458959 ms |
| Aura `int64` / CPython integer | 13.724042 ms | 321.096625 ms | 7.7378125 ms (10/11 valid) | 296.966042 ms (10 aligned pairs) |

Python has one arbitrary-precision integer lane, so the same CPython program is
shown against Aura's two fixed-width lanes.

### Numeric Arrays

Numeric Arrays were measured with NumPy 2.0.2 using one million `float64`
elements and 11 paired single-thread observations on the same host.

| exact Array workload | Aura median | NumPy median | Aura / NumPy |
| --- | ---: | ---: | ---: |
| fresh owned elementwise add | 1.142461 ms | 0.251602 ms | 4.540751 |
| existing-array sum reduction | 1.150392 ms | 0.174065 ms | 6.608975 |

The [Numeric Arrays](/manual/numeric-arrays) chapter records the complete Array
methodology and current API boundaries.

## Current Performance Gaps

The measurements identify two immediate gaps. Creating and joining 10,000 Aura
tasks takes about 1.96 times the CPython comparison workload. The measured Aura
Array addition and reduction kernels take about 4.54 and 6.61 times their NumPy
counterparts. The MIR backend also carries interpreter and synchronization
costs, so the direct native backend is the performance path.

These gaps are engineering targets. They do not change Aura's ownership,
failure, or concurrency semantics.

## Performance Direction

Later Aura releases will focus on closing the measured gaps while preserving
the language contract. The active direction includes:

- reducing task creation, join, wake, and scheduler synchronization overhead;
- expanding direct-backend optimization across call boundaries, loops, and
  temporary values;
- reducing allocation and copying in numeric workloads;
- adding specialized and vectorized Array kernels as the Array surface grows;
- profiling model-serving, agent-runtime, networking, and queue workloads at
  realistic concurrency levels; and
- keeping MIR and direct-backend behavior byte-compatible while the native
  path becomes faster.

Performance work remains benchmark-driven. A change closes a gap when the
repository harness reproduces the improvement on pinned workloads and the full
correctness and backend-parity gates remain green.

## Evidence And Reproduction

The release-performance raw evidence has SHA-256
`06cc1223630b1063c8a6806bf590449d6121a3be8d33e8dc1b0ffd17cee93ccb`.
Its SHA-linked summary has SHA-256
`4490e0d169d9a031ae57f04ade772d22169189f71a949356234f529d40e56236`.

The repository benchmark runner records commands, source and binary hashes,
raw observations, medians, dispersion, host inventory, boot identity, and the
environment policy. From a clean detached checkout on the pinned measurement
host, run the maintained harness with its Rust lanes:

```bash
npm run bench:release-performance -- \
  --label foundations-after-rust \
  --aura target/release/aura \
  --python /Applications/Xcode.app/Contents/Developer/usr/bin/python3 \
  --pairs 11 \
  --raw-json /tmp/aura-foundations-after-release-raw.json \
  --summary-json /tmp/aura-foundations-after-release-summary.json
```

The scalable-runtime and numeric-Array harnesses provide the deeper scheduler,
memory, and kernel evidence referenced by their Manual chapters.

## Executable Size

The pre-Batch-1 implementation reduces executable sizes while retaining panic
unwinding and Aura's embedded diagnostic metadata. Counts below are exact bytes
from separate clean detached builds; the table counts each executable, while
installed toolchains also ship the separate runtime archive.

| Executable | Before: v0.3.3-preview, Cargo default | After: Cargo default | After: tuned release | Reduction from before |
| --- | ---: | ---: | ---: | ---: |
| `aura` compiler | 15,424,032 | 15,866,008 | 10,897,392 | 29.35% |
| Native hello world | 23,646,792 | 1,702,088 | 1,586,968 | 93.29% |
| Retrying-worker stand-in | 23,715,816 | 4,049,240 | 3,666,600 | 84.54% |

The tuned profile uses level 3, fat LTO, one codegen unit, no debug data, and
stripped compiler symbols. The after-default control restores Cargo's default
release values through `CARGO_PROFILE_RELEASE_*` overrides in a separate target
directory; it retains the new Cranelift flag and user-executable link/strip steps.
Both after columns use macOS unused-section removal and debug/local-symbol
stripping of user binaries. The runtime archive retains its linkable symbols.

Provenance: Mac14,9 / Apple M2 Pro, arm64, macOS 26.5.2; Rust 1.95.0
(`59807616e`, LLVM 22.1.2); Apple clang 21.0.0 (`clang-2100.1.1.101`). The before
ref is `v0.3.3-preview` (`d3cc6b96104dd597687a98e9624f800a0cb3cf1e`). The after
comparison uses the delivered pre-Batch-1 implementation. Exact measured commits,
commands, profile overrides, source and executable SHA-256 hashes, and standalone
output are retained in the [versioned raw evidence](https://github.com/johnolafenwa/Aura/blob/main/work/2026-09-07-pre-batch-1-executable-sizes.json).

The hello input is `examples/basics/hello_world.au`, a single print. Because the
before ref predates that file, the tool stages identical source bytes in its
build area. `examples/agents/retrying_network_worker.au` is the reference-agent
stand-in until pre-Batch-1 item 6 lands. Both subjects produce identical output
across all three builds with Cargo unavailable during standalone execution.
Each detached checkout and target directory is removed after hashing.

Reproduce from the desired after checkout:

```bash
python3 scripts/bench-binary-size.py \
  --before-ref v0.3.3-preview \
  --after-ref "$(git rev-parse HEAD)" \
  --output /tmp/aura-executable-sizes.json
```

## Pending measurements

The Cranelift optimization-level and Rust-baseline timings will be collected in
a post-reboot session and are not yet published. Protocol-only Rust smoke checks
are correctness evidence, not timing results. Existing CPython and NumPy tables
above retain their original measurements and provenance.
