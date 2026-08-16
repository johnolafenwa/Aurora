# Aura Tutorials

This directory is the beginning of the Aura tutorial track: a book-style set of Markdown chapters that explains the language as it exists in the repository today.

These tutorials are intentionally scoped to the implemented Aura surface. They stay in sync with the compiler, examples, and normative Manual.

## Maintenance Rule

When the implemented language surface changes, update these in the same pass:

1. the relevant tutorial chapter
2. the relevant example program under `examples/`
3. any CLI or tooling docs that reference the changed behavior
4. `14-current-language-surface.md` if the supported surface changed

## Reading Order

1. [00-overview.md](00-overview.md)
2. [01-running-programs.md](01-running-programs.md)
3. [02-bindings-and-types.md](02-bindings-and-types.md)
4. [03-functions.md](03-functions.md)
5. [04-control-flow.md](04-control-flow.md)
6. [05-classes-and-data.md](05-classes-and-data.md)
7. [06-ownership-and-borrowing.md](06-ownership-and-borrowing.md)
8. [07-strings-and-numbers.md](07-strings-and-numbers.md)
9. [08-tooling.md](08-tooling.md)
10. [09-enums-and-match.md](09-enums-and-match.md)
11. [10-results-and-options.md](10-results-and-options.md)
12. [11-resource-management.md](11-resource-management.md)
13. [12-error-propagation.md](12-error-propagation.md)
14. [13-concurrency.md](13-concurrency.md)
15. [14-current-language-surface.md](14-current-language-surface.md)
16. [15-generics.md](15-generics.md)
17. [16-traits.md](16-traits.md)
18. [17-modules-and-visibility.md](17-modules-and-visibility.md)
19. [18-packages-and-workspaces.md](18-packages-and-workspaces.md)
20. [19-io-and-networking.md](19-io-and-networking.md)
21. [20-randomness.md](20-randomness.md)
22. [21-json.md](21-json.md)
23. [22-bytes.md](22-bytes.md)
24. [23-assertions-and-tests.md](23-assertions-and-tests.md)
25. [24-multiline-expressions.md](24-multiline-expressions.md)
26. [25-tuples.md](25-tuples.md)
27. [26-ffi.md](26-ffi.md)

## Scope Today

The current tutorial set covers:

- scripts and `main`
- bindings, mutability, and type annotations
- functions with explicit and omitted `None` return types
- capture-free named function values with `def(T1, mut T2, own T3) -> R`
  types, copy and `Transfer` semantics, indirect calls, storage, and task
  targets
- contextually typed expression lambdas with by-value Copy/non-Copy capture,
  exhaustive shared/mutable/owned capture lists, repeatable shared/mutable
  calls, consuming single-use calls, and structural Transfer
- classes with fields, default values, receiver forms, mutating methods, and `public` field syntax
- ownership, declaration-stable parameter defaults, explicit `own`, move
  semantics, copy types, place-based shared/mutable views, returned views,
  reborrowing, inferred loan lifetimes, and exclusivity
- owned `list[T]`, `dict[K, V]`, and `set[T]` collections with literals,
  storing APIs, bare-shared/`own` iteration, mutable list iteration, stable
  sorting, eager callback-powered `map`/`filter`, and eager owned list/set/dictionary
  comprehensions with filters and nested clauses, plus owned list/str slices
  with omitted endpoints, negative normalization, loud bounds, and
  Unicode-scalar str positions
- owned contiguous numeric `Array[T]` values for the four maintained dtypes,
  with exact shapes, row-major multidimensional indexing, first-axis copy
  slices, same-dtype arithmetic, explicit wrapping/saturating integer modes,
  mapping, and deterministic reductions
- enums with exhaustive `match`
- user-defined generic classes, enums, and functions
- trait declarations, trait impls, and bounded generic calls
- local file modules with `import`, `from ... import ...`, and `public` visibility at module boundaries
- `Aura.toml` packages with `src/`, local path dependencies, git dependencies, workspaces, and local lockfiles
- package-authorized FFI v0 with bodyless `extern "C"` declarations,
  fixed-width scalars, pointer-length str/byte views, opaque handles, and
  exact root dependency reports
- built-in `Result[T, E]`, `Option[T]`, `SendError[T]`, and bare `None`
- `try expr`
- conditional expressions such as `value if condition else alternative`, with
  exact-`bool` conditions and lazy selection of one arm
- `in` and `not in` over `list`, `set`, `dict` keys, and `str` substrings
- Python-style chained comparisons such as `low <= value < high`, which
  evaluate each operand once and short-circuit
- the `for ... in enumerate(seq):` and `for ... in zip(first, second):` loop
  forms, where `zip` stops at the shorter sequence
- the builtin functions `len(value)`, delegating to the value's own `len()`,
  and `str(value)`, producing the print rendering; `str.len()`,
  `str.byte_len()`, `list.len()`, `dict.len()`, and `set.len()` all return
  `int64`, matching range bounds, list indexes, slice endpoints, enumeration
  positions, and Array coordinates
- `with` using `close(mut self)` and `with TaskGroup() as group:`
- builtin `io`, `fs`, `net`, and `process` modules with scheduler-aware file I/O, maintained networking resource types, and shell-free subprocess helpers
- `Queue[T]()`, `Task[T].result()`, `TaskGroup()`, its ordinary and
  explicit-stack start methods, typed `select(...)` over Queue, Task, and
  relative-Duration sources, `wait_any(...)`, `wait_all(...)`, send-result
  errors, structural `Transfer` boundaries, single-consumer task results, and
  cooperative cancellation
- arithmetic including decimal/hexadecimal/binary/octal integer literals,
  fixed-width bitwise and shift operations, checked power, ties-to-even
  `round`, paired `divmod`, explicit floor division, integer-to-float
  conversion, and computed signed Duration values; strings, string
  parsing/formatting, booleans, and comparisons
- deterministic seeded randomness, unbiased ranges, mutable-list shuffle, and
  the separate OS-secure integer/byte boundary
- `control.retry` for eager `Result` workers with an attempt budget and
  exponential `Duration` backoff
- recursive `json.Value` trees, typed parse errors, exact accessors, consuming
  payload extraction, and deterministic compact or pretty dumping
- `list[uint8]` bytes, strict UTF-8 conversion, canonical hex/base64 codecs,
  typed malformed-input errors, and raw SHA-256
- `assert condition` and `assert condition, message`, with lazy messages,
  source-located `AU4001` failures, and file-level `aura test` behavior
- delimiter-based newline continuation inside `()`, `[]`, and `{}`, including
  multiline signatures, calls, grouping, indexing, and collection literals;
  ordinary trailing commas, backslash continuation, and multiline f-strings
  remain unavailable (singleton tuples require their one comma)
- fixed structural tuples with parenthesized value/type syntax, recursive
  assignment/loop unpacking and patterns, whole-source move behavior, and
  copy-only constant indexing; same-type recursive `==` and `!=` retain both
  operands, while tuple ordering remains unavailable
- `if`, `elif`, `else`, `for`, `while`, `match`, `break`, and `continue`
- `print`
- CLI inspection commands such as `check`, `ast`, `ast-json`, `analyze`, `complete`, and `mir`
- compiler-backed VS Code diagnostics, navigation, and completions

Use the normative [Language Specification](../docs/manual/language-specification.md) and [Manual](../docs/manual/index.md) as the exhaustive truth source. `14-current-language-surface.md` is a compact tutorial recap; the earlier chapters should explain the maintained surface progressively.

It does not yet attempt to teach features that are still only in the proposal.
