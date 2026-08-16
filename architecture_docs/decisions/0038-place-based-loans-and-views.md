# ADR-0038: Place-based loans and views

- Status: Implemented
- Date: 2026-07-30
- Accepted at: Batch 6 opening checkpoint
- Roadmap decision: Batch 5, Phase 6.5
- Implementation: Complete
- Version target: 0.3
- Related: ADR-0009, ADR-0013, ADR-0016, ADR-0022, ADR-0033, ADR-0037

## Status boundary

This is an implemented Aura 0.3 language feature. It changes the grammar,
ownership model, closure capture contract, semantic interface, MIR, direct
backend, diagnostics, editor tooling, examples, tutorials, and Manual as one
versioned implementation family.

Returned-view declarations lock their one declared origin conservatively while
the runtime handoff preserves the exact field or tuple projection selected by
control flow. MIR and direct execution therefore write through the same caller
place without a clone or delayed-writeback approximation.

This ADR supersedes ADR-0009's deferred live-alias reservation at the design
level and is the designed-from-scratch successor to borrowed returns. It does
not restore `borrow`, return labels, or `-> mut T`. ADR-0009's implemented
containment rule remains binding until a later implementation of this ADR
lands. ADR-0016's retained-expression sequencing remains binding and becomes
an input to the broader loan analysis rather than being silently replaced.

This decision amends ADR-0013 and ADR-0037 for explicitly requested in-loan
closure captures. A lambda without a capture list retains ADR-0037 by-value
capture behavior.

ADR-0040's Aurora 0.2 Vec and String slices are deliberately not an
implementation of this design. `value[start:end]` produces a fresh owned
collection or String and is never a place or view. It has no PlaceId, source
generation, lifetime, reborrow, write-through behavior, or returned-view
provenance. A future indexed-view amendment must use an explicit view form and
must not reinterpret the owned slice syntax as an alias.

## Context

Aurora 0.1 has call-scoped shared and mutable access, scoped shared aliases
from parameters and place traversal, and retained-expression conflict
checking. It does not have a first-class value that can safely expose an
owner's internal non-Copy state beyond one call.

That omission is deliberate. The current MIR runtime often transports cloned
runtime values, while mutable calls and mutable pattern/iteration operations
use explicit writeback. The direct ABI likewise returns mutable-parameter
writebacks as extra results in cases where it does not operate on stable
storage. A returned alias built on either mechanism could point at a clone,
lose a write on an abnormal exit, or behave differently across backends.

The successor design therefore starts with place identity and a single
aliasing model. It does not reinterpret existing syntax or treat a backend
optimization as the language contract.

## Goals

- expose shared and exclusive mutable views of caller-owned places without a
  hidden clone
- permit one explicitly declared returned view whose lifetime is tied to one
  receiver or parameter origin
- infer loan lifetimes without user-written lifetime parameters
- define overlap, reborrow, escape, task, closure, cleanup, and FFI behavior
  before accepting source
- give MIR and direct execution one write-through, identity-preserving model
- keep the ordinary bare / `mut` / `own` capability hierarchy established by
  ADR-0022

## Non-goals

- restoring the retired `borrow` keyword or `borrow[label]` syntax
- interpreting an ordinary `-> T` result as anything except an owned result
- user-written lifetime parameters
- arbitrary view-bearing aggregate or structural callable types
- views into Vec or Map elements, Set elements, Queue receives, Range values,
  or arbitrary temporaries in the first implementation
- loans across task, Queue, supervisor, detached, or FFI-retention boundaries
- FFI callbacks or foreign functions returning Aurora views
- using clone plus delayed writeback as an implementation of mutable views

## Vocabulary

A **place** is stable, addressable storage selected exactly once. A
**projection** identifies a statically known component of a place. A
**PlaceId** is the compiler/runtime identity of a root plus its normalized
projection path and storage generation.

A **loan** is the compiler-tracked permission and lifetime relation over a
place. A **view** is the source-level non-owning binding produced by a loan.
A shared view permits reads. A mutable view grants exclusive write-through
access.

These views are distinct from FFI v0's call-duration pointer/length views.
Foreign views never become Aurora view values.

