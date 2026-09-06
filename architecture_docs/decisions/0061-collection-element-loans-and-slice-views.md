# ADR-0061: Collection-element loans and slice views

- Status: Accepted direction; detailed design pending
- Date: 2026-09-06
- Implementation: Not started
- Roadmap: Batch 2
- Extends: ADR-0016, ADR-0038, ADR-0040, and ADR-0044
- Related: ADR-0052, ADR-0054, and ADR-0058

## Authority and current boundary

The user approved natural collection access in the
[roadmap](../14-priority-roadmap.md). ADR-0038 implements roots, fixed fields,
tuple positions, and contained loans. Indexes, keys, and slice views require
this extension. Existing ordinary list/string slices produce owned copies.

## Accepted decisions

Context may provide scoped shared access to a collection element. In the
future surface, both `print(tags[0])` and `tags[0].clone()` must compose without
first demanding an implicit owned read. The second expression explicitly
produces a cloned owner when the element type supports cloning.

An independent owned value still requires a valid Copy operation, explicit
clone, removal, or other owner operation. Contextual shared access must not
turn assignment into an implicit clone or let a view masquerade as ownership.

Extend place/lifetime reasoning to list elements, dictionary entries, and
slice views, including exclusive mutable access and write-through. Reject
structural operations that could invalidate a live view. Conflicting access
must be diagnosed statically, with the origin and invalidating operation.
The view's lifetime cannot exceed its owner or cross an unauthorized task
boundary.

Slice views and owned slices are separate operations. Their exact source
selection is still to be designed; this approval does not silently change an
existing owned slice into an alias. Zero-copy access must compose with future
Array views and callable/iterator lifetimes under one ownership model.

## Remaining detailed design

### Open conflicts

**Joint Batch 1–2, ADR-0038 / ADR-0058:** design the lifetime-bearing callable
type jointly with collection loans. Batch 1 callable storage first covers
owned/by-value captures and bound methods on owned or Copy receivers;
ADR-0038 loan-capturing closure storage waits for this shared design. Neither
ADR may treat the other as independently responsible for the lifetime model.

### Detailed contract

Specify indexed/keyed place identity, index/key evaluation counts, bounds and
missing-key behavior, negative positions, and which operations invalidate each
view. Define disjoint element/range reasoning, replacement/removal, dictionary
rehashing, slice ranges/steps, and any conservative restrictions needed when
indexes are known only at runtime. Do not assume reallocation is the only
source of invalidation: shifting or replacing an element also matters.

Define returned collection views, borrowing iterators, lifetime-bearing
callable storage, and backend/interface representation before exposing them.
The language must retain deterministic errors and cleanup under optimization.

## Completion evidence required

Pin shared indexed calls, explicit indexed cloning, owned-read rejection,
mutable write-through, simultaneous disjoint/conflicting access, bounds and
missing keys, invalidation by insertion/removal/replacement, returned-view
lifetimes, and non-cloneable elements. Check evaluation order and both backend
results/diagnostics, including early-exit cleanup. Update AU3005/AU3006 guidance
to describe the new contextual access rules when implementation lands.
