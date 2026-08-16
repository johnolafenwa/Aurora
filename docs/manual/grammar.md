# Grammar

This chapter defines the complete source grammar of Aura 0.3. The grammar is normative after lexical token formation. Static restrictions—types, visibility, ownership, exhaustiveness, valid receivers, and API-specific rules—are defined by [Static Semantics](/manual/static-semantics).

## Notation

The grammar uses an EBNF-style notation:

- quoted text is a literal token
- `name` is a nonterminal
- `[ item ]` is optional
- `{ item }` repeats zero or more times
- `( a | b )` selects one alternative
- a comma in the grammar separates sequence elements; `","` is the source comma token
- comments inside grammar blocks are informative

`NEWLINE`, `INDENT`, `DEDENT`, and `EOF` are layout tokens produced by the lexer. `IDENT`, `INTEGER`, `FLOAT`, `DURATION`, `STRING`, `FSTRING`, and `BOOLEAN` are lexical tokens described below.

Comma-separated source lists do not accept a trailing comma unless their
production explicitly adds one. The singleton tuple forms `(value,)`, `(T,)`,
and `(pattern,)` require their one comma; multi-element tuples do not accept a
trailing comma.

`NEWLINE` in the productions means a logical newline. A physical newline
suppressed inside an open `(`, `[`, or `{` never reaches this grammar.
Delimiter continuation changes token formation, not the expression
productions; it does not add a trailing comma to any list form.

## Lexical Grammar

```ebnf
ascii-letter = "A" … "Z" | "a" … "z" ;
digit        = "0" … "9" ;
binary-digit = "0" | "1" ;
octal-digit  = "0" … "7" ;
hex-digit    = digit | "a" … "f" | "A" … "F" ;

IDENT = (ascii-letter | "_"), { ascii-letter | digit | "_" } ;

decimal-digits  = digit, { digit } ;
decimal-integer = digit, { digit | ("_", digit) } ;
hex-integer     = ("0x" | "0X"), hex-digit,
                  { hex-digit | ("_", hex-digit) } ;
binary-integer  = ("0b" | "0B"), binary-digit,
                  { binary-digit | ("_", binary-digit) } ;
octal-integer   = ("0o" | "0O"), octal-digit,
                  { octal-digit | ("_", octal-digit) } ;
INTEGER  = decimal-integer | hex-integer | binary-integer | octal-integer ;
EXPONENT = ("e" | "E"), [ "+" | "-" ], digit, { digit } ;
FLOAT    = decimal-digits, ".", decimal-digits, [ EXPONENT ]
         | decimal-digits, EXPONENT ;
DURATION = decimal-digits, ("ms" | "s" | "m") ;
BOOLEAN  = "true" | "false" ;
```

Identifiers are ASCII and case-sensitive. Unicode is allowed in string
contents. Integers may be decimal, hexadecimal, binary, or octal and must fit
the lexer’s unsigned 128-bit literal representation before contextual typing.
An underscore is accepted only between digits valid for the selected base.
Floats must be finite `f64` values at lexing time. Duration literals represent
non-negative integral decimal milliseconds, seconds, or minutes and must fit
signed 128-bit nanoseconds after scaling. A negative number is unary `-`
applied to a positive literal, not one lexical token. Leading-dot and
trailing-dot float forms are not accepted.

## Keywords And Contextual Words

The reserved token words are:

```text
class enum def trait impl import from mut own indirect public extern opaque
return assert if elif else and or not match case for in while break
continue pass try with as true false
```

`from` is contextual: it introduces a from-import at module level, completes a
returned-view annotation, and may also be used as an identifier where the
grammar expects one. `view` is contextual in complete local-view,
returned-view, and `return view` forms. `lambda` is lexed as
an identifier but introduces a lambda at the start of an expression; member
and named-argument positions may still use that spelling. `copy`, `self`,
`None`, `set`, `Self`, and `_` are lexed as identifiers and acquire special
meaning only in the positions defined below.

## Strings And F-Strings