Public documentation continues to use *shared access*, *mutable access*, and
*ownership transfer*. Compiler internals may use borrow/loan terminology where
it accurately names the implementation.

## Accepted source design

### Local views

The contextual `view` introducer creates a non-rebindable alias binding:

```aurora
view display_name = user.name
view mut count = stats.count
```

The selected source expression is evaluated once. The binding's type is
inferred from the place; the first implementation does not add an explicit
local view-type annotation.

A shared binding reads the selected place. A mutable binding writes through to
the selected place:

```aurora
print(display_name)
count = count + 1
```

Assigning through a mutable view replaces the pointee; it does not retarget
the view. A view binding itself cannot be rebound.

Creating another view is an explicit reborrow:

```aurora
view another_name = display_name
view mut nested_count = count
```

A shared reborrow may come from a shared or mutable parent. A mutable reborrow
requires a mutable parent and makes that parent inaccessible until the child
loan ends. There is no `.clone()` operation on the view descriptor. Calling a
pointee's `.clone()` through a view, when the pointee is clone-safe, produces
an ordinary owned value.

### Returned views

A view-returning declaration names one origin:

```aurora
def name(user: User) -> view String from user:
    return view user.name

def name_mut(user: mut User) -> view mut String from user:
    return view mut user.name

class Account:
    profile: Profile

    def profile(self) -> view Profile from self:
        return view self.profile
```

`from source` is part of the callable contract. It names exactly one receiver
or ordinary parameter by source name, and exported interfaces encode it by
parameter ordinal rather than by a source-text label. It is not a revival of
ADR-0009's parameter/return lifetime labels.

The rules are:

- a shared result may derive from a bare or `mut` origin
- a mutable result requires a `mut` origin
- an `own` origin cannot produce a view because the owner is consumed
- a defaulted parameter cannot be the origin of a returned view
- a local, default temporary, expression temporary, or newly allocated callee
  value cannot be a returned origin
- every view-returning declaration names exactly one origin, including when
  only one candidate exists
- different projections of the same origin may be selected by control flow;
  different origin roots are rejected
- the returned expression must be the origin or a supported projection or
  reborrow of it
- an enum-payload binding is arm-scoped and cannot be returned in the first
  implementation, even when its provenance traces to the declared origin
- a call returning a view requires an addressable caller argument for the
  origin; a temporary is insufficient
- a method uses `from self` explicitly
- a trait declaration and implementation must identify the same origin slot
  even if their parameter names differ

At the call site, a returned view may initialize a matching view binding:

```aurora
view name = name(user)
view mut editable_name = name_mut(user)
```

A shared returned view may also be read directly inside one containing
expression; its loan ends after that expression's final use. An ordinary
owned binding cannot receive a view. Mutable results must initialize a
mutable view, be immediately reborrowed into a `mut` call, or be rejected.

Ordinary `-> T` remains an owned result in every context. Existing structural
`def(...) -> R` types cannot represent a returned-view origin and therefore
do not accept view-returning functions in the first implementation.

## Place identity

The first implementation admits these roots:

- owned local storage
- parameters and receivers
- task-local closure-owned storage
- an existing shared or mutable view

It admits these projections:

- statically resolved class fields, including indirect-class fields
- tuple positions
- an enum payload binding while its enclosing `match` arm remains active
- further field or tuple projections from another admitted projection

Root identity is independent of the source binding name. Rebinding or
reinitializing a root advances its storage generation; an old PlaceId never
retargets to the new value. Ancestor and descendant paths overlap. Distinct
fixed fields and tuple positions are disjoint when the checker can prove their
paths differ.

The first implementation rejects:

- Vec indexes and Map keys, including constant indexes
- Set elements
- Queue-received and Range-produced values without another owned local root
- arbitrary computed projections
- a temporary that would need hidden stable storage
- an enum payload outside the selecting arm

Indexed identity is deferred because it additionally needs evaluate-once
index/key ownership, collection generation and reallocation rules, bounds
behavior, alias overlap, structural-mutation invalidation, and exact backend
parity. A later ADR amendment may add it; no backend may approximate it with a
cloned element and writeback.

A shared view of a Copy place remains a logical loan. A backend may optimize a
read to copied bits only when identity and sequencing remain unobservable and
the static source lock is unchanged.

