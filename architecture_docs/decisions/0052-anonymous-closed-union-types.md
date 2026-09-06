# ADR-0052: Anonymous closed union types

- Status: Accepted direction; detailed design pending
- Ratified direction: 2026-09-06, user approval of the priority roadmap
- Date: 2026-08-02
- Version target: Aura 0.4
- Implementation: Not started
- Roadmap decision: Batch S1, design-only checkpoint
- Related: ADR-0001, ADR-0011, ADR-0022, ADR-0026, ADR-0028, ADR-0033,
  ADR-0034, ADR-0038, ADR-0039, and ADR-0044

## Decision boundary

The user approved explicit closed unions, replacement of `Option[T]` by
`T | None`, type aliases, safe narrowing, and deterministic normalization.
These features are not implemented. The ratification section distinguishes
settled behavior from remaining details; the other sections retain the design
baseline for those details. See the [approved roadmap](../14-priority-roadmap.md).

## Context

Aura needs an explicit way to store one of a finite set of unrelated types in
one statically known value. Agent tools, protocol decoders, configuration
layers, and data pipelines commonly need shapes such as text-or-bytes and
number-or-absence. A closed union can express those shapes while preserving
exhaustive checking, deterministic ownership, and static backend layouts.

The feature must not turn heterogeneous literals into dynamic containers or
make a union appear merely because unrelated expressions meet at a control-flow
join. The source must state the complete set of alternatives.

## Goals

- provide concise anonymous closed sums with exact static membership
- make optional values use one canonical type spelling
- require explicit union types at inference boundaries
- support exhaustive narrowing through type patterns
- derive value properties only when every member supports them
- keep MIR and direct layouts, tags, matching, and diagnostics identical

## Non-goals

- open unions, row polymorphism, runtime type registration, or `dynamic`
- implicit least-upper-bound inference for literals, branches, or returns
- subtyping between different union member sets
- user-selected discriminant values or layout control
- union types in the C FFI
- unions containing views, mutable capabilities, or other non-owning types
- ordering comparisons across union values

## Type syntax and identity

The type operator `|` forms an anonymous closed union:

```aura
int64 | str
list[int64 | str]
dict[str, int64 | float64 | None]
```

`|` binds less tightly than generic application, tuple construction, and every
non-union type constructor. Parentheses may group it wherever that improves
readability. A union must contain at least two distinct normalized members.

The compiler recursively flattens nested unions, removes duplicate members,
and sorts the remaining members by a deterministic canonical type key. Source
order therefore has no effect on type identity:

```text
int64 | str == str | int64
(int64 | str) | int64 == int64 | str
```

The canonical key includes the fully resolved module-qualified nominal name
and canonical generic arguments. It is used for interface serialization,
diagnostics, runtime tag assignment, native cache identity, and backend parity.
General type aliases are part of the approved type-foundation batch. Expand
aliases before normalization; aliases name an existing type and add no runtime
wrapper or new nominal identity. Alias declaration syntax, generic aliases,
visibility, cycle rejection, and imported diagnostic spelling still need a
detailed contract before implementation.

A member may be any complete owned value type except another union after
flattening, a view type, an unspecialized generic, or an FFI-only opaque view.
`None` is a valid member. A one-member result after normalization is rejected
as a redundant union type. It never silently becomes that member.

## One optional spelling

`T | None` is the sole canonical optional type. The union contains exactly the
ordinary `T` payload alternative and the payload-free `None` alternative. The
language does not define `Option[T]` as an alias or second spelling.

`None | T` normalizes and is displayed as `T | None`, with `None` placed last
for optional unions. If `T` itself normalizes to a union containing `None`, the
duplicate is removed. `None | None` is rejected as a redundant union.

This choice gives absence the same narrowing, ownership, exhaustiveness, and
generic-composition rules as every other closed union. One spelling also keeps
API signatures, hover text, generated documentation, and diagnostics stable.

Flattening cannot represent both absence and a present payload that is itself
`None`. Such APIs use distinct tagged cases. In particular, ADR-0054 uses
`Item(value)` and `End` for iterator advancement; a yielded `None` is an item.
The replacement must audit nested optional uses and lookup APIs carrying
optional payloads, preserving their observable distinctions with tagged cases.

## Explicit injection and inference

Union types are never inferred. A value enters a union only when its expression
is checked against an expected union type that was explicitly written in the
program's static interface. Valid expected-type positions include:

- an annotated binding
- a parameter or return annotation
- an explicitly specialized generic argument
- an explicitly typed collection or aggregate field

```aura
mut value: int64 | str = 41
values: list[int64 | str] = [1, "two", 3]

def decode(flag: bool) -> int64 | str:
    if flag:
        return 1
    return "one"
```

