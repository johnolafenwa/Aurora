# ADR-0055: Display trait and read-only properties

- Status: Accepted direction; detailed design pending
- Ratified direction: 2026-09-06, user approval of the priority roadmap
- Date: 2026-08-02
- Version target: Aura 0.4
- Implementation: Not started
- Roadmap decision: Batch S1, design-only checkpoint
- Parser coordination: ADR-0053; ordinary decorator execution is not a prerequisite
- Related: ADR-0004, ADR-0013, ADR-0015, ADR-0016, ADR-0022,
  ADR-0028, ADR-0030, ADR-0033, ADR-0038, ADR-0046, and ADR-0052

## Decision boundary

The user approved one shared Display contract and initial read-only computed
properties. Neither feature is implemented. Property syntax may land before
ordinary decorator execution, and Display can be implemented independently of
properties. Remaining details in this baseline are listed at the end. See the
[approved roadmap](../14-priority-roadmap.md).

## Context

Aura can render builtin and structural values, but user types need a safe,
typed way to control their human-facing text. Agent tools, logs, command-line
programs, diagnostics, and f-strings should all observe the same rendering
contract. A display hook must be shared and non-consuming, preserve atomic
output on failure, and compose recursively through collections.

Python-shaped APIs also benefit from computed read-only attributes. The
decorator surface provides a familiar spelling, but a property is a descriptor
with access semantics, not ordinary function rebinding.

## Goals

- provide one nominal `Display` trait for human-facing rendering
- make `print`, `str`, and default f-string fields use the same operation
- retain deterministic structural rendering when no custom implementation is
  present
- recurse through collections and aggregates without hidden ownership changes
- define output atomicity when rendering traps
- define a shared, zero-argument, read-only property descriptor

## Non-goals

- parsing display text back into a value
- a debugging/repr protocol distinct from Display
- locale-sensitive or process-global formatting
- mutation or consumption through the shared Display receiver
- a general compiler-enforced purity/effect system
- user implementations for foreign nominal types or builtin types
- blanket Display implementations
- numeric format interpretation for arbitrary custom Display values
- writable, deleting, cached, mutable, consuming, static, or class properties
- runtime descriptor objects or reflection

## Display trait

The prelude defines:

```aura
trait Display:
    def display(self) -> str
```

The receiver is bare shared access. The result is a fresh owned `str`.
Implementations cannot change the receiver to `mut self` or `own self`, add
parameters, make the method generic, or change the result. Calling `display`
does not move or mutate the displayed value.

A user implementation is permitted only in the module that declares the
target's outer nominal class or enum. Generic implementations may cover that
local nominal type when all required display obligations are explicit and
resolvable. Implementations for a type parameter, tuple, union, builtin,
foreign type, or nominal type declared by another module are rejected.

The compiler and standard library provide the builtin implementations. A local
nominal type may have at most one applicable Display implementation for any
concrete specialization; overlapping generic/specialized Display
implementations are rejected even if ordinary trait specificity could select
one. This gives every value one stable human rendering independent of imported
implementation sets.

## Unified rendering entry points

These operations use one display dispatcher:

- `value.display()` when a user or builtin Display implementation exists
- `str(value)`
- `print(value)`
- an f-string replacement field with no explicit format specifier
- recursive rendering of an element, key, value, field, tuple position, enum
  payload, or active union payload

An explicit Display implementation wins for its nominal target. Otherwise the
compiler uses the structural fallback below. Calling `str` on an existing
`str` returns an independently owned string under the ordinary text-copy
contract; no caller capability escapes.

Rendering evaluates the source expression once and holds only shared access
for the duration of rendering. It does not clone, move, or request mutable
access to a non-Copy value. A custom implementation may allocate and may call
Display recursively on fields to which it has shared access.

## Structural fallback

When no explicit Display implementation applies, the fallback is:

| Value | Display shape |
| --- | --- |
| `None`, booleans, integers, floats | canonical scalar spelling |
| `str` | its text, without added quotes |
| bytes | the canonical byte-list spelling |
| tuple | `(item, ...)`, including the one-item trailing comma |
| list | `[item, ...]` |
| set | `set{item, ...}` in maintained iteration order |
| dict | `{key: value, ...}` in maintained iteration order |
| class | `Type(field=value, ...)` in declaration order |
| enum | `Type.Variant` or `Type.Variant(payload, ...)` |
| union | the active payload's rendering; `None` renders as `None` |
| resource or opaque authority | its compiler-defined opaque type label |
| function or closure | its compiler-defined callable label, with no address |

Every nested value is rendered by the same dispatcher, so a custom Display
implementation applies inside containers and structural fields. Delimiters,
separators, type names, and field names are supplied by the enclosing
fallback, not by the nested implementation.