`STRING` is an ordinary, triple-quoted, or raw string. Ordinary strings use a
matching pair of single or double quotes. Triple-quoted strings use three
matching single or double quotes and may span physical lines. Ordinary and
triple-quoted strings accept the same escapes:

| Escape | Meaning |
| --- | --- |
| `\n` | line feed |
| `\t` | tab character in the decoded value |
| `\"` | double quote |
| `\'` | single quote |
| `\\` | backslash |
| `\0` | NUL |
| `\xHH` | byte-valued Unicode scalar from exactly two hexadecimal digits |
| `\u{H...}` | Unicode scalar from one or more hexadecimal digits |

An invalid scalar, unknown escape, missing digit, or missing or mismatched
closing quote is a lexical error. Triple-quoted values preserve every scalar
between their delimiters. Aura does not trim the first or last newline, remove
indentation, or normalize whitespace.

Raw strings use lowercase `r` immediately followed by one single or double
quote. Backslashes are content. A backslash may retain the active quote inside
the value, with both characters preserved. A raw string cannot span a physical
line or end in an odd run of backslashes. Raw triple strings, raw f-strings,
and byte strings are not tokens. There is no separate character-literal token.

`FSTRING` begins with `f"` and ends at the matching double quote.
`{ expression }` interpolates an ordinary Aura expression. Two opening braces insert one
literal opening brace, and two closing braces insert one literal closing brace.
A lone closing brace outside an interpolation is also literal in Aura 0.3.
Interpolations may contain nested braces and ordinary single- or double-quoted
strings; braces inside those strings do not change interpolation depth. Empty
or invalid interpolations are rejected. An interpolation may end with one
top-level `:` followed by this static format grammar:

```text
[[fill]align] [sign] [width] [","] ["." precision] [type]
```

`align` is `<`, `^`, or `>`; `sign` is `+`, `-`, or a space; and `type` is
`d`, `f`, `e`, `x`, `X`, `b`, `o`, `%`, or `s`. Width and precision are
decimal values through `1_000_000`. The parser accepts a complete expression
before looking for the separator, so colons inside slices, calls, dictionaries,
and other nested delimiters remain expression syntax. Nested fields and dynamic
specifications are rejected. Single-quoted f-strings and conversion flags are
not supported.

Although `\t` creates a tab in a decoded ordinary string, a physical tab is
rejected outside a triple-quoted string. A physical tab inside a triple-quoted
string is exact string content.

## Comments, Physical Lines, And Indentation

`#` starts a comment outside a string and consumes the rest of the physical line. There are no block comments.

The source is UTF-8. One optional UTF-8 BOM is ignored only at the beginning of the file.

Layout token formation is:

1. A blank or comment-only physical line produces no token and does not affect indentation.
2. Every other physical line is measured by its number of leading ASCII spaces.
3. In ordinary block-layout mode, an increase from the current indentation
   count emits one `INDENT` and pushes that exact count.
4. In ordinary block-layout mode, a decrease emits `DEDENT` tokens until an
   earlier count is reached. A count not present on the stack is inconsistent
   indentation and is rejected.
5. The line content is tokenized. An ordinary-layout line emits one
   `NEWLINE`; a continuation line suppresses it; and a delimited
   expression-`match` layout island emits only the layout tokens required by
   its header and arms.
6. At end of source, remaining indentation levels emit `DEDENT`, followed by `EOF`.

Outside an open delimiter, Aura does not prescribe four-space indentation;
it requires consistent return to previous block levels. The maintained
formatter and examples use four spaces.

While a `(`, `[`, or `{` remains open, ordinary physical newlines and their
leading spaces do not produce layout tokens. Delimiters must nest and match by
kind. A delimited expression-form `match` is a layout island: its header and
arms retain the layout tokens required by the match productions even though an
outer delimiter remains open.

Backslash continuation is unavailable. Ordinary, raw, and f-strings remain
single-line. Triple-quoted ordinary strings may span physical lines without
creating layout tokens. Existing comma-separated forms do not gain a trailing
comma.

## Punctuation And Operators

