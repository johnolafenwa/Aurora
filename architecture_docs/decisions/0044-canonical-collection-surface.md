# ADR-0044: Canonical collection surface

> Approved next extensions (2026-09-06): [ADR-0052](0052-anonymous-closed-union-types.md)
> adds explicitly typed union elements and replaces optional results;
> [ADR-0061](0061-collection-element-loans-and-slice-views.md) adds contextual
> indexed access and collection loans. These future changes do not alter the
> implemented collection contract below until their implementation families land.

- Status: Accepted
- Date: 2026-08-02
- Roadmap decision: Batch S1, Aura 0.3 Python surface
- Builds on: ADR-0004, ADR-0014, ADR-0015, ADR-0016, ADR-0022,
  ADR-0026, ADR-0028, ADR-0030, ADR-0039, and ADR-0040

## Context

Aura needs one compact collection vocabulary that feels natural to Python
programmers while preserving static typing, deterministic ownership, typed
absence, and backend-independent failures. The collection contract must make
element transfer explicit, keep shared searches non-consuming, and expose
allocation control without making allocation strategy part of program
semantics.

The surface also needs exact rules for negative positions, insertion bounds,
missing values, eager snapshots, stable sorting, and homogeneous literals.
These rules must be strong enough for the compiler, runtime, language server,
reference, examples, and tutorials to describe the same language.

## Decision

### Type spellings and literals

The four builtin collection and text types are:

```aura
list[T]
dict[K, V]
set[T]
str
```

`list[T]`, `dict[K, V]`, and `set[T]` are owned, mutable collection types.
`str` is an immutable owned UTF-8 text type whose length and slice positions
count Unicode scalar values. Generic type arguments are part of static type
identity.

The constructors `list[T]()`, `dict[K, V]()`, and `set[T]()` create empty
collections. List literals use `[a, b, c]`, dictionary literals use
`{key: value}`, and set literals use `{a, b, c}` when a `set[T]` context is
available. `{}` is a dictionary literal; an empty set uses `set[T]()`. Text
literals have type `str`.

Every collection literal is homogeneous in each type position:

- all list and set elements must infer or check as one exact `T`
- all dictionary keys must infer or check as one exact `K`
- all dictionary values must infer or check as one exact `V`

A literal does not infer a union or a common dynamic supertype. Mixed-type
elements, keys, or values are rejected statically with `AU2002`. Contextual
integer-literal typing still applies, but it does not convert an already typed
value. Programs that need a common nominal representation must construct that
representation explicitly before inserting it.

Collection literals evaluate their expressions once from left to right. A
dictionary entry evaluates its key before its value. An equal dictionary key
updates the value at that key's first insertion position. A duplicate set
element leaves one stored element.

### Shared collection principles

`len(collection) -> int64` reports the number of elements or entries, and
`collection.is_empty() -> bool` is equivalent to `len(collection) == 0`.
Iteration is eager over the collection's defined order and uses the ordinary
loop ownership rules. Membership is written with `in` and `not in`; collections
do not duplicate membership methods.

Methods that mutate a collection require a mutable receiver. Bare parameters
are shared, `mut` parameters grant mutable access, and `own` parameters
transfer a value. A mutating method returning `None` performs its mutation as
the observable result; it does not return a success flag.

Collection equality reads and consumes neither operand. Lists compare elements
in order. Dictionaries compare equal key/value mappings. Sets compare equal
membership. Equality-dependent operations are available only when their
element, key, or value types provide the required equality relation. Callables,
`random.Rng`, opaque FFI handles, and values containing any of those types do
not define equality. The compiler rejects every equality-dependent surface for
those types with `AU2008`; no backend identity comparison is available.

### Canonical `list[T]` surface

In addition to indexing, slicing, `len`, iteration, membership, `map`, and
`filter`, `list[T]` provides this method surface:

| Method | Signature | Contract |
| --- | --- | --- |
| `append` | `append(value: own T) -> None` | Transfers `value` to the end. |
| `pop` | `pop(index: int64 = -1) -> T` | Removes and transfers the normalized position. |
| `remove` | `remove(value: T) -> None` | Removes the first equal element. |
| `index` | `index(value: T) -> int64` | Returns the first position containing an equal element. |
| `count` | `count(value: T) -> int64` | Counts equal elements. |
| `insert` | `insert(index: int64, value: own T) -> None` | Transfers `value` immediately before the clamped position. |
| `extend` | `extend(other: own list[T]) -> None` | Transfers all elements of `other` to the end in order. |
| `clear` | `clear() -> None` | Removes all elements. |
| `reverse` | `reverse() -> None` | Reverses the elements in place. |
| `sort` | described below | Stably sorts in place. |
| `copy` | `copy() -> list[T]` | Returns an independent owned list; requires clone-safe `T`. |
| `get` | `get(index: int64) -> Option[T]` | Returns a cloned element or `None`; requires clone-safe `T`. |
| `set` | `set(index: int64, value: own T) -> T` | Replaces a normalized position and transfers out its element. |
| `swap` | `swap(first: int64, second: int64) -> None` | Swaps two normalized positions. |
| `reserve` | `reserve(additional: int64) -> None` | Ensures room for at least `len() + additional` elements. |
| `with_capacity` | `list[T].with_capacity(minimum: int64) -> list[T]` | Creates an empty list with room for at least `minimum` elements. |

Direct list indexing, `get`, `set`, `swap`, and `pop` normalize a negative
position `i` once as `len() + i`. A normalized position must be in
`0..len()`. `pop()` therefore selects the final element. `pop` on an empty
list and every invalid `pop`, `set`, or `swap` position trap with `AU4003`.
Direct indexed access follows its separately specified copy, ownership, and
failure contract; `get` represents an invalid position as `None`.

`insert` uses Python clamping. For length `n`, its effective position is:

1. `n + index` when `index` is negative, otherwise `index`
2. zero when that value is below zero
3. `n` when that value is above `n`
4. the value itself otherwise

The element is inserted before the effective position. This means a very
negative index inserts at the start and an index greater than the length
appends. `insert` evaluates and captures the index before evaluating and
transferring the value.

`remove`, `index`, and `count` require equality for `T` and search from the
first element toward the last. `remove` stops after deleting the first match;
`index` returns the first match; `count` examines the complete list. An absent
value traps with `AU4008` for `remove` and `index`. Their diagnostics include
help that names the `value in values` precheck when absence is expected.
`count` returns zero for an absent value.

`extend` captures its owned input before mutating the receiver, then transfers
elements in source order. It does not clone elements. `clear`, `reverse`,
`insert`, `append`, `extend`, and `swap` return `None`.

#### Stable sorting

The two static call shapes are:

```aura
sort(reverse: bool = false) -> None where T: Ord
sort[K](key: def(T) -> K, reverse: bool = false) -> None where K: Ord
```

The canonical calls are:

```aura
values.sort()
values.sort(reverse=true)
values.sort(key=make_key)
values.sort(key=make_key, reverse=true)
```

With no `key`, `T` must be orderable. With `key: def(T) -> K`, `K` must be
orderable. `reverse` has type `bool` and defaults to `false`. Equal elements or
equal produced keys retain their relative input order in both directions.

The receiver expression, `key` argument when present, and `reverse` argument
are each evaluated exactly once in source order. The selected key function is
called exactly once per element from first to last. Every key is computed and
stored before the list is mutated. If argument evaluation, key evaluation,
ordering, or allocation fails before mutation begins, the receiver remains
byte-for-byte unchanged. After successful key collection, sorting mutates only
the receiver and returns `None`.

### Canonical `dict[K, V]` surface

Dictionaries preserve insertion order for iteration and eager snapshots.
Indexing and assignment provide the primary lookup and storage syntax:

```aura
value = table[key]
table[key] = value
present = key in table
```

A direct indexed read follows the collection ownership rule for `V` and traps
with `AU4003` when the key is absent. Indexed assignment transfers its key and
value as needed, inserts an absent key, and updates an equal key without moving
its insertion position.

The method surface is:

| Method | Signature | Contract |
| --- | --- | --- |
| `get` | `get(key: K) -> Option[V]` | Returns a cloned value in `Some` or `None`; requires clone-safe `V`. |
| `remove` | `remove(key: K) -> Option[V]` | Removes the entry and transfers its value, or returns `None`. |
| `keys` | `keys() -> list[K]` | Returns cloned owned keys in insertion order; requires clone-safe `K`. |
| `values` | `values() -> list[V]` | Returns cloned owned values in insertion order; requires clone-safe `V`. |
| `items` | `items() -> list[(K, V)]` | Returns cloned owned key/value tuples in insertion order; requires clone-safe `K` and `V`. |
| `copy` | `copy() -> dict[K, V]` | Returns an independent owned dictionary; requires clone-safe `K` and `V`. |
| `update` | `update(other: own dict[K, V]) -> None` | Transfers all entries from `other` in insertion order. |
| `clear` | `clear() -> None` | Removes all entries. |
| `reserve` | `reserve(additional: int64) -> None` | Ensures room for at least `len() + additional` entries. |
| `with_capacity` | `dict[K, V].with_capacity(minimum: int64) -> dict[K, V]` | Creates an empty dictionary with room for at least `minimum` entries. |