Structural rendering tracks the identity of indirect class objects on the
active rendering path. Revisiting one emits `<cycle Type>` at that position.
Shared substructure reached on a later non-recursive path renders normally.
Addresses, allocation IDs, hash seeds, and backend-specific ordering are never
printed.

The output is human-facing and is not a serialization format. Equal values may
have different text when their nominal types or maintained insertion orders
differ, and different union tags may render the same payload text.

## Failure and atomic output

**Proposed baseline, not ratified; resolved in Batch 4.** Concurrent `print`
output atomicity remains open; staging a complete rendered value does not
alone settle interleaving between concurrent callers.

`str(value)` and f-string construction either return one complete owned string
or propagate the rendering trap; no partial string becomes observable.

`print(value)` first renders the complete value and newline into temporary
owned text. Only after rendering succeeds does it perform one logical output
write. The runtime retries host-level partial writes as needed under the
ordinary output contract. If rendering traps, the outer `print` performs no
final output write. This does not roll back side effects performed inside
user display code. If the output write itself fails, its existing I/O failure
behavior applies after the complete text has been rendered.

A trap from a nested Display implementation preserves its ordinary Aura call
chain and adds the structural path being rendered, such as
`Report.items[2].owner`. Active rendering temporaries are destroyed exactly
once. A custom implementation that recursively calls `str(self)` follows
ordinary recursion and stack-limit behavior; structural cycle detection does
not mask user-written recursion.

## F-string format specs for Display values

With no format specifier, an f-string uses Display exactly as `str(value)`.
For a nominal value using custom Display, an explicit format specifier accepts
only the string-format subset:

- one Unicode-scalar fill character followed by `<`, `>`, or `^`
- width in Unicode scalar values
- precision in Unicode scalar values, truncating the rendered text
- optional terminal `s`

Rendering happens first, precision truncation happens second, and fill/alignment
happens last. The source value and its Display implementation are each
evaluated once. Numeric type codes, sign flags, alternate form, zero padding,
and digit grouping are rejected for a custom Display value even if its result
text looks numeric.

Builtin numeric values continue to use their numeric format contracts. An
explicit `s` requests the string subset after normal display and is valid for
every displayable value.

## Read-only properties

Inside a class, `@property` marks one eligible getter:

```aura
class ModelReply:
    prompt_tokens: int64
    completion_tokens: int64

    @property
    def total_tokens(self) -> int64:
        return self.prompt_tokens + self.completion_tokens
```

An eligible getter:

- is an instance method on a nominal class
- has a bare shared `self` receiver
- has no ordinary parameters, defaults, or type parameters
- has an explicit owned result type other than `None`
- is not combined with another decorator
- does not collide with a stored field or another member

`value.total_tokens` evaluates `value` once, obtains shared receiver access,
calls the getter once, and produces its owned result. Parentheses are not used.
The property can be read wherever an ordinary member expression can be read.
Its result then follows normal copy, move, and lifetime rules.

The descriptor is read-only. Assignment, mutable argument binding to the
property place, ownership transfer from it as a place, address/view creation,
and compound mutation are rejected. An owned non-Copy result may itself be
moved because it is a fresh return value; this does not move from the receiver.
Calling `value.total_tokens()` is rejected because the member denotes the
computed value, not the getter function.

`@property` is recognized by the compiler and does not perform an ordinary
decorator expression evaluation. No module binding named `property` is looked
up. The getter remains the implementation symbol used by diagnostics and
stack traces, while member lookup exposes only the property descriptor.

Properties are excluded from trait requirements and implementations in the
first version. They cannot satisfy Display or another method merely because
their returned value has a matching callable type.

## Capability and ownership interactions

The approved API convention uses properties for computed reads and explicit
methods for mutation, I/O, and expensive work. A shared receiver enforces
access to that receiver; it is not a proof of whole-function purity. A method
could otherwise call an I/O function or affect separately accessible state.
This ADR does not introduce an effect checker or claim to reject every such
operation. Any stronger enforcement requires a separate effect contract.

Display and property getters receive shared access. They may call other shared
methods and read fields. The checker rejects mutation, moving a non-Copy field,
passing the receiver or one of its places as `mut` or `own`, or creating a view
whose region escapes the call.

The owned string returned by Display and the owned property result are cleaned
up normally. Their Transfer and Copy properties derive from their result types;
the shared receiver grants no capability to a spawned task. A Display
implementation or property getter used from a task must independently obey the
task's existing ownership and Transfer rules.

Display availability for an aggregate is recursive through the fallback, but
it does not imply Copy, clone, equality, hash, order, or Transfer. Those
properties remain independent.

## Backend and interface contract

Checked interfaces record a nominal type's unique Display implementation and
each exported property's receiver, result type, documentation identity, and
getter symbol. Importers perform the same static dispatch without loading an
open implementation registry.

