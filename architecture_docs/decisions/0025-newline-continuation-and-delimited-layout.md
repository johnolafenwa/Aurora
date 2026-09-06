# ADR-0025: Newline continuation and delimited layout

> Approved next extension (2026-09-06): [ADR-0063](0063-everyday-syntax-and-pattern-ergonomics.md)
> records consistent trailing commas and parenthesized imports. Detailed
> grammar must preserve grouping, indentation, and evaluation rules.

- Status: Accepted
- Date: 2026-07-24
- Roadmap decision: Phase 3.5 newline continuation

## Context

Aurora is indentation-sensitive. Before Phase 3.5, every ordinary nonblank
physical line ended a logical line and participated in block indentation, even
when `(`, `[`, or `{` remained open. Calls, signatures, type arguments,
grouping, indexing, and collection literals therefore had to stay on one
physical line.

Aurora already accepts an expression-form `match` inside another expression.
Its header and `case` arms require real layout tokens. Suppressing every
newline at nonzero delimiter depth would therefore regress an implemented
construct. The language also needs one precise policy for delimiter pairing,
continuation indentation, comments, trailing commas, backslashes, strings, and
physical source locations.

## Accepted Decision

- While at least one source `(`, `[`, or `{` remains open, an ordinary
  physical newline is lexical whitespace. It emits no `NEWLINE`, `INDENT`, or
  `DEDENT`.
- Source delimiters nest and close in last-opened, first-closed order. A closer
  must match the kind of the most recently opened delimiter.
- Leading spaces on an ordinary continuation line are formatting only. They
  neither consult nor modify the surrounding block-indentation stack. A
  physical tab remains `AU1001`, including when used for continuation
  indentation.
- Blank and comment-only physical lines remain ignored. A trailing comment may
  end a continued physical line while a delimiter remains open.
- The physical newline after the outermost delimiter closes is a logical
  newline and resumes normal indentation processing.
- An expression-form `match` nested in a delimiter creates a layout island.
  Its header, `case` arms, and optional indented arm expressions retain the
  layout tokens required by the match grammar. Outside that island, the
  enclosing delimiter continues suppressing ordinary layout. The containing
  delimiter may close after the final inline arm or on its own following line.
- This extension changes token boundaries only. Tokens retain their physical
  line and column. Typing, ownership, source-order evaluation, and runtime
  behavior are the same as for the equivalent one-line token sequence.
- Existing comma-separated forms still reject trailing commas. This decision
  does not pre-accept tuple syntax.
- Backslash continuation remains unsupported. Ordinary strings and f-strings
  remain single-line. Delimiters inside either token do not affect the source
  delimiter stack, and an f-string interpolation cannot cross a physical
  newline.
- An unexpected closer, a mismatched closer, or end of file with an open
  delimiter is `AU1001`. A mismatch reports at the closer and labels the
  corresponding opener. An unclosed delimiter reports at end of file and
  likewise labels its opener.
- The maintained parser nesting and expression-chain limit remains 128.

These choices were accepted at the Batch 3 entry checkpoint.

## Completion Evidence

- Lexer tests pin all three delimiter kinds, mixed nesting, ignored
  continuation indentation, comments and blank lines, physical token spans,
  tabs, strings/f-strings, layout islands, and exact delimiter diagnostics.
- Parser tests and parse/run fixtures pin multiline declarations and
  expressions while retaining the trailing-comma, backslash, and multiline
  f-string restrictions.
- `examples/basics/multiline_expressions.au` and the corresponding maintained
  example smoke test pin observable execution.
- Language-server and extension tests pin physical source ranges, related
  delimiter information, completion inside continuations, and editor
  indentation behavior.
- The Manual, Current Limits, conformance map, executable reference gate, and
  tutorial track document the same boundary.
