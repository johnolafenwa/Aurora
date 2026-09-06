# ADR-0063: Everyday syntax and pattern ergonomics

- Status: Accepted roadmap direction; detailed design pending
- Date: 2026-09-06
- Implementation: Not started
- Roadmap: Batch 4
- Extends: ADR-0025, ADR-0026, ADR-0049, and ADR-0051
- Related: ADR-0044, ADR-0046, ADR-0052, ADR-0059, and ADR-0061

## Authority and current boundary

The user approved the everyday-syntax batch in the
[roadmap](../14-priority-roadmap.md): consistent trailing commas,
parenthesized imports, richer unpacking, class patterns, and focused text/byte
conveniences. This records direction; exact grammar and library additions
were not individually selected. Existing tuple, import, and match limits
remain implemented behavior until their coordinated replacement lands.

## Accepted direction

Make multiline calls, collections, tuple forms, and imports consistent and
easy to edit. Preserve deterministic expression evaluation and the distinction
between grouping and tuple construction.

Extend unpacking and patterns to practical sequence/rest and class use cases.
Preserve source ownership, exact matching semantics, private-field boundaries,
and lifetime rules. Class patterns require their own specified access model;
the approval does not decide positional exposure or hidden property execution.

Select text and byte conveniences from representative parsers, agent tools,
and data-processing examples. Keep Unicode units, copying, indexing, slicing,
and typed failures explicit in each API's contract. Do not infer approval for
every missing Python builtin from this umbrella batch.

## Remaining detailed design and evidence

Specify the grammar positions admitting trailing commas, parenthesized import
layout, unpacking/rest result types and ownership, match evaluation/coverage,
and class visibility/property behavior. ADR-0049's class-pattern deferment is
now scheduled here but its implemented contract is unchanged.

Define the exact text/byte API list and signatures before coding. Fixtures
must pin accepted grammar, ambiguous forms, evaluation counts/order, Copy and
move unpacking, rest allocation/loans, failed-pattern cleanup, and class
visibility. Keep formatter, LSP, grammar reference, examples, and backend
parity aligned in each implemented feature family.
