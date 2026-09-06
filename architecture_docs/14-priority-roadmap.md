# Aura: Next Core Batches

Status: roadmap direction and decisions below approved by the user on 2026-09-06.

The 2026-09-06 ordering deliberately inverted the earlier foundations-first
draft: loans, lean binaries, Rust-class performance, multicore, freestanding,
bare-metal, then surface features. Before adoption, the dominant risk is that
Aura is not pleasant to write for agent and ML software; systems-level work
has no consumer until that is true. This usability-first ordering leaves the
[positioning document's long-term systems goal](../docs/positioning.md) unchanged.

Make Aura easy to program with Python-like syntax, static typing, deterministic
ownership, no garbage collection, typed failures, and structured concurrency.
Prioritize practical agent and ML software while building toward Rust-class
performance and eventual operating-system and device-driver development.

## Current Foundation

ADR-0038 loans/views is implemented; its v0.3.3-preview release is pending the
coverage gate. ADR-0045 testing is accepted
and implemented; ADR-0049 is accepted with class patterns formally deferred.
These are existing foundations. Collection-element loans are scheduled in Batch 2.
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

## Pre-Batch-1 foundations

These items start before Batch 1 design and run in parallel with it, scheduled
for the v0.3.3-preview update. Only ADR reconciliation is delivered by this
documentation task; the other completion criteria belong to later implementation.
The backend direction is recorded in
[ADR-0064](decisions/0064-native-backend-strategy-and-codegen-boundary.md).

1. **Cranelift optimization level.** The direct flag setup in
   `crates/aura-compiler/src/native_codegen.rs` sets `is_pic`, `unwind_info`, and
   `enable_multi_ret_implicit_sret`, but leaves `opt_level` at Cranelift's
   default. Completion: measure `opt_level=speed` on the release-performance,
   integer-loop, and numeric-Array harnesses; run the forced MIR/direct parity
   matrix; publish results and provenance in the Performance chapter. Adopt
   the flag if parity holds; otherwise revert it and record failing fixtures
   as Batch 7 input.
2. **Backend boundary contract.** Inventory semantic decisions currently in
   `native_codegen.rs` instead of MIR. Completion: the inventory and design
   note exist in `architecture_docs/15-backend-boundary.md`, describing semantic
   lowering into MIR and mechanical native emission through a small builder
   interface. Incremental refactoring begins in Batch 1 under parity and
   coverage gates, making a second native backend a bounded addition.
3. **Rust baselines.** Add Rust counterparts for fib30, 10,000 tasks, TCP
   fan-out, the retrying worker, integer loops, and numeric-Array add/sum.
   Use tokio for the three concurrent workloads and plain Rust elsewhere,
   with the same READY/GO/DONE protocol and provenance rules. Completion:
   paired Aura/Rust medians in the Performance chapter. This item ratifies no
   numeric performance target.
4. **Release profile and executable sizes.** The workspace has no
   `[profile.release]`; user executables link the `libaura_compiler.a` staticlib.
   Completion: configure release optimization, LTO, codegen units, stripping,
   and debug-data separation; retain the unwinding panic strategy required by
   the runtime; add dead-stripping user-link flags. Measure the `aura` binary,
   native hello world, and reference agent, preserving standalone execution
   and source diagnostics. This pulls Batch 5's mechanical work forward;
   runtime/compiler separation remains in Batch 5.
5. **ADR reconciliation.** Completion: mark the body/ratification contradictions
   in ADR-0052–0056, record cross-ADR open conflicts and their resolving batches,
   and add ADR-0064. Delivered by this documentation amendment.
6. **Reference agent program.** Create one maintained package under 400 lines
   in `examples/agents/`, using the current surface for a tool registry, typed
   tool schemas, retries, a streaming loop, and structured cleanup. Add no new
   library surface. Completion: run it on both backends and include it in the
   example smoke tests. Its before/after diff is the usability evidence for
   each Batch 1–4 and 6 feature.

## Priority Batches

| Order | Batch | Outcome and scope |
| --- | --- | --- |
| 1 | Callable and type foundations | Add unions, aliases, owned-capture closure storage under ADR-0037, bound methods on owned or Copy receivers, and preserved names/defaults/keyword-only contracts. Defer stored ADR-0038 loan captures to the joint Batch 1–2 lifetime design. Structurally split `sema.rs` into type representation, capability checking, and place/loan analysis. Use the Option-removal criteria and intermediate checkpoint below. |
| 2 | Natural collection access | Extend ADR-0038 to list elements, dictionary entries, and slice views. Make shared reads, explicit cloning, mutation, and ownership transfer compose naturally; define bounds, structural mutation, and invalidation rules. |
| 3 | Initialization and resource management | Add `__init__(self, ...)`, definite initialization, named fallible factories, and generic/multiple typed context managers. Use one partial-construction cleanup mechanism extending ADR-0038's ordered exit-action stack for initializer failure, multi-resource entry failure, and failed decodes. Resolve cancellation/reset and header-temporary entry-view obligations below. |
| 4 | Everyday syntax and API usability | Add consistent trailing commas, parenthesized imports, richer unpacking/class patterns, and text/byte conveniences. Implement ADR-0056 docstrings, generated API docs, ADR-0055 Display/properties, and a syntax-reflowing `aura fmt` beyond today's whitespace normalization, with editor support. Formatter and API-doc work move here from Batch 11. |
| 5 | Lean native binaries | Separate runtime from compiler-only code, partition dependencies, and link only required components. Release-profile tuning, dead stripping, and initial size baselines are pre-Batch-1 work. |
| 6 | Typed agent APIs and serialization | Add typed codecs, validation, schemas, and metadata. Decode classes with automatic field construction or an explicit decode factory; computed `__init__` fields have no general inverse. Reuse Batch 3's partial-cleanup mechanism. Implement ADR-0053 decorators for registration, routing, tracing, and ownership-correct retries. |
| 7 | Rust-class native performance | Use the Rust baselines and thin backend boundary to decide among LLVM via inkwell, C emission through the already-required host C compiler, and continued Cranelift improvement. C emission is a serious candidate: no LLVM build dependency and cheaper freestanding targets. Improve representations, allocation, retain/release, inlining, bounds checks, loops, and SIMD. No backend switch occurs before this batch. |
| 8 | Multicore runtime scalability | Improve load balancing/work stealing, preemption latency, cancellation responsiveness, wake paths, and task memory. Define which suspended frames may migrate and preserve structured cleanup across worker boundaries. |
| 9 | Iterators and streaming | Implement ADR-0054 with a nominal enum for item/end, associated types, lazy generators, explicit close, and persistent failure. Aura has two sum mechanisms: nominal enums and anonymous unions. Item/end names are to be designed in Batch 9. Settle recoverable stream errors and frame affinity. |
| 10 | Array v2 and ML interoperability | Build strided views, reshape/transpose, broadcasting, matrix operations, and vectorized kernels on the settled memory model. Add shared-memory and zero-copy foreign-buffer transport, then explicit tensor/device interop where needed. |
| 11 | Package ecosystem and tooling | Extend package support with registry/publishing, reproducible dependency resolution, supply-chain metadata, and responsive compiler-backed editor services. |
| 12 | Freestanding systems foundation — direction, not scheduled | Develop hosted/freestanding profiles, core/runtime separation, layout/ABI, raw memory, `unsafe`, volatile access, atomics/fences, allocation, cross-compilation, startup/panic controls, and hardware interfaces. |
| 13 | Operating-system and driver proof — direction, not scheduled | Select a QEMU target and demonstrate boot, memory management, interrupts, device I/O, and a minimal driver with reproducible emulator tests. |

Aura currently has no `unsafe`, raw pointers, atomics, volatile access, layout
control, or cross-compilation; its runtime depends on rustls, mio, libffi, and
corosensei. Batches 12–13 retain the long-term direction without a schedule.

**Batch 1 checkpoint and Option-removal criteria:** hold one reviewable
checkpoint after unions, aliases, and owned-capture closures, before removing
`Option[T]`. Removal requires conditional narrowing on stable places specified
and implemented in the same change family (the `if x is not None:` shape;
exact syntax to be designed in Batch 1), well-formed type-parameter union
members such as `V | None` for generic library APIs, and a stated FFI-boundary
replacement for nullable results. These criteria introduce no compatibility
path or migration diagnostics.

**Batch 3 design obligations:** reconcile ADR-0060's cleanup-under-cancellation
promise with ADR-0038's forced-frame-reset boundary. Define how a manager
constructed in a `with` header exposes a scoped entry view when temporaries
cannot currently be view origins. The shared partial-construction cleanup
mechanism is an extension of the existing exit-action stack, not three
independent initializer, manager, and decoder mechanisms.

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

The main dependencies are **callables → decorators**, **callables ↔ collection
loans** (one lifetime-bearing callable design shared by Batches 1 and 2),
**collection loans → zero-copy APIs**, **union/iterator decisions + scheduler
contract → generators**, **backend boundary → any new native backend**, and
**runtime separation + memory/ABI controls → freestanding → bare-metal proof**.
Version assignments follow ratification and delivery; the proposed 0.4 ADR
targets do not commit every batch to one release.

- Preserve the clean-slate policy: removed syntax has no compatibility layer.
- Define observable acceptance criteria before each batch, add failing behavior
  tests first, and keep compiler, backend parity, reference, examples, tutorials,
  and editor behavior aligned in the same change family.
- Implement each Batch 1–4 and 6 feature on both maintained backends through
  the thin boundary, with the forced parity matrix as the gate. Include the
  reference agent program's before/after diff as checkpoint evidence.
- Use focused checks during development and one complete verification at the
  implementation checkpoint. One green hosted CI run is sufficient; editorial
  work follows the repository's scoped documentation checks.
- Measure speed, allocation, memory, executable size, and compilation cost
  separately, with equivalent workloads and published provenance. Ratify
  numeric targets after baselining; record results against those targets.
- Keep build artifacts within the repository's cleanup policy. Track batch
  completion in the existing work board when implementation begins.

The next steps are the **pre-Batch-1 foundations**, followed by the **Batch 1
design checkpoint**, with foundation work continuing in parallel as scheduled.
