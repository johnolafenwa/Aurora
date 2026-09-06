use std::collections::BTreeSet;

use super::{
    AssertionOperand, Diagnostic, DiagnosticSeverity, RuntimeCallFrame, RuntimeSourceSpan,
    RuntimeTaskFrame, Span, DIAGNOSTIC_CODE_REGISTRY,
};

#[test]
fn renders_annotated_diagnostics_with_source_context() {
    let diagnostic = Diagnostic::at(Span::new(2, 9), "unknown name `value`");
    let rendered =
        diagnostic.render_with_source("examples/demo.au", "def main():\n    print(value)\n");

    assert!(rendered.contains("error[AU2001]: unknown name `value`"));
    assert!(rendered.contains("--> examples/demo.au:2:9"));
    assert!(rendered.contains("2 |     print(value)"));
    assert!(rendered.contains("|         ^"));
}

#[test]
fn renders_unspanned_and_out_of_range_diagnostics() {
    let plain = Diagnostic::new("plain failure");
    assert_eq!(plain.to_string(), "plain failure");
    assert_eq!(
        plain.render_with_source("examples/demo.au", "def main():\n    pass\n"),
        "error[AU2999]: plain failure\n --> examples/demo.au"
    );

    let out_of_range = Diagnostic::at(Span::new(4, 3), "missing line");
    let rendered = out_of_range.render_with_source("examples/demo.au", "def main():\n");
    assert!(rendered.contains("error[AU2999]: missing line"));
    assert!(rendered.contains("--> examples/demo.au:4:3"));
    assert_eq!(out_of_range.to_string(), "4:3: missing line");
}

#[test]
fn structured_diagnostics_preserve_codes_labels_help_and_edits() {
    let diagnostic = Diagnostic::coded_at("AU3001", Span::new(4, 11), "use of moved value `item`")
        .with_secondary(Span::new(2, 18), "value moved here")
        .with_note("non-copy values have one owner")
        .with_help("pass shared access or clone the value")
        .with_edit(Span::new(2, 11), Span::new(2, 11), ".clone()");

    let report = diagnostic.structured("examples/move.au");
    assert_eq!(report.code, "AU3001");
    assert_eq!(report.severity, DiagnosticSeverity::Error);
    assert_eq!(report.message, "use of moved value `item`");
    assert_eq!(report.primary_span.unwrap().path, "examples/move.au");
    assert_eq!(
        report.secondary_spans[0].label.as_deref(),
        Some("value moved here")
    );
    assert_eq!(report.notes, ["non-copy values have one owner"]);
    assert_eq!(report.help, ["pass shared access or clone the value"]);
    assert_eq!(report.edits[0].replacement, ".clone()");
    assert_eq!(report.edits[0].applicability, "machine-applicable");

    let json = serde_json::to_value(diagnostic.structured("examples/move.au"))
        .expect("structured diagnostic should serialize");
    assert_eq!(json["code"], "AU3001");
    assert_eq!(json["severity"], "error");
    assert_eq!(json["primary_span"]["start"]["line"], 4);
    assert_eq!(json["secondary_spans"][0]["label"], "value moved here");
    assert_eq!(json["edits"][0]["replacement"], ".clone()");

    let rendered = diagnostic.render_with_source(
        "examples/move.au",
        "def main():\n    take(item)\n    pass\n    print(item)\n",
    );
    assert!(rendered.contains("[AU3001]"));
    assert!(rendered.contains("value moved here"));
    assert!(rendered.contains("note: non-copy values have one owner"));
    assert!(rendered.contains("help: pass shared access or clone the value"));
    assert!(rendered.contains("fix: replace examples/move.au:2:11-2:11 with `.clone()`"));
}