Each injected expression must have exactly one member type after ordinary
literal contextual typing. If it matches no member, checking fails. If normal
contextual rules make it match more than one member, checking fails as
ambiguous and requires a member annotation before injection. Union injection
does not introduce numeric promotion, structural conversion, cloning, or an
implicit user conversion.

Without an explicitly written expected union, these remain errors:

- a mixed-type collection literal
- incompatible `if` or match-arm result types
- incompatible returns whose declaration omits a return type
- an unconstrained `None`
- a generic type argument inferred from values of different types

The inferred type of a union-valued expression is its already-established
union type. Flow joins may preserve that exact type; they cannot widen it or
construct a new member set.

The first implementation has no union cast. Code creates the expected type at
an annotation, declared call boundary, explicit generic specialization, or
typed aggregate boundary. This keeps injection within ordinary static checking
and avoids defining runtime downcasts alongside the initial feature.

## Runtime representation and equality

A union value stores one normalized member tag and that member's payload. Tags
are dense integers assigned by normalized member order. Tags are an internal
ABI detail; source code cannot observe or choose them.

Injection evaluates the payload expression exactly once and then constructs
the tagged value. Reading a tag never evaluates, clones, or converts the
payload. Cleanup destroys only the active payload, exactly once, on normal
scope exit, move, trap unwinding, propagated error, cancellation, or partial
aggregate cleanup.

Two values of the same union type compare equal only when their tags are equal
and their active payloads compare equal. Different tags compare unequal even
when their rendered values or numeric magnitudes coincide. Equality between
different union types is a static error.

Equality and hashing are available exactly when every normalized member
supports the corresponding operation. Hashing includes the member tag, so
different alternatives cannot collide solely because their payload hashes are
equal. Ordering operators and ordered-container keys are unavailable for union
types, even when every member is individually orderable.

## Type patterns and exhaustiveness

Straightforward optional-value checks must narrow access to the corresponding
member safely. Exact condition syntax, stable-place eligibility, mutation
invalidation, and branch-join rules remain to be specified. Such narrowing
does not grant an ownership capability absent from the source place.

`match` narrows a union with type patterns:

```aura
match value:
    case int64 as number:
        print(number + 1)
    case str as text:
        print(text)
    case None:
        print("missing")
```

`case Type as name` selects exactly the member whose normalized type is
`Type` and binds its payload as that type. `case None` selects the payload-free
member. A type pattern is valid only for a direct member of the scrutinee's
normalized union. It does not perform class-hierarchy, trait, numeric, or
structural tests.

Every union match must be exhaustive. It must cover each normalized member
exactly once through type patterns, compatible or-patterns, or one final
catch-all pattern. Duplicate and unreachable member patterns are rejected.
Guards do not contribute to exhaustiveness because they may be false. A guarded
member therefore requires another unguarded route that covers that member.

The binding capability follows the match capability:

- matching a bare union gives shared access to a non-Copy payload
- matching a mutable union with a mutable pattern binding gives write-through
  access to the active payload without changing its tag
- matching an owned union moves its active payload into the selected arm

Replacing a mutable payload with a different union member is an assignment to
the whole union place, not an operation on a narrowed binding. Existing arm
cleanup and writeback rules apply on fallthrough, return, break, continue,
propagated error, and trap.

## Ownership, capabilities, and traits

A union is Copy exactly when every member is Copy. It is clone-safe exactly
when every member is clone-safe, and cloning preserves the active tag. It is
`Transfer` exactly when every member is `Transfer`. A diagnostic for a failed
derived property names every union path needed to reach the first member that
lacks it.

Moving a non-Copy union moves only the active payload but consumes the entire
source union. Shared access cannot extract an owned non-Copy payload. Mutable
narrowing permits mutation of the active payload and does not permit moving it
out without replacing or consuming the whole union according to ordinary
place rules.

A union satisfies an ordinary trait obligation only when every member has the
required implementation and the operation has one statically coherent result
type. Dispatch first reads the tag and then invokes the active member's
implementation. The compiler does not expose a user-written implementation
for an anonymous union and does not use a common member name as implicit duck
typing.

Views and returned-view origins cannot be union members in this baseline.
ADR-0038 already implements place loans. Union narrowing must integrate with
that model so a contained projection cannot outlive its parent view. No union
syntax may bypass the lifetime requirements of future callable storage in
ADR-0058 or collection loans in ADR-0061.

## Backend and interface contract

Checked module interfaces serialize the normalized member list, not source
order. MIR and direct lowering must use the same normalized tags, active-payload
drop behavior, match coverage, and rendered type names. Native cache keys
include the normalized union schema.

