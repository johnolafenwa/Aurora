use std::error::Error;
use std::fmt;
use std::ops::{Deref, DerefMut};

use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Span {
    pub line: usize,
    pub column: usize,
}

impl Span {
    pub fn new(line: usize, column: usize) -> Self {
        Self { line, column }
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.column)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub code: String,
    pub severity: DiagnosticSeverity,
    pub message: String,
    pub span: Option<Span>,
    details: Box<DiagnosticDetails>,
}

/// Supplemental diagnostic data kept behind one pointer so `Diagnostic`
/// remains a compact error value when returned through `Result`.
///
/// The fields stay public because existing compiler and tooling consumers
/// access them directly through `Diagnostic`'s `Deref` implementation.
#[doc(hidden)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticDetails {
    pub secondary_spans: Vec<LabeledSpan>,
    pub notes: Vec<String>,
    pub assertion_operands: Vec<AssertionOperand>,
    pub help: Vec<String>,
    pub edits: Vec<DiagnosticEdit>,
    pub call_frames: Vec<RuntimeCallFrame>,
    pub task_ancestry: Vec<RuntimeTaskFrame>,
    pub render_path: Option<String>,
    pub render_source: Option<String>,
    pub partial_stdout: Option<String>,
    runtime_frames_captured: bool,
}

impl Deref for Diagnostic {
    type Target = DiagnosticDetails;

    fn deref(&self) -> &Self::Target {
        &self.details
    }
}