#[test]
fn assertion_operands_are_typed_structured_fields_and_human_notes() {
    let plain = Diagnostic::coded("AU4001", "assertion failed");
    let plain_json = serde_json::to_value(plain.structured("examples/assert.au"))
        .expect("plain diagnostic should serialize");
    assert!(
        plain_json.get("assertion_operands").is_none(),
        "diagnostics without captures must not grow an empty wire field"
    );

    let diagnostic = plain
        .with_assertion_operand("left", "int64", "41")
        .with_assertion_operand("right", "int64", "42");
    assert_eq!(
        diagnostic.assertion_operands,
        [
            AssertionOperand {
                label: "left".to_string(),
                r#type: "int64".to_string(),
                value: "41".to_string(),
                truncated: false,
            },
            AssertionOperand {
                label: "right".to_string(),
                r#type: "int64".to_string(),
                value: "42".to_string(),
                truncated: false,
            },
        ]
    );

    let json = serde_json::to_value(diagnostic.structured("examples/assert.au"))
        .expect("captured assertion diagnostic should serialize");
    assert_eq!(json["assertion_operands"][0]["label"], "left");
    assert_eq!(json["assertion_operands"][0]["type"], "int64");
    assert_eq!(json["assertion_operands"][0]["value"], "41");
    assert_eq!(json["assertion_operands"][0]["truncated"], false);

    let rendered = diagnostic.render_with_source("examples/assert.au", "assert 41 == 42\n");
    assert!(rendered.contains("note: left = 41"));
    assert!(rendered.contains("note: right = 42"));
    assert!(rendered.find("left = 41") < rendered.find("right = 42"));

    let membership = Diagnostic::coded("AU4001", "assertion failed")
        .with_assertion_operand("item", "str", "needle")
        .with_assertion_operand("collection", "list[str]", "[haystack]")
        .render_with_source("examples/assert.au", "assert item in values\n");
    assert!(membership.contains("note: item = needle"));
    assert!(membership.contains("note: collection = [haystack]"));
}

#[test]
fn assertion_operand_values_are_bounded_to_4096_utf8_bytes() {
    let exact = AssertionOperand::bounded("left", "str", "a".repeat(4_096));
    assert_eq!(exact.value.len(), 4_096);
    assert!(!exact.truncated);

    let long_ascii = AssertionOperand::bounded("right", "str", "b".repeat(4_097));
    assert_eq!(long_ascii.value.len(), 4_096);
    assert!(long_ascii.value.ends_with("... (truncated)"));
    assert!(long_ascii.truncated);

    let long_unicode = AssertionOperand::bounded("collection", "str", "é".repeat(2_049));
    assert!(long_unicode.value.len() <= 4_096);
    assert!(long_unicode
        .value
        .is_char_boundary(long_unicode.value.len() - "... (truncated)".len()));
    assert!(long_unicode.value.ends_with("... (truncated)"));
    assert!(long_unicode.truncated);
}

#[test]
fn uncoded_constructors_assign_stable_phase_banded_codes() {
    assert_eq!(Diagnostic::new("unexpected character `@`").code, "AU1001");
    assert_eq!(
        Diagnostic::new("invalid escape sequence `\\q`").code,
        "AU1001"
    );
    assert_eq!(
        Diagnostic::new("expected expression, found end of file").code,
        "AU1101"
    );
    assert_eq!(Diagnostic::new("unknown name `value`").code, "AU2001");
    assert_eq!(
        Diagnostic::new("python-style implicit imports are not supported").code,
        "AU2005"
    );
    assert_eq!(Diagnostic::new("use of moved value `value`").code, "AU3001");
    assert_eq!(
        Diagnostic::new("integer overflow in addition").code,
        "AU4002"
    );
    assert_eq!(
        Diagnostic::new("integer value `2147483648` does not fit in `int32`").code,
        "AU4002"
    );
    assert_eq!(
        Diagnostic::new("maximum call depth of 256 exceeded while calling `loop`").code,
        "AU4001"
    );
    assert_eq!(
        Diagnostic::new("`main` must return `int32` or `None` in the bootstrap runtime").code,
        "AU2999",
        "compile-time checks must not enter the runtime band merely because their message mentions the runtime"
    );
}

