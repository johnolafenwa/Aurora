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
environment policy. Run the maintained harness with:

```bash
npm run bench:release-performance
```

The scalable-runtime and numeric-Array harnesses provide the deeper scheduler,
memory, and kernel evidence referenced by their Manual chapters.

## Executable Size

The 0.3.4 foundations size tool builds clean detached refs with Rust 1.95.0,
records exact executable byte counts and SHA-256 hashes, and removes each build
tree after hashing. Subjects are the compiler, a single-print hello world, and
`examples/agents/retrying_network_worker.au`, used as the reference-agent stand-in
until pre-Batch-1 item 6 lands. The before ref is `v0.3.3-preview`. The after
comparison includes Cargo-default and tuned release-profile builds in separate
targets. The tuned profile retains unwinding and runtime diagnostic metadata.

Size measurements are pending completion of the local semantic gates.

## Pending measurements

The Cranelift optimization-level and Rust-baseline timings will be collected in
a post-reboot session and are not yet published. Protocol-only Rust smoke checks
are correctness evidence, not timing results. Existing CPython and NumPy tables
above retain their original measurements and provenance.