impl DerefMut for Diagnostic {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.details
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticCodeInfo {
    pub code: &'static str,
    pub band: &'static str,
    pub title: &'static str,
}

/// Public, append-only diagnostic-code registry. Never reuse or renumber an
/// entry; obsolete diagnostics keep their reserved code.
pub const DIAGNOSTIC_CODE_REGISTRY: &[DiagnosticCodeInfo] = &[
    DiagnosticCodeInfo {
        code: "AU1001",
        band: "lexical",
        title: "invalid lexical input",
    },
    DiagnosticCodeInfo {
        code: "AU1002",
        band: "lexical",
        title: "invalid f-string delimiter",
    },
    DiagnosticCodeInfo {
        code: "AU1101",
        band: "parse",
        title: "invalid syntax",
    },
    DiagnosticCodeInfo {
        code: "AU2001",
        band: "names/types",
        title: "name resolution",
    },
    DiagnosticCodeInfo {
        code: "AU2002",
        band: "names/types",
        title: "type mismatch",
    },
    DiagnosticCodeInfo {
        code: "AU2003",
        band: "names/types",
        title: "unsupported operator",
    },
    DiagnosticCodeInfo {
        code: "AU2004",
        band: "names/types",
        title: "argument binding",
    },
    DiagnosticCodeInfo {
        code: "AU2005",
        band: "names/types",
        title: "migration guidance",
    },
    DiagnosticCodeInfo {
        code: "AU2006",
        band: "names/types",
        title: "builtin handle method collision",
    },
    DiagnosticCodeInfo {
        code: "AU2007",
        band: "names/types",
        title: "builtin function redefinition",
    },
    DiagnosticCodeInfo {
        code: "AU2008",
        band: "names/types",
        title: "equality unavailable",
    },
    DiagnosticCodeInfo {
        code: "AU2999",
        band: "names/types",
        title: "general compile-time rejection",
    },
    DiagnosticCodeInfo {
        code: "AU3001",
        band: "ownership",
        title: "moved value",
    },
    DiagnosticCodeInfo {
        code: "AU3002",
        band: "ownership",
        title: "borrow violation",
    },
    DiagnosticCodeInfo {
        code: "AU3003",
        band: "ownership",
        title: "mutability violation",
    },
    DiagnosticCodeInfo {
        code: "AU3004",
        band: "ownership",
        title: "ownership mode",
    },
    DiagnosticCodeInfo {
        code: "AU3005",
        band: "ownership",
        title: "non-copy indexed read",
    },
    DiagnosticCodeInfo {
        code: "AU3006",
        band: "ownership",
        title: "non-copy indexed compound assignment",
    },
    DiagnosticCodeInfo {
        code: "AU3007",
        band: "ownership",
        title: "non-cloneable state duplication",
    },
    DiagnosticCodeInfo {
        code: "AU3008",
        band: "ownership",
        title: "non-Transfer task or queue boundary",
    },
    DiagnosticCodeInfo {
        code: "AU3009",
        band: "ownership",
        title: "single-consumer task-result duplication",
    },
    DiagnosticCodeInfo {
        code: "AU3010",
        band: "ownership",
        title: "view escape or returned-origin violation",
    },
    DiagnosticCodeInfo {
        code: "AU4001",
        band: "runtime",
        title: "runtime trap",
    },
    DiagnosticCodeInfo {
        code: "AU4002",
        band: "runtime",
        title: "arithmetic overflow or underflow",
    },
    DiagnosticCodeInfo {
        code: "AU4003",
        band: "runtime",
        title: "bounds or lookup violation",
    },
    DiagnosticCodeInfo {
        code: "AU4004",
        band: "runtime",
        title: "zero divisor",
    },
    DiagnosticCodeInfo {
        code: "AU4005",
        band: "runtime",
        title: "resource or I/O failure",
    },
    DiagnosticCodeInfo {
        code: "AU4006",
        band: "runtime",
        title: "invalid runtime configuration",
    },
    DiagnosticCodeInfo {
        code: "AU4007",
        band: "runtime",
        title: "array shape or reduction violation",
    },
    DiagnosticCodeInfo {
        code: "AU4008",
        band: "runtime",
        title: "collection value not found",
    },
];

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LabeledSpan {
    pub span: Span,
    pub label: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticEdit {
    pub start: Span,
    pub end: Span,
    pub replacement: String,
    pub applicability: String,
}

const MAX_ASSERTION_OPERAND_BYTES: usize = 4_096;
const ASSERTION_OPERAND_TRUNCATION_SUFFIX: &str = "... (truncated)";

/// One value captured while evaluating an introspected assertion condition.
///
/// `value` is the value's ordinary Aura `str()` rendering, bounded for both
/// human and structured diagnostics. The raw identifier serializes as the
/// public JSON field `type`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AssertionOperand {
    pub label: String,
    #[serde(rename = "type")]
    pub r#type: String,
    pub value: String,
    pub truncated: bool,
}

impl AssertionOperand {
    /// Builds an assertion operand whose rendered value occupies at most
    /// 4,096 UTF-8 bytes. Truncated values always end with the fixed
    /// `... (truncated)` suffix, and the retained prefix ends on a character
    /// boundary.
    pub fn bounded(
        label: impl Into<String>,
        type_name: impl Into<String>,
        rendered_value: impl Into<String>,
    ) -> Self {
        let mut value = rendered_value.into();
        let truncated = value.len() > MAX_ASSERTION_OPERAND_BYTES;
        if truncated {
            let mut prefix_bytes =
                MAX_ASSERTION_OPERAND_BYTES - ASSERTION_OPERAND_TRUNCATION_SUFFIX.len();
            while !value.is_char_boundary(prefix_bytes) {
                prefix_bytes -= 1;
            }
            value.truncate(prefix_bytes);
            value.push_str(ASSERTION_OPERAND_TRUNCATION_SUFFIX);
        }
        Self {
            label: label.into(),
            r#type: type_name.into(),
            value,
            truncated,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeSourceSpan {
    /// Absent only for internal source-only execution. Public structured
    /// diagnostics replace it with their caller-provided fallback path.
    pub path: Option<String>,
    pub start: Span,
    pub end: Span,
}

impl RuntimeSourceSpan {
    pub fn point(path: Option<String>, start: Span) -> Self {
        Self {
            path,
            start,
            end: Span::new(start.line, start.column.saturating_add(1)),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeCallFrame {
    pub function: String,
    pub span: RuntimeSourceSpan,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeTaskFrame {
    pub task_function: String,
    pub task_entry_span: RuntimeSourceSpan,
    pub parent_function: String,
    pub spawn_span: RuntimeSourceSpan,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StructuredSpan {
    pub path: String,
    pub start: Span,
    pub end: Span,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StructuredEdit {
    pub path: String,
    pub start: Span,
    pub end: Span,
    pub replacement: String,
    pub applicability: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StructuredRuntimeCallFrame {
    pub function: String,
    pub span: StructuredRuntimeSourceSpan,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StructuredRuntimeTaskFrame {
    pub task_function: String,
    pub task_entry_span: StructuredRuntimeSourceSpan,
    pub parent_function: String,
    pub spawn_span: StructuredRuntimeSourceSpan,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StructuredRuntimeSourceSpan {
    pub path: String,
    pub start: Span,
    pub end: Span,
}

impl RuntimeSourceSpan {
    fn structured(&self, fallback_path: &str) -> StructuredRuntimeSourceSpan {
        StructuredRuntimeSourceSpan {
            path: self
                .path
                .clone()
                .unwrap_or_else(|| fallback_path.to_string()),
            start: self.start,
            end: self.end,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StructuredDiagnostic {
    pub code: String,
    pub severity: DiagnosticSeverity,
    pub message: String,
    pub primary_span: Option<StructuredSpan>,
    pub secondary_spans: Vec<StructuredSpan>,
    pub notes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assertion_operands: Vec<AssertionOperand>,
    pub help: Vec<String>,
    pub edits: Vec<StructuredEdit>,
    #[serde(default)]
    pub call_frames: Vec<StructuredRuntimeCallFrame>,
    #[serde(default)]
    pub task_ancestry: Vec<StructuredRuntimeTaskFrame>,
}

impl Diagnostic {
    pub fn new(message: impl Into<String>) -> Self {
        let message = message.into();
        let code = stable_code_for_message(&message).to_string();
        Self {
            code,
            severity: DiagnosticSeverity::Error,
            message,
            span: None,
            details: Box::new(DiagnosticDetails {
                secondary_spans: Vec::new(),
                notes: Vec::new(),
                assertion_operands: Vec::new(),
                help: Vec::new(),
                edits: Vec::new(),
                call_frames: Vec::new(),
                task_ancestry: Vec::new(),
                render_path: None,
                render_source: None,
                partial_stdout: None,
                runtime_frames_captured: false,
            }),
        }
    }

    pub fn at(span: Span, message: impl Into<String>) -> Self {
        let mut diagnostic = Self::new(message);
        diagnostic.span = Some(span);
        diagnostic
    }

    pub fn coded(code: impl Into<String>, message: impl Into<String>) -> Self {
        let mut diagnostic = Self::new(message);
        diagnostic.code = code.into();
        debug_assert!(
            DIAGNOSTIC_CODE_REGISTRY
                .iter()
                .any(|entry| entry.code == diagnostic.code),
            "diagnostic code `{}` is not in the append-only registry",
            diagnostic.code
        );
        diagnostic
    }

    pub fn coded_at(code: impl Into<String>, span: Span, message: impl Into<String>) -> Self {
        let mut diagnostic = Self::coded(code, message);
        diagnostic.span = Some(span);
        diagnostic
    }

    /// Normalize a diagnostic at the runtime boundary. Legacy runtime helpers
    /// still use message-classified constructors in places; once execution has
    /// begun, no lexical, parse, type, or ownership code may leak through that
    /// compatibility path. Precise explicit AU40xx codes are preserved.
    pub(crate) fn into_runtime_trap(mut self) -> Self {
        if !self.code.starts_with("AU4") {
            self.code = "AU4001".to_string();
        }
        self
    }

    pub fn with_secondary(mut self, span: Span, label: impl Into<String>) -> Self {
        self.secondary_spans.push(LabeledSpan {
            span,
            label: label.into(),
        });
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    pub fn with_assertion_operand(
        mut self,
        label: impl Into<String>,
        type_name: impl Into<String>,
        rendered_value: impl Into<String>,
    ) -> Self {
        self.assertion_operands
            .push(AssertionOperand::bounded(label, type_name, rendered_value));
        self
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help.push(help.into());
        self
    }

    pub fn with_edit(mut self, start: Span, end: Span, replacement: impl Into<String>) -> Self {
        self.edits.push(DiagnosticEdit {
            start,
            end,
            replacement: replacement.into(),
            applicability: "machine-applicable".to_string(),
        });
        self
    }

    /// Snapshots Aura runtime frames exactly once. Empty vectors are a valid
    /// completed snapshot, so callers must use the private marker rather than
    /// emptiness to decide whether propagation has already captured state.
    pub fn capture_runtime_frames_once(
        &mut self,
        call_frames: Vec<RuntimeCallFrame>,
        task_ancestry: Vec<RuntimeTaskFrame>,
    ) -> bool {
        if self.details.runtime_frames_captured {
            return false;
        }
        self.call_frames = call_frames;
        self.task_ancestry = task_ancestry;
        self.details.runtime_frames_captured = true;
        true
    }

    pub fn structured(&self, path: &str) -> StructuredDiagnostic {
        let path = self.render_path.as_deref().unwrap_or(path).to_string();
        StructuredDiagnostic {
            code: self.code.clone(),
            severity: self.severity,
            message: self.message.clone(),
            primary_span: self.span.map(|span| StructuredSpan {
                path: path.clone(),
                start: span,
                end: Span::new(span.line, span.column.saturating_add(1)),
                label: None,
            }),
            secondary_spans: self
                .secondary_spans
                .iter()
                .map(|secondary| StructuredSpan {
                    path: path.clone(),
                    start: secondary.span,
                    end: Span::new(secondary.span.line, secondary.span.column.saturating_add(1)),
                    label: Some(secondary.label.clone()),
                })
                .collect(),
            notes: self.notes.clone(),
            assertion_operands: self.assertion_operands.clone(),
            help: self.help.clone(),
            edits: self
                .edits
                .iter()
                .map(|edit| StructuredEdit {
                    path: path.clone(),
                    start: edit.start,
                    end: edit.end,
                    replacement: edit.replacement.clone(),
                    applicability: edit.applicability.clone(),
                })
                .collect(),
            call_frames: self
                .call_frames
                .iter()
                .map(|frame| StructuredRuntimeCallFrame {
                    function: frame.function.clone(),
                    span: frame.span.structured(&path),
                })
                .collect(),
            task_ancestry: self
                .task_ancestry
                .iter()
                .map(|frame| StructuredRuntimeTaskFrame {
                    task_function: frame.task_function.clone(),
                    task_entry_span: frame.task_entry_span.structured(&path),
                    parent_function: frame.parent_function.clone(),
                    spawn_span: frame.spawn_span.structured(&path),
                })
                .collect(),
        }
    }

    pub fn with_render_context(
        mut self,
        path: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        self.render_path = Some(path.into());
        self.render_source = Some(source.into());
        self
    }

    pub fn with_partial_stdout(mut self, stdout: impl Into<String>) -> Self {
        self.partial_stdout = Some(stdout.into());
        self
    }

    pub fn partial_stdout(&self) -> Option<&str> {
        self.partial_stdout.as_deref()
    }

    pub fn render_with_source(&self, path: &str, source: &str) -> String {
        let (path, source) = match (&self.render_path, &self.render_source) {
            (Some(render_path), Some(render_source)) => {
                (render_path.as_str(), render_source.as_str())
            }
            _ => (path, source),
        };
        let mut rendered = match self.span {
            Some(span) => render_annotated(path, source, span, &self.code, &self.message),
            None => format!("error[{}]: {}\n --> {}", self.code, self.message, path),
        };
        for secondary in &self.secondary_spans {
            rendered.push_str(&format!(
                "\n  = related {}:{}:{}: {}",
                path, secondary.span.line, secondary.span.column, secondary.label
            ));
        }
        for note in &self.notes {
            rendered.push_str(&format!("\n  = note: {}", note));
        }
        for operand in &self.assertion_operands {
            rendered.push_str(&format!(
                "\n  = note: {} = {}",
                operand.label, operand.value
            ));
        }
        if !self.call_frames.is_empty() {
            let frames = self
                .call_frames
                .iter()
                .map(|frame| format!("{} at {}", frame.function, frame.span.start))
                .collect::<Vec<_>>()
                .join(" -> ");
            rendered.push_str(&format!(
                "\n  = note: Aura call chain (innermost first): {frames}"
            ));
        }
        if let Some(task) = self.task_ancestry.first() {
            rendered.push_str(&format!(
                "\n  = note: Aura task entry: {} at {}",
                task.task_function, task.task_entry_span.start
            ));
            let ancestry = self
                .task_ancestry
                .iter()
                .map(|frame| {
                    format!(
                        "{} spawned from {} at {}",
                        frame.task_function, frame.parent_function, frame.spawn_span.start
                    )
                })
                .collect::<Vec<_>>()
                .join(" -> ");
            rendered.push_str(&format!(
                "\n  = note: Aura task ancestry (youngest first): {ancestry}"
            ));
        }
        for help in &self.help {
            rendered.push_str(&format!("\n  = help: {}", help));
        }
        for edit in &self.edits {
            rendered.push_str(&format!(
                "\n  = fix: replace {}:{}:{}-{}:{} with `{}`",
                path,
                edit.start.line,
                edit.start.column,
                edit.end.line,
                edit.end.column,
                edit.replacement
            ));
        }
        rendered
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(span) = self.span {
            write!(f, "{}: {}", span, self.message)
        } else {
            write!(f, "{}", self.message)
        }
    }
}

impl Error for Diagnostic {}

pub type Result<T> = std::result::Result<T, Diagnostic>;

/// Stable public diagnostic categories. Existing codes are append-only: new
/// categories receive a new number and existing numbers are never reused.
fn stable_code_for_message(message: &str) -> &'static str {
    let lower = message.to_ascii_lowercase();
    if lower.contains("tab character") || lower.contains("invalid escape") {
        "AU1001"
    } else if lower.contains("f-strings must be double-quoted") {
        "AU1002"
    } else if lower.contains("unexpected character") || lower.contains("unterminated string") {
        "AU1001"
    } else if lower.starts_with("expected ")
        || lower.starts_with("unexpected ")
        || lower.contains("parse")
        || lower.contains("syntax")
    {
        "AU1101"
    } else if lower.contains("use of moved") || lower.contains("partially moved") {
        "AU3001"
    } else if lower.contains("borrow") {
        "AU3002"
    } else if lower.contains("immutable") || lower.contains("must be mutable") {
        "AU3003"
    } else if lower.contains("ownership") || lower.contains("declare it as `own") {
        "AU3004"
    } else if (lower.starts_with("integer value `")
        && (lower.contains("does not fit") || lower.contains("cannot be represented exactly")))
        || lower.contains("overflow")
        || lower.contains("underflow")
    {
        "AU4002"
    } else if lower.contains("out of bounds") || lower.contains("outside") {
        "AU4003"
    } else if lower.contains("division by zero") || lower.contains("modulo by zero") {
        "AU4004"
    } else if lower.starts_with("maximum call depth")
        || lower.starts_with("runtime ")
        || lower.contains("runtime trap")
    {
        "AU4001"
    } else if lower.starts_with("unknown ") || lower.contains("not found") {
        "AU2001"
    } else if lower.contains("integer `/`") || lower.contains("unsupported operator") {
        "AU2003"
    } else if lower.contains("did you mean")
        || lower.contains("not available yet")
        || lower.contains("arrives in a later aura release")
        || lower.contains("python-style")
    {
        "AU2005"
    } else if lower.contains("type") || lower.contains("expected `") {
        "AU2002"
    } else if lower.contains("argument") || lower.contains("arity") {
        "AU2004"
    } else {
        "AU2999"
    }
}

fn render_annotated(path: &str, source: &str, span: Span, code: &str, message: &str) -> String {
    let location = format!("{}:{}:{}", path, span.line, span.column);
    let Some(line_text) = source.lines().nth(span.line.saturating_sub(1)) else {
        return format!("error[{}]: {}\n --> {}", code, message, location);
    };

    let line_number = span.line.to_string();
    let gutter_width = line_number.len();
    let safe_column = span.column.max(1);
    let caret_padding = " ".repeat(safe_column.saturating_sub(1));

    format!(
        "error[{code}]: {message}\n --> {location}\n{blank:>width$} |\n{line_number:>width$} | {line_text}\n{blank:>width$} | {caret_padding}^",
        blank = "",
        width = gutter_width,
    )
}

#[cfg(test)]
#[path = "diag_tests.rs"]
mod tests;
