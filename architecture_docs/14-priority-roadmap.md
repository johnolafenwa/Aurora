# Aura: Next Core Batches

Status: proposed execution plan

Aura's next phase should strengthen the foundations already promised by the
language before adding more surface syntax. The work is ordered by dependency,
user impact, and the amount of later work each batch unlocks.

## Priorities

| Order | Batch | High-level outcome |
| --- | --- | --- |
| 0 | Contract closure | Ratify the completed ADR-0045 testing work and resolve ADR-0049's remaining class-pattern decision so the current contract has no provisional loose ends. |
| 1 | Loans and views | Implement ADR-0038 place-based loans and views across the checker, MIR, both native paths, diagnostics, tooling, and documentation. This is the semantic foundation for safe zero-copy APIs. |
| 2 | Lean native binaries | Establish representative size baselines, split the runtime from compiler-only code, link only required runtime components, and enable effective dead stripping. Small Aura programs should produce genuinely small standalone executables. |
| 3 | Rust-class native performance | Create an Aura-versus-Rust benchmark contract, then improve value representation, allocation, retain/release elimination, inlining, loop optimization, bounds-check elimination, and code generation until the measured gap is closed on the agreed workloads. |
| 4 | Multicore runtime scalability | Add dynamic load balancing or work stealing, bounded preemption, scalable wake paths, and more efficient task storage so CPU and mixed-I/O workloads scale predictably across cores. |
| 5 | Freestanding systems foundation | Ratify hosted and freestanding profiles, stable layout and ABI rules, raw memory and `unsafe` boundaries, volatile access, atomics, custom allocation, cross-compilation, startup, panic, and platform interfaces. |
| 6 | Bare-metal proof | Prove the systems foundation with a reproducible QEMU target that boots Aura code and exercises memory, interrupts or device I/O, allocation, failure handling, and a minimal driver-style boundary. |
| 7 | Array v2 and ML interoperability | Build strided views, reshape and transpose, broadcasting, matrix operations, vectorized kernels, shared-memory transport, and zero-copy interop with ADR-0038 as the single borrowing model. |
| 8 | Package ecosystem and tooling | Deliver dependable package resolution, registry and publishing workflows, reproducible builds, formatter and documentation tooling, and supply-chain metadata suitable for real projects. |
| 9 | Aura 0.4 language surface | Add the already-proposed union, decorator, display/property, docstring, generator, and iterator work in ADR dependency order. Generators follow loans/views and the required runtime work. |

## Execution Rules

- Aura remains clean-slate before broad adoption. Removed syntax and behavior are deleted, with no compatibility layer.
- Each batch begins by ratifying measurable acceptance criteria. Performance and size claims use reproducible Aura-versus-Rust workloads and published methodology.
- User-visible behavior lands atomically across compiler tests, backend parity, diagnostics, examples, tutorials, the manual, LSP, and extension surfaces where applicable.
- One complete green hosted CI run is sufficient for each merge-ready batch. Narrow checks should be used during development; the full gate runs at the batch checkpoint.
- Build outputs are cleaned when no longer useful so coverage, parity, and benchmark profiles do not accumulate indefinitely.

## Dependency Path

The critical path is:

**contract closure → loans/views → lean runtime → performance and scheduler work
→ freestanding foundation → bare-metal proof**

Array v2 depends on loans/views. Generator work depends on loans/views and its
runtime support. Package tooling can progress independently once the active
compiler and runtime contracts are stable, but it should not displace the
foundation work above it.

The next implementation batch is therefore **Contract closure + ADR-0038**.
It should stop at a reviewable checkpoint with the language contract, both
backends, tooling, reference documentation, and focused verification in sync.