MIR and direct lowering must share the renderer's delimiters, scalar spelling,
cycle marker, path tracking, recursive dispatch, format-spec order, and
atomic-output staging. Property access lowers to one direct or statically
resolved trait-independent getter call after once-only receiver evaluation.

Neither a Display environment nor a property descriptor is part of the C FFI.
An FFI-compatible getter may still be called by an ordinary exported wrapper
that returns an FFI-compatible owned result.

## Diagnostics

Focused diagnostics must identify:

- a malformed or nonconforming Display implementation
- a Display implementation outside the target nominal type's declaring module
- overlap between Display implementations
- a render operation for a value with neither Display nor structural fallback
- mutation, move, mutable access, or capability escape through shared `self`
- an invalid custom-display format flag with the accepted string subset
- a trap in nested rendering with the complete structural path
- an invalid property target, receiver, parameter list, generic list, return
  type, decorator combination, or name collision
- assignment, mutable use, view creation, or call syntax on a property
- a property getter whose body attempts to return shared non-Copy storage as an
  owned value

Diagnostics point to both the use and the implementation/getter declaration
when they are in different files. They name the exact receiver and result
capabilities. They never describe a property as a hidden field.

## Consequences

Human-facing text has one customization point and one recursive dispatcher.
Logs, agent traces, f-strings, and command-line output cannot disagree about a
type's default rendering. Nominal coherence prevents imports from changing
that text unexpectedly.

Properties provide concise computed reads without introducing writable
descriptors or hidden mutable access. Their dependency on the decorator parser
is syntactic; their semantics remain a dedicated checked member kind.

## Implementation adoption

Display implementations and read-only properties are additive source features.
Implementation adds the single `Display` contract and the single
compiler-defined `@property` form in independently deliverable feature families
across parsing, checking, both backends, formatting, the language server,
reference material, examples, and tutorials.

Adoption depends on coherent trait resolution, the callable and closure model,
shared decorator-shaped parsing for properties, shared-receiver capability
enforcement, stable structural rendering, cycle detection, and complete-output
staging. ADR-0056
uses the property descriptor and getter declaration identity when attaching
documentation.

Checked interfaces record the unique Display implementation and property
metadata. The semantic schema, interface format, renderer identity, native
cache key, language-server symbol index, and generated-reference identity are
bumped together. Cached artifacts are rebuilt so imported rendering and
property lookup always use matching versioned contracts.

## Completion-test matrix

- trait surface: exact `display(self) -> str`, receiver/result mismatches,
  generic local types, nominal locality, builtin/foreign/blanket rejection,
  duplicate and overlapping implementation rejection
- unified dispatch: direct display, `str`, `print`, default f-strings, and
  nested aggregate rendering all select the same implementation
- fallback: every scalar, text, bytes, tuple arity, list, set, dict, class,
  enum, union, resource, opaque value, and callable shape
- recursion: nested custom implementations, recursive nominal structures,
  `<cycle Type>`, repeated non-cyclic shared substructure, and user recursion
  stack limits
- ownership: non-Copy shared rendering, no hidden clone/move, forbidden
  mutation/capability escape, owned result cleanup, and task Transfer checks
- output: complete staging, rendering trap prevents the outer final write,
  no claimed rollback of user-code effects, host partial-write
  retry, output failure, concurrent print call atomicity, and cleanup paths
  (**Proposed baseline, not ratified; resolved in Batch 4.** Concurrent atomicity.)
- formatting: fill/alignment/width/precision/`s`, Unicode scalar counting,
  truncation-before-padding, rejected numeric flags, and once-only evaluation
- property parsing: eligible class getter, malformed placement, decorator
  combinations, member collisions, and formatting/semantic tokens
- property typing: exact receiver and parameter restrictions, explicit owned
  result, Copy and non-Copy results, once-only receiver evaluation, and generic
  rejection
- property use: read, chained member access, no parentheses, assignment and
  mutable-use rejection, no view/place behavior, and exact trap propagation
- interfaces/tooling: imported Display dispatch, property metadata, completion,
  hover, definition to getter, rename, doc attachment, examples, tutorials, and
  reference integrity
- parity: byte-identical MIR/direct rendering, diagnostics, property results,
  cleanup traces, format behavior, and forced-backend output

## Ratification and remaining design

Accepted on 2026-09-06: one shared Display contract for `print`, `str`, and
f-strings; initial read-only properties; explicit methods for mutation, I/O,
and expensive actions; and no unsupported purity guarantee from a shared
receiver. Display and property delivery may be separated from full decorators.

Remaining details include Display coherence/locality, cycle rendering, exact
format subsets, concurrent output atomicity, property eligibility and result
capabilities, and any future effect enforcement. The sections above retain the
baseline for these details, with the corrected side-effect boundary.
