# ADR-0062: Typed serialization, validation, and schemas

- Status: Accepted direction; detailed design pending
- Date: 2026-09-06
- Implementation: Not started
- Roadmap: Batch 6
- Extends: ADR-0021
- Related: ADR-0052, ADR-0053, ADR-0056, ADR-0058, and ADR-0059

## Authority and current boundary

The user approved typed class/enum serialization, validation, schema
generation, and field/parameter metadata in the
[roadmap](../14-priority-roadmap.md). The current JSON value/codec contract in
ADR-0021 does not derive schemas or typed user-object decoders.

## Accepted decisions

Classes and enums explicitly opt into codec/schema generation at compile
time. A runtime decorator name or prose docstring alone does not enable it.
Generated code must preserve static typing, ownership, and deterministic
cleanup; no runtime reflection system or garbage collector is implied.

Validation failures identify the relevant field path, including nested
collection positions. Define required fields, optional values, declaration
defaults, unknown fields, and validation errors as observable contracts.
Descriptions and typed metadata on fields/parameters supply a common source
for editor help, API documentation, validators, and agent-tool schemas.

Schema output and decoding must agree about the accepted input. A successful
decoder returns a fully initialized valid value and does not bypass custom
initialization/private invariants. Parsing or validation failure cleans up
partially created values exactly once. Serialization must define its ownership
requirements and cannot consume or clone a field implicitly to hide an error.

## Remaining detailed design

- Opt-in syntax, trait/generic interfaces, metadata attachment, schema format,
  naming/version policy, and generated-symbol visibility.
- Required/default/unknown-field choices, null versus absent fields, optional
  payloads containing `None`, enum tags, numeric conversions, and validation
  ordering/aggregation.
- Interaction with factories and initialization; field visibility; recursive
  types, unions, resources, borrowed data, and unsupported field diagnostics.
- Codec error types and whether decoders compose parse/validation errors;
  resource-limit policy under the existing JSON boundary.
- Tool registration schemas, parameter metadata, and matching decorator
  signatures without adding runtime annotation reflection.

No specific choice for rejecting/ignoring unknown fields, applying defaults,
or coercing input was selected by the high-level approval. Make those choices
explicit in the detailed design before implementation.

## Completion evidence required

Pin opt-in and opt-out behavior, class/enum round trips, nested field paths,
defaults, missing/unknown/null fields, validation failures, constructor
invariants, cleanup, schema/decoder agreement, imports, and metadata visibility.
Both backends, editor metadata, and generated reference examples must share
the same signatures and results. Existing raw JSON APIs retain their current
contract until an explicit implementation amendment changes it.