```text
( ) [ ] { } : , . ?
= == != < <= > >=
+ += - -= * *= ** **= / /= // //= % %=
& &= | |= ^ ^= ~ << <<= >> >>=
->
```

There is no semicolon, assignment expression, unary plus, or lambda arrow.

## Modules And Imports

```ebnf
module = { module-element }, EOF ;

module-element = import-declaration | module-constant | item | statement ;

module-constant
    = [ "public" ], IDENT, [ ":", type ], "=", expression, NEWLINE ;

import-declaration
    = "import", identifier-path, [ "as", import-alias ], NEWLINE
    | "from", identifier-path, "import",
      import-name, { ",", import-name }, NEWLINE ;

import-name  = identifier, [ "as", import-alias ] ;
import-alias = IDENT ;

identifier-path = identifier, { ".", identifier } ;
identifier      = IDENT | "from" ;
```

Imports, module constants, items, and executable top-level statements may be
interleaved syntactically. Imports resolve before initializer checking. Module
constants initialize after their dependencies and in declaration source order.
Executable entry statements run only after constant initialization completes.
The compiled module represents these as separate categories; programs MUST use
the defined category ordering and MUST NOT infer another execution order from
cross-category interleaving.

An `as` clause binds the complete imported module or declaration under the
written local alias. A from-import may mix direct and aliased names in one
declaration. Aliasing does not change the target module identity, visibility,
type identity, or package resolution path.

Wildcard imports, relative-dot imports, parenthesized import lists, and
trailing import commas are not part of the grammar.

## Items

```ebnf
item
    = [ "public" ], class-declaration
    | [ "public" ], enum-declaration
    | [ "public" ], function-declaration
    | [ "public" ], extern-function-declaration
    | [ "public" ], extern-opaque-declaration
    | [ "public" ], trait-declaration
    | impl-declaration ;

extern-function-declaration
    = "extern", STRING, "def", identifier,
      "(", [ parameter-list ], ")", "->", type, NEWLINE ;

extern-opaque-declaration
    = "extern", STRING, "opaque", "class", identifier, NEWLINE ;
```

`public` is not allowed on an implementation block. Item declarations are module-level; they are not statements and cannot appear inside function/control-flow suites.
Parsing requires the extern ABI string to be exactly `"C"`.
Extern declarations are bodyless and non-generic. Their parameter modes and
types are restricted by [FFI v0](/manual/ffi).

## Type References And Type Parameters

```ebnf
type
    = [ "indirect" ], type-primary,
      [ "?" ] ;

type-primary
    = identifier-path, [ "[", type-list, "]" ]
    | tuple-type
    | function-type ;

type-list = type, { ",", type } ;

tuple-type
    = "(", type, ",", ")"
    | "(", type, ",", type, { ",", type }, ")" ;

function-type
    = "def", "(", [ function-type-parameter,
      { ",", function-type-parameter } ], ")", "->", type ;

function-type-parameter
    = [ "mut" | "own" ], type ;

plain-type-parameters
    = "[", identifier, { ",", identifier }, "]" ;

bounded-type-parameters
    = "[", bounded-type-parameter,
      { ",", bounded-type-parameter }, "]" ;

bounded-type-parameter
    = identifier, [ ":", type, { "+", type } ] ;
```

A function type contains parameter modes and types, but no names or default
expressions: `def(int32, mut Counter, own str) -> bool`. A bare parameter
is shared, `mut` requires caller-visible mutable access, and `own` transfers
the argument. Parameter names are not accepted inside the list. `indirect` is
invalid on a function type because the value is already a code pointer.

`T?` denotes `Option[T]`, including when `T` is a tuple type. Type and
type-parameter lists are nonempty when brackets are present and do not accept
trailing commas. `(T,)` is a singleton tuple type; `(T)` is not a type. `()`
and a trailing comma on a multi-element tuple type are rejected. Although the
grammar places `indirect` before any type primary, it is statically valid only
on the complete named type reference where recursive-field rules permit it;
an `indirect` tuple type is rejected.

## Classes