#[test]
fn diagnostic_code_registry_is_unique_banded_and_append_only_shaped() {
    let mut codes = BTreeSet::new();
    for entry in DIAGNOSTIC_CODE_REGISTRY {
        assert!(codes.insert(entry.code), "duplicate code {}", entry.code);
        assert_eq!(entry.code.len(), 6);
        assert!(entry.code.starts_with("AU"));
        assert!(entry.code[2..].chars().all(|ch| ch.is_ascii_digit()));
        assert!(matches!(
            entry.band,
            "lexical" | "parse" | "names/types" | "ownership" | "runtime"
        ));
        assert!(!entry.title.is_empty());
    }
    assert!(
        DIAGNOSTIC_CODE_REGISTRY
            .iter()
            .any(|entry| entry.code == "AU2008"
                && entry.band == "names/types"
                && entry.title == "equality unavailable"),
        "equality-obligation rejections must retain their dedicated public registry entry"
    );
    assert!(
        DIAGNOSTIC_CODE_REGISTRY
            .iter()
            .any(|entry| entry.code == "AU4007"
                && entry.band == "runtime"
                && entry.title == "array shape or reduction violation"),
        "array shape failures must retain their dedicated public registry entry"
    );
}

#[test]
fn runtime_boundary_normalization_keeps_runtime_diagnostics_in_the_runtime_band() {
    let generic = Diagnostic::new("unsupported runtime operation").into_runtime_trap();
    assert_eq!(generic.code, "AU4001");

    let misleading = Diagnostic::new("unknown MIR place `temporary`").into_runtime_trap();
    assert_eq!(misleading.code, "AU4001");

    let precise = Diagnostic::coded_at(
        "AU4003",
        Span::new(3, 8),
        "map key `missing` was not present",
    )
    .into_runtime_trap();
    assert_eq!(precise.code, "AU4003");
    assert_eq!(precise.span, Some(Span::new(3, 8)));
}

#[test]
fn structured_diagnostics_always_serialize_typed_runtime_frame_arrays() {
    let diagnostic = Diagnostic::coded_at("AU4003", Span::new(3, 18), "out of bounds");
    let mut json = serde_json::to_value(diagnostic.structured("/workspace/main.au"))
        .expect("structured runtime diagnostic should serialize");

    assert_eq!(json["call_frames"], serde_json::json!([]));
    assert_eq!(json["task_ancestry"], serde_json::json!([]));

    let object = json
        .as_object_mut()
        .expect("structured diagnostic should serialize as an object");
    object.remove("call_frames");
    object.remove("task_ancestry");
    let legacy: super::StructuredDiagnostic = serde_json::from_value(json)
        .expect("records from before structured runtime frames should remain readable");
    assert!(legacy.call_frames.is_empty());
    assert!(legacy.task_ancestry.is_empty());
}

