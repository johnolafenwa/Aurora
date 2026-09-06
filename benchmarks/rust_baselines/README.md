# Rust baselines

Standalone Cargo project excluded from the Aura workspace, pinned to Rust
1.95.0 and tokio `=1.53.1` with its own committed lockfile. Release settings are
`opt-level=3`, `lto="fat"`, and `codegen-units=1`.

| Program | Equivalent work and output |
| --- | --- |
| `fib30` | Naive recursive fib(30), checksum 832040, same READY/GO/DONE records. |
| `tasks_10000` | Create and join 10,000 tokio tasks, sum indexes to 49995000, same protocol. |
| `tcp_fanout` | 20 pre-bound ephemeral loopback listeners, 20 clients, ping/pong and 100 ms handlers; checksum 80. No single-listener variant. |
| `retrying_worker` | Same local HTTP fixture and path/status schedule as existing Aura/CPython harness sources: 16 cycles, statuses 503/200/503/429/503/503/503, delays 4/6/3/5 ms per cycle, 112 requests, checksum 18112. |
| `int32_loop`, `int64_loop` | Ten million checked increments in the declared width; overflow fails, never wraps. Existing Aura whole-process output is `10000000` with a newline, with no handshake. |
| `startup` | Empty whole-process control. |
| `float64_add` | One million elements; fresh `Vec<f64>` each of 512 additions, checksum 2048.0; same warmup and protocol. |
| `float64_sum` | One million elements, 1024 sequential left-to-right sums, checksum 4096000000.0; same warmup and protocol. |

Concurrent programs use tokio's multi-thread runtime with workers equal to host
parallelism. They own and join their server/task work before DONE. The runtime is
initialized before READY. The Rust local HTTP server preserves the existing
fixture's wire contract; the runner does not substitute a remote service.
`black_box` prevents constant-folding/elimination of fib inputs, checked loops,
and Array kernels. Integer loops expose each counter update to the optimizer
barrier; this overhead is part of the disclosed Rust reference. Array addition
retains full fresh allocation; reduction does not use parallel or reassociated
summation. No timing claims are made by this implementation task.

Build and protocol-only verification:

```bash
cargo +1.95.0 build --manifest-path benchmarks/rust_baselines/Cargo.toml --release --locked --bins
python3 scripts/rust_baselines.py
```

The smoke tool verifies exact output/checksums once per binary and prints only
PASS records. Hosted `npm run ci` includes the build and smoke contract without
recording durations. The three timing runners build references in temporary
separate targets, retain provenance, then remove the targets. Profile environment
overrides are cleared for references so an Aura profile experiment cannot change
the Rust baseline unnoticed.