```ebnf
class-declaration
    = [ "copy" ], "class", identifier,
      [ bounded-type-parameters ],
      ":", NEWLINE, INDENT,
      class-member, { class-member },
      DEDENT ;

class-member
    = "pass", NEWLINE
    | [ "public" ], field-declaration
    | [ "public" ], method-declaration ;

field-declaration
    = identifier, ":", type,
      [ "=", expression ], NEWLINE ;
```

`copy` is contextual and is recognized only immediately before `class`. Fields and methods may be interleaved. `pass` permits an otherwise empty class body; a comment-only body is not a suite.

## Enums

```ebnf
enum-declaration
    = "enum", identifier, [ bounded-type-parameters ],
      ":", NEWLINE, INDENT,
      enum-variant, { enum-variant },
      DEDENT ;

enum-variant
    = identifier, [ "(", enum-payload-list, ")" ], NEWLINE ;

enum-payload-list
    = type, { ",", type }
    | identifier, ":", type,
      { ",", identifier, ":", type } ;
```

A variant payload list is either entirely positional or entirely named. Empty payload parentheses and mixed positional/named declarations are rejected. A no-payload variant omits parentheses.

## Functions, Methods, And Parameters

```ebnf
function-declaration
    = "def", identifier, [ bounded-type-parameters ],
      "(", [ parameter-list ], ")",
      [ return-annotation ],
      ":", NEWLINE, suite ;

method-declaration
    = "def", identifier, [ bounded-type-parameters ],
      "(", [ method-parameter-list ], ")",
      [ return-annotation ],
      ":", NEWLINE, suite ;

parameter-list
    = parameter, { ",", parameter } ;

method-parameter-list
    = receiver, [ ",", parameter, { ",", parameter } ]
    | parameter-list ;

receiver
    = "self"
    | "mut", "self"
    | "own", "self" ;

parameter
    = identifier, ":",
      [ "mut" | "own" ],
      type,
      [ "=", expression ] ;

return-annotation
    = "->", type
    | "->", "view", [ "mut" ], type,
      "from", identifier ;
```

A receiver, when present, is the first method parameter. Bare `self` is the shared receiver, `mut self` is mutable, and `own self` is consuming. There is exactly one spelling per capability. A first method parameter written as `self: Type` is rejected rather than interpreted as an ordinary parameter; use one of the receiver forms above. Ordinary parameter capabilities appear after the colon: bare `T` is shared, `mut T` is mutable, and `own T` is consuming. Call sites pass the value directly and never prefix an argument with a capability.

Bare means shared access for every type, including declaration-known copy
types. An ordinary type annotation is an owned return. A view annotation names
one receiver or ordinary parameter origin; static semantics validates its
kind, provenance, and caller-place requirements.

Parameter lists, calls, and return annotations do not accept trailing commas. Static checking further restricts duplicate names, default placement/availability, and mutable task targets.

## Traits And Implementations

```ebnf
trait-declaration
    = "trait", identifier, [ plain-type-parameters ], ":",
      [ type, { ",", type }, ":" ],
      NEWLINE, INDENT,
      trait-member, { trait-member },
      DEDENT ;

trait-member
    = "pass", NEWLINE
    | trait-method ;

trait-method
    = "def", identifier, [ bounded-type-parameters ],
      "(", [ method-parameter-list ], ")",
      [ return-annotation ],
      ( NEWLINE | ":", NEWLINE, suite ) ;

impl-declaration
    = "impl", [ bounded-type-parameters ],
      identifier, [ "[", type-list, "]" ],
      "for", type,
      ":", NEWLINE, INDENT,
      impl-member, { impl-member },
      DEDENT ;

impl-member
    = "pass", NEWLINE
    | method-declaration ;
```

Trait-declaration type parameters use the plain form; bounds on those parameters are expressed through supertraits or method constraints rather than inline bounds in the trait parameter list. Trait methods may be signature-only (newline immediately after the return annotation) or provide one default body after `:`.

The second colon in a trait header separates an optional comma-separated supertrait list from the body, for example `trait Child: Parent, Named:`.

## Suites And Statements