#[test]
fn runtime_frames_capture_once_clone_and_render_without_polluting_notes() {
    let mut diagnostic = Diagnostic::coded_at("AU4003", Span::new(3, 18), "out of bounds")
        .with_note("semantic note")
        .with_help("check the collection length")
        .with_edit(Span::new(3, 18), Span::new(3, 19), "0");
    let first_call_frames = vec![
        RuntimeCallFrame {
            function: "child".to_string(),
            span: RuntimeSourceSpan::point(
                Some("/workspace/worker.au".to_string()),
                Span::new(1, 1),
            ),
        },
        RuntimeCallFrame {
            function: "main".to_string(),
            span: RuntimeSourceSpan::point(Some("/workspace/main.au".to_string()), Span::new(6, 1)),
        },
    ];
    let first_ancestry = vec![RuntimeTaskFrame {
        task_function: "child".to_string(),
        task_entry_span: RuntimeSourceSpan::point(
            Some("/workspace/worker.au".to_string()),
            Span::new(1, 1),
        ),
        parent_function: "main".to_string(),
        spawn_span: RuntimeSourceSpan::point(
            Some("/workspace/main.au".to_string()),
            Span::new(8, 15),
        ),
    }];
    assert!(
        diagnostic.capture_runtime_frames_once(first_call_frames.clone(), first_ancestry.clone())
    );
    assert!(!diagnostic.capture_runtime_frames_once(
        vec![RuntimeCallFrame {
            function: "observer".to_string(),
            span: RuntimeSourceSpan::point(None, Span::new(99, 1)),
        }],
        Vec::new(),
    ));
    assert_eq!(diagnostic.call_frames, first_call_frames);
    assert_eq!(diagnostic.task_ancestry, first_ancestry);
    assert_eq!(diagnostic.notes, ["semantic note"]);
    assert_eq!(diagnostic.clone(), diagnostic);

    let structured = diagnostic.structured("/fallback.au");
    assert_eq!(structured.call_frames[0].span.path, "/workspace/worker.au");
    assert_eq!(
        structured.task_ancestry[0].spawn_span.path,
        "/workspace/main.au"
    );
    let json = serde_json::to_value(&structured).expect("typed frames should serialize");
    assert_eq!(json["call_frames"][0]["function"], "child");
    assert_eq!(
        json["task_ancestry"][0]["task_entry_span"]["end"]["column"],
        2
    );
    assert_eq!(json["notes"], serde_json::json!(["semantic note"]));
    let round_trip: super::StructuredDiagnostic = serde_json::from_value(json)
        .expect("the compiler-owned structured diagnostic wire record should round trip");
    assert_eq!(round_trip, structured);

    let rendered = diagnostic.render_with_source("/fallback.au", "pass\n");
    let semantic_note = rendered
        .find("note: semantic note")
        .expect("semantic note should render");
    let call_chain = rendered
        .find("note: Aura call chain (innermost first): child at 1:1 -> main at 6:1")
        .expect("call chain should be synthesized");
    let task_entry = rendered
        .find("note: Aura task entry: child at 1:1")
        .expect("task entry should be synthesized");
    let ancestry = rendered
        .find("note: Aura task ancestry (youngest first): child spawned from main at 8:15")
        .expect("task ancestry should be synthesized");
    let help = rendered
        .find("help: check the collection length")
        .expect("help should render");
    let edit = rendered
        .find("fix: replace /fallback.au:3:18-3:19 with `0`")
        .expect("edit should render");
    assert!(
        semantic_note < call_chain
            && call_chain < task_entry
            && task_entry < ancestry
            && ancestry < help
            && help < edit
    );
    assert_eq!(rendered.matches("Aura call chain").count(), 1);
}

#[test]
fn structured_runtime_frames_use_the_report_path_when_source_paths_are_absent() {
    let mut diagnostic = Diagnostic::coded("AU4001", "runtime trap");
    diagnostic.capture_runtime_frames_once(
        vec![RuntimeCallFrame {
            function: "worker".to_string(),
            span: RuntimeSourceSpan::point(None, Span::new(4, 9)),
        }],
        vec![RuntimeTaskFrame {
            task_function: "worker".to_string(),
            task_entry_span: RuntimeSourceSpan::point(None, Span::new(4, 1)),
            parent_function: "main".to_string(),
            spawn_span: RuntimeSourceSpan::point(None, Span::new(8, 17)),
        }],
    );

    let structured = diagnostic.structured("/workspace/main.au");
    assert_eq!(structured.call_frames[0].span.path, "/workspace/main.au");
    assert_eq!(
        structured.task_ancestry[0].task_entry_span.path,
        "/workspace/main.au"
    );
    assert_eq!(
        structured.task_ancestry[0].spawn_span.path,
        "/workspace/main.au"
    );
}

#[test]
fn an_empty_runtime_frame_snapshot_is_still_complete() {
    let uncaptured = Diagnostic::coded("AU4006", "runtime configuration failed");
    let mut diagnostic = uncaptured.clone();
    assert!(diagnostic.capture_runtime_frames_once(Vec::new(), Vec::new()));
    assert_ne!(
        diagnostic, uncaptured,
        "the private capture marker participates in equality because an empty completed snapshot \
         must not be confused with an uncaptured diagnostic during propagation"
    );
    assert_eq!(diagnostic.clone(), diagnostic);
    assert!(!diagnostic.capture_runtime_frames_once(
        vec![RuntimeCallFrame {
            function: "late observer".to_string(),
            span: RuntimeSourceSpan::point(None, Span::new(1, 1)),
        }],
        Vec::new(),
    ));
    assert!(diagnostic.call_frames.is_empty());
    assert!(diagnostic.task_ancestry.is_empty());
}