Aura's dictionary `get` is a deliberate typed divergence from Python's
default-return form: it accepts no default argument and represents absence
with `Option[V]`. This gives typed control flow without a caller-supplied
default whose evaluation and ownership could be ambiguous. `remove` is the
typed extraction operation: it can transfer a non-cloneable stored value
without first duplicating it.

`keys`, `values`, and `items` are eager snapshots with independent owned
storage. They are not live views. Each source entry is visited once in
insertion order; a cloning or allocation failure cleans up the partial result
and leaves the dictionary unchanged.

`update` evaluates and captures its owned input before mutating the receiver.
Entries are then transferred in their input insertion order. A key already in
the receiver keeps its position and receives the incoming value; a new key is
appended to the order. `update` and `clear` return `None`.

The language uses indexed assignment for unconditional storage and `in` for
membership. The dictionary surface therefore has no duplicate storage or
membership methods. It also has exactly one eager key/value-pair method:
`items`.

### Canonical `set[T]` surface

Sets store one value per equality class. Set membership uses `value in values`
and `value not in values`. The method surface is:

| Method | Signature | Contract |
| --- | --- | --- |
| `add` | `add(value: own T) -> None` | Transfers `value` into the set; an equal stored value remains the member. |
| `remove` | `remove(value: T) -> None` | Removes an equal value; absence traps with `AU4008`. |
| `discard` | `discard(value: T) -> None` | Removes an equal value when present; absence is silent. |
| `copy` | `copy() -> set[T]` | Returns an independent owned set; requires clone-safe `T`. |
| `clear` | `clear() -> None` | Removes all values. |
| `reserve` | `reserve(additional: int64) -> None` | Ensures room for at least `len() + additional` values. |
| `with_capacity` | `set[T].with_capacity(minimum: int64) -> set[T]` | Creates an empty set with room for at least `minimum` values. |

`add`, `remove`, `discard`, and membership require equality for `T`. Searches
read their argument through shared access. `add`, `remove`, `discard`, and
`clear` return `None`; callers use membership when they need to distinguish
presence. An absent `remove` diagnostic includes help naming the
`value in values` precheck.

Set algebra, including union, intersection, difference, symmetric difference,
relation methods, and corresponding operators, is designed separately. This
decision does not infer those operations from the core set surface.

Default rendering uses `{first, second}` for a non-empty set in its defined
iteration order and `set()` for an empty set. The runtime never emits a
type-name-prefixed brace form. Rendering does not change the set or expose its
capacity.

### Capacity control

`reserve(additional)` on each mutable collection ensures that capacity is at
least the checked sum `len() + additional`. It never changes the collection's
length, contents, order, or ownership. `with_capacity(minimum)` creates an
empty collection whose capacity is at least `minimum`.

A negative `additional` or `minimum` traps with `AU4003`. Integer overflow in
the requested capacity, a request above the maintained allocation limit, or
allocation failure traps with `AU4005`. The receiver remains unchanged when a
reserve operation fails.

Capacity is not observable in Aura source. There is no capacity query,
capacity does not participate in equality or serialization, and iteration
cannot observe reallocations. Growth factors, bucket counts, load factors, and
the amount of spare capacity above the requested minimum are implementation
details. MIR and direct execution may use different allocation strategies as
long as their observable results and failures match.

### Ownership and evaluation

Collection values are non-Copy. `copy`, `get`, `keys`, `values`, `items`, and
other non-removing operations that produce owned stored values establish
clone-safety obligations for the types they duplicate. `append`, `insert`,
`extend`, dictionary assignment, `update`, and `add` take ownership of values
that enter a collection. `pop`, list `set`, and dictionary `remove` transfer
stored values out. Shared search arguments are never consumed.