```ebnf
suite = INDENT, statement, { statement }, DEDENT ;

statement
    = assignment-statement
    | view-statement
    | return-statement
    | assert-statement
    | pass-statement
    | if-statement
    | match-statement
    | for-statement
    | with-statement
    | while-statement
    | break-statement
    | continue-statement
    | expression-statement ;

statement-end = NEWLINE | DEDENT | EOF ;

assignment-statement
    = [ "mut" ], assignment-target,
      [ ":", type ],
      assignment-operator,
      expression, statement-end
    | unpack-target, "=", expression, statement-end ;

view-statement
    = "view", [ "mut" ], identifier,
      "=", expression, statement-end ;

assignment-target
    = identifier,
      { ".", identifier | "[", expression, "]" } ;

unpack-target
    = binding-target, ",", binding-target,
      { ",", binding-target }
    | "(", binding-target-list, ")" ;

binding-target-list
    = binding-target, ","
    | binding-target, ",", binding-target,
      { ",", binding-target } ;

binding-target
    = identifier
    | "(", binding-target-list, ")" ;

assignment-operator
    = "=" | "+=" | "-=" | "*=" | "**=" | "/=" | "//=" | "%="
    | "&=" | "|=" | "^=" | "<<=" | ">>=" ;

return-statement
    = "return", [ expression ], statement-end
    | "return", "view", [ "mut" ],
      expression, statement-end ;
assert-statement     = "assert", non-tuple-expression,
                       [ ",", non-tuple-expression ], statement-end ;
pass-statement       = "pass", NEWLINE ;
break-statement      = "break", NEWLINE ;
continue-statement   = "continue", NEWLINE ;
expression-statement = expression, statement-end ;
```

An annotation is valid only on a simple-name assignment target. Place
assignment targets cannot contain calls. An unpack target contains only names
and recursively parenthesized binding-target lists; it uses plain `=`, has no
annotation or leading `mut`, and must match one exact tuple shape. The
top-level comma distinguishes `left, right = pair` from an expression.
Parentheses group or nest an unpack target. One-line suites are not supported.
The optional top-level comma in an assertion belongs to `assert-statement`;
tuple operands must be parenthesized.

## Conditional And Loop Statements

```ebnf
if-statement
    = "if", expression, ":", NEWLINE, suite,
      { "elif", expression, ":", NEWLINE, suite },
      [ "else", ":", NEWLINE, suite ] ;

while-statement
    = "while", expression, ":", NEWLINE, suite ;

for-statement
    = "for", loop-target, "in",
      [ "mut" | "own" ],
      expression, ":", NEWLINE, suite ;

loop-target = identifier | unpack-target ;
```

The loop target is one identifier or a recursively nested tuple unpack target.
Tuple leaves inherit the yielded element's ownership provenance. A tuple
target is rejected with `mut` iteration because the minimal tuple
surface has no recursive writeback. Loop `else` clauses are not supported.
For collection-place traversal, an absent modifier is shared iteration. Queue
and Range use their iterable-specific bare defaults instead: Queue receives
owned items, while Range yields independent copy `int64` values. Explicit
modifiers are rejected for Queue because it is a receive operation and for
Range because there is no place or ownership transfer to modify.

The iterable position also recognizes two compiler-known call shapes,
`enumerate(expression)` and `zip(expression, expression)`. They are not values
and have no production outside this position; static semantics reject either
name elsewhere, and a user declaration of the name shadows the loop form.
Explicit ownership modifiers are rejected for both, because they iterate over
the bare-loop shared default.

## `with` Statements

```ebnf
with-statement
    = "with", identifier, "=", expression,
      ":", NEWLINE, suite
    | "with", expression, "as", identifier,
      ":", NEWLINE, suite ;
```

The two forms are equivalent. Static semantics require a supported resource and a fresh binding.

## Patterns And Statement Matches

