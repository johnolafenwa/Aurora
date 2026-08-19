# Classes

Classes define nominal product types: one value contains a fixed set of named fields and exposes class methods. Ordinary classes are move types unless declared `copy class`.

The complete syntax is in [Grammar](/manual/grammar#classes). Class names, field types, defaults, methods, visibility, constructors, ownership category, and recursive layout are all statically checked.

## Declaration

```aura
class Point:
    x: float64
    y: float64
```

A class body contains one or more fields, methods, or `pass` entries. Fields and methods may be interleaved. Field names must be unique among fields, and method names must be unique among methods.

Every field has an explicit type. A field may have a default expression of exactly that type:

```aura
class Server:
    host: str = "127.0.0.1"
    port: int32 = 8080
```

Field defaults are evaluated afresh for each construction where the field is omitted. They are not shared mutable singletons. A default is checked in declaration context, not in the caller's local scope.

Generic classes declare bounded or unbounded type parameters after the name:

```aura
class Box[T]:
    value: T

class NamedBox[T: Named]:
    value: T
```

Type parameter names must be unique, all field and method types must be known with the correct arity, and every concrete substitution must satisfy its bounds. Generic arguments are invariant. See [Generics And Traits](/manual/generics-and-traits).

## Construction

Calling the class name constructs a value. Arguments may be positional in field declaration order, named by field, or positional followed by named:

```aura
point = Point(3.0, 4.0)
server = Server()
custom = Server("0.0.0.0", port=9090)
named = Point(x=3.0, y=4.0)
```

Every constructor field is an owned position. Conceptually, `Point` exposes
`Point(x: own float64, y: own float64)` and `Box[T]` exposes `Box(value: own
T)`: a non-copy argument moves into the new object. Defaults likewise create
fresh owned field values.

Construction follows these rules:

1. positional arguments fill fields in declaration order
2. positional arguments cannot follow a named argument
3. a field cannot be supplied more than once
4. an unknown field name or excess positional argument is rejected
5. every field without a default must be supplied
6. each provided or default value must have the field's exact substituted type
7. every field argument is `own`; constructing with a move value consumes it,
   while copy values are duplicated

Every supplied field expression is evaluated first, in call-site source order.
Its copy or move result is captured into the owned field slot before the next
supplied expression begins, so later side effects cannot change an earlier
captured field value. Aura then evaluates the defaults for omitted fields in field
declaration order. Binding positional or named arguments to field slots never
reorders evaluation, and supplying a field suppresses that field's default
completely.

Generic arguments may be explicit:

```aura
box = Box[int32](value=42)
```

Without explicit arguments, the checker infers them from provided fields or an expected class type. Every declared type parameter must resolve, even when it appears only in an omitted/defaulted field.

## Visibility And Construction Across Modules

Classes, fields, and methods are private to their defining module unless marked `public`:

```aura
public class Counter:
    public value: int32 = 0

    public def get(self) -> int32:
        return self.value
```

Another module may import only a `public class`. It may read or call only public members. A cross-module constructor may explicitly initialize only public fields. Consequently, a private field on a publicly constructed class must have a declaration default; otherwise an external caller cannot satisfy the required field.

Imported declarations retain their defining module identity for private-access checks. See [Names And Scopes](/manual/names-and-scopes#imports).

## Methods And Receivers

```aura
class Counter:
    value: int32 = 0

    def get(self) -> int32:
        return self.value

    def increment(mut self):
        self.value += 1

    def into_value(own self) -> int32:
        return self.value

    def zero() -> Counter:
        return Counter(value=0)
```

The receiver, when present, is the first method parameter:

| Receiver | Call contract |
| --- | --- |
| `self` | Shared receiver and the default spelling. It can read, but cannot mutate or move non-copy fields out. |
| `own self` | Consuming receiver. A non-copy instance is moved into the call. |
| `mut self` | Exclusive mutable receiver. The call requires a mutable place and may mutate it. |
| none | Associated method. It is called through the type, not an instance. |

```aura
mut counter = Counter.zero()
counter.increment()
print(counter.get())
value = counter.into_value()
```

Methods otherwise follow the function rules for generic parameters, ordinary
parameters, defaults, and owned returns. Ordinary parameter names are unique
and cannot collide with a declared `self` receiver. A typed first parameter
such as `self: Counter` is not a receiver and is rejected with a diagnostic
naming the valid forms. `Self` may be used in class method parameter and return
type positions and denotes the enclosing class specialization.

An associated method has no implicit `self` and is called as `Counter.zero()`. Instance syntax is reserved for methods with a compatible receiver and for trait methods selected for the instance type.

## Mutation

A field assignment requires a mutable base place:

```aura
mut counter = Counter.zero()
counter.value = 10
counter.increment()
```

An owned local is mutable only when introduced with `mut`. Inside a `mut self` method, `self` is a mutable place even though parameter bindings themselves are not reassigned. Inside shared `self` (whether written `self` or `self`), mutation through `self` is rejected.

Moving one non-copy field from an owned class partially moves that value. Disjoint fields remain usable, but use of the complete class is rejected until the moved field is reinitialized. See [Ownership And Borrowing](/manual/ownership-and-borrowing#partial-moves-and-reinitialization).

## Returning Fields

A consuming receiver may return an owned field because it owns the class value:

```aura
class User:
    name: str

    def into_name(own self) -> str:
        return self.name
```

A shared-borrowed receiver cannot move an owned field. When the field type
supports cloning, clone to produce an owned result:

```aura
class User:
    name: str

    def name_copy(self) -> str:
        return self.name.clone()
```

Returning a copy-valued field produces an ordinary independent copy:

```aura
class Counter:
    value: int32

    def value_copy(self) -> int32:
        return self.value
```

Returning a non-copy field through an ordinary `-> T` result requires
ownership: clone it when clone-safe, or consume the owner with `own self`. A
method may instead declare `-> view [mut] T from self` and return non-owning
access to the receiver or one of its supported fixed projections. See
[Functions](/manual/functions#owned-returns).

## `copy class`

```aura
copy class Pair:
    left: int32
    right: int32
```

A `copy class` value is duplicated by assignment and by-value use. The declaration is valid only when every field is statically copyable. A `str`, collection, resource, ordinary class, or enum with move payloads therefore prevents copy-class declaration.

Copyability is structural through copy classes and eligible enum payloads, but generic type parameters are not assumed copyable merely because one later instantiation happens to use a copy type. The complete current categories are listed in [Types](/manual/types#copy-and-move-categories).

## Recursive Fields And `indirect`

A field layout cannot contain its class again through an all-direct class-field path. This includes direct self-recursion, recursion nested inside another type, and mutual recursion through other classes.

Mark a field `indirect` to break the direct layout cycle:

```aura
class Node:
    value: int32
    next: indirect Option[Node] = Option.None
```

`indirect` applies to the complete following type reference. It is a field-layout marker, not a general pointer expression and not valid as an arbitrary runtime operation. At least one field on every recursive layout cycle must provide the indirection.

## User Resource Classes

A non-generic user class may be managed by `with` when it declares this exact instance method shape:

```aura
class Resource:
    name: str

    def close(mut self) -> None:
        print("closing " + self.name)
```

The method must be named `close`, use `mut self`, take no ordinary parameters, and return `None`. Generic user resource classes are not supported by `with` in Aura 0.3.

```aura
with resource = Resource(name="db"):
    print("using resource")
```

`with` consumes the resource expression into a fresh mutable managed binding. That binding cannot be moved out while cleanup is active. Cleanup runs exactly once for the registration on normal and maintained abnormal exits, in reverse nesting order. See [Execution Model](/manual/execution-model#resource-lifetime-and-cleanup).

## Grammar

The normative productions for `class`, `copy class`, visibility, type
parameters, fields, field defaults, `indirect`, methods, receivers, and
associated methods are in [Grammar](/manual/grammar#classes). A class suite
contains fields, methods, and/or `pass`; Aura has no separate constructor,
property, inheritance, or destructor declaration grammar.

## Typing Rules

Classes are nominal and generic arguments are invariant. Every field has one
declared type; defaults and constructor arguments must have that exact type
after substitution. Constructor binding follows field declaration order,
requires every non-defaulted accessible field, and rejects duplicate, unknown,
inaccessible private, or excess arguments. Receiver mode controls legal field
access. A `copy class` requires every field to be statically copyable, and
every direct recursive layout cycle requires `indirect`. Cross-module
visibility and the exact user-resource `close(mut self) -> None` shape
are checked before lowering.

## Runtime Semantics

Construction creates one fresh nominal value. Every supplied field expression
is evaluated first in call-site source order and its copy or move result is
captured into the owned field slot before later field-expression side effects,
followed by every
omitted field default in field declaration order. Each default is evaluated
afresh; binding the resulting values to field slots does not reorder evaluation,
and a supplied field's default is not evaluated.

Instance calls invoke the statically selected inherent or trait method;
associated methods receive no implicit instance. Class equality compares the
nominal class identity and represented field values. A managed user-resource
class is closed exactly once by its active `with` registration under the
cleanup rules in [Execution Model](/manual/execution-model).

## Ownership And Evaluation Order

Every constructor field is an owned destination: copy arguments are copied and
non-copy arguments move into the new value. Ordinary classes move; valid
`copy class` values copy. Shared receivers read, `own self` consumes, and
`mut self` requires an exclusive mutable place. Moving an owned
non-copy field partially moves its class until that field is reinitialized;
moving through a borrowed receiver is rejected. Aura inserts no hidden clone
at a constructor, field, receiver, or return boundary. Constructor side effects
follow the supplied-then-default order above even when named arguments bind
fields in a different declaration order.

## Diagnostics

`AU1101` reports malformed class, field, method, and receiver syntax. `AU2001`
reports unresolved classes, field types, methods, and members. `AU2002` covers
field/default/constructor type mismatch, generic arity or bound failure, and an
invalid non-copy field in a `copy class`. `AU2004` reports constructor or method
argument-binding failures. `AU2999` covers duplicate declarations, invalid
visibility or recursive layout, unsupported member use, and other class
rejections without a narrower category. `AU3001` reports use of a moved class
or field. `AU3002` reports overlapping receiver/argument borrows, moving a
field through shared access, or an invalid user-resource close contract.
`AU3003` reports mutation through an immutable
class place, including a shared `self` receiver, and `AU3004` reports an
invalid ownership or receiver mode. A field default, method, or cleanup body
retains the diagnostic for the operation that traps: `AU4001` for a general
runtime trap, `AU4002` for arithmetic overflow or underflow, `AU4003` for a
bounds or lookup violation, `AU4004` for a zero divisor, and `AU4005` for a
resource or I/O failure.

## Backend Support

Nominal classes, generic specialization, fields and defaults, all maintained
receiver modes, partial moves, structural equality, `copy class`, `indirect`,
visibility, and user-resource cleanup are implemented by both MIR execution
and direct native generation. Both receive the same checked class and method
metadata; compiler analysis and the LSP use that same metadata for member
resolution and signatures.

## Limits And Implementation-Defined Behavior

Aura 0.3 has no class inheritance, overloads, property syntax, custom
constructor hook, or general destructor hook. Generic user classes cannot be
managed directly by `with`. A class field default cannot call a user-defined
function in the current compiler; compute that value before construction and
pass it as an explicit field argument. `indirect` is only a recursive
field-layout marker; its storage representation and the physical order or
padding of fields are not observable language contracts. Construction and
method evaluation order are language-defined rather than
implementation-defined.

## Status

Ordinary and copy classes, generic classes, construction, defaults,
visibility, inherent and associated methods, all maintained receiver modes,
partial-field moves, recursive `indirect` fields, and non-generic user-resource
classes are implemented for the post-Phase 1.5 surface. ADR-0038 adds local
shared or mutable views of fixed class fields and returned-view contracts tied
to one receiver or parameter; views still cannot be stored in class fields or
other owned aggregates. Inheritance, properties,
custom constructor/destructor hooks, and generic `with` resources are
unavailable and MUST NOT be inferred from accepted class syntax. The
constructor evaluation rule is implemented under
`architecture_docs/decisions/0015-explicit-and-default-argument-order.md`,
whose status is **Accepted**, and is pinned by
`crates/aura-compiler/tests/fixtures/run-pass/explicit_and_default_argument_order.au`
on both backends.
