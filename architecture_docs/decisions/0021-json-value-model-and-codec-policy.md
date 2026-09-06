# ADR-0021: JSON value model and codec policy

> Approved next extension (2026-09-06): [ADR-0062](0062-typed-serialization-validation-and-schemas.md)
> records opt-in typed codecs, validation, and schemas. The raw JSON contract
> below remains the baseline. [ADR-0052](0052-anonymous-closed-union-types.md)
> owns the future optional-result migration across APIs.

- Status: Accepted
- Date: 2026-07-23
- Roadmap decision: Phase 3 JSON gap-fill policy

## Context

The Phase 3 roadmap fixes the public shape of a recursive `json.Value`, typed
parse failures, `json.parse`, `json.dumps`, and typed accessors. It deliberately
leaves several observable policies to a documented gap-fill decision: number
classification, object ordering and duplicate members, parse-error locations,
nesting and byte limits, exact formatting, ownership of accessors, and the
boundary between typed errors and runtime diagnostics.

Those choices affect protocol compatibility. A different number class can
change a successful match, a different duplicate-member rule can change data,
and a different key or whitespace policy can change signed payloads, golden
files, caches, and reproducible service messages. Resource limits and failure
categories must also agree across the MIR and direct backends.

## Accepted decision

- `json.Value` is the recursive enum `Null`, `Bool(bool)`, `Int(int64)`,
  `Float(float64)`, `String(String)`, `Array(Vec[json.Value])`, and
  `Object(Map[String, json.Value])`.
- `json.Error` is parse-only. Its variants are
  `Syntax(message: String, line: int32, column: int32)`,
  `NumberOutOfRange(line: int32, column: int32)`,
  `NestingTooDeep(limit: int32, line: int32, column: int32)`, and
  `InputTooLarge(actual_bytes: int64, limit_bytes: int64)`.
- The public functions are
  `json.parse(text: String) -> Result[json.Value, json.Error]` and
  `json.dumps(value: json.Value, indent: Option[int64] = None) -> String`.
  The ordinary non-copy parameter default means both inputs are shared
  borrows; neither call consumes its argument.
- `json.is_null`, `json.as_bool`, `json.as_int`, and `json.as_float` are
  module functions whose `value` parameter is a shared borrow. An `as_*`
  function succeeds only for the exactly named variant; it performs no numeric
  coercion. `json.into_string`, `json.into_array`, and `json.into_object` take
  `value: own json.Value` and return the owned payload only for the exactly
  named variant. Every wrong-variant accessor returns `Option.None`.
- Parsing accepts one strict RFC 8259 JSON value plus surrounding JSON
  whitespace and rejects comments, trailing commas, leading-zero integers,
  non-JSON escapes, `NaN`, and infinities. Non-whitespace after the first value
  is a syntax error.
- Parse-error lines and columns are one-based. A column counts Unicode scalar
  values from the start of its line rather than UTF-8 bytes.
  `NumberOutOfRange` points at the first scalar of the number token;
  `NestingTooDeep` points at the opening bracket or brace that exceeds the
  limit; `Syntax` points at the first unexpected scalar or, for unexpected end
  of input, the position immediately after the last scalar.
- Number classification uses the exact source number, not an already rounded
  binary64 approximation. A mathematical integer in the `int64` range becomes
  `Value.Int`, even when written with a fractional or exponent form. Thus
  `1.0`, `1e0`, `1.5e1`, and `-0.0` become integer values. Every other number
  whose IEEE-754 binary64 conversion is finite becomes `Value.Float`, with
  normal binary64 rounding and underflow; a source number whose conversion
  overflows returns `Error.NumberOutOfRange`.
- Object members are retained in source insertion order. When a key repeats,
  the last value wins while the key keeps the insertion slot established by
  its first occurrence. Duplicate comparison uses decoded String values, so
  differently escaped spellings of the same key are duplicates.
- Parse nesting counts containers only. A root scalar has depth zero and a
  root array or object has depth one. Depth 128 is accepted; the first
  container that would have depth 129 returns
  `Error.NestingTooDeep(limit=128, ...)`.
- Parse input is capped independently at 67,108,864 UTF-8 bytes. The exact
  boundary is accepted. A larger string returns
  `Error.InputTooLarge(actual_bytes, limit_bytes=67108864)` before syntax or
  nesting analysis.