```ebnf
match-statement
    = "match", [ "mut" | "own" ],
      expression, ":", NEWLINE,
      INDENT, match-statement-arm,
      { match-statement-arm }, DEDENT ;

match-statement-arm
    = "case", pattern, [ "if", expression ],
      ":", NEWLINE, suite ;

pattern
    = closed-pattern, { "|", closed-pattern } ;

closed-pattern
    = "_"
    | BOOLEAN
    | STRING
    | FLOAT
    | INTEGER
    | "-", (INTEGER | FLOAT)
    | tuple-pattern
    | binding-pattern
    | variant-pattern ;

binding-pattern = IDENT ;

variant-pattern
    = identifier-path,
      [ "(", [ pattern, { ",", pattern } ], ")" ] ;

tuple-pattern
    = "(", pattern, ")"
    | "(", pattern, ",", ")"
    | "(", pattern, ",", pattern,
      { ",", pattern }, ")" ;
```

Pattern parsing uses these contextual rules:

- exact `_` is the wildcard
- one unparenthesized, unqualified name beginning with lowercase ASCII or `_` is a binding
- a dotted name, a capitalized name, or any name followed by parentheses is a variant pattern
- payload patterns are positional even when the variant declaration used named payload fields
- a parenthesized comma form is a fixed-arity recursive tuple pattern
- `|` has the lowest pattern precedence and joins alternatives
- parentheses group one pattern when no comma is present

Every or-pattern alternative must bind the same names with identical exact
types and capabilities. A guard is an ordinary expression checked as exactly
`bool`; its pattern bindings are in scope. A top-level binding is an
irrefutable catch-all when unguarded and must be the final arm. A guarded
top-level binding does not contribute to exhaustiveness. There are no ranges, collection
destructuring, rest patterns, named-payload patterns, duration patterns, or
f-string patterns.
`match mut` rejects a tuple pattern because mutable tuple
reconstruction/writeback is not part of the minimal surface. Statement match
arms always contain suites; `case pattern: statement` is not valid.

## Expressions And Precedence

From lowest to highest precedence:

| Level | Form | Associativity |
| --- | --- | --- |
| 1 | conditional expression | right |
| 2 | `or` | left |
| 3 | `and` | left |
| 4 | prefix `not` | right |
| 5 | `==`, `!=`, `<`, `<=`, `>`, `>=`, `in`, `not in` | chained left to right |
| 6 | `|` | left |
| 7 | `^` | left |
| 8 | `&` | left |
| 9 | `<<`, `>>` | left |
| 10 | `+`, `-` | left |
| 11 | `*`, `/`, `//`, `%` | left |
| 12 | prefix `match`, `try`, unary `-`, unary `~` | right/prefix |
| 13 | `**` | right |
| 14 | specialization, indexing, slicing, member access, call, numeric cast | left-to-right postfix chain |
| 15 | primary | — |

```ebnf
expression           = lambda-expression | non-tuple-expression ;
non-tuple-expression = conditional-expression ;

lambda-expression
    = "lambda", [ lambda-capture-list ],
      [ lambda-parameter,
      { ",", lambda-parameter } ], ":", expression ;

lambda-capture-list
    = "[", lambda-capture,
      { ",", lambda-capture }, "]" ;

lambda-capture
    = [ "mut" | "own" ], identifier ;

lambda-parameter
    = [ "mut" | "own" ], identifier ;

conditional-expression
    = or-expression,
      [ "if", or-expression, "else", conditional-expression ] ;

or-expression
    = and-expression, { "or", and-expression } ;

and-expression
    = not-expression, { "and", not-expression } ;

not-expression
    = { "not" }, comparison-expression ;

comparison-expression
    = bitwise-or-expression,
      { comparison-operator, bitwise-or-expression } ;

comparison-operator
    = "==" | "!=" | "<" | "<=" | ">" | ">=" | "in" | "not", "in" ;

bitwise-or-expression
    = bitwise-xor-expression, { "|", bitwise-xor-expression } ;

bitwise-xor-expression
    = bitwise-and-expression, { "^", bitwise-and-expression } ;

bitwise-and-expression
    = shift-expression, { "&", shift-expression } ;

shift-expression
    = additive-expression, { ("<<" | ">>"), additive-expression } ;

additive-expression
    = multiplicative-expression,
      { ("+" | "-"), multiplicative-expression } ;

multiplicative-expression
    = prefix-expression,
      { ("*" | "/" | "//" | "%"), prefix-expression } ;

prefix-expression
    = match-expression
    | "try", prefix-expression
    | "-", prefix-expression
    | "~", prefix-expression
    | power-expression ;

power-expression
    = postfix-expression, [ "**", prefix-expression ] ;

postfix-expression
    = primary-expression,
      { specialization-suffix
      | index-suffix
      | member-suffix
      | call-suffix
      | numeric-cast-suffix } ;

index-suffix
    = "[", expression, { ",", expression }, "]"
    | "[", [ expression ], ":", [ expression ], "]" ;
member-suffix = ".", identifier ;
call-suffix   = "(", [ argument, { ",", argument } ], ")" ;
argument      = [ identifier, "=" ], expression ;

numeric-cast-suffix = "as", numeric-type ;

numeric-type
    = "int" | "int8" | "int16" | "int32" | "int64" | "int128" | "intsize"
    | "uint8" | "uint16" | "uint32" | "uint64" | "uint128" | "uintsize"
    | "float32" | "float64" ;
```

