# ADR-0058: First-class callables and binding contracts

- Status: Accepted direction; detailed design pending
- Date: 2026-09-06
- Implementation: Not started
- Roadmap: Batch 1
- Extends: ADR-0013, ADR-0037, ADR-0038, and ADR-0051
- Required by: ADR-0053

## Authority and current boundary

The user approved the [roadmap](../14-priority-roadmap.md), including the
follow-up explanation of stored closures, bound methods, calling capabilities,
and keyword-only restrictions. This records that future contract. Current
written `def(...)` types describe capture-free code pointers; implemented
compiler-known closure sites and ADR-0038 loan captures remain the baseline.

## Accepted decisions

Functions, capturing closures, and bound methods can be passed as arguments,
returned, and stored in fields or collections through suitable callable types.
The callable representation preserves its environment and these call kinds:

| Kind | Required access | Reuse |
| --- | --- | --- |
| Shared | Shared access to callable and captured state | Repeated calls |
| Mutable | Exclusive mutable access during each call | Repeated calls |
| Consuming | Ownership of callable/captured value consumed by invocation | One call |

Call kind is distinct from each argument's bare, `mut`, or `own` capability.
A repeatable callback may consume a fresh argument on every invocation.
Calling does not implicitly clone captures or consumed inputs.

Owned captures remain alive with their callable after the creating function
returns. Loan captures retain their origin/lifetime constraints; storage does
not let a callback outlive a borrowed owner or hide mutable aliasing. Support
for general storage must include a sound lifetime-bearing contract, rather
than erasing loan provenance at a type boundary. Aggregates containing loan
callbacks inherit the necessary lifetime constraints.

A bound method retains its receiver relation. Shared methods retain shared
access, mutable methods require exclusive access, and an owning receiver moves
into a consuming callback. The receiver cannot be destroyed or invalidated
while a retained loan requires it. Exact acquisition/reborrow timing remains
part of the detailed design.

Cross-task use requires the full environment to satisfy structural `Transfer`
and the task API's call contract. Shared/mutable loan captures do not become
transferable merely because they are stored in a callable. Capture-free or
owned transferable environments may qualify.

Callable contracts preserve exposed argument names, default availability,
keyword-only restrictions, parameter capabilities, and result/view-origin
contracts. Assignment to a variable cannot silently make a keyword-only
parameter positionally callable. A future intentionally restricted interface
must specify its conversions explicitly; there is no implicit loss of a
declared calling restriction.

## Remaining detailed design

### Open conflicts

**Joint Batch 1–2, ADR-0038 / ADR-0061:** the lifetime-bearing callable type is
one design shared by callable storage and collection loans. Batch 1 storage
is scoped to ADR-0037 owned/by-value captures and bound methods on owned or
Copy receivers. Storage of ADR-0038 shared/mutable loan captures is deferred
to that joint design. The broad accepted storage behavior above is retained
as the eventual contract, with this explicit delivery split.

Batch 1 also structurally splits `sema.rs` into type representation,
capability checking, and place/loan analysis, since unions change `Type` and
callables change capability checking. It has one checkpoint after unions,
aliases, and owned-capture closures, before the ADR-0052 optional removal.

### Detailed contract

- Source syntax for call kinds, owned environments, and lifetime-bearing
  callable interfaces; inference and callable aliases.
- Type equality, conversions/variance, generic callable bounds, and heterogeneous
  closure storage with a common call contract.
- Default-expression identity, evaluation timing, and forwarding through
  wrappers; names and keyword-only metadata across imports.
- Receiver binding/reborrowing, return origins, and trait-method values.
- Inline versus allocated environments, allocation failure, destruction,
  dispatch/ABI layout, and optimization. No mandatory boxing or garbage
  collection is implied by general callable storage.

These details must be written before the compiler exposes the extended
surface. ADR-0051's implemented deferral is resolved in direction, not yet
in implementation. Decorators use this model rather than a private storage
exception.

## Completion evidence required

Test shared, mutable, and consuming closures through parameters, returns,
fields, collections, and imports; owned-capture survival; invalid loan escape;
bound receiver cleanup and exclusivity; repeat/single-use enforcement; task
Transfer; keyword-only/default/name preservation; and wrapper forwarding.
Both backends and serialized interfaces must preserve the same environment,
capabilities, lifetime contract, and diagnostics. Update formatter, hover,
completion, examples, and the Manual with implementation.