A nonescaping local view of a bare parameter uses stable callee input storage
for the duration of the call. The ordinary ADR-0022 temporary extension is
therefore sufficient, and an imported caller may pass a temporary when no view
escapes. A local view of a `mut` parameter reborrows the caller place, which is
already required to be addressable. Checked callable metadata marks any
parameter that needs stable source access so lowering cannot substitute a
hidden non-Copy clone. Only a returned view extends the caller's origin lock
beyond the call and therefore requires the declared `from` contract and
returned-loan call-site rules.

An enum-payload PlaceId is deliberately local to its selected arm. It may be
reborrowed inside that arm, but it cannot be returned: returning it would need
the variant lock and payload identity to survive arm teardown or mutable-match
reconstruction, which is outside the first implementation.

## Lifetime and provenance

Aurora infers regions; source code does not name lifetimes.

A loan begins when its `view` initializer executes. It ends after its last
possible use, conservatively extended across control-flow joins. Lexical scope
is the upper bound, not automatically the exact endpoint.

The checker enforces:

- the owner outlives every view
- a returned view is bounded by the named origin's remaining lifetime and the
  result's last use
- a reborrow is contained by its parent loan
- a view created in a loop iteration ends on every iteration edge
- a match-payload view ends before arm teardown or mutable-match
  reconstruction
- a view into a managed resource ends before `close(mut self)` cleanup
- moves, drops, rebinding, reinitialization, and overlapping structural
  mutation are forbidden while a conflicting loan remains live
- branch joins preserve origin and kind conservatively; incompatible origins
  do not become an implicit union
- a trap or cancellation releases runtime loan registrations exactly once

Every normal fallthrough, `return`, `break`, `continue`, propagated error, and
maintained cleanup path ends the loans whose regions it exits.

Mutable views are write-through. Mutation happens immediately at the selected
place. Ending a mutable loan releases exclusivity; it does not commit a copied
value. A later trap, early return, or error propagation therefore cannot
silently discard an earlier successful write.

A task may keep a loan over a scheduler suspension when both owner and loan
remain inside that same pinned task. Cancellation still performs exact-once
loan teardown. A loan never crosses into another task or worker.

## Aliasing and exclusivity

- any number of overlapping shared loans may coexist
- a mutable loan excludes every overlapping shared or mutable access
- a shared loan permits other shared access but blocks overlapping mutation,
  rebinding, cleanup, or ownership transfer
- a mutable loan blocks every overlapping source access except through that
  loan or one valid contained reborrow
- disjoint proven fields may be borrowed independently
- moving a root or overlapping field is rejected while a loan remains live
- a shared view may create more explicit shared reborrows
- a mutable view is affine and cannot be duplicated
- neither view kind permits moving a non-Copy pointee out; mutable access
  permits mutation or replacement, not ownership theft
- consuming a Copy pointee still obeys logical loan sequencing

Equality, ordering, string rendering, and pattern tests operate on the
pointee's value when that operation exists. Aurora exposes no view address,
identity comparison, hashing, or pointer arithmetic.

Passing a shared view to a bare parameter creates a call-duration shared
reborrow. Passing a mutable view to a bare parameter creates a shared
reborrow. Passing it to a `mut` parameter creates a contained mutable
reborrow and temporarily suspends direct use of the parent view. A view can
never satisfy an `own` argument for a non-Copy pointee.

Bare and mutable matching, iteration, and member operations through a view use
the same capability rules as the underlying place and cannot extend the
view's region.

## Escape and storage matrix

| Destination | First-implementation design |
| --- | --- |
| inferred `view` local | allowed within the owner's region |
| direct synchronous argument | allowed by contained reborrow |
| direct read expression | allowed; ends after last expression use |
| declared `-> view ... from source` | allowed from the exact admitted origin |
| field, enum payload, tuple, or collection storage | rejected |
| ordinary owned local or return | rejected |
| structural `def(...) -> R` storage | rejected when the callable returns or captures a view |
| module/global state | rejected |
| task capture, task result, Queue, supervisor, detached work | rejected with `AU3008` |
| FFI result, retained pointer, or callback | rejected |
| explicit in-loan closure environment | allowed only under the next section |

