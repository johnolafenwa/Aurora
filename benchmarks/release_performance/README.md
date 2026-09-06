# Release-performance workloads

This directory contains the Aura and CPython inputs established for Batch 6.
The maintained runner now also builds equivalent pinned Rust inputs from
`../rust_baselines/` and records all three lanes with host and source provenance.

## Workloads

- `fib30.au` and `fib30.py` run the same naive recursive `fib(30)` algorithm
  and must produce `832040`.
- `tasks_10000.au` and `tasks_10000.py` create exactly 10,000 tasks after the
  measurement starts, join every task, and must produce the checksum
  `49995000`.
- `tcp_fanout.au` and `tcp_fanout.py` run 20 concurrent loopback clients and
  20 delayed handlers. Each client receives `pong`, producing the checksum
  `80`. Aura uses 20 pre-bound ephemeral listeners because Aura 0.2 does
  not permit transferring an accepted `TcpStream` to a handler task
  (`AU3008`); a single listener would serialize handler work rather than
  measure fan-out.
- `retrying_worker.au` and `retrying_worker.py` execute 16 identical HTTP
  retry cycles. Each cycle covers recovery after `503`, rate limiting with
  `429`, and exhausted `503` retries. A valid run serves 112 requests, has
  288 ms of specified retry delay, and produces the checksum `18112`.
- `python_int_loop.py` and `python_startup.py` are the CPython counterparts to
  the accepted Aura V6 sources in `../direct_integer_loops/`. Python has one
  arbitrary-precision integer lane, so the same Python loop is paired
  separately with Aura's `int32` and `int64` lanes.

The numeric-Array comparison is intentionally not rerun by this harness. Its
separately qualified NumPy evidence is linked in the raw and summary reports
and merged into the consolidated benchmark note.

## Measurement protocol

The fib, task, TCP, and retry inputs use exact `READY`/`GO`/`DONE` records.
The host starts timing only after receiving and validating `READY`, then sends
the exact `GO` record. For example:

```text
READY release-performance fib30 30
GO release-performance fib30
DONE release-performance fib30 832040
```

The other successful records are:

```text
READY release-performance tasks 10000
GO release-performance tasks
DONE release-performance tasks 10000 49995000

READY release-performance tcp-fanout 20 100 4
GO release-performance tcp-fanout
DONE release-performance tcp-fanout 20 80

READY release-performance retrying-worker 16 112 288
GO release-performance retrying-worker
DONE release-performance retrying-worker 112 18112
```

Any unexpected record, checksum, standard-error output, timeout, or nonzero
exit invalidates the run. V6 remains a whole-process comparison so the exact
accepted sources are reused. The runner reports raw whole-process durations
as primary evidence and also reports same-repetition startup subtraction as a
loop-only estimate. Nonpositive adjusted samples are retained as invalid
observations and excluded from the estimate rather than invalidating unrelated
measurements.

## Reproducing the qualified run

Run from a clean detached checkout on the accepted post-reboot Mac14,9 host
with no competing sustained-CPU process:

```bash
/Applications/Xcode.app/Contents/Developer/usr/bin/python3 \
  scripts/bench-release-performance.py \
  --label batch6-final-post-reboot \
  --aura target/release/aura \
  --python /Applications/Xcode.app/Contents/Developer/usr/bin/python3 \
  --pairs 11 \
  --raw-json /private/tmp/aura-b6-release-performance-raw.json \
  --summary-json /private/tmp/aura-b6-release-performance-summary.json
```

The runner requires exactly 11 rotating pairs, performs one excluded warmup
per lane, builds a fresh locked release compiler and all Aura workload
binaries before timing, verifies CPython identity, clears known
runtime-affecting environment overrides, and rechecks the repository and input
hashes after timing. It records three quiet-host inventories, boot and hardware
identity, commands, raw observations, hashes, median, MAD, nearest-rank p95,
best, and paired Aura/CPython and Aura/Rust ratios for protocol workloads. The summary links the exact raw report
by SHA-256.

`--allow-competing-processes` exists only for explicitly non-contractual
diagnostic runs. Do not use it for release evidence.

## Rust comparison lane (0.3.4 foundations)

The runner builds pinned Rust 1.95.0 references from `benchmarks/rust_baselines/`
with `--release --locked`, fat LTO, and one codegen unit. Report schema is now 2.
It records source/lockfile and binary SHA-256 identities and paired Aura/Rust
samples. Rust timing results are pending the post-reboot measurement session;
protocol smoke checks are not published as performance evidence.

See [the Rust baseline contract](../rust_baselines/README.md) for exact workload,
allocation, arithmetic, scheduling, and protocol equivalence. The integer-loop
lane retains its whole-process checksum protocol; other lanes use READY/GO/DONE.
