# Direct integer loop baseline

This is the V6 workload: a ten-million iteration counter loop, built with the
direct native backend, at `int32` and `int64` width. Both widths are kept here
so the relationship between them stays visible instead of being summarized
away. `startup.au` is a silent, empty direct program with the same runtime
entry path. The scalable-runtime runner pairs it with both loops so fixed
process startup is not mistaken for loop cost.

Run it with:

```bash
npm run bench:direct-integer-loops -- \
  --aura target/release/aura --repeats 11 \
  --raw-json /tmp/aura-rust-integer-loops.json
```

The runner reports raw samples, medians, minima, and paired Aura/Rust ratios
after one excluded warmup. Lane and width order alternate across repetitions.
Run from a clean checkout with a freshly built compiler. Both this helper and the scalable-runtime runner launch each
measured binary in a dedicated process group and verify that the complete
group is reaped on success, error, timeout, or interrupt.

The contractual scalable-runtime runner also reports a startup split:

```bash
npm run bench:scalable-runtime -- \
  --label v6-startup-split \
  --aura target/release/aura \
  --json /tmp/aura-v6-startup-split.json
```

It rotates the startup, `int32`, and `int64` process order on each repetition
and publishes both whole-process summaries and paired
`whole process - startup` loop estimates under
`benchmarks.v6.startup_vs_loop`.

The separate scalable-runtime report uses schema version 4. Schema version 3 identified each V6 run with
a `width` field and contained only the two integer loops. Version 4 identifies
each run with `workload` and adds the `startup` workload and split summary.

## Recorded baseline

Development workstation, Apple silicon, debug `aura` driving a release-quality
Cranelift build, seven repeats.

| Width | Before the V6 fix | After the V6 fix |
| --- | --- | --- |
| `int32` | 0.0697s | 0.0327s |
| `int64` | 0.0115s | 0.0111s |
| `int32` / `int64` | 6.05x | 2.95x |

`int64` is unchanged, as expected: the fix touches only the narrow-width range
check. See `work/2026-07-25-v6-direct-int32-loops.md` for the diagnosis and for
what still separates the two widths.

## Reactor-era baseline

The accepted post-Phase-5 whole-process baseline on the clean Mac14,9 host is
`37.436334 ms` for `int32` and `15.005584 ms` for `int64`, using the median of
five measured repetitions after warmup. These are respectively `14.36%` and
`46.42%` above the Batch-2 medians of `32.734250 ms` and `10.248625 ms`, so
they do not satisfy the attempted “within 10%” restoration target.

The split measures a fixed runtime-entry component separately from the loop.
One 21-repetition run measured a `7.679583 ms` startup median,
whole-process medians of `49.391916 ms` / `18.875542 ms`, and paired
loop-estimate medians of `41.746208 ms` / `11.123916 ms` for
`int32` / `int64`. That run occurred in a dirty checkout during concurrent
Batch-5 work. It proves that the maintained split works, but it does not
establish the complete cause of the regression or replace the clean baseline:
in particular, its `41.746208 ms` `int32` loop estimate does not reproduce the
Batch-2 whole-process median.

A separate `AURA_WORKERS=1` diagnostic measured a `7.851334 ms` startup
median. This gives no evidence that selecting one worker reduces the measured
startup component; it does not by itself prove which initialization work
causes the historical gap. The direct root scheduler remains the boundary
that owns task cleanup, traps, cancellation, and cooperative scheduling, so it
is not bypassed for scalar programs without evidence for a safe replacement.

Under the alternate disposition permitted by B5.0-d, the clean Phase-5.10
`37.436334 ms` / `15.005584 ms` whole-process pair is accepted as the
reactor-era baseline. The available evidence does not isolate startup as the
complete cause of the regression. The startup split remains in the maintained
runner so future loop work can compare loop estimates separately from process
entry cost.

Batch 6 repeated schema 4 after a real reboot of the baseline host. The clean,
contractual five-repetition replay measured `36.691666 ms` for `int32`,
`14.837417 ms` for `int64`, and `6.574667 ms` for direct startup, with paired
loop-estimate medians of `30.292500 ms` and `8.255709 ms`. The whole-process
pair is within `1.99%` / `1.12%` of, and slightly faster than, the accepted
Phase-5.10 pair. It did not reproduce the dirty
`49.391916 ms` / `18.875542 ms` diagnostic. The accepted reactor-era pair
therefore remains the maintained baseline; the cold-boot result and full
provenance are recorded in
`work/2026-07-27-phase5-runtime-benchmarks.md`.

## Batch S1 post-S2 replay

The clean, contractual schema-4 replay at
`face52e3900f775a3284df56a2519622d8381d60` used a fresh locked release build
at `2026-08-02T20:13:03.022427+00:00` on the same Mac14,9 Apple M2 Pro / 16 GiB
host. It retained the
established one warmup, five rotating repetitions, exact-output validation,
and paired startup subtraction.

| Lane | Whole-process median | Paired loop-estimate median |
| --- | ---: | ---: |
| `int32` | 36.222917 ms | 29.305958 ms |
| `int64` | 14.673875 ms | 7.744333 ms |
| startup | 6.570375 ms | — |

All five adjusted pairs were valid. The whole-process medians are 1.28% and
1.10% lower than the post-reboot Batch 6 observations, and the width ratio is
2.469x versus 2.473x. S2's unified `int64` index domain therefore introduces no
V6 regression. It does not change the outstanding narrow-arithmetic
representation work. The raw report is
`/tmp/aura-s1-post-s2-v6-face52e.json`, SHA-256
`491d1268398c46b0c55393d7542d63a93804034ba6e8b128be67565f93fcdf64`.

## Rust comparison lane (0.3.4 foundations)

The runner builds pinned Rust 1.95.0 references from `benchmarks/rust_baselines/`
with `--release --locked`, fat LTO, and one codegen unit. Report schema is now 2.
It records source/lockfile and binary SHA-256 identities and paired Aura/Rust
samples. Rust timing results are pending the post-reboot measurement session;
protocol smoke checks are not published as performance evidence.

See [the Rust baseline contract](../rust_baselines/README.md) for exact workload,
allocation, arithmetic, scheduling, and protocol equivalence. The integer-loop
lane retains its whole-process checksum protocol; other lanes use READY/GO/DONE.