View-bearing `Option`, `Result`, fields, collections, generic aggregates,
multi-origin returned views, and lifetime-parameterized structural function
types are deferred. Code needing those shapes returns an owned clone, index,
opaque handle, owner operation, or a purpose-built owned enum.

A shared view is reborrowable but is not an ordinary user-Copy value. A mutable
view is affine and non-cloneable. Both are compiler-derived non-Transfer,
regardless of the pointee's type.

## In-loan closure captures

An ordinary lambda without a capture list keeps ADR-0037 behavior exactly:
Copy values copy, owned non-Copy values move, and bare or mutable capabilities
remain rejected.

Loan capture uses a new explicit, exhaustive capture list:

```aurora
callback = lambda [settings, own cache] item: transform(settings, cache, item)
mut update = lambda [mut stats] value: stats.record(value)
```

The entries follow ADR-0022's hierarchy:

| Entry | Capture |
| --- | --- |
| `value` | shared loan of the named place |
| `mut value` | exclusive mutable loan of the named place |
| `own value` | ADR-0037 by-value copy or move |

The first implementation accepts local identifiers in the list. A projected
place must first receive an explicit view binding. Every resolved outer local
used by the body must appear exactly once, and every listed local must be used
by the body; module items, builtins, and lambda parameters are not entries.
An unused entry is rejected because acquiring its loan or moving its value
would otherwise have observable ownership effects. Entries are acquired
strictly left to right using ADR-0016 sequencing, and a failed construction
unwinds already acquired entries in reverse order. Parent/child overlap with
incompatible modes is rejected at the later entry.

Capture mode also depends on the named binding's existing capability:

| Named binding | bare entry | `mut` entry | `own` entry |
| --- | --- | --- | --- |
| owned place | shared loan | mutable loan if the place is mutable | ADR-0037 copy/move capture |
| shared view | contained shared reborrow | rejected; no capability escalation | rejected; a view does not own its pointee |
| mutable view | contained shared reborrow, suspending the parent | contained mutable reborrow, suspending the parent | rejected; use an owned clone local for value capture |

Thus `own` never moves a view descriptor or snapshots its pointee implicitly.
The programmer explicitly clones the pointee into an owned local and captures
that local when an owned snapshot is required.

A bare capture of a Copy place is a live shared loan, not a value snapshot.
Use `own value` for a snapshot or ownership capture. Loan capture begins when
the closure is created and ends after the closure's final possible use or
scope teardown; it never lengthens the source owner's lifetime.

Closure call capability has three ordered forms:

1. **shared-repeatable**: reads shared loans and owned captures without
   consuming them; callable repeatedly through shared closure access
2. **mutable-repeatable**: mutably accesses a loan capture; callable
   sequentially only through a mutable closure place
3. **consuming**: consumes a non-Copy owned capture; the call consumes the
   closure under `AU3001`, even if the environment also contains loans

A mutable-repeatable closure must be stored in a `mut` local or passed through
a future callback contract that explicitly requires mutable callable access.
It is non-reentrant. Its source remains exclusively loaned for the closure's
complete live region.

Loan closures are non-Copy and non-Transfer. The first implementation permits
them only for immediate invocation, matching inferred locals, nested contained
reborrows, and compiler-known synchronous non-retaining callback sites whose
required call capability matches. Current `Vec.map`, `filter`, `sort_by`, and
`control.retry` take shared-repeatable callbacks, so they may admit shared-loan
closures but not mutable-repeatable ones.

The first implementation rejects loan closures in fields, aggregates,
collections, ordinary returns, conditional/match closure unions, arbitrary
written `def(...) -> R` parameters, tasks, Queues, supervisors, detached
work, and FFI callbacks. User-defined non-retaining callable parameters,
returned loan closures, scoped-task borrowing, and lifetime-bearing callable
types require later design.

A nested shared capture may reborrow an outer shared or mutable capture. A
nested mutable capture requires an outer mutable capture and suspends the
overlapping outer access until the inner closure ends. Diagnostics preserve
the provenance chain from original place through every closure reborrow.

## Tasks, suspension, and Transfer

