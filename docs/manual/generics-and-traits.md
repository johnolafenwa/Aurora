# Generics And Traits

Generics parameterize declarations over types. Traits are nominal interfaces used for generic bounds, method dispatch, operator dispatch, supertrait requirements, and `try` error conversion.

Aura does not use structural typing: having methods with matching spellings does not satisfy a trait. A visible applicable `impl` is required.

## Generic Declarations

Classes, enums, functions, methods, and implementation blocks may declare type parameters:

```aura
class Box[T]:
    value: T

enum MaybePair[T]:
    One(T)
    Two(T, T)

def identity[T](value: own T) -> T:
    return value
```

Type parameter names must be unique within their declaration. `Self` is reserved and cannot be declared as a type parameter. A generic use must provide exactly the declared arity; generic arguments are invariant and are never implicitly widened or structurally converted.

Bounds follow a type parameter after `:`. `+` means every listed bound is required:

```aura
def use_value[T: Display + Score](value: T) -> int32:
    print(value.display())
    return value.score()
```

Classes and enums may also carry bounds. The checker enforces them when resolving construction and when the specialized value is used through bounded generic operations:

```aura
class NamedBox[T: Named]:
    value: T

enum MaybeNamed[T: Named]:
    Some(T)
    Empty
```

The exact parameter-list forms are in [Grammar](/manual/grammar#type-references-and-type-parameters).

## Inference And Specialization

Generic calls infer substitutions by unifying argument types with parameter type patterns. An available expected result type may add constraints. Generic class and enum construction similarly use provided fields/payloads and an expected constructed type.

Parameter ownership is resolved at the generic declaration. Because an
unconstrained `T` is not assumed copyable, a bare `value: T` is a shared borrow
and remains declaration-stable even when a call later specializes `T` to a
copy type. Use `value: own T` when the generic body consumes, stores, or returns
the argument.

```aura
boxed = Box(value=7)          # Box[int64]
value = identity("Aura")   # str
```

Every declared type parameter must resolve. The checker does not invent a type for a parameter that appears nowhere in supplied values or expected context.

Explicit specialization fixes the arguments:

```aura
boxed = Box[int64](value=42)
value = identity[int64](42)
ok = Result[int32, str].Ok(7)
```

Explicit arguments must have exact arity and satisfy all substituted bounds. Specialization and indexing share bracket syntax; the parser rules that distinguish them are specified in [Grammar](/manual/grammar#explicit-specialization).

## Inferred Clone-Safety Obligations

Generic clone-safety obligations are inferred from clone-producing operations in callable bodies.
An operation over an unresolved type parameter is accepted when the checker can
record which declared parameters must be safe to clone. A concrete call
discharges those obligations after substitution. A type is safe for this
purpose when duplicating it cannot duplicate `random.Rng` state through an
ordinary class, enum, or collection path; `Task[T]` and `Queue[T]` handles stop
that clone-safety traversal because an allowed handle copy does not observe or
copy `T`. Accepted ADR-0033 separately requires Queue payloads and task
results to be `Transfer` and makes `Task[T]` non-copy when `T` is not
repeatable. A clone barrier is therefore not an escape from the task-boundary
rule.

A generic-to-generic call propagates the obligation to the caller. Inference
continues to a fixed point independent of declaration order, and the resulting
contract is retained by imported functions and methods. The same rules apply
to ordinary, inherent, associated, task-target, trait, operator, and `From`
calls. There is no source annotation for this obligation in Aura 0.3.

When a type is concrete, a substitution containing `random.Rng` is rejected
with `AU3007`. A concrete type whose clone safety cannot be proved is rejected
conservatively. Moving, removing, receiving, or rearranging one owned value is
not clone-producing and introduces no such obligation.

## Trait Declarations

A trait declares a nominal method contract:

```aura
trait Greeter:
    def greet(self) -> str
```

Trait methods may be signature-only, ending at the newline, or may provide a default body after `:`:

```aura
trait Named:
    def name(self) -> str

    def label(self) -> str:
        return "name=" + self.name()
```

A marker trait contains `pass` and no required methods:

```aura
trait Marker:
    pass
```

Trait names and method names must be unique in their scopes. Trait type parameter lists use the plain parameter form:

```aura
trait Mapper[T]:
    def map(self, value: own T) -> T
```

Bounds may appear on a trait method's own generic parameters. Ordinary trait method parameters cannot have defaults.

An obligation inferred from a trait default body is part of the trait method's
contract. It is substituted through `Self`, trait arguments, and method type
arguments for every implementation and every form of dispatch. A
signature-only trait method has no inferred clone-safety obligation.

A trait is private to its defining module unless declared `public trait`. Implementation blocks have no independent exported name and cannot be prefixed with `public`; their methods become available through the implemented public trait/type context when the implementation is loaded.

## `Self`

`Self` denotes the implementing or enclosing concrete class specialization in supported class, trait, and implementation method type positions:

```aura
trait Combine:
    def combine(self, other: Self) -> Self
```

`Self` takes no type arguments. It is not a global type and is unavailable in an unrelated top-level function. Inside a trait declaration it is initially a placeholder; inside an implementation it is substituted with the implementation target.

## Implementations

An implementation attaches one trait specialization to one target type pattern:

```aura
class Person:
    name: str

impl Greeter for Person:
    def greet(self) -> str:
        return "hello " + self.name
```

Generic and specialized implementations are supported:

```aura
impl Mapper[int32] for Doubler:
    def map(self, value: own int32) -> int32:
        return value * self.factor
```

```aura
impl[T] Mapper[T] for Box[T]:
    def map(self, value: own T) -> T:
        return value
```

```aura
impl Displayable for Box[str]:
    def display(self) -> str:
        return self.value.clone()
```

An implementation target must have a concrete or generic named outer type such as `Box[T]`; a bare target type parameter in `impl[T] Trait for T` is rejected. Implementation type parameters may have bounds, and every parameter used by the target/trait pattern must resolve during applicability checking.

Two implementations with exactly the same trait specialization and target are duplicates and are rejected. More general and more specialized overlapping patterns may coexist. Dispatch selects the unique applicable implementation with greatest structural specificity; equal-best matches are ambiguous and rejected. Source order is never a tie breaker.

Aura 0.3 does not impose a separate orphan-rule restriction, but an implementation must refer to known visible types and traits and participates only where that implementation is present in the loaded module/package context.

## Implementation Method Conformance

An implementation may define only methods belonging to the trait. It must provide every signature-only required method; a trait method with a default body is inherited when omitted. An implementation may override a default method.

For an explicitly implemented method, conformance compares:

- receiver presence and passing mode (shared `self`, consuming `own self`,
  `mut self`, or none)
- ordinary parameter count and substituted types
- each ordinary parameter's resolved owned/shared/mutable access mode
- owned return type
- the trait method's substituted clone-safety obligations

Ordinary parameter names may differ between the trait and implementation when
their positions and types still match.

Implementation methods cannot add default ordinary arguments. Extra methods, missing required methods, receiver mismatches, and signature mismatches are rejected before body execution.

An explicit implementation MUST NOT strengthen its trait method's clone-safety contract.
Its body may rely on obligations already inferred by the trait method, but it
cannot introduce a requirement that bound-based callers cannot see. Because
Aura 0.3 has no explicit clone-safety annotation, generic clone-producing
behavior belongs in a trait default body. An implementation that adds such a
requirement is rejected with `AU3007`.

An `impl` targeting any builtin type MUST
NOT explicitly define or inherit a trait method whose name is a builtin member
of that target. This covers the
runtime handles `Queue[T]`, `Task[T]`, `TaskGroup`, `random.Rng`, `fs.File`,
and the `net` and `process` handles, and equally the builtin value types such
as `str`, `list[T]`, `dict[K, V]`, `set[T]`, `Duration`, and the scalar types.
Builtin member names are reserved for their runtime operation; a collision
reports `AU2006` and the trait method must be renamed. A trait method whose name
does not collide still implements and dispatches normally on a builtin target.
This rule is applied after default trait methods are inherited.

## Trait Method Dispatch

For a concrete value, member lookup considers inherent class methods and applicable visible trait implementations. The selected method keeps its declared receiver and argument ownership behavior.

For a type parameter, only methods justified by declared bounds are available:

```aura
def say_hello[T: Greeter](value: T):
    print(value.greet())
```

Specialized trait bounds provide their type arguments:

```aura
def apply[M: Mapper[int32]](mapper: M, value: int32) -> int32:
    return mapper.map(value)
```

If multiple bounds or equally specific implementations expose an indistinguishable applicable method, the call is ambiguous and rejected.

Concrete and bound-based dispatch enforce the same substituted clone-safety
contract. Associated trait methods follow the same rule as receiver methods.

Traits may also declare associated methods without `self`:

```aura
trait Factory:
    def make() -> int32

impl Factory for Widget:
    def make() -> int32:
        return 7

value = Widget.make()
```

## Supertraits

A trait may require one or more supertraits:

```aura
trait Labelled: Named:
    def label(self) -> str:
        return "name=" + self.name()
```

The second colon terminates the header. Multiple supertraits are comma-separated.

An `impl Labelled for User` is valid only when the same target also satisfies `Named` through an applicable implementation. Implementing the child does not synthesize the parent implementation. Supertrait methods are available through a child bound, and default child methods may call them.

Supertrait types must name known traits with exact arity. Requirements are transitively closed during bound and dispatch checking.

## Operator Traits

When builtin numeric/string operator rules do not apply, these operator spellings request traits and method names:

| Source operator | Trait method |
| --- | --- |
| `left + right` | `Add.add` |
| `left - right` | `Sub.sub` |
| `left * right` | `Mul.mul` |
| `left / right` | `Div.div` |
| `left // right` | `FloorDiv.floor_div` |
| `left % right` | `Mod.mod` |
| `-value` | `Neg.neg` |
| `not value` | `Not.not` |
| `<`, `<=`, `>`, `>=` | `Ord.lt`, `Ord.le`, `Ord.gt`, `Ord.ge` |

Builtin numeric `//` and the heterogeneous builtin `Duration // int64` rule
take precedence over trait dispatch. Otherwise `//` and `//=` request an
applicable `FloorDiv.floor_div` implementation. Equal integer operands with
`/` are rejected with the integer-division teaching diagnostic rather than
dispatched to `Div.div`; `/` can still request `Div.div` for an applicable
non-numeric user type. The divisor-sign rule for `%` describes builtin numeric
remainder; `Mod.mod` on a user type has the semantics of that implementation.

The maintained generic shapes are illustrated by:

```aura
trait Add[Rhs, Out]:
    def add(self, rhs: Rhs) -> Out

trait FloorDiv[Rhs, Out]:
    def floor_div(self, rhs: Rhs) -> Out

trait Neg[Out]:
    def neg(self) -> Out

trait Ord[Rhs]:
    def lt(self, rhs: Rhs) -> bool
    def le(self, rhs: Rhs) -> bool
    def gt(self, rhs: Rhs) -> bool
    def ge(self, rhs: Rhs) -> bool
```

`Sub`, `Mul`, `Div`, and `Mod` follow the same binary `Rhs, Out` shape as
`Add` and `FloorDiv`; `Not` follows the unary `Out` shape. Ordering methods
must return `bool`.

`and` and `or` do not dispatch through traits. Builtin `==` and `!=` also do
not use an equality trait in Aura 0.3. This includes recursive structural
tuple equality, which a trait implementation cannot override. Builtin
operations take precedence wherever their concrete value rule applies.

When an operator selects a trait method, it also enforces that method's
substituted clone-safety obligations.

## `From` And `try`

When `try` propagates `Result[T, SourceError]` from a function returning `Result[U, TargetError]`, exact error-type equality needs no trait. Otherwise the checker looks for an applicable `impl From[SourceError] for TargetError` containing `from`.

The conventional contract is:

```aura
trait From[Source]:
    def from(value: own Source) -> Self
```

The selected conversion runs before `Result.Err` is returned from the enclosing function. If no applicable conversion exists, `try` is rejected. See [Functions](/manual/functions#try-and-result-returns).

The selected `From.from` method's clone-safety obligations are enforced before
the conversion is accepted.

## Current Generic And Trait Boundaries

- generic arguments are invariant and there is no general subtyping
- type inference is local/contextual rather than whole-program inference
- trait and implementation method defaults for ordinary parameters are not supported
- generic user classes cannot currently serve as `with` resources
- generic task targets are permitted when their callable type arguments can be
  resolved; bare shared and `own` targets use task-owned captures, while `mut`
  targets are rejected
- equal-specificity overlapping implementations remain an error at the use site
- clone-safety obligations are inferred rather than written, and an explicit
  implementation cannot strengthen the contract inferred by its trait method

Observable syntax and implementation limits are collected in [Current Limits](/manual/current-limits), while cross-cutting type rules are in [Static Semantics](/manual/static-semantics#generics-traits-and-implementations).

## Grammar

The normative productions for type parameters, bounds, explicit
specialization, trait declarations, supertraits, `Self`, and implementation
blocks are in [Grammar](/manual/grammar). Classes, enums, functions, methods,
traits, and implementations use the declaration-specific parameter forms
shown above. Trait methods may be signature-only or have a default suite;
implementation methods always use ordinary method-definition syntax.

## Typing Rules

Generic arguments are invariant and have exact arity. Inference is local and
contextual, must resolve every declared parameter, and must satisfy every
substituted bound. Trait satisfaction is nominal through a visible applicable
`impl`, never structural. Implementations must conform after substituting
receiver mode, parameter modes and types, owned return type, clone-safety
obligations, and supertrait requirements. Dispatch selects one unique
greatest-specificity applicable implementation; equal-best matches are
rejected. `Self` denotes the enclosing/implementing concrete specialization
only in its supported declaration contexts.

## Runtime Semantics

Generic construction and calls use the statically resolved specialization;
there is no runtime generic inference. Trait member and operator calls invoke
the statically selected implementation, inheriting a trait default body when
the implementation omits that method. Source order never resolves overlapping
implementations. `try` invokes the selected `From[Source]` conversion before
constructing the enclosing `Result.Err`. Traits do not create runtime
reflection, dynamic method dictionaries, or implicit conversions.

## Ownership And Evaluation Order

Parameter ownership is resolved at the generic declaration and remains stable
after specialization: an unresolved bare `T` is shared, even when one later
substitution is copy, while `own T` is the explicit consuming form. Trait and
implementation signatures must agree on that resolved mode. Receiver
evaluation precedes ordinary arguments, selected methods keep their declared
receiver/parameter behavior, and `From.from` owns its source error. No generic
or trait boundary inserts a hidden clone, coercion, or ownership-mode change.
Clone-producing bodies infer obligations, generic calls propagate them, and
concrete dispatch discharges them after substitution.

## Diagnostics

`AU1101` reports malformed generic, trait, supertrait, specialization, or
implementation syntax. `AU2001` reports unknown types, traits, methods, and
members. `AU2002` covers inference failure, generic arity, unsatisfied bounds,
missing trait satisfaction, ambiguous equal-specificity dispatch, invalid
specialization, and substituted type mismatch. `AU2003` reports an unsupported
operator when no builtin rule or applicable operator trait supplies it.
`AU2004` reports call argument binding and the prohibition on ordinary default
arguments in trait methods. `AU2006` reports builtin method collisions.
`AU2999` covers duplicate/invalid implementations, method-conformance or
supertrait failure, unsupported implementation targets, and remaining
generic/trait rejections. `AU3001` reports use after an owned generic or
receiver move. `AU3002` reports borrow conflicts or storing through a bare
shared generic parameter.
`AU3003` reports a mutable receiver call through an immutable place, and
`AU3004` reports an invalid ownership mode. `AU3007` reports an unsafe concrete
clone specialization, an unprovable concrete requirement, or an implementation
that would strengthen its trait method's clone-safety contract. A selected body retains its runtime
diagnostic: `AU4001` for a general trap, `AU4002` for arithmetic overflow or
underflow, `AU4003` for a bounds or lookup violation, `AU4004` for a zero
divisor, and `AU4005` for a resource or I/O failure.

## Backend Support

Generic functions, classes, enums, methods, traits, supertraits, default trait
bodies, generic and specialized implementations, operator dispatch, `Self`,
and `From` conversion are implemented for MIR execution and direct native
generation. User-trait dispatch on builtin values, including `Queue[T]`,
`Task[T]`, `TaskGroup`, `random.Rng`, `str`, and the builtin collections, is
maintained for noncolliding method names on both backends;
builtin target members always retain builtin dispatch. The checker
supplies one resolved specialization and implementation target to lowering,
analysis, and the LSP, including inferred clone-safety obligations; the parity gate rejects backend-specific dispatch
behavior.

## Limits And Implementation-Defined Behavior

Aura 0.3 has no trait objects, dynamic dispatch, associated types or
constants, higher-kinded parameters, default type arguments, `where` clauses,
specialization annotations, general subtyping, or separate orphan-rule
restriction. A bare target parameter in `impl[T] Trait for T` is unsupported.
Equal-specificity overlaps remain errors, ordinary trait/impl parameters cannot
add defaults, and generic user classes cannot be `with` resources. Inference
and dispatch are
defined by the rules above rather than source order or backend implementation
choice.

## Status

Invariant generics, local/contextual inference, explicit specialization,
nominal traits and bounds, supertraits, default methods, generic and specialized
implementations, unique-most-specific dispatch, operator traits, `Self`, and
`From`-based `try` conversion plus inferred clone-safety contracts are implemented for the post-Phase 1.5 surface.
Ordinary `-> T` return values are owned. Generic functions and methods may
instead declare `-> view [mut] T from origin`; trait implementations preserve
the trait declaration's origin slot as well as its specialized pointee type.
Trait objects, dynamic dispatch, associated types,
higher-kinded types, general
subtyping, and arbitrary blanket implementation targets are unavailable.

### Verified Clone-Safety Contracts

The following blocks pin the observable boundary. A generic clone helper is
valid for a safe specialization:

```aura
def duplicate[T](values: list[T]) -> list[T]:
    return values.copy()

def main() -> int32:
    values = [1, 2]
    print(duplicate(values))
    return 0
```

The same callable rejects an unsafe concrete specialization:

```aura
import random

def duplicate[T](values: list[T]) -> list[T]:
    return values.copy()

def reject(values: list[random.Rng]) -> list[random.Rng]:
    return duplicate(values)
```

The requirement also survives a generic-to-generic call:

```aura
import random

def duplicate[T](values: list[T]) -> list[T]:
    return values.copy()

def forward[T](values: list[T]) -> list[T]:
    return duplicate(values)

def reject(values: list[random.Rng]) -> list[random.Rng]:
    return forward(values)
```

A signature-only trait method does not let an implementation add a hidden
requirement:

```aura
trait Copier[T]:
    def copy_values(self) -> list[T]

class Wrapper[T]:
    values: list[T]

impl[T] Copier[T] for Wrapper[T]:
    def copy_values(self) -> list[T]:
        return self.values.copy()
```

A trait default body can establish the requirement for safe specializations:

```aura
trait Duplicator[T]:
    def duplicate(self, values: list[T]) -> list[T]:
        return values.copy()

class Marker[T]:
    value: T

impl[T] Duplicator[T] for Marker[T]:
    pass

def main() -> int32:
    marker = Marker(0)
    values = [4, 5]
    print(marker.duplicate(values))
    return 0
```

Its unsafe specialization is rejected through the same contract:

```aura
import random

trait Duplicator[T]:
    def duplicate(self, values: list[T]) -> list[T]:
        return values.copy()

class Marker[T]:
    value: T

impl[T] Duplicator[T] for Marker[T]:
    pass

def reject(marker: Marker[random.Rng], values: list[random.Rng]) -> list[random.Rng]:
    return marker.duplicate(values)
```