All receiver and argument expressions evaluate exactly once, left to right.
Mutating methods retain exclusive access to the receiver across their complete
operation. A callback or comparison cannot observe a partially sorted list.
A trap cleans up owned arguments and partial temporary results according to
the ordinary structured-unwinding contract.

Conditional method availability is checked after generic specialization.
Equality-dependent methods require the corresponding equality relation;
sorting requires the applicable ordering relation; cloning methods require
clone-safe stored types. A rejected call identifies the unmet type obligation.

### Diagnostics and backend parity

This decision adds `AU4008`, **value not found**, for search operations whose
contract requires presence:

- `list.remove(value)` when no equal element exists
- `list.index(value)` when no equal element exists
- `set.remove(value)` when no equal member exists

Each `AU4008` diagnostic identifies the operation and missing value and
includes an `in`-based precheck in its help. The diagnostic has the same source
span, message, help, call frames, and structured fields in MIR and direct
execution.

`AU4003` covers invalid positions, empty `pop`, missing direct dictionary
indexing, and negative capacity requests. `AU4005` covers capacity arithmetic,
maintained allocation limits, and allocation failures. Static type mismatch,
mixed literal, unavailable equality, unavailable ordering, and clone-safety
failures use the applicable compile-time diagnostic and never defer a known
failure to runtime.

MIR and direct execution must be byte-identical for output, collection order,
ownership outcomes, evaluation order, success results, runtime diagnostic
codes and text, structured diagnostics, and cleanup. Hash-table layout,
allocation size, and sorting implementation are outside parity because Aura
source cannot observe them.

## Consequences

Aura gains one Python-shaped vocabulary for everyday collection work while
retaining static element types, explicit ownership transfer, typed dictionary
absence, stable sorting, and loud invariant failures. Collection algorithms
can reserve storage for predictable allocation behavior without exposing
capacity as language state.

Typed optional dictionary lookup and extraction make absence and ownership
visible in control flow. Eager key, value, and item snapshots have predictable
ownership and ordering costs. Homogeneous literals keep type inference local
and deterministic.

## Completion matrix

| Area | Required proof |
| --- | --- |
| Lexer, parser, and AST | Lowercase generic type forms; constructors; homogeneous list, dictionary, and set literals; named `sort` arguments; malformed arity and mixed-type rejection. |
| Static semantics | Exact method signatures; mutable receiver requirements; owned and shared parameter modes; equality, ordering, and clone-safety conditional availability; `int64` position and capacity types. |
| List runtime | Append; every positive and negative `pop` boundary; empty `pop`; first-match `remove` and `index`; `count`; Python-clamped `insert`; transfer-only `extend`; `clear`; `reverse`; `copy`; `get`; `set`; and `swap`. |
| Sorting runtime | Natural and keyed stable order; `reverse` composition; argument and key once-only evaluation; unchanged receiver on every pre-mutation failure; cleanup of key temporaries. |
| Dictionary runtime | Indexed read and assignment; `in`; optional `get`; transferring `remove`; insertion order; duplicate-key position; eager `keys`, `values`, and `items`; `copy`; `update`; and `clear`. |
| Set runtime | Deduplicating `add`; membership; loud `remove`; silent `discard`; `copy`; and `clear`. |
| Capacity runtime | Zero, exact, large, and negative requests; checked `len + additional`; allocation limits and failure; content/order preservation; all three collection types. |
| Diagnostics | Exact `AU4008`, `AU4003`, and `AU4005` text, help, source spans, call frames, and structured records; static obligation diagnostics; no partial mutation. |
| Ownership | Source reuse after shared searches; moves into collections; transfers out; clone-safe snapshot specialization; non-cloneable values; unwind cleanup. |
| Backend parity | Forced MIR/direct matrix for every successful operation, boundary, failure, evaluation-order trace, and diagnostic oracle. |
| Compiler analysis | Type inference, generic specialization, capability analysis, retained access, move checking, and method resolution for every surface row. |
| Editor tooling | Completion, signature help, hover, go-to-definition, diagnostics, semantic tokens, language-server protocol tests, and packaged extension tests for the canonical types and methods. |
| Maintained language surface | Manual type and collection chapters, diagnostics registry, conformance map, tutorials, examples, executable reference blocks, and reference-integrity hashes updated together. |

## Ratification

Batch S1 accepts this decision as the binding Aura 0.3 collection contract.
Compiler semantics, both execution backends, diagnostics, editor tooling,
reference material, examples, tutorials, and conformance evidence implement
this surface as one coordinated language change.