The direct backend may choose an optimized payload layout, including a
null-pointer niche, only when the optimization is unobservable and preserves
tag identity, cleanup, size/alignment metadata, and cross-module ABI. No union
crosses the C FFI in either direction.

## Diagnostics

Dedicated diagnostics must identify:

- a union created where no explicit expected union exists
- a value matching no member or ambiguously matching multiple members
- a mixed literal with the exact explicit union annotation that would make
  its intended member set clear
- redundant one-member unions and invalid member categories
- missing, duplicate, unreachable, or guarded-only match coverage
- a type pattern that is not a direct normalized member
- unavailable Copy, clone, Transfer, equality, or hash behavior, including
  the blocking member path
- ordering and FFI use of a union
- an attempted move from shared or mutable narrowed payload access

Diagnostics print the deterministic normalized type and point both to the use
and, when applicable, to the explicitly written union declaration. They do not
invent a wider union or silently select a member.

## Consequences

Aura gains heterogeneous values without dynamic typing. The explicit expected
type makes every member set visible in APIs and storage, while exhaustive type
patterns keep consumers complete as member sets evolve.

The all-members rules are intentionally conservative. Adding a member can
remove Copy, clone, Transfer, equality, or hash availability and can make every
match non-exhaustive. That is an API-significant change, which is appropriate
for a closed sum.

## Implementation adoption

The approved union system lands as one atomic clean-slate feature family once
its remaining details are settled.
`T | None` is the sole optional type spelling and `None` is its sole absence
value. Parsing, normalization, type patterns, exhaustive matching,
checked-interface support, both backends, compiler fixtures, examples,
tutorials, generated reference material, editor tooling, and package tests all
adopt that one surface together.

Remove `Option[T]` and its constructors from the maintained language and library
surface in that implementation family, including APIs and fixtures. There is no
compatibility alias, parser support, or migration-specific diagnostic mode for
the retired spelling. Historical records may still describe the old contract.

This adoption requires the union normalizer, stable runtime tags, type-pattern
exhaustiveness, ownership/property derivation, and checked-interface encoding
to land together. The semantic database, checked-interface format, MIR schema,
native cache identity, language-server index, and generated-reference identity
all receive incompatible version bumps. Importers accept only the matching
interface version, and cached analysis and native artifacts are rebuilt after
the version change.

## Completion-test matrix

- lexer and parser: precedence, parentheses, generic nesting, multiline unions,
  `None`, malformed bars, and forbidden union member categories
- normalization: recursive flattening, duplicate removal, source-order
  independence, deterministic qualified-name order, and one-member rejection
- optional typing: sole `T | None` spelling, `None` placement, nested optional
  normalization, unconstrained `None` rejection, narrowing invalidation, and
  distinct present-`None`/absent outcomes where the API needs both
- aliases: expansion, equality with the named type, imports, generic policy,
  cycle diagnostics, and normalized union identity
- injection: every explicit expected-type position, one-time evaluation,
  unmatched and ambiguous members, and exact preservation through flow joins
- no inference: mixed literals, branches, returns, generic inference, and
  control-flow joins remain errors without an explicitly written union
- matching: every member type, `None`, aliases after normalization,
  or-patterns, guards, catch-all patterns, duplicate/unreachable coverage, and
  precise narrowed hover types
- ownership: Copy and non-Copy injection, shared/mutable/owned narrowing,
  replacement, moves, partial cleanup, and every abnormal arm exit
- derived properties: positive and negative Copy, clone, Transfer, equality,
  and hash cases with nested diagnostic paths; ordering always rejected
- runtime: tag-sensitive equality/hash, active-payload destruction exactly
  once, nested aggregate cleanup, and deterministic rendering
- traits: all-member dispatch, missing-member rejection, coherent result
  checking, and no common-name duck typing
- boundaries: task and Queue Transfer checks, place-view containment,
  cross-module interface round trips, FFI rejection, and native cache changes
- tooling: formatting, completion, hover, definition, semantic tokens,
  exhaustiveness diagnostics, generated reference, examples, and tutorials
- parity: byte-identical MIR/direct results and diagnostics across the entire
  matrix, plus forced-backend fixtures and release archive smoke tests

## Ratification and remaining design

Accepted on 2026-09-06: explicitly typed mixed collections with a fixed member
set, homogeneous literal inference, `T | None` as the sole optional surface,
safe narrowing, type aliases, deterministic normalization, and static rejection
of unsupported operations. Mixed-literal union inference is deferred.

Remaining details are alias syntax and generic/recursive policy; conditional
narrowing and invalidation; one-member normalization; exact injection/type-pattern
rules; derived operations and FFI boundaries; and physical layout/ABI choices.
The corresponding sections above are the design baseline for these details,
not a claim that the user individually ratified every original proposal.
