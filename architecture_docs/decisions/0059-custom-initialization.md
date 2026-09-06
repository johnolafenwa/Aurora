# ADR-0059: Custom initialization and fallible factories

- Status: Accepted direction; detailed design pending
- Date: 2026-09-06
- Implementation: Not started
- Roadmap: Batch 3
- Related: ADR-0005, ADR-0015, ADR-0022, ADR-0058, and ADR-0060

## Authority and current boundary

The user approved `__init__(self, ...)` in the
[roadmap](../14-priority-roadmap.md). Aura currently constructs classes from
their fields and per-instance defaults; `__init__` has no constructor meaning
in the implemented language.

## Accepted decisions

- A class defining `__init__(self, ...)` uses that initializer's parameters for
  class calls. `self` denotes the object being initialized and is not supplied
  by the caller.
- A class without an initializer retains automatic field-based construction.
- Every required field must be initialized on every successful construction
  path. The compiler rejects reads of uninitialized fields and escape of a
  partially initialized object, including through callbacks, tasks, or storage.
- Initializer execution may validate inputs and compute fields. Failure must
  clean up initialized fields and owned temporaries exactly once; it cannot
  expose a partially initialized instance.
- Fallible construction initially uses named factories returning
  `Result[Class, Error]`. An initializer returning a fallible construction
  result is not part of the initial accepted surface.
- Field/argument types and ownership remain statically checked. Custom
  initialization does not introduce dynamic attributes or inheritance.

## Remaining detailed design

Define the initializer result/signature restrictions, generic and visibility
rules, interaction with declaration defaults, evaluation order, and delegation
or alternate-constructor policy. Definite initialization must cover branches,
loops, early exits, nested fields, and cleanup after partial construction.

`self` uses initializer-specific authority to establish fields. Specify that
authority and its transition to ordinary initialized ownership explicitly;
the spelling does not change the shared meaning of bare `self` on ordinary
methods. Decide whether helper calls can receive initialized projections
before completion and how their effects are checked. A factory must not bypass
private-field rules or construct an invalid object on its successful path.

## Completion evidence required

Fixtures must cover default constructors, custom argument binding, computed
fields, missing/duplicate initialization, read-before-initialization, attempted
partial-object escape, branch coverage, and exact-once partial cleanup.
Run-pass factories must preserve typed errors and return fully initialized
objects. Pin generic/imported constructors, ownership, evaluation order, both
backends, interface metadata, and editor constructor signatures. Update class
examples and the Manual only when implementation lands.