- Parse construction and both runtime conversion directions share a fixed
  262,144-node materialization limit. The root and every scalar, array, or
  object value count; object keys do not count separately. The exact boundary
  is accepted. The next node traps with `AU4005` rather than becoming a
  `json.Error`, because structurally dense but valid input has exhausted a
  runtime resource budget rather than violated the JSON data grammar.
- `json.dumps` visits arrays in element order and emits object keys in
  lexicographically sorted UTF-8 order, independently of the map's insertion
  order. Integer variants have no decimal point. Finite float variants use the
  maintained shortest round-tripping binary64 spelling while preserving a
  floating marker for integral finite values and signed zero.
- JSON strings retain non-ASCII Unicode scalar values. The quotation mark,
  reverse solidus, and the standard backspace, tab, line-feed, form-feed, and
  carriage-return controls use their short JSON escapes. Remaining U+0000
  through U+001F controls use lowercase `\u00xx`; solidus is not escaped.
- `indent=None` selects compact output with no insignificant whitespace.
  `indent=Some(n)` accepts `0 <= n <= 16`, uses LF line endings, `n` ASCII
  spaces per container level, a single space after each object colon, compact
  `[]` and `{}` for empty containers, and no final newline. Every nonempty
  container places each element or member on its own line, places commas after
  every item except the last, and places the closing delimiter on its own line
  aligned with the container's opening level.
- Dump nesting follows the same container-depth rule and limit as parsing.
  Invalid indent or excessive nesting traps with `AU4003`. A non-finite
  `Value.Float` traps with `AU4001`.
- Dump output is capped independently at 67,108,864 UTF-8 bytes. The exact
  boundary is accepted. Exceeding the cap or failing allocation traps with
  `AU4005`; no partial string is returned.
- A codec-controlled allocation failure or a failure while constructing the
  parsed runtime tree traps with `AU4005`; it does not add an allocation variant
  to `json.Error`. Runtime-to-codec conversion and output allocation failures
  use the same code. Unrecoverable allocator failure in dependency-owned or host
  internals remains an external process condition rather than a promised
  catchable outcome.
- Parse data failures are `json.Error` values because callers can recover from
  untrusted input. Dump failures are diagnostics because the roadmap signature
  returns `String`, not `Result`; this ADR does not amend that signature.
- The MIR and direct backends use the same parser, serializer policy, typed
  variants, limits, ordering, output bytes, and diagnostic categories.
- Existing `json.is_valid`, `json.stringify_map`, and
  `json.parse_string_map` remain supported with their existing contracts.
  This addition does not silently change their acceptance or error behavior.
- Derived class/enum schemas and generated codecs remain deferred beyond Phase 6.
  Arbitrary-precision JSON numbers, streaming parse/dump APIs, and binary
  serialization formats remain outside Phase 3.

These choices were accepted at the Batch 3 entry checkpoint. The
compiler, both maintained backends, analysis service, fixtures, maintained
example, and executable Manual fence implement and pin this contract.

## Completion tests

- Parser tests pin strict syntax, source positions, trailing data, every
  variant, exact numeric classification around both `int64` boundaries,
  exponent and signed-zero cases, non-representable numbers, duplicate keys,
  insertion-slot retention, depth 128/129, both sides of the input cap, and the
  exact 262,144/262,145 materialized-node boundary.
- Serializer tests pin sorted nested keys, array order, escaping, compact
  output, indentation 0 and 16, empty containers, no final newline, integer
  versus float spelling, signed zero, non-finite rejection, depth 128/129, and
  both sides of the output cap. Runtime conversion tests pin the same node
  budget before dump emission.
- Static and fixture tests pin constructor, pattern, accessor parameter modes,
  ownership, default-argument, named-argument, and wrong-variant behavior.
- MIR and direct run-pass/run-fail coverage pins identical parsed values,
  serialized bytes, errors, traps, and source-visible diagnostics.
- Deterministic allocation-failure injection pins `AU4005` propagation through
  the shared, MIR, and direct parse adapters without consuming the borrowed
  input.
- Analysis and language-server tests pin module, enum, variant, function,
  method, parameter, and return-type completion and hover contracts, including
  canonical identity through imports.
- Legacy JSON string-map tests remain green, and the Manual's executable
  example is hash-pinned and run by the reference-integrity gate.