Conditional expressions associate to the right, their condition is an
`or-expression`, and their two value arms may contain nested conditional
expressions through grouping or the recursive alternative arm. Arithmetic,
shift, bitwise, and Boolean chains are left-folded except for power, which
associates to the right. Power binds more tightly than a unary operator on its
left, while its right operand may begin with unary `-` or `~`. Equality,
ordering, and membership share the
one comparison level and chain the Python way rather than left-folding, so
`a < b <= c` is one chain of two links over three operands. A chain of `n`
operators means the conjunction of its `n` adjacent comparisons, with each
operand evaluated at most once. `not a == b` means `not (a == b)`, because
prefix `not` binds looser than the comparison level, while `a not in b` is one
comparison operator. Casts bind more tightly than power and arithmetic.

Comma-separated index expressions are accepted only for `Array[T]`, where
one `int64` coordinate is required per runtime axis. Other indexable
types retain one index expression.

The one-colon bracket forms are owned slices. Each endpoint is optional, so
`value[start:end]`, `value[:end]`, `value[start:]`, and `value[:]` all use the
second `index-suffix` alternative. On `Array[T]`, the range copies the first
axis. A second colon is reserved step syntax and is rejected with `AU2005`; it
is not part of the accepted grammar. A slice suffix is an expression only and
cannot be an assignment target.

## Primary Expressions And Literals

```ebnf
primary-expression
    = identifier
    | INTEGER
    | DURATION
    | FLOAT
    | BOOLEAN
    | STRING
    | FSTRING
    | parenthesized-expression
    | list-literal
    | brace-literal
    | list-comprehension
    | set-comprehension
    | dictionary-comprehension ;

list-literal
    = "[", [ expression, { ",", expression } ], "]" ;

brace-literal
    = "{", "}"
    | "{", expression, { ",", expression }, "}"
    | "{", expression, ":", expression,
      { ",", expression, ":", expression }, "}" ;

list-comprehension
    = "[", expression, comprehension-clauses, "]" ;

set-comprehension
    = "{", expression, comprehension-clauses, "}" ;

dictionary-comprehension
    = "{", expression, ":", expression,
      comprehension-clauses, "}" ;

comprehension-clauses
    = comprehension-for,
      { comprehension-if | comprehension-for } ;

comprehension-for
    = "for", loop-target, "in", comprehension-component ;

comprehension-if
    = "if", comprehension-component ;

comprehension-component
    = lambda-expression | or-expression ;

parenthesized-expression
    = "(", expression, ")"
    | tuple-expression ;

tuple-expression
    = "(", expression, ",", ")"
    | "(", expression, ",", expression,
      { ",", expression }, ")" ;
```

Lambda parameters receive their types from an expected structural function
type, whose result also constrains the body. A zero-parameter lambda may infer
its result from the body. The colon introduces one expression, not a suite.
Lambda parameters do not accept annotations, defaults, generics, or a trailing
comma. A capture list is exhaustive and nonempty; its entries name outer local
places and request shared, mutable, or by-value owned capture.