A view and any closure containing one are never Transfer. This applies even to
views of Copy or otherwise Transfer values.

Task start, Queue insertion, task results, supervisor storage, and detached
work reject any direct or nested view. A task may create and use loans to its
own task-local storage, including across scheduler suspension, but it cannot
publish them through a task/Queue handle or another worker.

Lexically scoped child-task borrowing is not part of this design. Structured
scope alone does not solve concurrent shared/mutable access, cancellation, or
worker migration; it requires a separate concurrency ADR.

## FFI interaction

FFI v0 never accepts or returns an Aurora loan descriptor.

Within one synchronous foreign call, an Aurora view may supply the ordinary
declared FFI value:

- shared `String` or `Vec[uint8]` access produces the existing const
  pointer/length call-duration view
- mutable `Vec[uint8]` access uses the existing fixed-length scratch
  copy-in/out and writes through the active mutable loan after native return
- scalar access supplies the declared fixed-width bits
- shared opaque-handle access supplies the handle without transferring it

FFI result validation follows existing ordering. Mutable byte writeback
occurs after native return and before result validation. It updates the
actively loaned source and does not end that loan unless its inferred region
ends there. This scratch buffer is a narrowly defined synchronous ABI
conversion, not the Aurora loan representation: no Aurora code can observe or
access the source while C is running, the mutable loan remains exclusive for
the whole call, and every recoverable native-return path performs writeback
before surfacing its result. Foreign process termination retains the existing
FFI abort boundary rather than promising recovery.

An extern declaration cannot accept a raw loan descriptor, return an Aurora
view, manufacture one from a foreign pointer, or retain a loan closure.
Foreign pointer retention remains outside Aurora's safety contract.

## Semantic and MIR model

Semantic analysis assigns:

```text
PlaceId = root slot + normalized projections + generation
LoanId
LoanKind = Shared | Mutable
RegionId
ReturnOrigin =
    receiver or parameter ordinal
    + static footprint (one exact projection or the whole origin root)
```

The checked program records creation, source, projection, kind, origin,
creation span, and last-use span. Exported callable metadata includes the
return kind, origin slot, and a conservative static footprint. When every
return selects the same fixed field/tuple projection, that projection is the
footprint. When control flow may select different projections, callers lock
the whole origin root. The runtime token still records the exact selected
projection. Trait conformance, generic specialization, analysis/LSP output,
MIR serialization, and native cache keys use the same metadata.

MIR gains explicit operations equivalent to:

```text
BeginLoan { loan, kind, source, region }
Reborrow { loan, kind, parent, projection, region }
ReadLoan { target, loan }
WriteLoan { loan, value }
EndLoan { loan }
ReturnLoan { loan, origin_slot, static_footprint }
```

`ReturnLoan` performs an atomic callee-to-caller handoff. It reparents the
selected reborrow token to the caller frame/region and suppresses the callee's
ordinary `EndLoan` action for that token while still ending every other
callee loan. A shared result from a bare origin continues the shared origin
loan. A shared result from a `mut` origin atomically downgrades the exclusive
call loan to the caller-visible shared loan. A mutable result continues the
exclusive origin loan without an unlock/relock gap. An error or trap before
`ReturnLoan` transfers nothing and ends the complete call-duration loan set.
Caller last use ultimately ends the transferred token and releases the origin
lock.

The MIR validator proves:

- creation dominates every use; every path from creation to a region exit
  executes exactly one appropriate end or legal `ReturnLoan`, and no path can
  reach a use after its selected end
- no use occurs after end
- shared/mutable overlap rules hold
- the owner is not moved, rebound, cleaned up, or destroyed while locked
- each reborrow is contained by its parent
- every loop iteration ends its loans
- every returned loan matches the declaration's origin slot
- no callee local or temporary becomes a returned source
- no loan crosses a task boundary

Public or serialized arbitrary MIR cannot manufacture an unchecked address or
loan. Invalid loan metadata produces a typed runtime diagnostic before access.

## Backend lowering

MIR execution needs stable slots instead of aliases into cloned `Value`
objects. One task-local loan arena resolves descriptors containing task,
frame, slot, generation, projection, kind, and region. It tracks shared counts,
exclusive ownership, active frames, and exact-once release. Rust references
are never stored across scheduler yields.

