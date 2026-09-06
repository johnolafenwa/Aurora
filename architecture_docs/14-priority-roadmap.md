# Aura: Next Core Batches

Status: roadmap direction and decisions below approved by the user on 2026-09-06.

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

The five amended ADRs are 0052 (unions), 0053 (decorators), 0054
(generators/iteration), 0055 (Display/properties), and 0056 (docstrings).
ADRs 0058–0063 now record callables, initialization, context managers,
collection loans, typed schemas, and everyday syntax. The batches below cover
all of them. The approved decisions below are reflected in those records and
supersede conflicting earlier proposals. Do not reopen settled choices.
Exact syntax, representations, and unresolved
contracts still need design. Approval records intended behavior, not implementation
completion. See the [ADR index](decisions/README.md).

## Priority Batches

| Order | Batch | Outcome and scope |
| --- | --- | --- |
| 1 | Callable and type foundations | Design storable capturing closures, bound method values, repeatable/mutable/consuming callable capabilities, and preserved names/defaults/keyword-only contracts. Add type aliases and ADR-0052 closed unions for explicitly typed mixed collections such as `list[int \| str]`, optional values, and safe narrowing. |
| 2 | Natural collection access | Extend ADR-0038 to list elements, dictionary entries, and slice views. Make shared reads, explicit cloning, mutation, and ownership transfer compose naturally; define bounds, structural mutation, and invalidation rules. |
| 3 | Initialization and resource management | Add `__init__(self, ...)`, definite field initialization, and validation while retaining automatic constructors for classes without an initializer. Use named `Result`-returning factories for fallible construction initially. Extend `with` to generic managers, multiple resources, typed entry/exit, and scoped access with deterministic cleanup. |
| 4 | Everyday syntax and API usability | Add consistent trailing commas, parenthesized imports, richer unpacking and class patterns, and focused text/byte conveniences. Implement ADR-0056 docstrings and ADR-0055 Display/read-only properties, including local/imported editor support. |
| 5 | Lean native binaries | Measure representative programs, separate runtime from compiler-only code, partition runtime dependencies, and link only required components. Apply dead stripping and debug-data separation while retaining standalone execution and useful source diagnostics. |
| 6 | Typed agent APIs and serialization | Add typed class/enum codecs, validation with field-path errors, schema generation, and field/parameter metadata. Implement ADR-0053 decorators on the settled callable model for tool registration, routing, tracing, and ownership-correct retries. |
| 7 | Rust-class native performance | Establish equivalent Rust/Aura workloads and ratified targets. Improve representations, allocation, retain/release elimination, inlining, bounds checks, loop optimization, and SIMD. Evaluate an optimizing release backend if measurements justify it. |
| 8 | Multicore runtime scalability | Improve load balancing/work stealing, preemption latency, cancellation responsiveness, wake paths, and task memory. Define which suspended frames may migrate and preserve structured cleanup across worker boundaries. |
| 9 | Iterators and streaming | Implement ADR-0054 with distinct item/end cases, associated types, lazy generators, explicit close, and persistent failure state. Settle recoverable stream-error and frame-affinity details; preserve effects, ownership, and early-exit cleanup. |
| 10 | Array v2 and ML interoperability | Build strided views, reshape/transpose, broadcasting, matrix operations, and vectorized kernels on the settled memory model. Add shared-memory and zero-copy foreign-buffer transport, then explicit tensor/device interop where needed. |
| 11 | Package ecosystem and tooling | Extend current package support with registry/publishing, reproducible dependency resolution, supply-chain metadata, generated API docs, formatter maturity, and responsive compiler-backed editor services. |
| 12 | Freestanding systems foundation | Ratify and implement hosted/freestanding profiles, core/runtime separation, stable layouts and ABI, raw memory and `unsafe`, volatile access, atomics/fences, custom allocators, cross-compilation, linker/startup/panic controls, and hardware interfaces. |
| 13 | Operating-system and driver proof | Select a QEMU target and demonstrate boot, memory management, interrupts, device I/O, and a minimal driver. Maintain reproducible build and emulator tests as the basis for wider platform support. |

## Approved Decisions

1. **Mixed collections:** require an explicit expected union type initially,
   such as `list[int | str]`; the allowed member set stays fixed. Homogeneous
   literals retain inference. Mixed-literal union inference is deferred.
2. **Optional values:** replace `Option[T]` throughout the language and standard
   library with `T | None`, with straightforward type narrowing. Use distinct
   tagged cases wherever present-with-`None` must differ from absence.
3. **Initialization:** use `__init__(self, ...)`; its parameters define the class
   call when present. Otherwise retain automatic field-based construction.
   Require complete field initialization and prevent partially initialized
   `self` from escaping. Fallible creation initially uses named factories
   returning `Result[Class, Error]`.
4. **Context managers:** support generic managers, multiple resources, and typed
   entry/exit. Register exit after successful entry and run it on early return,
   failure, and cancellation. Preserve a body failure as primary and attach
   cleanup failures; managers cannot silently suppress the body failure.
5. **Collection access:** provide scoped shared element access where context
   permits, including `print(tags[0])` and `tags[0].clone()`. Independent owned
   reads require a valid copy, clone, or removal. Reject structural operations
   that could invalidate a live element view.
6. **First-class callables:** store capturing closures and bound methods in
   parameters, returns, fields, and collections through suitable callable types.
   Shared callbacks may repeat; state-mutating callbacks require exclusive
   mutable access; consuming callbacks are single-use. Owned captures survive
   their creation scope; loan captures cannot outlive their owners. Bound methods
   retain the receiver's shared, mutable, or owning capability. Cross-task use
   requires structurally transferable captured state. Preserve parameter names,
   defaults, and keyword-only restrictions through callable values wherever
   exposed by their contract. Exact capability syntax and storage/allocation
   rules remain detailed design work.
7. **Decorators:** preserve the complete callable contract, including ownership
   and return type. Start with free functions and repeatable wrappers; ordinary
   method decoration follows settled receiver binding. Retrying consumed input
   requires explicit cloning, reconstruction, or an appropriate shared-input
   contract; no implicit resource duplication.
8. **Iteration:** distinguish `Item(value)` from `End`, allowing `None` as an
   ordinary item; loops hide this protocol. Generators are lazy, support
   idempotent `close()`, clean up on early exit, and remain observably failed
   after failure. Initially pin live frames while safe migration is developed.
   Protocol spelling, recoverable errors, and scheduler integration remain to
   be specified.
9. **Properties and display:** begin with read-only computed properties; use
   explicit methods for mutation, I/O, and expensive operations. One shared
   Display contract serves `print`, `str`, and f-strings. A shared receiver does
   not itself enforce freedom from all side effects; stronger enforcement
   requires a separate effect contract.
10. **Documentation and schemas:** support Markdown docstrings in editor help
    and generated docs, with readable indentation normalization in presentation.
    Extend metadata to fields and parameters. Typed serialization and schema
    generation use explicit compile-time opt-in for classes/enums, with defined
    required-field, default, unknown-field, and validation-error behavior.

Technical defaults are deterministic union normalization, compile-time rejection
of unsupported operations, source documentation surviving decoration, and one
coherent iterator associated-type model. These decisions amend the earlier
union-based exhaustion and exact-whitespace presentation proposals. Remaining
ADR questions and numeric performance targets must be settled in their batches;
this approval does not silently choose unspecified details.

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
