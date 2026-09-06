# Aura: Next Core Batches

Status: proposed roadmap, revised 2026-09-06 following the open-ADR review.

Make Aura easy to program with Python-like syntax, static typing, deterministic
ownership, no garbage collection, typed failures, and structured concurrency.
Prioritize practical agent and ML software while building toward Rust-class
performance and eventual operating-system and device-driver development.

## Current Foundation

ADR-0038 loans/views is recorded as implemented; ADR-0045 testing is accepted
and implemented; ADR-0049 is accepted with class patterns formally deferred.
These are existing foundations. Collection-element loans remain future work.
Classes already have field-based constructors and per-instance defaults;
`with` already manages builtin resources and eligible non-generic user classes.

The five proposed ADRs are 0052 (unions), 0053 (decorators), 0054
(generators/iteration), 0055 (Display/properties), and 0056 (docstrings).
The batches below cover all five and the missing designs identified around
them. New designs and changes to proposed ADRs require ratification before
implementation. See the [ADR index](decisions/README.md) for their contracts.

## Priority Batches

| Order | Batch | Outcome and scope |
| --- | --- | --- |
| 1 | Callable and type foundations | Design storable capturing closures, bound method values, repeatable/mutable/consuming callable capabilities, and preserved names/defaults/keyword-only contracts. Add type aliases and ADR-0052 closed unions for explicitly typed mixed collections such as `list[int \| str]`, optional values, and safe narrowing. |
| 2 | Natural collection access | Extend ADR-0038 to list elements, dictionary entries, and slice views. Make shared reads, explicit cloning, mutation, and ownership transfer compose naturally; define bounds, structural mutation, and invalidation rules. |
| 3 | Initialization and resource management | Design custom initialization with `self`, definite field initialization, validation, and fallible construction while retaining simple field constructors. Extend `with` to generic managers, multiple resources, typed entry/exit, and scoped access with deterministic cleanup. |
| 4 | Everyday syntax and API usability | Add consistent trailing commas, parenthesized imports, richer unpacking and class patterns, and focused text/byte conveniences. Implement ADR-0056 docstrings and ADR-0055 Display/read-only properties, including local/imported editor support. |
| 5 | Lean native binaries | Measure representative programs, separate runtime from compiler-only code, partition runtime dependencies, and link only required components. Apply dead stripping and debug-data separation while retaining standalone execution and useful source diagnostics. |
| 6 | Typed agent APIs and serialization | Add typed class/enum codecs, validation with field-path errors, schema generation, and field/parameter metadata. Implement ADR-0053 decorators on the settled callable model for tool registration, routing, tracing, and ownership-correct retries. |
| 7 | Rust-class native performance | Establish equivalent Rust/Aura workloads and ratified targets. Improve representations, allocation, retain/release elimination, inlining, bounds checks, loop optimization, and SIMD. Evaluate an optimizing release backend if measurements justify it. |
| 8 | Multicore runtime scalability | Improve load balancing/work stealing, preemption latency, cancellation responsiveness, wake paths, and task memory. Define which suspended frames may migrate and preserve structured cleanup across worker boundaries. |
| 9 | Iterators and streaming | Implement ADR-0054 after resolving item/end representation, associated types, recoverable stream errors, and frame affinity. Support library-defined iteration, lazy generators, early-exit cleanup, and optimization that preserves effects and ownership. |
| 10 | Array v2 and ML interoperability | Build strided views, reshape/transpose, broadcasting, matrix operations, and vectorized kernels on the settled memory model. Add shared-memory and zero-copy foreign-buffer transport, then explicit tensor/device interop where needed. |
| 11 | Package ecosystem and tooling | Extend current package support with registry/publishing, reproducible dependency resolution, supply-chain metadata, generated API docs, formatter maturity, and responsive compiler-backed editor services. |
| 12 | Freestanding systems foundation | Ratify and implement hosted/freestanding profiles, core/runtime separation, stable layouts and ABI, raw memory and `unsafe`, volatile access, atomics/fences, custom allocators, cross-compilation, linker/startup/panic controls, and hardware interfaces. |
| 13 | Operating-system and driver proof | Select a QEMU target and demonstrate boot, memory management, interrupts, device I/O, and a minimal driver. Maintain reproducible build and emulator tests as the basis for wider platform support. |

## Design Decisions to Resolve First

- **Unions and absence:** ADR-0052 proposes replacing `Option[T]` with
  `T | None`. Ratify its API-wide impact and explicit annotation rule;
  mixed-literal inference would be a separate amendment.
- **Iteration termination:** `next() -> T | None` loses the distinction between
  a yielded `None` and exhaustion when `T` includes `None`. Use a distinct
  item/end representation or ratify an explicit element-type restriction.
- **Callable identity and retry:** ADR-0053 assumes metadata deferred by
  ADR-0051. Settle it first, and require an explicit clone, reconstruction, or
  shared-input policy when retries call a function that consumes its request.
- **Suspension and migration:** reconcile ADR-0054's worker-affine generators
  with scheduler evolution; define fallible advancement and cleanup failures.
- **Safe convenience:** define initializer escape rules, collection-loan
  invalidation, context-manager exit behavior, and property access costs.
  Shared receivers alone do not establish freedom from all I/O or side effects.

## Sequencing and Completion

Priority expresses engineering focus; independent work can proceed in parallel.
Docstrings, basic syntax polish, and binary-size baselines can start early.
Display and properties need not wait for general decorator execution; their
parser and metadata dependencies can be delivered independently.

The main dependencies are **callables → decorators**, **collection loans →
zero-copy APIs**, **union/iterator decisions + scheduler contract → generators**,
and **runtime separation + memory/ABI controls → freestanding → bare-metal proof**.
Version assignments follow ratification and delivery; the proposed 0.4 ADR
targets do not commit every batch to one release.

- Preserve the clean-slate policy: removed syntax has no compatibility layer.
- Define observable acceptance criteria before each batch, add failing behavior
  tests first, and keep compiler, backend parity, reference, examples, tutorials,
  and editor behavior aligned in the same change family.
- Use focused checks during development and one complete verification at the
  implementation checkpoint. One green hosted CI run is sufficient; editorial
  work follows the repository's scoped documentation checks.
- Measure speed, allocation, memory, executable size, and compilation cost
  separately, with equivalent workloads and published provenance. Ratify
  numeric targets after baselining; record results against those targets.
- Keep build artifacts within the repository's cleanup policy. Track batch
  completion in the existing work board when implementation begins.

The next design checkpoint is **Batch 1: callable and type foundations**,
with early syntax/docstring and binary-footprint work available in parallel.