The direct backend uses an opaque runtime-managed descriptor as its canonical
ABI. It may optimize validated cases to a direct address or SSA read only when
the optimization preserves the same PlaceId, lifetime, lock, and cleanup
behavior. Loanable scalars spill to stable slots; plain-class storage has
stable field offsets; opaque values use runtime-owned cells. No returned
descriptor points into a callee-local stack slot or movable host value.

Both backends must replace loan-related special-case exit rewrites with one
ordered exit-action stack that can contain:

```text
EndLoan
DropLoanClosureEnvironment
MutableCallWriteback
MutableMatchWriteback
MutableIterationWriteback
ResourceCleanup
```

Every fallthrough, return, break, continue, propagated error, trap teardown,
and cancellation drains the required suffix in reverse acquisition order.
Thus a loan into a resource ends before resource cleanup, a match-payload loan
ends before reconstruction, and an inner reborrow ends before its parent.

A closure environment owns the release action for every captured loan.
`DropLoanClosureEnvironment` ends those tokens exactly once; the same tokens
must not also receive independent `EndLoan` actions.

Full source exit actions run only while the generated frame and its Aurora
places remain intact. If a direct task is forcibly abandoned after generated
frame reset, host-side containment may release opaque loan descriptors,
runtime cells, and ledger registrations exactly once, but it must not execute
arbitrary Aurora cleanup, reconstruction, or writeback against a destroyed
frame. Static lowering must arrange all ordinary write-through and source
cleanup before that forced-reset boundary.

The existing clone/writeback mutable-call ABI may remain only for a call whose
arguments are not rooted in a live view and whose checked callee metadata
proves that no local or returned loan requires stable source access. Its
`MutableCallWriteback` action must preserve the already-defined ordinary-call
exit semantics. Passing a mutable view, creating a view of a mutable
parameter, returning a view, or otherwise exposing a loan-capable path
requires stable source storage and immediate write-through; clone/writeback is
forbidden on that path.

## Diagnostics and tooling

- `AU1101` reports malformed `view`, `view mut`, capture-list, return-origin,
  or `from` syntax
- `AU3002` reports overlapping loans or blocked source access and points to
  the loan origin plus the last use keeping it live
- `AU3003` reports mutation through a shared view or calling a mutable closure
  without a mutable closure place
- `AU3004` reports a non-place, immutable mutable-view target, unsupported
  projection, or invalid view capability
- `AU3001` reports use after moving a mutable view/consuming closure or an
  attempted move of a locked owner
- `AU3008` reports task, Queue, or other Transfer-boundary escape
- `AU3010` is reserved for view escape and returned-origin/provenance failures

Guidance recommends removing later uses so the inferred region ends earlier,
shortening the lexical scope, creating an owned clone, returning an
index/handle/owner operation, taking ownership, or keeping a loan closure
synchronous and local.

Analysis, hover, definition, completion, diagnostics, formatter, TextMate
grammar, snippets, semantic-interface schema, MIR serialization schema, and
native cache identity must all change in the implementation family. Hover
renders the view kind and declared `from` origin; definitions trace a view to
its source place when unambiguous. The source feature cannot ship without both
backend and editor parity.

`view` is contextual only in the accepted binding, return-type, and
return-expression positions. It has no keyword role inside a lambda capture
list: `[view]` captures an ordinary identifier named `view`.

At statement start, only `view name =` and `view mut name =` select the view
binding production; `view = value` remains assignment to an identifier. After
`->`, the parser selects a view-result production only when the sequence
completes `view Type from name` or `view mut Type from name`; otherwise `view`
may remain an ordinary type identifier. After `return`, `view` introduces a
view result only inside a function already declared with a view result;
otherwise `return view` returns the identifier named `view`. These
context-sensitive choices take precedence only in their complete productions.
This accepted design does not reserve `view` as a general identifier. The retired
`borrow` compatibility diagnostic remains unchanged.

## Implementation and compatibility strategy

Implementation targets Aurora 0.3, not 0.2. The source feature
requires a stable-slot model, typed places, region dataflow, a unified
exit-action stack, new MIR/runtime operations, callable metadata, and a cache
schema change. Compressing that storage-model work into 0.2 would put
correctness and backend parity at risk.