`(value)` is grouping and `(value,)` is a singleton tuple. Tuple value
expressions require parentheses; an unparenthesized comma is accepted only in
an unpack target. `()` and a trailing comma on a multi-element tuple are
rejected. A nonempty brace literal is a set when its first element is not
followed by `:`, otherwise it is a dictionary. `{}` is an empty dictionary.
An empty set uses the typed `set[T]()` constructor.

A comprehension has one or more `for` clauses. A clause may be followed by
zero or more `if` filters before another `for` clause. Clause targets use
`loop-target`, including recursive tuple targets, but the iterable position
has no `mut` or `own` modifier: comprehension clauses always use the bare-loop
contract. The non-conditional `or-expression` alternative keeps a following
comprehension `if` distinct from a conditional expression; use parentheses when
an iterable or filter itself needs a conditional expression. A lambda remains
syntactically admissible as a component and is then subject to the ordinary
iterable or exact-Boolean static rule. The result expression, or the dictionary key
and value expressions, may be any expression. A comma after comprehension
clauses, or a mixture of comma-separated literal entries and clauses, is
invalid. Generator expressions are not part of this grammar.

## Explicit Specialization

```ebnf
specialization-suffix = "[", type-list, "]" ;
```

Specialization and indexing use the same brackets, so parser and static
context disambiguate them. Brackets form specialization when their contents
scan as one or more type references and either:

1. `(` follows and the base is a name or member, or
2. `.` follows and the final target name begins with uppercase ASCII.

A bare bracket suffix is initially an index expression. Static resolution
reinterprets `function[Types]` as explicit specialization when `function`
resolves to a generic named function and the complete expression is used as a
function value. Otherwise the brackets remain indexing. Consequently,
`Box[int32](value)` and `Result[int32, str].Ok(1)` specialize,
`show[int32]` may produce one concrete function value, and `value[index]`
indexes.

A top-level colon inside the brackets selects slicing rather than
specialization or indexing. Slice endpoints are expressions and are checked
under the exact rules in
[Static Semantics](/manual/static-semantics#indexing-slicing-and-members).

## Match Expressions

```ebnf
match-expression
    = "match", [ "mut" | "own" ],
      expression, ":", NEWLINE,
      INDENT, match-expression-arm,
      { match-expression-arm }, DEDENT ;

match-expression-arm
    = "case", pattern, [ "if", expression ], ":",
      ( expression, match-expression-arm-end
      | NEWLINE, INDENT, expression, statement-end, DEDENT ) ;

match-expression-arm-end
    = NEWLINE | DEDENT | ")" | "]" | "}" | EOF ;
```

A match-expression arm contains exactly one expression, either inline after the colon or on one indented following line. It is not a general statement suite.

A complete match expression may appear in a return, initializer, call
argument, collection element, grouping expression, or other expression
position. When it appears inside a continued delimiter, its header and arms
form a layout island and retain their required layout tokens. The containing
delimiter may close after the final inline arm or on its own following line.

## Syntactic Complexity Limits

The implementation rejects source that exceeds the maintained parser complexity budget rather than risking host stack exhaustion:

- nested expressions, prefix forms, parentheses, types, patterns, and statements are limited to 128 parser levels
- binary-operator and postfix chains reject the 128th chained operation
- one comprehension rejects a 128th combined `for` clause or `if` filter
- f-string interpolation brace nesting is limited to 128

These are observable implementation limits of Aura 0.3. Inputs that exceed
them must be rejected cleanly.

## Syntax Not In Aura 0.3

The grammar intentionally excludes:

- semicolons and multiple statements on one physical line
- backslash line continuation
- multiline f-strings; multiline ordinary text uses triple quotes
- local item declarations, decorators, and attributes
- wildcard/aliased/relative import syntax
- ordinary trailing commas other than the required singleton-tuple comma
- collection, range, rest, and class patterns
- call-site capability annotations
- exception statements, `raise`, and `yield`
- generator expressions and generator functions

If a form is absent from this grammar, examples and books must not present it as implemented Aura.