The implementation landed atomically across static checking, MIR and direct
execution, analysis/LSP, extension grammar and snippets, semantic-interface
schema 6, reference material, tutorials, examples, diagnostics, and parity
tests. Ordinary `-> T` returns remain owned; `-> view ... from ...` is the
only returned-view exception.

## Consequences

The accepted design makes aliasing visible at local creation, returned-view
declaration, and closure capture. It supports owner methods that expose
internal state without cloning while retaining Aurora's no-hidden-cost and
task-isolation principles.

The cost is substantial compiler/runtime infrastructure and a deliberately
narrow initial place/storage surface. Indexed and aggregate views remain
future work rather than receiving semantics that cannot be made identical
across backends.

## Ratification result

Batch 6 answered all ten questions yes as recommended:

1. **Answer: Yes.** Accept `view name = place`, `view mut name = place`,
   `-> view [mut] T from source`, and `return view [mut] place`.
2. **Answer: Yes.** Keep `view` contextual only in the listed positions.
3. **Answer: Yes.** Use one explicit receiver/parameter origin per returned
   view in the first implementation.
4. **Answer: Yes.** Limit the initial place set to roots, fixed fields, tuple
   positions, scoped enum payloads, and reborrows; defer all indexed/keyed
   places.
5. **Answer: Yes.** Use inferred last-use regions, permit same-task
   suspension, and categorically reject cross-task loans.
6. **Answer: Yes.** Make shared views explicitly reborrowable, mutable views
   affine, both non-Transfer, and defer every view-bearing owned aggregate.
7. **Answer: Yes.** Accept the exhaustive
   `lambda [value, mut value, own value] ...` capture list without changing
   ordinary lambda capture.
8. **Answer: Yes.** Accept the mutable-repeatable closure call kind while
   deferring user-defined non-retaining callback types and mutable
   standard-library callbacks.
9. **Answer: Yes.** Require one unified exit-action model before any
   user-facing implementation.
10. **Answer: Yes.** Target implementation at 0.3 rather than 0.2.

## Implementation completion tests

Tests must pin observable semantics, diagnostics, or backend parity rather than
execute lines only.

- parser/formatter coverage for every local, return, origin, and capture-list
  form plus recovery from retired and misplaced syntax
- root/member/tuple/indirect-field identity, ancestor overlap, sibling
  disjointness, root generations, and exact indexed/keyed rejection
- shared/shared acceptance and every shared/mutable/move/rebind/cleanup
  conflict, including Copy pointees
- last-use shortening, branches, joins, loops, nested reborrows, match payload
  teardown, and resource cleanup ordering
- returned shared/mutable receiver and parameter views, generic
  specialization, trait origin-slot conformance, imported interfaces, and
  rejection of local/default/temporary/owned/multi-origin results
- direct reads, mutation, replacement, reborrow, pointee clone, equality,
  rendering, matching, iteration, and ordinary call argument behavior
- every normal and abnormal exit: fallthrough, return, break, continue,
  `try` propagation, trap, cancellation, mutable-match reconstruction,
  mutable-loop writeback, and `with` cleanup
- proof that mutation is immediate and no hidden deep clone or deferred
  writeback can lose or duplicate state
- shared-repeatable, mutable-repeatable, consuming, mixed owned/loan, nested,
  and never-called closure behavior plus final-use loan release
- rejection of every closure/view storage, aggregate, task, Queue, supervisor,
  detached, returned-callable, and FFI escape
- same-task suspension over yield, timer, Queue wait, and blocking I/O;
  exact-once teardown on cancellation
- FFI shared string/bytes, mutable bytes, scalar, opaque-handle, empty-view,
  error/writeback ordering, result-view rejection, and callback rejection
- MIR round-trip and validator rejection of missing begin, double end,
  use-after-end, generation mismatch, overlapping mutable tokens, illegal
  return source, cross-task token, and forged public serialized metadata
- compiler analysis, LSP, extension, cache/schema invalidation, reference,
  tutorials, maintained examples, forced-backend parity, coverage, audits,
  Clippy, and hygiene
