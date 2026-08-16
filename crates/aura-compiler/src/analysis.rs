use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Serialize;

use crate::ast::{
    AssignStmt, AssignTarget, BinaryOp, ComprehensionOutput, Expr, ExprKind, FunctionDecl,
    ImportKind, Item, LambdaParam, MatchArm, Module, Param, ParamMode, Pattern, ReceiverKind, Stmt,
    TypeRef, VariantPattern, ViewStmt,
};
use crate::call::{
    BuiltinAssociatedFunction, BuiltinClassConstructor, BuiltinFunction, BuiltinMember,
    ALL_BUILTIN_ASSOCIATED_FUNCTIONS, ALL_BUILTIN_FUNCTIONS,
};
use crate::diag::{Diagnostic, Result, RuntimeSourceSpan, Span};
use crate::parser;
use crate::sema::{
    builtin_duration_binary_result, resolve_param_passing, substitute_trait_bound, ClassInfo,
    ClosureInfo, ComprehensionInfo, EnumInfo, ExternFunctionInfo, FunctionInfo,
    FunctionParamContract, MethodInfo, OpaqueHandleInfo, Program, TraitBound, Type,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AnalysisOutput {
    pub diagnostics: Vec<AnalysisDiagnostic>,
    pub symbols: Vec<AnalysisSymbol>,
    pub occurrences: Vec<AnalysisOccurrence>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AnalysisDiagnostic {
    pub code: String,
    pub line: usize,
    pub start_character: usize,
    pub end_character: usize,
    pub message: String,
    pub severity: u8,
    pub secondary_spans: Vec<AnalysisDiagnosticSpan>,
    pub notes: Vec<String>,
    pub help: Vec<String>,
    pub edits: Vec<AnalysisDiagnosticEdit>,
    pub call_frames: Vec<AnalysisRuntimeCallFrame>,
    pub task_ancestry: Vec<AnalysisRuntimeTaskFrame>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AnalysisDiagnosticSpan {
    pub line: usize,
    pub start_character: usize,
    pub end_character: usize,
    pub label: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AnalysisDiagnosticEdit {
    pub line: usize,
    pub start_character: usize,
    pub end_character: usize,
    pub replacement: String,
    pub applicability: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AnalysisFrameSpan {
    pub file_path: Option<String>,
    pub line: usize,
    pub start_character: usize,
    pub end_character: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AnalysisRuntimeCallFrame {
    pub function: String,
    pub span: AnalysisFrameSpan,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AnalysisRuntimeTaskFrame {
    pub task_function: String,
    pub task_entry_span: AnalysisFrameSpan,
    pub parent_function: String,
    pub spawn_span: AnalysisFrameSpan,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AnalysisSymbol {
    pub name: String,
    pub kind: String,
    pub detail: String,
    pub line: usize,
    pub start_character: usize,
    pub end_character: usize,
    pub children: Vec<AnalysisSymbol>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AnalysisOccurrence {
    pub line: usize,
    pub start_character: usize,
    pub end_character: usize,
    pub hover: String,
    pub definition: Option<AnalysisRange>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AnalysisRange {
    pub file_path: Option<String>,
    pub line: usize,
    pub start_character: usize,
    pub end_character: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AnalysisCompletion {
    pub name: String,
    pub kind: String,
    pub detail: String,
}

#[derive(Clone)]
struct BindingInfo {
    ty: Type,
    trait_bounds: Vec<TraitBound>,
    definition: AnalysisRange,
    hover: String,
}

#[derive(Clone)]
struct ResolvedSymbol {
    hover: String,
    definition: Option<AnalysisRange>,
}

#[derive(Clone)]
struct ResolvedMember {
    hover: String,
    definition: Option<AnalysisRange>,
    ty: Option<Type>,
}

pub fn analyze_source(source: &str) -> AnalysisOutput {
    analyze_with_checker(source, crate::check_source)
}

pub fn analyze_path_source(path: &Path, source: &str) -> AnalysisOutput {
    analyze_with_checker(source, |candidate| {
        crate::check_path_with_source_without_lockfile(path, candidate)
    })
}

pub fn complete_path_source(
    path: &Path,
    source: &str,
    line: usize,
    character: usize,
    trigger_character: Option<char>,
) -> Result<Vec<AnalysisCompletion>> {
    complete_with_checker(source, line, character, trigger_character, |candidate| {
        crate::check_path_with_source_without_lockfile(path, candidate)
    })
}

pub fn analyze_program(source: &str, program: &Program) -> AnalysisOutput {
    let symbols = symbols_from_module(&program.module);
    AnalysisBuilder::new(source, program, symbols).build()
}

pub fn complete_source(
    source: &str,
    line: usize,
    character: usize,
    trigger_character: Option<char>,
) -> Result<Vec<AnalysisCompletion>> {
    complete_with_checker(
        source,
        line,
        character,
        trigger_character,
        crate::check_source,
    )
}

fn analyze_with_checker<F>(source: &str, mut check_program: F) -> AnalysisOutput
where
    F: FnMut(&str) -> Result<Program>,
{
    match parser::parse(source) {
        Err(error) => {
            if let Some(program) =
                recover_checked_program_after_parse_error_with(source, &error, &mut check_program)
            {
                let mut output = analyze_program(source, &program);
                output.diagnostics.insert(0, analysis_diagnostic(&error));
                output
            } else {
                AnalysisOutput {
                    diagnostics: vec![analysis_diagnostic(&error)],
                    symbols: Vec::new(),
                    occurrences: Vec::new(),
                }
            }
        }
        Ok(module) => {
            let symbols = symbols_from_module(&module);
            match check_program(source) {
                Err(error) => AnalysisOutput {
                    diagnostics: vec![analysis_diagnostic(&error)],
                    symbols,
                    occurrences: Vec::new(),
                },
                Ok(program) => AnalysisBuilder::new(source, &program, symbols).build(),
            }
        }
    }
}

fn complete_with_checker<F>(
    source: &str,
    line: usize,
    character: usize,
    trigger_character: Option<char>,
    mut check_program: F,
) -> Result<Vec<AnalysisCompletion>>
where
    F: FnMut(&str) -> Result<Program>,
{
    let program = match check_program(source) {
        Ok(program) => program,
        Err(error) if trigger_character == Some('.') => {
            recover_checked_program_after_position(source, line, character, &mut check_program)
                .ok_or(error)?
        }
        Err(error) => return Err(error),
    };
    let builder = AnalysisBuilder::new(source, &program, Vec::new());
    builder.complete(line, character, trigger_character)
}

struct AnalysisBuilder<'a> {
    source_lines: Vec<&'a str>,
    program: &'a Program,
    output: AnalysisOutput,
}

impl<'a> AnalysisBuilder<'a> {
    fn new(source: &'a str, program: &'a Program, symbols: Vec<AnalysisSymbol>) -> Self {
        Self {
            source_lines: source.lines().collect(),
            program,
            output: AnalysisOutput {
                diagnostics: Vec::new(),
                symbols,
                occurrences: Vec::new(),
            },
        }
    }

    fn build(mut self) -> AnalysisOutput {
        self.visit_import_aliases();
        let mut top_level_scope = BTreeMap::new();
        for constant in &self.program.module.constants {
            self.visit_expr(&constant.value, &top_level_scope);
            if let Some(info) = self.program.constants.get(&constant.name) {
                top_level_scope.insert(
                    constant.name.clone(),
                    BindingInfo {
                        ty: info.ty.clone(),
                        trait_bounds: Vec::new(),
                        definition: self.constant_definition(info),
                        hover: format_value_hover("module constant", &constant.name, &info.ty),
                    },
                );
            }
        }
        self.visit_stmts(&self.program.top_level_stmts, &mut top_level_scope);

        for item in &self.program.module.items {
            match item {
                Item::Function(function_decl) => {
                    let function_info = self.program.functions.get(&function_decl.name).unwrap();
                    let mut scope = self.function_scope(function_decl, function_info);
                    self.visit_stmts(&function_decl.body, &mut scope);
                }
                Item::Class(class_decl) => {
                    let class_info = self.program.classes.get(&class_decl.name).unwrap();
                    for method in &class_decl.methods {
                        let method_info = class_info.methods.get(&method.name).unwrap();
                        let mut scope =
                            self.method_scope(class_decl.name.as_str(), method, method_info);
                        self.visit_stmts(&method.body, &mut scope);
                    }
                }
                Item::Enum(_)
                | Item::ExternFunction(_)
                | Item::ExternOpaqueClass(_)
                | Item::Trait(_)
                | Item::Impl(_) => {}
            }
        }

        self.output
    }

    fn visit_import_aliases(&mut self) {
        let empty_scope = BTreeMap::new();
        let imports = self.program.module.imports.clone();
        for import in &imports {
            match &import.kind {
                ImportKind::Module {
                    path,
                    alias: Some(alias),
                } => {
                    let Some(range) = self.find_module_alias_range(import.span.line, alias) else {
                        continue;
                    };
                    let target = path.join(".");
                    let definition = self.find_imported_module_range(&target);
                    self.push_occurrence(
                        range,
                        format!("```aura\nmodule {alias} = {target}\n```"),
                        definition,
                    );
                }
                ImportKind::From { names, .. } => {
                    for imported_name in names {
                        let Some(alias) = imported_name.alias.as_deref() else {
                            continue;
                        };
                        let Some(range) = self.find_from_import_alias_range(
                            imported_name.span.line,
                            imported_name.span.column,
                            alias,
                        ) else {
                            continue;
                        };
                        let Some(resolved) = self.resolve_name(alias, &empty_scope) else {
                            continue;
                        };
                        self.push_occurrence(range, resolved.hover, resolved.definition);
                    }
                }
                ImportKind::Module { alias: None, .. } => {}
            }
        }
    }

    fn find_module_alias_range(&self, line_number: usize, alias: &str) -> Option<AnalysisRange> {
        let line_index = line_number.checked_sub(1)?;
        let line = *self.source_lines.get(line_index)?;
        let start = line.rfind(alias)?;
        Some(AnalysisRange {
            file_path: self.current_source_path(),
            line: line_index,
            start_character: start,
            end_character: start + alias.len(),
        })
    }

    fn find_from_import_alias_range(
        &self,
        line_number: usize,
        imported_name_column: usize,
        alias: &str,
    ) -> Option<AnalysisRange> {
        let line_index = line_number.checked_sub(1)?;
        let line = *self.source_lines.get(line_index)?;
        let segment_start = imported_name_column.saturating_sub(1);
        let remainder = line.get(segment_start..)?;
        let segment = remainder.split(',').next()?;
        let relative_start = segment.rfind(alias)?;
        let start = segment_start + relative_start;
        Some(AnalysisRange {
            file_path: self.current_source_path(),
            line: line_index,
            start_character: start,
            end_character: start + alias.len(),
        })
    }

    fn complete(
        &self,
        line: usize,
        character: usize,
        trigger_character: Option<char>,
    ) -> Result<Vec<AnalysisCompletion>> {
        let line_text = self.source_lines.get(line).copied().unwrap_or("");

        if trigger_character == Some('.') {
            let Some(receiver_text) = extract_receiver_before_dot(line_text, character) else {
                return Ok(Vec::new());
            };
            let receiver_expr = parser::parse_expression(&receiver_text)?;
            let scope = self.scope_for_position(line, character);
            if let ExprKind::Name(name) = &receiver_expr.kind {
                if let Some(binding) = scope.get(name) {
                    if !binding.trait_bounds.is_empty() {
                        return Ok(self.trait_bound_member_completions(&binding.trait_bounds));
                    }
                }
                if !scope.contains_key(name) {
                    let associated = builtin_associated_function_completions(name);
                    if !associated.is_empty() {
                        return Ok(associated);
                    }
                }
            }
            if let ExprKind::Index { object, .. } = &receiver_expr.kind {
                if let ExprKind::Name(name) = &object.kind {
                    if matches!(name.as_str(), "Array" | "list" | "dict" | "set") {
                        return Ok(builtin_specialized_associated_function_completions(
                            name,
                            &receiver_text,
                        ));
                    }
                }
            }
            if let ExprKind::Specialize { expr, .. } = &receiver_expr.kind {
                if let ExprKind::Name(name) = &expr.kind {
                    if matches!(name.as_str(), "Array" | "list" | "dict" | "set") {
                        return Ok(builtin_specialized_associated_function_completions(
                            name,
                            &receiver_text,
                        ));
                    }
                }
            }
            let Some(receiver_type) = self.infer_expr_type(&receiver_expr, &scope) else {
                return Ok(Vec::new());
            };
            return Ok(self.member_completions(&receiver_type));
        }

        let scope = self.scope_for_position(line, character);
        let mut completions = self.top_level_completions();
        completions.retain(|completion| {
            completion.kind != "constant" || scope.contains_key(&completion.name)
        });
        let mut names = completions
            .iter()
            .map(|completion| completion.name.clone())
            .collect::<BTreeSet<_>>();
        for (name, binding) in scope {
            if names.insert(name.clone()) {
                completions.push(AnalysisCompletion {
                    name,
                    kind: "variable".to_string(),
                    detail: binding.ty.to_string(),
                });
            }
        }
        Ok(completions)
    }

    fn function_scope(
        &self,
        function_decl: &FunctionDecl,
        function_info: &FunctionInfo,
    ) -> BTreeMap<String, BindingInfo> {
        let mut scope = self.constant_scope();
        for (param, ty) in function_decl
            .params
            .iter()
            .zip(&function_info.signature.params)
        {
            let range = range_from_span(param.span, param.name.len());
            scope.insert(
                param.name.clone(),
                BindingInfo {
                    ty: ty.clone(),
                    trait_bounds: match ty {
                        Type::TypeParam(name) => function_info
                            .type_param_bounds
                            .get(name)
                            .cloned()
                            .unwrap_or_default(),
                        _ => Vec::new(),
                    },
                    definition: range.clone(),
                    hover: format_param_hover(param, ty),
                },
            );
        }
        scope
    }

    fn constant_scope(&self) -> BTreeMap<String, BindingInfo> {
        self.program
            .constants
            .iter()
            .map(|(name, info)| (name.clone(), self.constant_binding_info(name, info)))
            .collect()
    }

    fn constant_scope_before_line(&self, target_line: usize) -> BTreeMap<String, BindingInfo> {
        let ready_local_constants = self
            .program
            .module
            .constants
            .iter()
            .filter(|constant| expression_end_line(&constant.value) < target_line)
            .map(|constant| constant.name.as_str())
            .collect::<BTreeSet<_>>();

        self.program
            .constants
            .iter()
            .filter(|(name, info)| {
                info.module_name != self.program.module_name
                    || ready_local_constants.contains(name.as_str())
            })
            .map(|(name, info)| (name.clone(), self.constant_binding_info(name, info)))
            .collect()
    }

    fn top_level_constant_scope(&self, target_line: usize) -> BTreeMap<String, BindingInfo> {
        let inside_initializer = self.program.module.constants.iter().any(|constant| {
            constant.span.line <= target_line && target_line <= expression_end_line(&constant.value)
        });
        if inside_initializer {
            self.constant_scope_before_line(target_line)
        } else {
            // Executable top-level statements start only after the complete
            // module initialization phase, so every constant is ready there.
            self.constant_scope()
        }
    }

    fn constant_binding_info(&self, name: &str, info: &crate::sema::ConstantInfo) -> BindingInfo {
        BindingInfo {
            ty: info.ty.clone(),
            trait_bounds: Vec::new(),
            definition: self.constant_definition(info),
            hover: format_value_hover("module constant", name, &info.ty),
        }
    }

    fn method_scope(
        &self,
        class_name: &str,
        method_decl: &FunctionDecl,
        method_info: &MethodInfo,
    ) -> BTreeMap<String, BindingInfo> {
        let mut scope = self.function_scope(
            method_decl,
            &FunctionInfo {
                module_name: self.program.module_name.clone(),
                decl: method_decl.clone(),
                signature: method_info.signature.clone(),
                type_param_bounds: method_info.type_param_bounds.clone(),
            },
        );

        if method_decl.receiver.is_some() {
            let definition = self
                .find_identifier_range(method_decl.span.line, "self")
                .unwrap_or_else(|| range_from_span(method_decl.span, method_decl.name.len()));
            scope.insert(
                "self".to_string(),
                BindingInfo {
                    ty: Type::named(class_name),
                    trait_bounds: Vec::new(),
                    definition,
                    hover: format_value_hover("param", "self", &Type::named(class_name)),
                },
            );
        }

        scope
    }

    fn scope_for_line(&self, line: usize) -> BTreeMap<String, BindingInfo> {
        let target_line = line + 1;

        if let Some((function_decl, function_info)) = self.enclosing_function(target_line) {
            let mut scope = self.function_scope(function_decl, function_info);
            self.accumulate_scope_from_stmts(&function_decl.body, target_line, &mut scope);
            return scope;
        }

        if let Some((class_name, method_decl, method_info)) = self.enclosing_method(target_line) {
            let mut scope = self.method_scope(class_name, method_decl, method_info);
            self.accumulate_scope_from_stmts(&method_decl.body, target_line, &mut scope);
            return scope;
        }

        let mut scope = self.top_level_constant_scope(target_line);
        self.accumulate_scope_from_stmts(&self.program.top_level_stmts, target_line, &mut scope);
        scope
    }

    fn scope_for_position(&self, line: usize, character: usize) -> BTreeMap<String, BindingInfo> {
        let mut scope = self.scope_for_line(line);
        let target_line = line + 1;

        if let Some((function_decl, _)) = self.enclosing_function(target_line) {
            self.extend_lambda_scope_from_stmts(
                &function_decl.body,
                target_line,
                character,
                &mut scope,
            );
        } else if let Some((_, method_decl, _)) = self.enclosing_method(target_line) {
            self.extend_lambda_scope_from_stmts(
                &method_decl.body,
                target_line,
                character,
                &mut scope,
            );
        } else {
            self.extend_lambda_scope_from_stmts(
                &self.program.top_level_stmts,
                target_line,
                character,
                &mut scope,
            );
        }

        scope
    }

    fn closure_info(&self, expr: &Expr) -> Option<&ClosureInfo> {
        self.program.closures.values().find(|info| {
            info.span.line == expr.span.line
                && info.span.column == expr.span.column
                && info.id.module_name == self.program.module_name
        })
    }

    fn comprehension_info(&self, expr: &Expr) -> Option<&ComprehensionInfo> {
        self.program.comprehensions.values().find(|info| {
            info.id.line == expr.span.line
                && info.id.column == expr.span.column
                && info.id.module_name == self.program.module_name
        })
    }

    fn extend_lambda_scope_from_stmts(
        &self,
        stmts: &[Stmt],
        target_line: usize,
        character: usize,
        scope: &mut BTreeMap<String, BindingInfo>,
    ) {
        for stmt in stmts {
            match stmt {
                Stmt::Assign(assign) => {
                    self.extend_lambda_scope_from_expr(
                        &assign.value,
                        target_line,
                        character,
                        scope,
                    );
                    match &assign.target {
                        AssignTarget::Member { object, .. } => self.extend_lambda_scope_from_expr(
                            object,
                            target_line,
                            character,
                            scope,
                        ),
                        AssignTarget::Index { object, index } => {
                            self.extend_lambda_scope_from_expr(
                                object,
                                target_line,
                                character,
                                scope,
                            );
                            self.extend_lambda_scope_from_expr(
                                index,
                                target_line,
                                character,
                                scope,
                            );
                        }
                        AssignTarget::Name(_) => {}
                    }
                }
                Stmt::View(view) => {
                    self.extend_lambda_scope_from_expr(&view.source, target_line, character, scope)
                }
                Stmt::Destructure(destructure) => self.extend_lambda_scope_from_expr(
                    &destructure.value,
                    target_line,
                    character,
                    scope,
                ),
                Stmt::Return(ret) => {
                    if let Some(value) = &ret.value {
                        self.extend_lambda_scope_from_expr(value, target_line, character, scope);
                    }
                }
                Stmt::Assert(assert_stmt) => {
                    self.extend_lambda_scope_from_expr(
                        &assert_stmt.condition,
                        target_line,
                        character,
                        scope,
                    );
                    if let Some(message) = &assert_stmt.message {
                        self.extend_lambda_scope_from_expr(message, target_line, character, scope);
                    }
                }
                Stmt::If(if_stmt) => {
                    for branch in &if_stmt.branches {
                        self.extend_lambda_scope_from_expr(
                            &branch.condition,
                            target_line,
                            character,
                            scope,
                        );
                        self.extend_lambda_scope_from_stmts(
                            &branch.body,
                            target_line,
                            character,
                            scope,
                        );
                    }
                    if let Some(body) = &if_stmt.else_body {
                        self.extend_lambda_scope_from_stmts(body, target_line, character, scope);
                    }
                }
                Stmt::Match(match_stmt) => {
                    self.extend_lambda_scope_from_expr(
                        &match_stmt.scrutinee,
                        target_line,
                        character,
                        scope,
                    );
                    for arm in &match_stmt.arms {
                        self.extend_lambda_scope_from_stmts(
                            &arm.body,
                            target_line,
                            character,
                            scope,
                        );
                    }
                }
                Stmt::For(for_stmt) => {
                    self.extend_lambda_scope_from_expr(
                        &for_stmt.iterable,
                        target_line,
                        character,
                        scope,
                    );
                    self.extend_lambda_scope_from_stmts(
                        &for_stmt.body,
                        target_line,
                        character,
                        scope,
                    );
                }
                Stmt::With(with_stmt) => {
                    self.extend_lambda_scope_from_expr(
                        &with_stmt.value,
                        target_line,
                        character,
                        scope,
                    );
                    self.extend_lambda_scope_from_stmts(
                        &with_stmt.body,
                        target_line,
                        character,
                        scope,
                    );
                }
                Stmt::While(while_stmt) => {
                    self.extend_lambda_scope_from_expr(
                        &while_stmt.condition,
                        target_line,
                        character,
                        scope,
                    );
                    self.extend_lambda_scope_from_stmts(
                        &while_stmt.body,
                        target_line,
                        character,
                        scope,
                    );
                }
                Stmt::Expr(expr_stmt) => self.extend_lambda_scope_from_expr(
                    &expr_stmt.expr,
                    target_line,
                    character,
                    scope,
                ),
                Stmt::Pass(_) | Stmt::Break(_) | Stmt::Continue(_) => {}
            }
        }
    }

    fn extend_lambda_scope_from_expr(
        &self,
        expr: &Expr,
        target_line: usize,
        character: usize,
        scope: &mut BTreeMap<String, BindingInfo>,
    ) {
        match &expr.kind {
            ExprKind::Lambda { params, body, .. } => {
                if expression_contains_position(body, target_line, character) {
                    if let Some(info) = self.closure_info(expr) {
                        for (param, contract) in params.iter().zip(&info.params) {
                            let definition = range_from_span(param.span, param.name.len());
                            scope.insert(
                                param.name.clone(),
                                BindingInfo {
                                    ty: contract.ty.clone(),
                                    trait_bounds: Vec::new(),
                                    definition,
                                    hover: format_lambda_param_hover(param, &contract.ty),
                                },
                            );
                        }
                    }
                    self.extend_lambda_scope_from_expr(body, target_line, character, scope);
                }
            }
            ExprKind::Membership {
                value, container, ..
            } => {
                self.extend_lambda_scope_from_expr(value, target_line, character, scope);
                self.extend_lambda_scope_from_expr(container, target_line, character, scope);
            }
            ExprKind::CompareChain { first, links } => {
                self.extend_lambda_scope_from_expr(first, target_line, character, scope);
                for link in links {
                    self.extend_lambda_scope_from_expr(
                        &link.operand,
                        target_line,
                        character,
                        scope,
                    );
                }
            }
            ExprKind::Member { object, .. }
            | ExprKind::Specialize { expr: object, .. }
            | ExprKind::Cast { expr: object, .. }
            | ExprKind::Unary { expr: object, .. }
            | ExprKind::Try(object)
            | ExprKind::Group(object) => {
                self.extend_lambda_scope_from_expr(object, target_line, character, scope);
            }
            ExprKind::Call { callee, args } => {
                self.extend_lambda_scope_from_expr(callee, target_line, character, scope);
                for arg in args {
                    self.extend_lambda_scope_from_expr(&arg.value, target_line, character, scope);
                }
            }
            ExprKind::FString(parts) => {
                for part in parts {
                    match part {
                        crate::ast::FormatPart::Expr(part_expr)
                        | crate::ast::FormatPart::Formatted {
                            expr: part_expr, ..
                        } => {
                            self.extend_lambda_scope_from_expr(
                                part_expr,
                                target_line,
                                character,
                                scope,
                            );
                        }
                        crate::ast::FormatPart::Literal(_) => {}
                    }
                }
            }
            ExprKind::Binary { left, right, .. } => {
                self.extend_lambda_scope_from_expr(left, target_line, character, scope);
                self.extend_lambda_scope_from_expr(right, target_line, character, scope);
            }
            ExprKind::Conditional {
                then_expr,
                condition,
                else_expr,
            } => {
                self.extend_lambda_scope_from_expr(then_expr, target_line, character, scope);
                self.extend_lambda_scope_from_expr(condition, target_line, character, scope);
                self.extend_lambda_scope_from_expr(else_expr, target_line, character, scope);
            }
            ExprKind::Comprehension { output, clauses } => {
                self.extend_comprehension_scope_for_position(
                    expr,
                    output,
                    clauses,
                    target_line,
                    character,
                    scope,
                );
            }
            ExprKind::Tuple(elements) | ExprKind::List(elements) | ExprKind::Set(elements) => {
                for element in elements {
                    self.extend_lambda_scope_from_expr(element, target_line, character, scope);
                }
            }
            ExprKind::Map(entries) => {
                for entry in entries {
                    self.extend_lambda_scope_from_expr(&entry.key, target_line, character, scope);
                    self.extend_lambda_scope_from_expr(&entry.value, target_line, character, scope);
                }
            }
            ExprKind::Index { object, index } => {
                self.extend_lambda_scope_from_expr(object, target_line, character, scope);
                self.extend_lambda_scope_from_expr(index, target_line, character, scope);
            }
            ExprKind::Slice {
                object, start, end, ..
            } => {
                self.extend_lambda_scope_from_expr(object, target_line, character, scope);
                if let Some(start) = start {
                    self.extend_lambda_scope_from_expr(start, target_line, character, scope);
                }
                if let Some(end) = end {
                    self.extend_lambda_scope_from_expr(end, target_line, character, scope);
                }
            }
            ExprKind::Match {
                scrutinee, arms, ..
            } => {
                self.extend_lambda_scope_from_expr(scrutinee, target_line, character, scope);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.extend_lambda_scope_from_expr(guard, target_line, character, scope);
                    }
                    self.extend_lambda_scope_from_expr(&arm.value, target_line, character, scope);
                }
            }
            ExprKind::Name(_)
            | ExprKind::Int(_)
            | ExprKind::DurationNanos(_)
            | ExprKind::BuiltinOmitted
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::String(_) => {}
        }
    }

    fn extend_comprehension_scope_for_position(
        &self,
        expr: &Expr,
        output: &ComprehensionOutput,
        clauses: &[crate::ast::ComprehensionClause],
        target_line: usize,
        character: usize,
        scope: &mut BTreeMap<String, BindingInfo>,
    ) {
        let Some(first_clause) = clauses.first() else {
            return;
        };
        let expression_start = expression_start_span(expr);
        if position_is_before_span(target_line, character, expression_start)
            || target_line > expression_end_line(expr)
        {
            return;
        }
        let mut comprehension_scope = scope.clone();
        let checked_clause_types = self
            .comprehension_info(expr)
            .map(|info| {
                info.clauses
                    .iter()
                    .map(|clause| clause.binding_type.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        // The output is written first but executes after every clause. A
        // cursor before the first `for` therefore sees all comprehension
        // targets, in execution order.
        if position_is_before_span(target_line, character, first_clause.span) {
            for (clause_index, clause) in clauses.iter().enumerate() {
                let binding_ty = checked_clause_types
                    .get(clause_index)
                    .cloned()
                    .unwrap_or(Type::Unit);
                self.insert_scope_target_exact(
                    &clause.target,
                    &binding_ty,
                    "local",
                    &mut comprehension_scope,
                );
            }
            self.extend_lambda_scope_from_comprehension_output(
                output,
                target_line,
                character,
                &mut comprehension_scope,
            );
            *scope = comprehension_scope;
            return;
        }

        for (clause_index, clause) in clauses.iter().enumerate() {
            if position_is_before_span(
                target_line,
                character,
                expression_start_span(&clause.iterable),
            ) {
                *scope = comprehension_scope;
                return;
            }

            let next_clause_span = clauses.get(clause_index + 1).map(|next| next.span);
            let first_filter_span = clause.filters.first().map(expression_start_span);
            let iterable_boundary = first_filter_span.or(next_clause_span);
            if iterable_boundary.is_some_and(|boundary| {
                position_is_before_span(target_line, character, boundary)
                    && !first_filter_span.is_some_and(|filter_span| {
                        self.position_is_after_filter_keyword(
                            target_line,
                            character,
                            clause.span,
                            filter_span,
                        )
                    })
            }) {
                self.extend_lambda_scope_from_expr(
                    &clause.iterable,
                    target_line,
                    character,
                    &mut comprehension_scope,
                );
                *scope = comprehension_scope;
                return;
            }
            if iterable_boundary.is_none() && target_line <= expression_end_line(&clause.iterable) {
                self.extend_lambda_scope_from_expr(
                    &clause.iterable,
                    target_line,
                    character,
                    &mut comprehension_scope,
                );
                *scope = comprehension_scope;
                return;
            }

            let binding_ty = checked_clause_types
                .get(clause_index)
                .cloned()
                .unwrap_or(Type::Unit);
            self.insert_scope_target_exact(
                &clause.target,
                &binding_ty,
                "local",
                &mut comprehension_scope,
            );

            for (filter_index, filter) in clause.filters.iter().enumerate() {
                if position_is_before_span(target_line, character, expression_start_span(filter)) {
                    *scope = comprehension_scope;
                    return;
                }
                let filter_boundary = clause
                    .filters
                    .get(filter_index + 1)
                    .map(expression_start_span)
                    .or(next_clause_span);
                if filter_boundary.is_some_and(|boundary| {
                    position_is_before_span(target_line, character, boundary)
                }) || (filter_boundary.is_none() && target_line <= expression_end_line(filter))
                {
                    self.extend_lambda_scope_from_expr(
                        filter,
                        target_line,
                        character,
                        &mut comprehension_scope,
                    );
                    *scope = comprehension_scope;
                    return;
                }
            }
        }
    }

    fn position_is_after_filter_keyword(
        &self,
        target_line: usize,
        character: usize,
        clause_span: Span,
        following_span: Span,
    ) -> bool {
        let clause_line = clause_span.line.saturating_sub(1);
        let following_line = following_span.line.saturating_sub(1);
        let source_between = self.source_lines[clause_line..=following_line]
            .iter()
            .enumerate()
            .map(|(line_offset, line)| {
                let start = if line_offset == 0 {
                    clause_span.column.saturating_sub(1)
                } else {
                    0
                };
                let end = if clause_line + line_offset == following_line {
                    following_span.column.saturating_sub(1)
                } else {
                    line.len()
                };
                &line[start..end]
            })
            .collect::<Vec<_>>()
            .join("\n");
        let Some(keyword_span) = crate::lexer::lex(&source_between).ok().and_then(|tokens| {
            tokens.into_iter().rev().find_map(|token| {
                matches!(token.kind, crate::lexer::TokenKind::KwIf).then_some(token.span)
            })
        }) else {
            return false;
        };
        let keyword_line = clause_line + keyword_span.line.saturating_sub(1);
        let keyword_byte = if keyword_span.line == 1 {
            clause_span.column.saturating_sub(1) + keyword_span.column.saturating_sub(1)
        } else {
            keyword_span.column.saturating_sub(1)
        };
        let keyword_end = self.source_lines[keyword_line][..keyword_byte]
            .encode_utf16()
            .count()
            + "if".encode_utf16().count();
        (target_line.saturating_sub(1), character) >= (keyword_line, keyword_end)
    }

    fn extend_lambda_scope_from_comprehension_output(
        &self,
        output: &ComprehensionOutput,
        target_line: usize,
        character: usize,
        scope: &mut BTreeMap<String, BindingInfo>,
    ) {
        match output {
            ComprehensionOutput::List(value) | ComprehensionOutput::Set(value) => {
                self.extend_lambda_scope_from_expr(value, target_line, character, scope);
            }
            ComprehensionOutput::Map { key, value } => {
                self.extend_lambda_scope_from_expr(key, target_line, character, scope);
                self.extend_lambda_scope_from_expr(value, target_line, character, scope);
            }
        }
    }

    fn enclosing_function(&self, line: usize) -> Option<(&FunctionDecl, &FunctionInfo)> {
        self.program
            .module
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Function(function_decl)
                    if callable_contains_line(&function_decl.body, line) =>
                {
                    Some((
                        function_decl,
                        self.program.functions.get(&function_decl.name).unwrap(),
                    ))
                }
                _ => None,
            })
            .next_back()
    }

    fn enclosing_method(&self, line: usize) -> Option<(&str, &FunctionDecl, &MethodInfo)> {
        self.program
            .module
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Class(class_decl) => class_decl
                    .methods
                    .iter()
                    .filter(|method| callable_contains_line(&method.body, line))
                    .map(|method| {
                        (
                            class_decl.name.as_str(),
                            method,
                            self.program.classes[&class_decl.name]
                                .methods
                                .get(&method.name)
                                .unwrap(),
                        )
                    })
                    .next_back(),
                _ => None,
            })
            .next_back()
    }

    fn accumulate_scope_from_stmts(
        &self,
        stmts: &[Stmt],
        target_line: usize,
        scope: &mut BTreeMap<String, BindingInfo>,
    ) {
        for stmt in stmts {
            if stmt_start_line(stmt) > target_line {
                break;
            }

            match stmt {
                Stmt::Assign(assign) => self.bind_assignment(assign, scope),
                Stmt::View(view) => {
                    let ty = self
                        .infer_expr_type(&view.source, scope)
                        .unwrap_or(Type::Unit);
                    self.insert_scope_binding(
                        &view.name,
                        ty,
                        view.span.line,
                        if view.mutable { "mutable view" } else { "view" },
                        scope,
                    );
                }
                Stmt::Destructure(destructure) => {
                    let ty = self
                        .infer_expr_type(&destructure.value, scope)
                        .unwrap_or(Type::Unit);
                    self.insert_scope_target(
                        &destructure.target,
                        &ty,
                        destructure.span.line,
                        "binding",
                        scope,
                    );
                }
                Stmt::If(if_stmt) => {
                    for branch in &if_stmt.branches {
                        if block_contains_line(&branch.body, target_line) {
                            self.accumulate_scope_from_stmts(&branch.body, target_line, scope);
                            return;
                        }
                    }
                    if let Some(body) = &if_stmt.else_body {
                        if block_contains_line(body, target_line) {
                            self.accumulate_scope_from_stmts(body, target_line, scope);
                            return;
                        }
                    }
                }
                Stmt::Match(match_stmt) => {
                    let scrutinee_type = self.infer_expr_type(&match_stmt.scrutinee, scope);
                    for arm in &match_stmt.arms {
                        if block_contains_line(&arm.body, target_line) {
                            self.bind_match_arm_scope(arm, scrutinee_type.as_ref(), scope);
                            self.accumulate_scope_from_stmts(&arm.body, target_line, scope);
                            return;
                        }
                    }
                }
                Stmt::For(for_stmt) => {
                    if block_contains_line(&for_stmt.body, target_line) {
                        let binding_ty = self
                            .infer_iterable_binding_type(&for_stmt.iterable, scope)
                            .unwrap_or(Type::Unit);
                        self.insert_scope_target(
                            &for_stmt.target,
                            &binding_ty,
                            for_stmt.span.line,
                            "local",
                            scope,
                        );
                        self.accumulate_scope_from_stmts(&for_stmt.body, target_line, scope);
                        return;
                    }
                }
                Stmt::With(with_stmt) => {
                    if block_contains_line(&with_stmt.body, target_line) {
                        let binding_ty = self
                            .infer_expr_type(&with_stmt.value, scope)
                            .unwrap_or(Type::Unit);
                        self.insert_scope_binding(
                            &with_stmt.binding,
                            binding_ty,
                            with_stmt.span.line,
                            "local",
                            scope,
                        );
                        self.accumulate_scope_from_stmts(&with_stmt.body, target_line, scope);
                        return;
                    }
                }
                Stmt::While(while_stmt) => {
                    if block_contains_line(&while_stmt.body, target_line) {
                        self.accumulate_scope_from_stmts(&while_stmt.body, target_line, scope);
                        return;
                    }
                }
                Stmt::Pass(_)
                | Stmt::Assert(_)
                | Stmt::Return(_)
                | Stmt::Break(_)
                | Stmt::Continue(_)
                | Stmt::Expr(_) => {}
            }
        }
    }

    fn bind_assignment(&self, assign: &AssignStmt, scope: &mut BTreeMap<String, BindingInfo>) {
        let AssignTarget::Name(name) = &assign.target else {
            return;
        };
        if scope.contains_key(name) {
            return;
        }
        let inferred_ty = self.infer_expr_type(&assign.value, scope);
        let binding_ty = match inferred_ty {
            Some(ty @ Type::Closure { .. }) => ty,
            inferred_ty => assign
                .annotation
                .as_ref()
                .map(|ty| self.lower_analysis_type_ref(ty))
                .or(inferred_ty)
                .unwrap_or(Type::Unit),
        };
        let definition = self
            .find_identifier_range(assign.span.line, name)
            .unwrap_or_else(|| range_from_span(assign.span, name.len()));
        scope.insert(
            name.clone(),
            BindingInfo {
                ty: binding_ty.clone(),
                trait_bounds: Vec::new(),
                definition: definition.clone(),
                hover: format_value_hover("binding", name, &binding_ty),
            },
        );
    }

    fn top_level_completions(&self) -> Vec<AnalysisCompletion> {
        let mut completions = Vec::new();
        for keyword in KEYWORDS {
            completions.push(AnalysisCompletion {
                name: keyword.to_string(),
                kind: "keyword".to_string(),
                detail: "Aura keyword".to_string(),
            });
        }
        for (visible_name, class_info) in &self.program.classes {
            completions.push(AnalysisCompletion {
                name: visible_name.clone(),
                kind: "class".to_string(),
                detail: format_class_detail(class_info),
            });
        }
        for visible_name in self.program.enums.keys() {
            completions.push(AnalysisCompletion {
                name: visible_name.clone(),
                kind: "enum".to_string(),
                detail: "Aura enum".to_string(),
            });
        }
        for visible_name in self.program.traits.keys() {
            completions.push(AnalysisCompletion {
                name: visible_name.clone(),
                kind: "trait".to_string(),
                detail: "Aura trait".to_string(),
            });
        }
        for builtin_enum in BUILTIN_ENUM_COMPLETIONS {
            completions.push(AnalysisCompletion {
                name: builtin_enum.name.to_string(),
                kind: "enum".to_string(),
                detail: builtin_enum.detail.to_string(),
            });
        }
        completions.push(AnalysisCompletion {
            name: "Array".to_string(),
            kind: "class".to_string(),
            detail: "Array[T] numeric multidimensional array".to_string(),
        });
        for (visible_name, function_info) in &self.program.functions {
            completions.push(AnalysisCompletion {
                name: visible_name.clone(),
                kind: "function".to_string(),
                detail: format_function_detail(&function_info.decl),
            });
        }
        for (visible_name, constant) in &self.program.constants {
            completions.push(AnalysisCompletion {
                name: visible_name.clone(),
                kind: "constant".to_string(),
                detail: constant.ty.to_string(),
            });
        }
        for (visible_name, function_info) in &self.program.extern_functions {
            completions.push(AnalysisCompletion {
                name: visible_name.clone(),
                kind: "function".to_string(),
                detail: format_extern_function_detail(&function_info.decl),
            });
        }
        for (visible_name, handle_info) in &self.program.opaque_handles {
            completions.push(AnalysisCompletion {
                name: visible_name.clone(),
                kind: "class".to_string(),
                detail: format_extern_opaque_detail(&handle_info.decl),
            });
        }
        for builtin in ALL_BUILTIN_FUNCTIONS {
            completions.push(AnalysisCompletion {
                name: builtin.name().to_string(),
                kind: "function".to_string(),
                detail: builtin.detail().to_string(),
            });
        }
        for (visible_name, namespace) in &self.program.imported_modules {
            completions.push(AnalysisCompletion {
                name: visible_name.clone(),
                kind: "module".to_string(),
                detail: format!("module {}", namespace.path),
            });
        }
        completions
    }

    fn member_completions(&self, receiver_type: &Type) -> Vec<AnalysisCompletion> {
        let mut completions = Vec::new();
        if let Type::Module(path) = receiver_type {
            if let Some(namespace) = self.module_namespace(path) {
                for child in namespace.modules.values() {
                    completions.push(AnalysisCompletion {
                        name: child.name.clone(),
                        kind: "module".to_string(),
                        detail: format!("module {}", child.path),
                    });
                }
                for function in namespace.functions.values() {
                    completions.push(AnalysisCompletion {
                        name: function.decl.name.clone(),
                        kind: "function".to_string(),
                        detail: format_function_detail(&function.decl),
                    });
                }
                for constant in namespace.constants.values() {
                    completions.push(AnalysisCompletion {
                        name: constant.decl.name.clone(),
                        kind: "constant".to_string(),
                        detail: constant.ty.to_string(),
                    });
                }
                for function in namespace.extern_functions.values() {
                    completions.push(AnalysisCompletion {
                        name: function.decl.name.clone(),
                        kind: "function".to_string(),
                        detail: format_extern_function_detail(&function.decl),
                    });
                }
                for handle in namespace.opaque_handles.values() {
                    completions.push(AnalysisCompletion {
                        name: handle.decl.name.clone(),
                        kind: "class".to_string(),
                        detail: format_extern_opaque_detail(&handle.decl),
                    });
                }
                for class_info in namespace.classes.values() {
                    completions.push(AnalysisCompletion {
                        name: class_info.decl.name.clone(),
                        kind: "class".to_string(),
                        detail: format_class_detail(class_info),
                    });
                }
                for enum_info in namespace.enums.values() {
                    completions.push(AnalysisCompletion {
                        name: enum_info.decl.name.clone(),
                        kind: "enum".to_string(),
                        detail: "Aura enum".to_string(),
                    });
                }
                for trait_info in namespace.traits.values() {
                    completions.push(AnalysisCompletion {
                        name: trait_info.decl.name.clone(),
                        kind: "trait".to_string(),
                        detail: "Aura trait".to_string(),
                    });
                }
            }
            return completions;
        }
        let base_name = base_type_name(receiver_type);

        if let Some(class_info) = self.class_info_for_type_name(base_name) {
            for (name, field) in &class_info.fields {
                completions.push(AnalysisCompletion {
                    name: name.clone(),
                    kind: "field".to_string(),
                    detail: field.ty.to_string(),
                });
            }
            for (name, method) in &class_info.methods {
                completions.push(AnalysisCompletion {
                    name: name.clone(),
                    kind: "method".to_string(),
                    detail: format_function_detail(&method.decl),
                });
            }
        }

        for trait_impl in self.trait_impls_in_scope() {
            if self
                .trait_impl_substitutions(trait_impl, receiver_type)
                .is_some()
            {
                for (name, method) in &trait_impl.methods {
                    if completions.iter().any(|existing| existing.name == *name) {
                        continue;
                    }
                    completions.push(AnalysisCompletion {
                        name: name.clone(),
                        kind: "method".to_string(),
                        detail: format_function_detail(&method.decl),
                    });
                }
            }
        }

        if let Some(enum_info) = self.resolve_named_enum_info(base_name) {
            let enum_name = self.canonical_enum_identity(base_name, enum_info);
            for (name, variant) in &enum_info.variants {
                completions.push(AnalysisCompletion {
                    name: name.clone(),
                    kind: "variant".to_string(),
                    detail: if variant.payloads.is_empty() {
                        format!("{} -> {}", name, enum_name)
                    } else {
                        format!(
                            "{}({}) -> {}",
                            name,
                            variant
                                .payloads
                                .iter()
                                .map(format_enum_variant_payload)
                                .collect::<Vec<_>>()
                                .join(", "),
                            enum_name
                        )
                    },
                });
            }
        }

        for builtin in builtin_enum_variant_completions(base_name) {
            completions.push(builtin);
        }
        for builtin in builtin_member_completions(receiver_type) {
            completions.push(builtin);
        }
        completions
    }

    fn trait_impls_in_scope(&self) -> impl Iterator<Item = &crate::sema::TraitImplInfo> + '_ {
        self.program.trait_impls.iter().chain(
            self.program
                .module_registry
                .values()
                .flat_map(|namespace| namespace.trait_impls.iter()),
        )
    }

    fn trait_impl_substitutions(
        &self,
        trait_impl: &crate::sema::TraitImplInfo,
        actual: &Type,
    ) -> Option<std::collections::HashMap<String, Type>> {
        let type_params = trait_impl
            .type_params
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut substitutions = std::collections::HashMap::new();
        if !crate::sema::type_pattern_matches(
            &trait_impl.for_type,
            actual,
            &type_params,
            &mut substitutions,
        ) {
            return None;
        }
        for (type_param, bounds) in &trait_impl.type_param_bounds {
            let actual_ty = substitutions.get(type_param)?;
            for bound in bounds {
                let resolved_bound = substitute_trait_bound(bound, &substitutions);
                if !self.type_implements_trait_bound(actual_ty, &resolved_bound) {
                    return None;
                }
            }
        }
        Some(substitutions)
    }

    fn trait_impl_substitutions_for_bound(
        &self,
        trait_impl: &crate::sema::TraitImplInfo,
        actual: &Type,
        bound: &TraitBound,
    ) -> Option<std::collections::HashMap<String, Type>> {
        if trait_impl.trait_name != bound.trait_name
            || trait_impl.trait_args.len() != bound.trait_args.len()
        {
            return None;
        }
        let type_params = trait_impl
            .type_params
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let mut substitutions = std::collections::HashMap::new();
        if !crate::sema::type_pattern_matches(
            &trait_impl.for_type,
            actual,
            &type_params,
            &mut substitutions,
        ) {
            return None;
        }
        for (pattern, actual_arg) in trait_impl.trait_args.iter().zip(&bound.trait_args) {
            if !crate::sema::type_pattern_matches(
                pattern,
                actual_arg,
                &type_params,
                &mut substitutions,
            ) {
                return None;
            }
        }
        for (type_param, bounds) in &trait_impl.type_param_bounds {
            let actual_ty = substitutions.get(type_param)?;
            for impl_bound in bounds {
                let resolved_bound = substitute_trait_bound(impl_bound, &substitutions);
                if !self.type_implements_trait_bound(actual_ty, &resolved_bound) {
                    return None;
                }
            }
        }
        Some(substitutions)
    }

    fn type_implements_trait_bound(&self, ty: &Type, bound: &TraitBound) -> bool {
        self.trait_impls_in_scope().any(|trait_impl| {
            self.trait_impl_substitutions_for_bound(trait_impl, ty, bound)
                .or_else(|| {
                    if bound.trait_args.is_empty() && trait_impl.trait_name == bound.trait_name {
                        self.trait_impl_substitutions(trait_impl, ty)
                    } else {
                        None
                    }
                })
                .is_some()
        })
    }

    fn trait_method_for_receiver(
        &self,
        receiver_type: &Type,
        field: &str,
    ) -> Option<(
        &crate::sema::TraitImplInfo,
        &crate::sema::TraitImplMethodInfo,
        std::collections::HashMap<String, Type>,
    )> {
        self.trait_impls_in_scope()
            .filter_map(|trait_impl| {
                self.trait_impl_substitutions(trait_impl, receiver_type)
                    .map(|substitutions| (trait_impl, substitutions))
            })
            .find_map(|(trait_impl, substitutions)| {
                trait_impl
                    .methods
                    .get(field)
                    .map(|method| (trait_impl, method, substitutions))
            })
    }

    fn module_namespace(&self, path: &str) -> Option<&crate::sema::ModuleNamespace> {
        if let Some(namespace) = self.program.module_registry.get(path) {
            return Some(namespace);
        }
        let mut segments = path.split('.');
        let first = segments.next()?;
        let mut namespace = self.program.imported_modules.get(first)?;
        for segment in segments {
            namespace = namespace.modules.get(segment)?;
        }
        Some(namespace)
    }

    fn class_info_for_type_name(&self, name: &str) -> Option<&ClassInfo> {
        self.program.classes.get(name).or_else(|| {
            let (module_path, item_name) = name.rsplit_once('.')?;
            self.program
                .module_registry
                .get(module_path)
                .or_else(|| self.module_namespace(module_path))
                .and_then(|namespace| {
                    namespace
                        .classes
                        .get(item_name)
                        .or_else(|| namespace.all_classes.get(item_name))
                })
        })
    }

    fn current_source_path(&self) -> Option<String> {
        self.program.source_path.clone()
    }

    fn module_source_path(&self, module_name: &str) -> Option<String> {
        if self.program.module_name == module_name {
            return self.current_source_path();
        }
        self.program
            .module_registry
            .get(module_name)
            .and_then(|namespace| namespace.source_path.clone())
    }

    fn definition_range(&self, module_name: &str, span: Span, len: usize) -> AnalysisRange {
        range_from_span_with_path(span, len, self.module_source_path(module_name))
    }

    fn function_definition(&self, function: &FunctionInfo) -> AnalysisRange {
        self.definition_range(
            &function.module_name,
            function.decl.span,
            function.decl.name.len(),
        )
    }

    fn constant_definition(&self, constant: &crate::sema::ConstantInfo) -> AnalysisRange {
        self.definition_range(
            &constant.module_name,
            constant.decl.span,
            constant.decl.name.len(),
        )
    }

    fn extern_function_definition(&self, function: &ExternFunctionInfo) -> AnalysisRange {
        self.definition_range(
            &function.module_name,
            function.decl.name_span,
            function.decl.name.len(),
        )
    }

    fn opaque_handle_definition(&self, handle: &OpaqueHandleInfo) -> AnalysisRange {
        self.definition_range(
            &handle.module_name,
            handle.decl.name_span,
            handle.decl.name.len(),
        )
    }

    fn class_definition(&self, class_info: &ClassInfo) -> AnalysisRange {
        self.definition_range(
            &class_info.module_name,
            class_info.decl.span,
            class_info.decl.name.len(),
        )
    }

    fn enum_definition(&self, enum_info: &EnumInfo) -> AnalysisRange {
        self.definition_range(
            &enum_info.module_name,
            enum_info.decl.span,
            enum_info.decl.name.len(),
        )
    }

    fn trait_definition(&self, trait_info: &crate::sema::TraitInfo) -> AnalysisRange {
        self.definition_range(
            &trait_info.module_name,
            trait_info.decl.span,
            trait_info.decl.name.len(),
        )
    }

    fn find_imported_module_range(&self, target_path: &str) -> Option<AnalysisRange> {
        if let Some(namespace) = self.module_namespace(target_path) {
            if let Some(file_path) = &namespace.source_path {
                return Some(AnalysisRange {
                    file_path: Some(file_path.clone()),
                    line: 0,
                    start_character: 0,
                    end_character: 0,
                });
            }
        }
        let target_segments = target_path.split('.').collect::<Vec<_>>();
        for import in &self.program.module.imports {
            let ImportKind::Module { path, .. } = &import.kind else {
                continue;
            };
            if path.len() < target_segments.len() {
                continue;
            }
            if !path
                .iter()
                .take(target_segments.len())
                .map(String::as_str)
                .eq(target_segments.iter().copied())
            {
                continue;
            }
            let line_index = import.span.line.checked_sub(1)?;
            let line = *self.source_lines.get(line_index)?;
            let token = target_segments.join(".");
            if let Some((start, end)) = line.find(&token).map(|start| (start, start + token.len()))
            {
                return Some(AnalysisRange {
                    file_path: self.current_source_path(),
                    line: line_index,
                    start_character: start,
                    end_character: end,
                });
            }
        }
        None
    }

    fn resolve_named_enum_info(&self, name: &str) -> Option<&EnumInfo> {
        if let Some((module_path, item_name)) = name.rsplit_once('.') {
            return self
                .program
                .module_registry
                .get(module_path)
                .or_else(|| self.module_namespace(module_path))
                .and_then(|namespace| {
                    namespace
                        .enums
                        .get(item_name)
                        .or_else(|| namespace.all_enums.get(item_name))
                });
        }
        self.program.enums.get(name)
    }

    fn canonical_enum_identity(&self, surface_name: &str, enum_info: &EnumInfo) -> String {
        self.program
            .canonical_type_names
            .get(surface_name)
            .cloned()
            .unwrap_or_else(|| {
                if surface_name.contains('.') || enum_info.module_name == self.program.module_name {
                    surface_name.to_string()
                } else {
                    format!("{}.{}", enum_info.module_name, enum_info.decl.name)
                }
            })
    }

    fn resolve_match_variant_enum(&self, enum_name: &str) -> Option<ResolvedSymbol> {
        match enum_name {
            "Option" => Some(ResolvedSymbol {
                hover: builtin_enum_hover(
                    "Option[T]",
                    "Optional values with `Some(T)` and `None`.",
                ),
                definition: None,
            }),
            "Result" => Some(ResolvedSymbol {
                hover: builtin_enum_hover(
                    "Result[T, E]",
                    "Success-or-error values with `Ok(T)` and `Err(E)`.",
                ),
                definition: None,
            }),
            "SendError" => Some(ResolvedSymbol {
                hover: builtin_enum_hover(
                    "SendError[T]",
                    "Queue send failures that preserve the unsent value.",
                ),
                definition: None,
            }),
            _ => self
                .resolve_named_enum_info(enum_name)
                .map(|enum_info| ResolvedSymbol {
                    hover: format_enum_hover_named(
                        &self.canonical_enum_identity(enum_name, enum_info),
                    ),
                    definition: Some(self.enum_definition(enum_info)),
                }),
        }
    }

    fn resolve_match_variant(
        &self,
        scrutinee_type: Option<&Type>,
        variant: &VariantPattern,
    ) -> Option<ResolvedSymbol> {
        if let Some(ty) = scrutinee_type {
            if let Some(resolved) = match (base_type_name(ty), variant.variant_name.as_str()) {
                ("Option", "Some") => Some(ResolvedSymbol {
                    hover: format_variant_hover("Option", "Some", ty.type_arguments().first()),
                    definition: None,
                }),
                ("Option", "None") => Some(ResolvedSymbol {
                    hover: format_variant_hover("Option", "None", None),
                    definition: None,
                }),
                ("Result", "Ok") => Some(ResolvedSymbol {
                    hover: format_variant_hover("Result", "Ok", ty.type_arguments().first()),
                    definition: None,
                }),
                ("Result", "Err") => Some(ResolvedSymbol {
                    hover: format_variant_hover("Result", "Err", ty.type_arguments().get(1)),
                    definition: None,
                }),
                ("SendError", "Closed" | "Cancelled") => Some(ResolvedSymbol {
                    hover: format_variant_hover(
                        "SendError",
                        variant.variant_name.as_str(),
                        ty.type_arguments().first(),
                    ),
                    definition: None,
                }),
                _ => None,
            } {
                return Some(resolved);
            }
        }

        let enum_name = variant
            .enum_name
            .as_deref()
            .or_else(|| scrutinee_type.map(base_type_name))?;
        let enum_info = self.resolve_named_enum_info(enum_name)?;
        let canonical_enum_name = self.canonical_enum_identity(enum_name, enum_info);
        let variant_decl = enum_info
            .decl
            .variants
            .iter()
            .find(|decl| decl.name == variant.variant_name)?;
        let variant_info = enum_info.variants.get(&variant.variant_name)?;
        Some(ResolvedSymbol {
            hover: format_variant_hover_payloads(
                &canonical_enum_name,
                &variant.variant_name,
                variant_info
                    .payloads
                    .iter()
                    .map(format_enum_variant_payload),
            ),
            definition: Some(self.definition_range(
                &enum_info.module_name,
                variant_decl.span,
                variant_decl.name.len(),
            )),
        })
    }

    fn trait_bound_member_completions(&self, bounds: &[TraitBound]) -> Vec<AnalysisCompletion> {
        let mut completions = Vec::new();
        for bound in bounds {
            let Some(trait_info) = self.program.traits.get(&bound.trait_name) else {
                continue;
            };
            for method in trait_info.methods.values() {
                if completions
                    .iter()
                    .any(|existing: &AnalysisCompletion| existing.name == method.decl.name)
                {
                    continue;
                }
                completions.push(AnalysisCompletion {
                    name: method.decl.name.clone(),
                    kind: "method".to_string(),
                    detail: format_function_detail(&method.decl),
                });
            }
        }
        completions
    }

    fn visit_stmts(&mut self, stmts: &[Stmt], scope: &mut BTreeMap<String, BindingInfo>) {
        for stmt in stmts {
            self.visit_stmt(stmt, scope);
        }
    }

    fn visit_stmt(&mut self, stmt: &Stmt, scope: &mut BTreeMap<String, BindingInfo>) {
        match stmt {
            Stmt::Assign(assign) => self.visit_assign(assign, scope),
            Stmt::View(view) => {
                self.visit_expr(&view.source, scope);
                let ty = self
                    .infer_expr_type(&view.source, scope)
                    .unwrap_or(Type::Unit);
                self.bind_view_value(view, ty, scope);
            }
            Stmt::Return(ret) => {
                if let Some(value) = &ret.value {
                    self.visit_expr(value, scope);
                }
            }
            Stmt::Assert(assert_stmt) => {
                self.visit_expr(&assert_stmt.condition, scope);
                if let Some(message) = &assert_stmt.message {
                    self.visit_expr(message, scope);
                }
            }
            Stmt::If(if_stmt) => {
                for branch in &if_stmt.branches {
                    self.visit_expr(&branch.condition, scope);
                    let mut branch_scope = scope.clone();
                    self.visit_stmts(&branch.body, &mut branch_scope);
                }
                if let Some(body) = &if_stmt.else_body {
                    let mut else_scope = scope.clone();
                    self.visit_stmts(body, &mut else_scope);
                }
            }
            Stmt::Match(match_stmt) => {
                self.visit_expr(&match_stmt.scrutinee, scope);
                let scrutinee_type = self.infer_expr_type(&match_stmt.scrutinee, scope);
                for arm in &match_stmt.arms {
                    let mut arm_scope = scope.clone();
                    self.visit_match_arm_pattern(arm, scrutinee_type.as_ref());
                    self.bind_match_arm(arm, scrutinee_type.as_ref(), &mut arm_scope);
                    if let Some(guard) = &arm.guard {
                        self.visit_expr(guard, &arm_scope);
                    }
                    self.visit_stmts(&arm.body, &mut arm_scope);
                }
            }
            Stmt::For(for_stmt) => {
                self.visit_expr(&for_stmt.iterable, scope);
                let mut body_scope = scope.clone();
                let binding_ty = self
                    .infer_iterable_binding_type(&for_stmt.iterable, scope)
                    .unwrap_or(Type::Unit);
                self.bind_target_value(
                    &for_stmt.target,
                    &binding_ty,
                    for_stmt.span.line,
                    "local",
                    &mut body_scope,
                );
                self.visit_stmts(&for_stmt.body, &mut body_scope);
            }
            Stmt::Destructure(destructure) => {
                self.visit_expr(&destructure.value, scope);
                let ty = self
                    .infer_expr_type(&destructure.value, scope)
                    .unwrap_or(Type::Unit);
                self.bind_target_value(
                    &destructure.target,
                    &ty,
                    destructure.span.line,
                    "binding",
                    scope,
                );
            }
            Stmt::With(with_stmt) => {
                self.visit_expr(&with_stmt.value, scope);
                let mut body_scope = scope.clone();
                let binding_ty = self
                    .infer_expr_type(&with_stmt.value, scope)
                    .unwrap_or(Type::Unit);
                self.bind_named_value(
                    &with_stmt.binding,
                    binding_ty,
                    with_stmt.span.line,
                    "local",
                    &mut body_scope,
                );
                self.visit_stmts(&with_stmt.body, &mut body_scope);
            }
            Stmt::While(while_stmt) => {
                self.visit_expr(&while_stmt.condition, scope);
                let mut loop_scope = scope.clone();
                self.visit_stmts(&while_stmt.body, &mut loop_scope);
            }
            Stmt::Pass(_) | Stmt::Break(_) | Stmt::Continue(_) => {}
            Stmt::Expr(expr_stmt) => self.visit_expr(&expr_stmt.expr, scope),
        }
    }

    fn visit_assign(&mut self, assign: &AssignStmt, scope: &mut BTreeMap<String, BindingInfo>) {
        self.visit_expr(&assign.value, scope);

        match &assign.target {
            AssignTarget::Name(name) => {
                if let Some(existing) = scope.get(name) {
                    self.push_occurrence(
                        self.find_identifier_range(assign.span.line, name)
                            .unwrap_or_else(|| range_from_span(assign.span, name.len())),
                        existing.hover.clone(),
                        Some(existing.definition.clone()),
                    );
                    return;
                }

                let inferred_ty = self.infer_expr_type(&assign.value, scope);
                let binding_ty = match inferred_ty {
                    Some(ty @ Type::Closure { .. }) => ty,
                    inferred_ty => assign
                        .annotation
                        .as_ref()
                        .map(lower_type_ref)
                        .or(inferred_ty)
                        .unwrap_or(Type::Unit),
                };
                self.bind_named_value(name, binding_ty, assign.span.line, "binding", scope);
            }
            AssignTarget::Member { object, field } => {
                self.visit_expr(object, scope);
                if let Some(member) = self.resolve_member_expr(object, field, scope) {
                    if let Some(range) = self.find_identifier_range(assign.span.line, field) {
                        self.push_occurrence(range, member.hover, member.definition);
                    }
                }
            }
            AssignTarget::Index { object, index } => {
                self.visit_expr(object, scope);
                self.visit_expr(index, scope);
            }
        }
    }

    fn bind_match_arm(
        &mut self,
        arm: &MatchArm,
        scrutinee_type: Option<&Type>,
        scope: &mut BTreeMap<String, BindingInfo>,
    ) {
        let mut bindings = Vec::new();
        self.collect_match_pattern_bindings(&arm.pattern, scrutinee_type, &mut bindings);
        for (name, ty, line) in bindings {
            self.bind_named_value(&name, ty, line, "local", scope);
        }
    }

    fn bind_match_arm_scope(
        &self,
        arm: &MatchArm,
        scrutinee_type: Option<&Type>,
        scope: &mut BTreeMap<String, BindingInfo>,
    ) {
        let mut bindings = Vec::new();
        self.collect_match_pattern_bindings(&arm.pattern, scrutinee_type, &mut bindings);
        for (name, ty, line) in bindings {
            self.insert_scope_binding(&name, ty, line, "local", scope);
        }
    }

    fn collect_match_pattern_bindings(
        &self,
        pattern: &Pattern,
        expected_type: Option<&Type>,
        bindings: &mut Vec<(String, Type, usize)>,
    ) {
        match pattern {
            Pattern::Or(pattern) => {
                // Every alternative has the same checked binding set. Record
                // the first once so completion/hover expose one logical arm
                // scope rather than duplicate definitions.
                if let Some(alternative) = pattern.alternatives.first() {
                    self.collect_match_pattern_bindings(alternative, expected_type, bindings);
                }
            }
            Pattern::Binding(binding) => bindings.push((
                binding.name.clone(),
                expected_type.cloned().unwrap_or(Type::Unit),
                binding.span.line,
            )),
            Pattern::Tuple(tuple) => {
                let element_types = match expected_type {
                    Some(Type::Tuple(elements)) => Some(elements.as_slice()),
                    _ => None,
                };
                for (index, element) in tuple.elements.iter().enumerate() {
                    self.collect_match_pattern_bindings(
                        element,
                        element_types.and_then(|types| types.get(index)),
                        bindings,
                    );
                }
            }
            Pattern::Variant(variant) => {
                let payload_types = self.match_binding_types(
                    expected_type,
                    variant.enum_name.as_deref(),
                    &variant.variant_name,
                );
                for (index, subpattern) in variant.subpatterns.iter().enumerate() {
                    self.collect_match_pattern_bindings(
                        subpattern,
                        payload_types.get(index),
                        bindings,
                    );
                }
            }
            Pattern::Literal(_) | Pattern::Wildcard(_) => {}
        }
    }

    fn visit_match_arm_pattern(&mut self, arm: &MatchArm, scrutinee_type: Option<&Type>) {
        self.visit_match_pattern_occurrences(&arm.pattern, scrutinee_type);
    }

    fn visit_match_pattern_occurrences(&mut self, pattern: &Pattern, expected_type: Option<&Type>) {
        match pattern {
            Pattern::Or(pattern) => {
                for alternative in &pattern.alternatives {
                    self.visit_match_pattern_occurrences(alternative, expected_type);
                }
            }
            Pattern::Tuple(tuple) => {
                let element_types = match expected_type {
                    Some(Type::Tuple(elements)) => Some(elements.as_slice()),
                    _ => None,
                };
                for (index, element) in tuple.elements.iter().enumerate() {
                    self.visit_match_pattern_occurrences(
                        element,
                        element_types.and_then(|types| types.get(index)),
                    );
                }
            }
            Pattern::Variant(variant) => {
                if let Some(resolved) = self.resolve_match_variant(expected_type, variant) {
                    if let Some(range) = self.find_match_variant_range(variant.span.line, variant) {
                        self.push_occurrence(range, resolved.hover, resolved.definition);
                    }
                }
                if let Some(enum_name) = &variant.enum_name {
                    if let Some(resolved_enum) = self.resolve_match_variant_enum(enum_name) {
                        if let Some(range) =
                            self.find_match_enum_range(variant.span.line, enum_name)
                        {
                            self.push_occurrence(
                                range,
                                resolved_enum.hover,
                                resolved_enum.definition,
                            );
                        }
                    }
                }
                let payload_types = self.match_binding_types(
                    expected_type,
                    variant.enum_name.as_deref(),
                    &variant.variant_name,
                );
                for (index, subpattern) in variant.subpatterns.iter().enumerate() {
                    self.visit_match_pattern_occurrences(subpattern, payload_types.get(index));
                }
            }
            Pattern::Binding(_) | Pattern::Literal(_) | Pattern::Wildcard(_) => {}
        }
    }

    fn bind_named_value(
        &mut self,
        name: &str,
        ty: Type,
        line: usize,
        kind: &str,
        scope: &mut BTreeMap<String, BindingInfo>,
    ) {
        let definition = self
            .find_identifier_range(line, name)
            .unwrap_or(AnalysisRange {
                file_path: self.current_source_path(),
                line: line.saturating_sub(1),
                start_character: 0,
                end_character: name.len(),
            });
        let hover = format_value_hover(kind, name, &ty);
        scope.insert(
            name.to_string(),
            BindingInfo {
                ty,
                trait_bounds: Vec::new(),
                definition: definition.clone(),
                hover: hover.clone(),
            },
        );
        self.push_occurrence(definition.clone(), hover, Some(definition));
    }

    fn bind_view_value(
        &mut self,
        view: &ViewStmt,
        ty: Type,
        scope: &mut BTreeMap<String, BindingInfo>,
    ) {
        let declaration = self
            .find_identifier_range(view.span.line, &view.name)
            .unwrap_or_else(|| range_from_span(view.span, view.name.len()));
        let source = render_view_source(&view.source).unwrap_or_else(|| "<place>".to_string());
        let definition = view_source_root(&view.source)
            .and_then(|root| scope.get(root))
            .map(|binding| binding.definition.clone())
            .unwrap_or_else(|| declaration.clone());
        let hover = format!(
            "```aura\nview {}{}: {} from {}\n```",
            if view.mutable { "mut " } else { "" },
            view.name,
            ty,
            source
        );
        scope.insert(
            view.name.clone(),
            BindingInfo {
                ty,
                trait_bounds: Vec::new(),
                definition: definition.clone(),
                hover: hover.clone(),
            },
        );
        self.push_occurrence(declaration, hover, Some(definition));
    }

    fn bind_target_value(
        &mut self,
        target: &crate::ast::BindingTarget,
        ty: &Type,
        line: usize,
        kind: &str,
        scope: &mut BTreeMap<String, BindingInfo>,
    ) {
        match (target, ty) {
            (crate::ast::BindingTarget::Name { name, .. }, ty) => {
                self.bind_named_value(name, ty.clone(), line, kind, scope)
            }
            (crate::ast::BindingTarget::Tuple { elements, .. }, Type::Tuple(element_types)) => {
                for (element, element_ty) in elements.iter().zip(element_types) {
                    self.bind_target_value(element, element_ty, line, kind, scope);
                }
            }
            _ => {}
        }
    }

    fn bind_target_value_exact(
        &mut self,
        target: &crate::ast::BindingTarget,
        ty: &Type,
        kind: &str,
        scope: &mut BTreeMap<String, BindingInfo>,
    ) {
        match (target, ty) {
            (crate::ast::BindingTarget::Name { name, span }, ty) => {
                let definition =
                    range_from_span_with_path(*span, name.len(), self.current_source_path());
                let hover = format_value_hover(kind, name, ty);
                scope.insert(
                    name.clone(),
                    BindingInfo {
                        ty: ty.clone(),
                        trait_bounds: Vec::new(),
                        definition: definition.clone(),
                        hover: hover.clone(),
                    },
                );
                self.push_occurrence(definition.clone(), hover, Some(definition));
            }
            (crate::ast::BindingTarget::Tuple { elements, .. }, Type::Tuple(element_types)) => {
                for (element, element_ty) in elements.iter().zip(element_types) {
                    self.bind_target_value_exact(element, element_ty, kind, scope);
                }
            }
            _ => {}
        }
    }

    fn visit_expr(&mut self, expr: &Expr, scope: &BTreeMap<String, BindingInfo>) {
        match &expr.kind {
            ExprKind::Membership {
                value, container, ..
            } => {
                self.visit_expr(value, scope);
                self.visit_expr(container, scope);
            }
            ExprKind::CompareChain { first, links } => {
                self.visit_expr(first, scope);
                for link in links {
                    self.visit_expr(&link.operand, scope);
                }
            }
            ExprKind::Name(name) => {
                if let Some(resolved) = self.resolve_name(name, scope) {
                    self.push_occurrence(
                        range_from_span(expr.span, name.len()),
                        resolved.hover,
                        resolved.definition,
                    );
                }
            }
            ExprKind::Member { object, field } => {
                self.visit_expr(object, scope);
                if let Some(resolved) = self.resolve_member_expr(object, field, scope) {
                    self.push_occurrence(
                        range_from_span(expr.span, field.len()),
                        resolved.hover,
                        resolved.definition,
                    );
                }
            }
            ExprKind::Specialize { expr, .. } => self.visit_expr(expr, scope),
            ExprKind::Call { callee, args } => {
                self.visit_expr(callee, scope);
                for arg in args {
                    self.visit_expr(&arg.value, scope);
                }
            }
            ExprKind::FString(parts) => {
                for part in parts {
                    match part {
                        crate::ast::FormatPart::Expr(expr)
                        | crate::ast::FormatPart::Formatted { expr, .. } => {
                            self.visit_expr(expr, scope);
                        }
                        crate::ast::FormatPart::Literal(_) => {}
                    }
                }
            }
            ExprKind::Binary { left, right, .. } => {
                self.visit_expr(left, scope);
                self.visit_expr(right, scope);
            }
            ExprKind::Conditional {
                then_expr,
                condition,
                else_expr,
            } => {
                self.visit_expr(then_expr, scope);
                self.visit_expr(condition, scope);
                self.visit_expr(else_expr, scope);
            }
            ExprKind::Comprehension { output, clauses } => {
                let mut comprehension_scope = scope.clone();
                let checked_clause_types = self
                    .comprehension_info(expr)
                    .map(|info| {
                        info.clauses
                            .iter()
                            .map(|clause| clause.binding_type.clone())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                for (clause_index, clause) in clauses.iter().enumerate() {
                    self.visit_expr(&clause.iterable, &comprehension_scope);
                    let binding_ty = checked_clause_types
                        .get(clause_index)
                        .cloned()
                        .unwrap_or(Type::Unit);
                    self.bind_target_value_exact(
                        &clause.target,
                        &binding_ty,
                        "local",
                        &mut comprehension_scope,
                    );
                    for filter in &clause.filters {
                        self.visit_expr(filter, &comprehension_scope);
                    }
                }
                self.visit_comprehension_output(output, &comprehension_scope);
            }
            ExprKind::Lambda { params, body, .. } => {
                let mut lambda_scope = scope.clone();
                if let Some(contracts) = self.closure_info(expr).map(|info| info.params.clone()) {
                    for (param, contract) in params.iter().zip(&contracts) {
                        let definition = range_from_span(param.span, param.name.len());
                        let binding = BindingInfo {
                            ty: contract.ty.clone(),
                            trait_bounds: Vec::new(),
                            definition: definition.clone(),
                            hover: format_lambda_param_hover(param, &contract.ty),
                        };
                        self.push_occurrence(
                            definition,
                            binding.hover.clone(),
                            Some(binding.definition.clone()),
                        );
                        lambda_scope.insert(param.name.clone(), binding);
                    }
                }
                self.visit_expr(body, &lambda_scope);
            }
            ExprKind::Cast { expr, .. } => self.visit_expr(expr, scope),
            ExprKind::Unary { expr, .. } => self.visit_expr(expr, scope),
            ExprKind::Try(inner) | ExprKind::Group(inner) => self.visit_expr(inner, scope),
            ExprKind::Tuple(elements) | ExprKind::List(elements) | ExprKind::Set(elements) => {
                for element in elements {
                    self.visit_expr(element, scope);
                }
            }
            ExprKind::Map(entries) => {
                for entry in entries {
                    self.visit_expr(&entry.key, scope);
                    self.visit_expr(&entry.value, scope);
                }
            }
            ExprKind::Index { object, index } => {
                self.visit_expr(object, scope);
                self.visit_expr(index, scope);
            }
            ExprKind::Slice {
                object, start, end, ..
            } => {
                self.visit_expr(object, scope);
                if let Some(start) = start {
                    self.visit_expr(start, scope);
                }
                if let Some(end) = end {
                    self.visit_expr(end, scope);
                }
            }
            ExprKind::Match {
                scrutinee, arms, ..
            } => {
                self.visit_expr(scrutinee, scope);
                let scrutinee_type = self.infer_expr_type(scrutinee, scope);
                for arm in arms {
                    let mut arm_scope = scope.clone();
                    self.visit_match_pattern_occurrences(&arm.pattern, scrutinee_type.as_ref());
                    let mut bindings = Vec::new();
                    self.collect_match_pattern_bindings(
                        &arm.pattern,
                        scrutinee_type.as_ref(),
                        &mut bindings,
                    );
                    for (name, ty, line) in bindings {
                        self.bind_named_value(&name, ty, line, "local", &mut arm_scope);
                    }
                    if let Some(guard) = &arm.guard {
                        self.visit_expr(guard, &arm_scope);
                    }
                    self.visit_expr(&arm.value, &arm_scope);
                }
            }
            ExprKind::Int(_)
            | ExprKind::DurationNanos(_)
            | ExprKind::BuiltinOmitted
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::String(_) => {}
        }
    }

    fn visit_comprehension_output(
        &mut self,
        output: &ComprehensionOutput,
        scope: &BTreeMap<String, BindingInfo>,
    ) {
        match output {
            ComprehensionOutput::List(value) | ComprehensionOutput::Set(value) => {
                self.visit_expr(value, scope);
            }
            ComprehensionOutput::Map { key, value } => {
                self.visit_expr(key, scope);
                self.visit_expr(value, scope);
            }
        }
    }

    fn resolve_name(
        &self,
        name: &str,
        scope: &BTreeMap<String, BindingInfo>,
    ) -> Option<ResolvedSymbol> {
        if let Some(binding) = scope.get(name) {
            return Some(ResolvedSymbol {
                hover: binding.hover.clone(),
                definition: Some(binding.definition.clone()),
            });
        }

        if let Some(constant) = self.program.constants.get(name) {
            return Some(ResolvedSymbol {
                hover: append_alias_target(
                    format_value_hover("module constant", name, &constant.ty),
                    name,
                    &constant.module_name,
                    &constant.decl.name,
                ),
                definition: Some(self.constant_definition(constant)),
            });
        }

        if let Some(function) = self.program.functions.get(name) {
            return Some(ResolvedSymbol {
                hover: append_alias_target(
                    format_function_hover(&function.decl),
                    name,
                    &function.module_name,
                    &function.decl.name,
                ),
                definition: Some(self.function_definition(function)),
            });
        }

        if let Some(function) = self.program.extern_functions.get(name) {
            return Some(ResolvedSymbol {
                hover: append_alias_target(
                    format_extern_function_hover(&function.decl),
                    name,
                    &function.module_name,
                    &function.decl.name,
                ),
                definition: Some(self.extern_function_definition(function)),
            });
        }

        if let Some(handle) = self.program.opaque_handles.get(name) {
            return Some(ResolvedSymbol {
                hover: append_alias_target(
                    format_extern_opaque_hover(&handle.decl),
                    name,
                    &handle.module_name,
                    &handle.decl.name,
                ),
                definition: Some(self.opaque_handle_definition(handle)),
            });
        }

        if let Some(class_info) = self.program.classes.get(name) {
            return Some(ResolvedSymbol {
                hover: append_alias_target(
                    format_class_hover(class_info),
                    name,
                    &class_info.module_name,
                    &class_info.decl.name,
                ),
                definition: Some(self.class_definition(class_info)),
            });
        }

        if let Some(enum_info) = self.program.enums.get(name) {
            return Some(ResolvedSymbol {
                hover: append_alias_target(
                    format_enum_hover_named(&self.canonical_enum_identity(name, enum_info)),
                    name,
                    &enum_info.module_name,
                    &enum_info.decl.name,
                ),
                definition: Some(self.enum_definition(enum_info)),
            });
        }

        if let Some(trait_info) = self.program.traits.get(name) {
            return Some(ResolvedSymbol {
                hover: append_alias_target(
                    format!("```aura\ntrait {}\n```", trait_info.decl.name),
                    name,
                    &trait_info.module_name,
                    &trait_info.decl.name,
                ),
                definition: Some(self.trait_definition(trait_info)),
            });
        }

        if let Some(builtin) = BuiltinFunction::from_name(name) {
            return Some(ResolvedSymbol {
                hover: builtin_function_hover(builtin.detail(), builtin.docs()),
                definition: None,
            });
        }

        if let Some(namespace) = self.program.imported_modules.get(name) {
            return Some(ResolvedSymbol {
                hover: if namespace.path == name {
                    format!("```aura\nmodule {}\n```", namespace.path)
                } else {
                    format!("```aura\nmodule {name} = {}\n```", namespace.path)
                },
                definition: self.find_imported_module_range(&namespace.path),
            });
        }

        match name {
            "Array" => Some(ResolvedSymbol {
                hover: builtin_type_hover(
                    "Array[T]",
                    "An owned multidimensional Array with dtype int32, int64, float32, or float64.",
                ),
                definition: None,
            }),
            "Duration" => Some(ResolvedSymbol {
                hover: builtin_type_hover(
                    "Duration",
                    "A signed nanosecond-precision duration value.",
                ),
                definition: None,
            }),
            "Option" => Some(ResolvedSymbol {
                hover: builtin_enum_hover(
                    "Option[T]",
                    "Optional values with `Some(T)` and `None`.",
                ),
                definition: None,
            }),
            "Result" => Some(ResolvedSymbol {
                hover: builtin_enum_hover(
                    "Result[T, E]",
                    "Success-or-error values with `Ok(T)` and `Err(E)`.",
                ),
                definition: None,
            }),
            "SendError" => Some(ResolvedSymbol {
                hover: builtin_enum_hover(
                    "SendError[T]",
                    "Queue send failures that preserve the unsent value.",
                ),
                definition: None,
            }),
            _ => None,
        }
    }

    fn resolve_member_expr(
        &self,
        object: &Expr,
        field: &str,
        scope: &BTreeMap<String, BindingInfo>,
    ) -> Option<ResolvedMember> {
        if let ExprKind::Specialize {
            expr: specialized,
            type_args,
        } = &object.kind
        {
            if matches!(&specialized.kind, ExprKind::Name(name) if name == "Array")
                && type_args.len() == 1
            {
                let associated = BuiltinAssociatedFunction::resolve("Array", field)?;
                let dtype = self.lower_analysis_type_ref(&type_args[0]);
                return Some(ResolvedMember {
                    hover: builtin_function_hover(associated.detail(), associated.docs()),
                    definition: None,
                    ty: Some(Type::Named("Array".to_string(), vec![dtype])),
                });
            }
        }
        if let ExprKind::Name(type_name) = &object.kind {
            if !scope.contains_key(type_name) {
                let associated_ty = match (type_name.as_str(), field) {
                    ("Duration", "ms" | "seconds" | "minutes") => Some(Type::named("Duration")),
                    ("str", "from_bytes") => Some(Type::Named(
                        "Result".to_string(),
                        vec![Type::named("str"), Type::named("bytes.Error")],
                    )),
                    _ => None,
                };
                if let Some(ty) = associated_ty {
                    let associated = BuiltinAssociatedFunction::resolve(type_name, field)
                        .expect("recognized associated member should resolve");
                    return Some(ResolvedMember {
                        hover: builtin_function_hover(associated.detail(), associated.docs()),
                        definition: None,
                        ty: Some(ty),
                    });
                }
            }
        }
        let receiver_type = self.infer_expr_type(object, scope)?;
        self.resolve_member_type(&receiver_type, field)
    }

    fn resolve_member_type(&self, receiver_type: &Type, field: &str) -> Option<ResolvedMember> {
        if let Type::Module(path) = receiver_type {
            let namespace = self.module_namespace(path)?;
            if let Some(child) = namespace.modules.get(field) {
                return Some(ResolvedMember {
                    hover: format!("```aura\nmodule {}\n```", child.path),
                    definition: self.find_imported_module_range(&child.path),
                    ty: Some(Type::Module(child.path.clone())),
                });
            }
            if let Some(constant) = namespace.constants.get(field) {
                return Some(ResolvedMember {
                    hover: format_value_hover("module constant", field, &constant.ty),
                    definition: Some(self.constant_definition(constant)),
                    ty: Some(constant.ty.clone()),
                });
            }
            if let Some(function) = namespace.functions.get(field) {
                return Some(ResolvedMember {
                    hover: format_function_hover(&function.decl),
                    definition: Some(self.function_definition(function)),
                    ty: Some(Type::Function {
                        params: function
                            .decl
                            .params
                            .iter()
                            .zip(&function.signature.params)
                            .zip(&function.signature.param_passings)
                            .map(|((decl, ty), passing)| FunctionParamContract {
                                name: decl.name.clone(),
                                ty: ty.clone(),
                                passing: *passing,
                                has_default: decl.default.is_some(),
                                default_erased: false,
                            })
                            .collect(),
                        return_type: Box::new(function.signature.return_type.clone()),
                    }),
                });
            }
            if let Some(function) = namespace.extern_functions.get(field) {
                return Some(ResolvedMember {
                    hover: format_extern_function_hover(&function.decl),
                    definition: Some(self.extern_function_definition(function)),
                    ty: Some(Type::Function {
                        params: function
                            .decl
                            .params
                            .iter()
                            .zip(&function.signature.params)
                            .zip(&function.signature.param_passings)
                            .map(|((decl, ty), passing)| FunctionParamContract {
                                name: decl.name.clone(),
                                ty: ty.clone(),
                                passing: *passing,
                                has_default: false,
                                default_erased: false,
                            })
                            .collect(),
                        return_type: Box::new(function.signature.return_type.clone()),
                    }),
                });
            }
            if let Some(handle) = namespace.opaque_handles.get(field) {
                return Some(ResolvedMember {
                    hover: format_extern_opaque_hover(&handle.decl),
                    definition: Some(self.opaque_handle_definition(handle)),
                    ty: Some(Type::named(format!(
                        "{}.{}",
                        namespace.path, handle.decl.name
                    ))),
                });
            }
            if let Some(class_info) = namespace.classes.get(field) {
                return Some(ResolvedMember {
                    hover: format_class_hover(class_info),
                    definition: Some(self.class_definition(class_info)),
                    ty: Some(self.analysis_class_type(
                        &format!("{}.{}", namespace.path, class_info.decl.name),
                        class_info,
                        Vec::new(),
                    )),
                });
            }
            if let Some(enum_info) = namespace.enums.get(field) {
                let enum_name = format!("{}.{}", namespace.path, enum_info.decl.name);
                return Some(ResolvedMember {
                    hover: format_enum_hover_named(&enum_name),
                    definition: Some(self.enum_definition(enum_info)),
                    ty: Some(Type::named(enum_name)),
                });
            }
            if let Some(trait_info) = namespace.traits.get(field) {
                return Some(ResolvedMember {
                    hover: format!("```aura\ntrait {}\n```", trait_info.decl.name),
                    definition: Some(self.trait_definition(trait_info)),
                    ty: None,
                });
            }
            return None;
        }

        let base_name = base_type_name(receiver_type);
        if let Some(class_info) = self.class_info_for_type_name(base_name) {
            if let Some(field_info) = class_info.fields.get(field) {
                return Some(ResolvedMember {
                    hover: format_value_hover("field", field, &field_info.ty),
                    definition: Some(self.definition_range(
                        &class_info.module_name,
                        field_info.span,
                        field.len(),
                    )),
                    ty: Some(field_info.ty.clone()),
                });
            }
            if let Some(method_info) = class_info.methods.get(field) {
                return Some(ResolvedMember {
                    hover: format_method_hover(&method_info.decl),
                    definition: Some(self.definition_range(
                        &class_info.module_name,
                        method_info.decl.span,
                        method_info.decl.name.len(),
                    )),
                    ty: Some(method_info.signature.return_type.clone()),
                });
            }
        }

        if let Some((trait_impl, trait_method, substitutions)) =
            self.trait_method_for_receiver(receiver_type, field)
        {
            return Some(ResolvedMember {
                hover: format_method_hover(&trait_method.decl),
                definition: Some(self.definition_range(
                    &trait_impl.module_name,
                    trait_method.decl.span,
                    trait_method.decl.name.len(),
                )),
                ty: Some(crate::sema::substitute_type(
                    &trait_method.signature.return_type,
                    &substitutions,
                )),
            });
        }

        if let Some(enum_info) = self.resolve_named_enum_info(base_name) {
            if let Some(variant_info) = enum_info.variants.get(field) {
                let enum_name = self.canonical_enum_identity(base_name, enum_info);
                return Some(ResolvedMember {
                    hover: format_variant_hover_payloads(
                        &enum_name,
                        field,
                        variant_info
                            .payloads
                            .iter()
                            .map(format_enum_variant_payload),
                    ),
                    definition: Some(self.definition_range(
                        &enum_info.module_name,
                        variant_info.span,
                        field.len(),
                    )),
                    ty: Some(Type::named(enum_name)),
                });
            }
        }

        if let Some(builtin_member) = BuiltinMember::resolve(base_name, field) {
            let ty =
                match builtin_member {
                    BuiltinMember::FloatSqrt
                    | BuiltinMember::IntegerToFloat
                    | BuiltinMember::DurationToMilliseconds
                    | BuiltinMember::DurationToSeconds => Some(Type::named("float64")),
                    BuiltinMember::IntegerWrappingAdd
                    | BuiltinMember::IntegerWrappingSub
                    | BuiltinMember::IntegerWrappingMul
                    | BuiltinMember::IntegerSaturatingAdd
                    | BuiltinMember::IntegerSaturatingSub
                    | BuiltinMember::IntegerSaturatingMul
                    | BuiltinMember::IntegerWrappingShl
                    | BuiltinMember::IntegerWrappingShr
                    | BuiltinMember::IntegerSaturatingShl
                    | BuiltinMember::IntegerSaturatingShr => Some(receiver_type.clone()),
                    BuiltinMember::StringLen | BuiltinMember::StringByteLen => {
                        Some(Type::named("int64"))
                    }
                    BuiltinMember::StringToBytes => {
                        Some(Type::Named("list".to_string(), vec![Type::named("uint8")]))
                    }
                    BuiltinMember::StringContains
                    | BuiltinMember::StringStartsWith
                    | BuiltinMember::StringEndsWith => Some(Type::named("bool")),
                    BuiltinMember::StringSplit => {
                        Some(Type::Named("list".to_string(), vec![Type::named("str")]))
                    }
                    BuiltinMember::StringReplace
                    | BuiltinMember::StringToLower
                    | BuiltinMember::StringToUpper
                    | BuiltinMember::StringTrim
                    | BuiltinMember::StringJoin
                    | BuiltinMember::ScalarToString => Some(Type::named("str")),
                    BuiltinMember::StringStripPrefix | BuiltinMember::StringStripSuffix => {
                        Some(Type::Named("Option".to_string(), vec![Type::named("str")]))
                    }
                    BuiltinMember::ArrayShape => {
                        Some(Type::Named("list".to_string(), vec![Type::named("int64")]))
                    }
                    BuiltinMember::ArrayLen => Some(Type::named("int64")),
                    BuiltinMember::ArrayClone
                    | BuiltinMember::ArrayWrappingAdd
                    | BuiltinMember::ArrayWrappingSub
                    | BuiltinMember::ArrayWrappingMul
                    | BuiltinMember::ArraySaturatingAdd
                    | BuiltinMember::ArraySaturatingSub
                    | BuiltinMember::ArraySaturatingMul => Some(receiver_type.clone()),
                    BuiltinMember::ArrayGet | BuiltinMember::ArraySet => {
                        let payload = receiver_type
                            .type_arguments()
                            .first()
                            .cloned()
                            .unwrap_or(Type::Unit);
                        Some(Type::Named("Option".to_string(), vec![payload]))
                    }
                    BuiltinMember::ArrayFill => Some(Type::Unit),
                    BuiltinMember::ArrayMap => None,
                    BuiltinMember::ArraySum | BuiltinMember::ArrayMin | BuiltinMember::ArrayMax => {
                        receiver_type.type_arguments().first().cloned()
                    }
                    BuiltinMember::ArrayMean => Some(Type::named("float64")),
                    BuiltinMember::VecLen => Some(Type::named("int64")),
                    BuiltinMember::VecIsEmpty => Some(Type::named("bool")),
                    BuiltinMember::VecClone => Some(receiver_type.clone()),
                    BuiltinMember::VecPush
                    | BuiltinMember::VecRemove
                    | BuiltinMember::VecClear
                    | BuiltinMember::VecReverse
                    | BuiltinMember::VecSort
                    | BuiltinMember::VecInsert
                    | BuiltinMember::VecSwap
                    | BuiltinMember::VecExtend
                    | BuiltinMember::VecReserve => Some(Type::Unit),
                    BuiltinMember::VecIndex | BuiltinMember::VecCount => Some(Type::named("int64")),
                    BuiltinMember::VecMap => None,
                    BuiltinMember::VecFilter => Some(receiver_type.clone()),
                    BuiltinMember::VecContains => Some(Type::named("bool")),
                    BuiltinMember::MapLen => Some(Type::named("int64")),
                    BuiltinMember::MapIsEmpty => Some(Type::named("bool")),
                    BuiltinMember::MapClone => Some(receiver_type.clone()),
                    BuiltinMember::MapContainsKey => Some(Type::named("bool")),
                    BuiltinMember::MapKeys => receiver_type
                        .type_arguments()
                        .first()
                        .cloned()
                        .map(|key| Type::Named("list".to_string(), vec![key])),
                    BuiltinMember::MapValues => receiver_type
                        .type_arguments()
                        .get(1)
                        .cloned()
                        .map(|value| Type::Named("list".to_string(), vec![value])),
                    BuiltinMember::MapItems => Some(Type::Named(
                        "list".to_string(),
                        vec![Type::Tuple(vec![
                            receiver_type
                                .type_arguments()
                                .first()
                                .cloned()
                                .unwrap_or(Type::Unit),
                            receiver_type
                                .type_arguments()
                                .get(1)
                                .cloned()
                                .unwrap_or(Type::Unit),
                        ])],
                    )),
                    BuiltinMember::MapClear
                    | BuiltinMember::MapExtend
                    | BuiltinMember::MapReserve => Some(Type::Unit),
                    BuiltinMember::MapGet | BuiltinMember::MapSet | BuiltinMember::MapRemove => {
                        let payload = receiver_type
                            .type_arguments()
                            .get(1)
                            .cloned()
                            .unwrap_or(Type::Unit);
                        Some(Type::Named("Option".to_string(), vec![payload]))
                    }
                    BuiltinMember::VecPop | BuiltinMember::VecSet => {
                        let payload = receiver_type
                            .type_arguments()
                            .first()
                            .cloned()
                            .unwrap_or(Type::Unit);
                        Some(payload)
                    }
                    BuiltinMember::VecGet => {
                        let payload = receiver_type
                            .type_arguments()
                            .first()
                            .cloned()
                            .unwrap_or(Type::Unit);
                        Some(Type::Named("Option".to_string(), vec![payload]))
                    }
                    BuiltinMember::StringClone => Some(Type::named("str")),
                    BuiltinMember::SetLen => Some(Type::named("int64")),
                    BuiltinMember::SetIsEmpty => Some(Type::named("bool")),
                    BuiltinMember::SetClone => Some(receiver_type.clone()),
                    BuiltinMember::SetContains => Some(Type::named("bool")),
                    BuiltinMember::SetInsert
                    | BuiltinMember::SetRemove
                    | BuiltinMember::SetDiscard
                    | BuiltinMember::SetClear
                    | BuiltinMember::SetReserve => Some(Type::Unit),
                    BuiltinMember::QueuePut | BuiltinMember::QueueTryPut => {
                        let payload = receiver_type
                            .type_arguments()
                            .first()
                            .cloned()
                            .unwrap_or(Type::Unit);
                        Some(Type::Named(
                            "Result".to_string(),
                            vec![
                                Type::Unit,
                                Type::Named("SendError".to_string(), vec![payload]),
                            ],
                        ))
                    }
                    BuiltinMember::QueueGet => {
                        let payload = receiver_type
                            .type_arguments()
                            .first()
                            .cloned()
                            .unwrap_or(Type::Unit);
                        Some(Type::Named("QueueReceive".to_string(), vec![payload]))
                    }
                    BuiltinMember::QueueGetOrNone => {
                        let payload = receiver_type
                            .type_arguments()
                            .first()
                            .cloned()
                            .unwrap_or(Type::Unit);
                        Some(Type::Named("Option".to_string(), vec![payload]))
                    }
                    BuiltinMember::QueueGetOr => receiver_type.type_arguments().first().cloned(),
                    BuiltinMember::QueueClose | BuiltinMember::TaskGroupCancel => Some(Type::Unit),
                    BuiltinMember::TaskResult => Some(Type::Named(
                        "TaskResult".to_string(),
                        vec![receiver_type
                            .type_arguments()
                            .first()
                            .cloned()
                            .unwrap_or(Type::Unit)],
                    )),
                    BuiltinMember::TaskResultOrNone => Some(Type::Named(
                        "Option".to_string(),
                        vec![receiver_type
                            .type_arguments()
                            .first()
                            .cloned()
                            .unwrap_or(Type::Unit)],
                    )),
                    BuiltinMember::TaskResultOr => receiver_type.type_arguments().first().cloned(),
                    BuiltinMember::TaskGroupStart | BuiltinMember::TaskGroupStartWithStack => {
                        Some(Type::Named("Task".to_string(), vec![Type::Unit]))
                    }
                    BuiltinMember::TaskGroupStartSoon
                    | BuiltinMember::TaskGroupStartSoonWithStack => Some(Type::Unit),
                    BuiltinMember::ProcessChildStdin
                    | BuiltinMember::ProcessChildStdout
                    | BuiltinMember::ProcessChildStderr => Some(Type::Named(
                        "Option".to_string(),
                        vec![Type::Named("process.Pipe".to_string(), Vec::new())],
                    )),
                    BuiltinMember::ProcessChildWait => {
                        Some(Type::Named("process.Wait".to_string(), Vec::new()))
                    }
                    BuiltinMember::ProcessChildWaitOrNone => Some(Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::Named(
                                "Option".to_string(),
                                vec![Type::Named("process.ExitStatus".to_string(), Vec::new())],
                            ),
                            Type::Named("process.Error".to_string(), Vec::new()),
                        ],
                    )),
                    BuiltinMember::ProcessChildWaitOk => Some(Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::Named("process.ExitStatus".to_string(), Vec::new()),
                            Type::Named("process.Error".to_string(), Vec::new()),
                        ],
                    )),
                    BuiltinMember::ProcessChildKill | BuiltinMember::ProcessChildTerminate => {
                        Some(Type::Named(
                            "Result".to_string(),
                            vec![
                                Type::Unit,
                                Type::Named("process.Error".to_string(), Vec::new()),
                            ],
                        ))
                    }
                    BuiltinMember::ProcessChildClose => Some(Type::Unit),
                    BuiltinMember::ProcessPipeReadAll => Some(Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::named("str"),
                            Type::Named("process.Error".to_string(), Vec::new()),
                        ],
                    )),
                    BuiltinMember::ProcessPipeReadLine => Some(Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::Named("Option".to_string(), vec![Type::named("str")]),
                            Type::Named("process.Error".to_string(), Vec::new()),
                        ],
                    )),
                    BuiltinMember::ProcessPipeReadBytes => Some(Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::Named(
                                "Option".to_string(),
                                vec![Type::Named("list".to_string(), vec![Type::named("uint8")])],
                            ),
                            Type::Named("process.Error".to_string(), Vec::new()),
                        ],
                    )),
                    BuiltinMember::ProcessPipeWriteAll
                    | BuiltinMember::ProcessPipeWriteBytes
                    | BuiltinMember::ProcessPipeFlush => Some(Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::Unit,
                            Type::Named("process.Error".to_string(), Vec::new()),
                        ],
                    )),
                    BuiltinMember::ProcessPipeClose => Some(Type::Unit),
                    BuiltinMember::ProcessCompletedStatus => {
                        Some(Type::Named("process.ExitStatus".to_string(), Vec::new()))
                    }
                    BuiltinMember::ProcessCompletedSuccess => Some(Type::named("bool")),
                    BuiltinMember::ProcessCompletedStdout
                    | BuiltinMember::ProcessCompletedStderr => Some(Type::named("str")),
                    BuiltinMember::ProcessCompletedStdoutBytes
                    | BuiltinMember::ProcessCompletedStderrBytes => {
                        Some(Type::Named("list".to_string(), vec![Type::named("uint8")]))
                    }
                    BuiltinMember::ProcessCompletedCheck => Some(Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::Unit,
                            Type::Named("process.Error".to_string(), Vec::new()),
                        ],
                    )),
                    BuiltinMember::ProcessSupervisorStart
                    | BuiltinMember::ProcessSupervisorStop => Some(Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::Unit,
                            Type::Named("process.Error".to_string(), Vec::new()),
                        ],
                    )),
                    BuiltinMember::ProcessSupervisorWait => Some(Type::Named(
                        "process.SupervisorWait".to_string(),
                        Vec::new(),
                    )),
                    BuiltinMember::ProcessSupervisorWaitOrNone => Some(Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::Named(
                                "Option".to_string(),
                                vec![Type::Named(
                                    "process.SupervisorEvent".to_string(),
                                    Vec::new(),
                                )],
                            ),
                            Type::Named("process.Error".to_string(), Vec::new()),
                        ],
                    )),
                    BuiltinMember::ProcessSupervisorIsEmpty => Some(Type::named("bool")),
                    BuiltinMember::ProcessSupervisorClose => Some(Type::Unit),
                    BuiltinMember::RngNextInt => Some(Type::named("int64")),
                    BuiltinMember::RngNextFloat => Some(Type::named("float64")),
                    BuiltinMember::RngShuffle => Some(Type::Unit),
                    BuiltinMember::FileReadAll => Some(Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::named("str"),
                            Type::Named("io.Error".to_string(), Vec::new()),
                        ],
                    )),
                    BuiltinMember::FileReadBytes => Some(Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::Named("list".to_string(), vec![Type::named("uint8")]),
                            Type::Named("io.Error".to_string(), Vec::new()),
                        ],
                    )),
                    BuiltinMember::FileWriteAll
                    | BuiltinMember::FileWriteBytes
                    | BuiltinMember::FileFlush => Some(Type::Named(
                        "Result".to_string(),
                        vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
                    )),
                    BuiltinMember::FileClose => Some(Type::Unit),
                    BuiltinMember::TcpListenerAccept => Some(Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::Named("net.TcpStream".to_string(), Vec::new()),
                            Type::Named("io.Error".to_string(), Vec::new()),
                        ],
                    )),
                    BuiltinMember::TcpListenerLocalAddr => Some(Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::named("str"),
                            Type::Named("io.Error".to_string(), Vec::new()),
                        ],
                    )),
                    BuiltinMember::TcpListenerClose => Some(Type::Unit),
                    BuiltinMember::TcpStreamReadAll
                    | BuiltinMember::TcpStreamLocalAddr
                    | BuiltinMember::TcpStreamPeerAddr => Some(Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::named("str"),
                            Type::Named("io.Error".to_string(), Vec::new()),
                        ],
                    )),
                    BuiltinMember::TcpStreamReadLine => Some(Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::Named("Option".to_string(), vec![Type::named("str")]),
                            Type::Named("io.Error".to_string(), Vec::new()),
                        ],
                    )),
                    BuiltinMember::TcpStreamReadBytes => Some(Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::Named(
                                "Option".to_string(),
                                vec![Type::Named("list".to_string(), vec![Type::named("uint8")])],
                            ),
                            Type::Named("io.Error".to_string(), Vec::new()),
                        ],
                    )),
                    BuiltinMember::TcpStreamReadExact => Some(Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::Named("list".to_string(), vec![Type::named("uint8")]),
                            Type::Named("io.Error".to_string(), Vec::new()),
                        ],
                    )),
                    BuiltinMember::TcpStreamWriteAll
                    | BuiltinMember::TcpStreamWriteBytes
                    | BuiltinMember::TcpStreamFlush
                    | BuiltinMember::TcpStreamShutdownRead
                    | BuiltinMember::TcpStreamShutdownWrite
                    | BuiltinMember::TcpStreamShutdownBoth => Some(Type::Named(
                        "Result".to_string(),
                        vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
                    )),
                    BuiltinMember::TcpStreamClose => Some(Type::Unit),
                    BuiltinMember::UdpSocketSendText | BuiltinMember::UdpSocketSendBytes => {
                        Some(Type::Named(
                            "Result".to_string(),
                            vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
                        ))
                    }
                    BuiltinMember::UdpSocketRecv => Some(Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::Named(
                                "Option".to_string(),
                                vec![Type::Named("list".to_string(), vec![Type::named("uint8")])],
                            ),
                            Type::Named("io.Error".to_string(), Vec::new()),
                        ],
                    )),
                    BuiltinMember::UdpSocketRecvFrom => Some(Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::Named(
                                "Option".to_string(),
                                vec![Type::Named("net.UdpDatagram".to_string(), Vec::new())],
                            ),
                            Type::Named("io.Error".to_string(), Vec::new()),
                        ],
                    )),
                    BuiltinMember::UdpSocketLocalAddr | BuiltinMember::UdpSocketPeerAddr => {
                        Some(Type::Named(
                            "Result".to_string(),
                            vec![
                                Type::named("str"),
                                Type::Named("io.Error".to_string(), Vec::new()),
                            ],
                        ))
                    }
                    BuiltinMember::UdpSocketClose => Some(Type::Unit),
                    BuiltinMember::UdpDatagramAddress => Some(Type::named("str")),
                    BuiltinMember::UdpDatagramBytes => {
                        Some(Type::Named("list".to_string(), vec![Type::named("uint8")]))
                    }
                    BuiltinMember::UdpDatagramText => Some(Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::named("str"),
                            Type::Named("io.Error".to_string(), Vec::new()),
                        ],
                    )),
                    BuiltinMember::HttpListenerAccept => Some(Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::Named("net.HttpExchange".to_string(), Vec::new()),
                            Type::Named("io.Error".to_string(), Vec::new()),
                        ],
                    )),
                    BuiltinMember::HttpListenerLocalAddr => Some(Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::named("str"),
                            Type::Named("io.Error".to_string(), Vec::new()),
                        ],
                    )),
                    BuiltinMember::HttpListenerClose => Some(Type::Unit),
                    BuiltinMember::HttpExchangeMethod | BuiltinMember::HttpExchangePath => {
                        Some(Type::named("str"))
                    }
                    BuiltinMember::HttpExchangeHeaders | BuiltinMember::HttpResponseHeaders => {
                        Some(Type::Named(
                            "dict".to_string(),
                            vec![Type::named("str"), Type::named("str")],
                        ))
                    }
                    BuiltinMember::HttpExchangeBodyText | BuiltinMember::HttpResponseText => {
                        Some(Type::Named(
                            "Result".to_string(),
                            vec![
                                Type::named("str"),
                                Type::Named("io.Error".to_string(), Vec::new()),
                            ],
                        ))
                    }
                    BuiltinMember::HttpExchangeBodyBytes | BuiltinMember::HttpResponseBytes => {
                        Some(Type::Named("list".to_string(), vec![Type::named("uint8")]))
                    }
                    BuiltinMember::HttpExchangeRespondText
                    | BuiltinMember::HttpExchangeRespondBytes => Some(Type::Named(
                        "Result".to_string(),
                        vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
                    )),
                    BuiltinMember::HttpResponseStatus => Some(Type::named("int32")),
                    BuiltinMember::HttpResponseReason => Some(Type::named("str")),
                    BuiltinMember::WebSocketListenerAccept => Some(Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::Named("net.WebSocket".to_string(), Vec::new()),
                            Type::Named("io.Error".to_string(), Vec::new()),
                        ],
                    )),
                    BuiltinMember::WebSocketListenerLocalAddr => Some(Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::named("str"),
                            Type::Named("io.Error".to_string(), Vec::new()),
                        ],
                    )),
                    BuiltinMember::WebSocketSendText | BuiltinMember::WebSocketSendBytes => {
                        Some(Type::Named(
                            "Result".to_string(),
                            vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
                        ))
                    }
                    BuiltinMember::WebSocketRecvText => Some(Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::Named("Option".to_string(), vec![Type::named("str")]),
                            Type::Named("io.Error".to_string(), Vec::new()),
                        ],
                    )),
                    BuiltinMember::WebSocketRecvBytes => Some(Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::Named(
                                "Option".to_string(),
                                vec![Type::Named("list".to_string(), vec![Type::named("uint8")])],
                            ),
                            Type::Named("io.Error".to_string(), Vec::new()),
                        ],
                    )),
                    BuiltinMember::WebSocketClose => Some(Type::Unit),
                    BuiltinMember::UnixListenerAccept => Some(Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::Named("net.UnixStream".to_string(), Vec::new()),
                            Type::Named("io.Error".to_string(), Vec::new()),
                        ],
                    )),
                    BuiltinMember::UnixListenerClose => Some(Type::Unit),
                    BuiltinMember::UnixStreamReadLine => Some(Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::Named("Option".to_string(), vec![Type::named("str")]),
                            Type::Named("io.Error".to_string(), Vec::new()),
                        ],
                    )),
                    BuiltinMember::UnixStreamReadExact => Some(Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::Named("list".to_string(), vec![Type::named("uint8")]),
                            Type::Named("io.Error".to_string(), Vec::new()),
                        ],
                    )),
                    BuiltinMember::UnixStreamWriteAll => Some(Type::Named(
                        "Result".to_string(),
                        vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
                    )),
                    BuiltinMember::UnixStreamClose => Some(Type::Unit),
                    BuiltinMember::TlsListenerAccept => Some(Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::Named("net.TlsStream".to_string(), Vec::new()),
                            Type::Named("io.Error".to_string(), Vec::new()),
                        ],
                    )),
                    BuiltinMember::TlsListenerLocalAddr => Some(Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::named("str"),
                            Type::Named("io.Error".to_string(), Vec::new()),
                        ],
                    )),
                    BuiltinMember::TlsListenerClose => Some(Type::Unit),
                    BuiltinMember::TlsStreamReadLine => Some(Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::Named("Option".to_string(), vec![Type::named("str")]),
                            Type::Named("io.Error".to_string(), Vec::new()),
                        ],
                    )),
                    BuiltinMember::TlsStreamReadExact => Some(Type::Named(
                        "Result".to_string(),
                        vec![
                            Type::Named("list".to_string(), vec![Type::named("uint8")]),
                            Type::Named("io.Error".to_string(), Vec::new()),
                        ],
                    )),
                    BuiltinMember::TlsStreamWriteAll => Some(Type::Named(
                        "Result".to_string(),
                        vec![Type::Unit, Type::Named("io.Error".to_string(), Vec::new())],
                    )),
                    BuiltinMember::TlsStreamClose => Some(Type::Unit),
                };
            return Some(ResolvedMember {
                hover: builtin_function_hover(builtin_member.detail(), builtin_member.docs()),
                definition: None,
                ty,
            });
        }

        match base_name {
            "Option" if field == "Some" => Some(ResolvedMember {
                hover: format_variant_hover("Option", "Some", Some(&Type::named("T"))),
                definition: None,
                ty: Some(Type::named("Option")),
            }),
            "Option" if field == "None" => Some(ResolvedMember {
                hover: format_variant_hover("Option", "None", None),
                definition: None,
                ty: Some(Type::named("Option")),
            }),
            "Result" if field == "Ok" => Some(ResolvedMember {
                hover: format_variant_hover("Result", "Ok", Some(&Type::named("T"))),
                definition: None,
                ty: Some(Type::named("Result")),
            }),
            "Result" if field == "Err" => Some(ResolvedMember {
                hover: format_variant_hover("Result", "Err", Some(&Type::named("E"))),
                definition: None,
                ty: Some(Type::named("Result")),
            }),
            "SendError" if matches!(field, "Closed" | "Cancelled" | "TimedOut" | "Full") => {
                Some(ResolvedMember {
                    hover: format_variant_hover("SendError", field, Some(&Type::named("T"))),
                    definition: None,
                    ty: Some(Type::named("SendError")),
                })
            }
            "QueueReceive" if field == "Item" => Some(ResolvedMember {
                hover: format_variant_hover("QueueReceive", field, Some(&Type::named("T"))),
                definition: None,
                ty: Some(Type::named("QueueReceive")),
            }),
            "QueueReceive" if matches!(field, "Closed" | "TimedOut" | "Cancelled") => {
                Some(ResolvedMember {
                    hover: format_variant_hover("QueueReceive", field, None),
                    definition: None,
                    ty: Some(Type::named("QueueReceive")),
                })
            }
            "TaskResult" if field == "Ready" => Some(ResolvedMember {
                hover: format_variant_hover("TaskResult", field, Some(&Type::named("T"))),
                definition: None,
                ty: Some(Type::named("TaskResult")),
            }),
            "TaskResult" if field == "Error" => Some(ResolvedMember {
                hover: format_variant_hover("TaskResult", field, Some(&Type::named("str"))),
                definition: None,
                ty: Some(Type::named("TaskResult")),
            }),
            "TaskResult" if matches!(field, "TimedOut" | "Cancelled") => Some(ResolvedMember {
                hover: format_variant_hover("TaskResult", field, None),
                definition: None,
                ty: Some(Type::named("TaskResult")),
            }),
            "WaitAny" if field == "Ready" => Some(ResolvedMember {
                hover: format_variant_hover_payloads(
                    "WaitAny",
                    field,
                    ["own int64".to_string(), "own T".to_string()],
                ),
                definition: None,
                ty: Some(Type::named("WaitAny")),
            }),
            "WaitAny" if field == "Error" => Some(ResolvedMember {
                hover: format_variant_hover_payloads(
                    "WaitAny",
                    field,
                    ["own int64".to_string(), "own str".to_string()],
                ),
                definition: None,
                ty: Some(Type::named("WaitAny")),
            }),
            "WaitAny" if matches!(field, "TimedOut" | "Cancelled") => Some(ResolvedMember {
                hover: format_variant_hover("WaitAny", field, None),
                definition: None,
                ty: Some(Type::named("WaitAny")),
            }),
            "WaitAll" if field == "Ready" => Some(ResolvedMember {
                hover: format_variant_hover_payloads("WaitAll", field, ["own list[T]".to_string()]),
                definition: None,
                ty: Some(Type::named("WaitAll")),
            }),
            "WaitAll" if field == "Error" => Some(ResolvedMember {
                hover: format_variant_hover_payloads(
                    "WaitAll",
                    field,
                    ["own int64".to_string(), "own str".to_string()],
                ),
                definition: None,
                ty: Some(Type::named("WaitAll")),
            }),
            "WaitAll" if matches!(field, "TimedOut" | "Cancelled") => Some(ResolvedMember {
                hover: format_variant_hover("WaitAll", field, None),
                definition: None,
                ty: Some(Type::named("WaitAll")),
            }),
            "SelectOutcome" if field == "Queue" => Some(ResolvedMember {
                hover: format_variant_hover_payloads(
                    "SelectOutcome",
                    field,
                    ["own int64".to_string(), "own QueueReceive[Q]".to_string()],
                ),
                definition: None,
                ty: Some(Type::named("SelectOutcome")),
            }),
            "SelectOutcome" if field == "Task" => Some(ResolvedMember {
                hover: format_variant_hover_payloads(
                    "SelectOutcome",
                    field,
                    ["own int64".to_string(), "own TaskResult[T]".to_string()],
                ),
                definition: None,
                ty: Some(Type::named("SelectOutcome")),
            }),
            "SelectOutcome" if field == "Deadline" => Some(ResolvedMember {
                hover: format_variant_hover("SelectOutcome", field, Some(&Type::named("int64"))),
                definition: None,
                ty: Some(Type::named("SelectOutcome")),
            }),
            "SelectOutcome" if field == "Cancelled" => Some(ResolvedMember {
                hover: format_variant_hover("SelectOutcome", field, None),
                definition: None,
                ty: Some(Type::named("SelectOutcome")),
            }),
            _ => None,
        }
    }

    fn lower_analysis_type_ref(&self, ty: &TypeRef) -> Type {
        match &ty.kind {
            crate::ast::TypeRefKind::Tuple(elements) => Type::Tuple(
                elements
                    .iter()
                    .map(|element| self.lower_analysis_type_ref(element))
                    .collect(),
            ),
            crate::ast::TypeRefKind::Function {
                params,
                return_type,
            } => Type::Function {
                params: params
                    .iter()
                    .map(|param| FunctionParamContract {
                        name: String::new(),
                        ty: self.lower_analysis_type_ref(&param.ty),
                        passing: resolve_param_passing(param.mode),
                        has_default: false,
                        default_erased: true,
                    })
                    .collect(),
                return_type: Box::new(self.lower_analysis_type_ref(return_type)),
            },
            crate::ast::TypeRefKind::Named { name, args } => {
                if name == "None" {
                    return Type::Unit;
                }
                let name = match name.as_str() {
                    "str" => "str",
                    "int" => "int64",
                    name => name,
                };
                let args = args
                    .iter()
                    .map(|arg| self.lower_analysis_type_ref(arg))
                    .collect::<Vec<_>>();
                self.program
                    .classes
                    .get(name)
                    .map(|class_info| self.analysis_class_type(name, class_info, args.clone()))
                    .unwrap_or_else(|| {
                        Type::Named(
                            self.program
                                .canonical_type_names
                                .get(name)
                                .cloned()
                                .unwrap_or_else(|| name.to_string()),
                            args,
                        )
                    })
            }
        }
    }

    fn analysis_class_type(
        &self,
        surface_name: &str,
        class_info: &ClassInfo,
        args: Vec<Type>,
    ) -> Type {
        let name = class_info
            .builtin_constructor()
            .map(|constructor| constructor.qualified_name().to_string())
            .or_else(|| self.program.canonical_type_names.get(surface_name).cloned())
            .unwrap_or_else(|| {
                if surface_name.contains('.') {
                    surface_name.to_string()
                } else if class_info.module_name == self.program.module_name {
                    class_info.decl.name.clone()
                } else {
                    format!("{}.{}", class_info.module_name, class_info.decl.name)
                }
            });
        Type::Named(name, args)
    }

    fn infer_expr_type(&self, expr: &Expr, scope: &BTreeMap<String, BindingInfo>) -> Option<Type> {
        match &expr.kind {
            ExprKind::Membership { .. } | ExprKind::CompareChain { .. } => {
                Some(Type::named("bool"))
            }
            ExprKind::Int(_) => Some(Type::named("int64")),
            ExprKind::DurationNanos(_) => Some(Type::named("Duration")),
            ExprKind::BuiltinOmitted => None,
            ExprKind::Float(_) => Some(Type::named("float64")),
            ExprKind::Bool(_) => Some(Type::named("bool")),
            ExprKind::String(_) => Some(Type::named("str")),
            ExprKind::Lambda { .. } => self.closure_info(expr).map(ClosureInfo::ty),
            ExprKind::Tuple(elements) => Some(Type::Tuple(
                elements
                    .iter()
                    .map(|element| {
                        self.infer_expr_type(element, scope)
                            .unwrap_or(Type::named("Unknown"))
                    })
                    .collect(),
            )),
            ExprKind::List(elements) => Some(Type::Named(
                "list".to_string(),
                vec![elements
                    .first()
                    .and_then(|element| self.infer_expr_type(element, scope))
                    .unwrap_or(Type::named("Unknown"))],
            )),
            ExprKind::Set(elements) => Some(Type::Named(
                "set".to_string(),
                vec![elements
                    .first()
                    .and_then(|element| self.infer_expr_type(element, scope))
                    .unwrap_or(Type::named("Unknown"))],
            )),
            ExprKind::Map(entries) => Some(Type::Named(
                "dict".to_string(),
                vec![
                    entries
                        .first()
                        .and_then(|entry| self.infer_expr_type(&entry.key, scope))
                        .unwrap_or(Type::named("Unknown")),
                    entries
                        .first()
                        .and_then(|entry| self.infer_expr_type(&entry.value, scope))
                        .unwrap_or(Type::named("Unknown")),
                ],
            )),
            ExprKind::Comprehension { .. } => self
                .comprehension_info(expr)
                .map(|info| info.result_type.clone()),
            ExprKind::FString(_) => Some(Type::named("str")),
            ExprKind::Specialize { expr, type_args } => match &expr.kind {
                ExprKind::Name(name)
                    if self.program.classes.contains_key(name)
                        || self.program.enums.contains_key(name)
                        || matches!(
                            name.as_str(),
                            "Option"
                                | "Result"
                                | "SendError"
                                | "Queue"
                                | "Array"
                                | "list"
                                | "set"
                                | "dict"
                                | "Task"
                        ) =>
                {
                    let args = type_args
                        .iter()
                        .map(|ty| self.lower_analysis_type_ref(ty))
                        .collect::<Vec<_>>();
                    Some(
                        self.program
                            .classes
                            .get(name)
                            .map(|class_info| {
                                self.analysis_class_type(name, class_info, args.clone())
                            })
                            .unwrap_or_else(|| {
                                Type::Named(
                                    self.program
                                        .canonical_type_names
                                        .get(name)
                                        .cloned()
                                        .unwrap_or_else(|| name.clone()),
                                    args,
                                )
                            }),
                    )
                }
                _ => self.infer_expr_type(expr, scope),
            },
            ExprKind::Group(inner) => self.infer_expr_type(inner, scope),
            ExprKind::Cast { ty, .. } => Some(self.lower_analysis_type_ref(ty)),
            ExprKind::Unary { op, expr } => {
                let inner_ty = self.infer_expr_type(expr, scope)?;
                match op {
                    crate::ast::UnaryOp::Not => Some(Type::named("bool")),
                    crate::ast::UnaryOp::Neg | crate::ast::UnaryOp::BitNot => Some(inner_ty),
                }
            }
            ExprKind::Try(inner) => {
                let inner_ty = self.infer_expr_type(inner, scope)?;
                match inner_ty {
                    Type::Named(name, mut args) if name == "Result" && args.len() == 2 => {
                        Some(args.remove(0))
                    }
                    _ => None,
                }
            }
            ExprKind::Name(name) => {
                if let Some(binding) = scope.get(name) {
                    return Some(binding.ty.clone());
                }
                if let Some(namespace) = self.program.imported_modules.get(name) {
                    return Some(Type::Module(namespace.path.clone()));
                }
                if let Some(class_info) = self.program.classes.get(name) {
                    return Some(self.analysis_class_type(name, class_info, Vec::new()));
                }
                if self.program.enums.contains_key(name)
                    || matches!(
                        name.as_str(),
                        "Option"
                            | "Result"
                            | "SendError"
                            | "QueueReceive"
                            | "TaskResult"
                            | "WaitAny"
                            | "WaitAll"
                            | "SelectOutcome"
                            | "Queue"
                            | "TaskGroup"
                    )
                {
                    return Some(Type::named(
                        self.program
                            .canonical_type_names
                            .get(name)
                            .cloned()
                            .unwrap_or_else(|| name.clone()),
                    ));
                }
                if let Some(function) = self.program.functions.get(name) {
                    return Some(Type::Function {
                        params: function
                            .decl
                            .params
                            .iter()
                            .zip(&function.signature.params)
                            .zip(&function.signature.param_passings)
                            .map(|((decl, ty), passing)| FunctionParamContract {
                                name: decl.name.clone(),
                                ty: ty.clone(),
                                passing: *passing,
                                has_default: decl.default.is_some(),
                                default_erased: false,
                            })
                            .collect(),
                        return_type: Box::new(function.signature.return_type.clone()),
                    });
                }
                builtin_function_return_type(name)
            }
            ExprKind::Member { object, field } => self
                .resolve_member_expr(object, field, scope)
                .and_then(|member| member.ty),
            ExprKind::Index { object, index } => {
                self.infer_expr_type(object, scope)
                    .and_then(|ty| match &ty {
                        Type::Tuple(elements) => match &index.kind {
                            ExprKind::Int(value) => usize::try_from(*value)
                                .ok()
                                .and_then(|index| elements.get(index).cloned()),
                            _ => None,
                        },
                        _ => match base_type_name(&ty) {
                            "Array" => ty.type_arguments().first().cloned(),
                            "list" => ty.type_arguments().first().cloned(),
                            "dict" => ty.type_arguments().get(1).cloned(),
                            _ => None,
                        },
                    })
            }
            ExprKind::Slice { object, .. } => self.infer_expr_type(object, scope).and_then(|ty| {
                matches!(base_type_name(&ty), "Array" | "list" | "str").then_some(ty)
            }),
            ExprKind::Call { callee, args } => self.infer_call_type(callee, args, scope),
            ExprKind::Conditional {
                then_expr,
                else_expr,
                ..
            } => {
                let then_ty = self.infer_expr_type(then_expr, scope);
                let else_ty = self.infer_expr_type(else_expr, scope);
                match (then_ty, else_ty) {
                    (None, other) | (other, None) => other,
                    (Some(then_ty), Some(else_ty)) if then_ty == else_ty => Some(then_ty),
                    (Some(Type::Unit), Some(else_ty)) => Some(else_ty),
                    (Some(then_ty), Some(Type::Unit)) => Some(then_ty),
                    (Some(then_ty), Some(else_ty))
                        if analysis_type_contains_unknown(&then_ty)
                            && !analysis_type_contains_unknown(&else_ty) =>
                    {
                        Some(else_ty)
                    }
                    (Some(then_ty), Some(else_ty))
                        if analysis_type_contains_unknown(&else_ty)
                            && !analysis_type_contains_unknown(&then_ty) =>
                    {
                        Some(then_ty)
                    }
                    (Some(_), Some(else_ty))
                        if analysis_is_integer_literal(then_expr)
                            && analysis_is_numeric_type(&else_ty) =>
                    {
                        Some(else_ty)
                    }
                    (Some(then_ty), Some(_))
                        if analysis_is_integer_literal(else_expr)
                            && analysis_is_numeric_type(&then_ty) =>
                    {
                        Some(then_ty)
                    }
                    (Some(_), Some(else_ty))
                        if analysis_is_float_literal(then_expr)
                            && !analysis_is_float_literal(else_expr)
                            && analysis_is_float_type(&else_ty) =>
                    {
                        Some(else_ty)
                    }
                    (Some(then_ty), Some(_))
                        if analysis_is_float_literal(else_expr)
                            && !analysis_is_float_literal(then_expr)
                            && analysis_is_float_type(&then_ty) =>
                    {
                        Some(then_ty)
                    }
                    (Some(then_ty), Some(_)) => Some(then_ty),
                }
            }
            ExprKind::Match { arms, .. } => arms
                .first()
                .and_then(|arm| self.infer_expr_type(&arm.value, scope)),
            ExprKind::Binary { op, left, right } => {
                let left_ty = self.infer_expr_type(left, scope)?;
                let right_ty = self.infer_expr_type(right, scope)?;
                let left_array = (base_type_name(&left_ty) == "Array").then_some(&left_ty);
                let right_array = (base_type_name(&right_ty) == "Array").then_some(&right_ty);
                if left_array.is_some() || right_array.is_some() {
                    debug_assert!(matches!(
                        op,
                        BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div
                    ));
                    return left_array.or(right_array).cloned();
                }
                if let Some(result) = builtin_duration_binary_result(*op, &left_ty, &right_ty) {
                    return Some(result);
                }
                match op {
                    BinaryOp::And | BinaryOp::Or => Some(Type::named("bool")),
                    BinaryOp::Eq
                    | BinaryOp::NotEq
                    | BinaryOp::Less
                    | BinaryOp::LessEq
                    | BinaryOp::Greater
                    | BinaryOp::GreaterEq => Some(Type::named("bool")),
                    BinaryOp::Add
                        if left_ty == Type::named("str") && right_ty == Type::named("str") =>
                    {
                        Some(Type::named("str"))
                    }
                    _ if left_ty == Type::named("float64")
                        || right_ty == Type::named("float64") =>
                    {
                        Some(Type::named("float64"))
                    }
                    _ if left_ty == right_ty => Some(left_ty),
                    _ => None,
                }
            }
        }
    }

    fn infer_call_type(
        &self,
        callee: &Expr,
        args: &[crate::ast::Argument],
        scope: &BTreeMap<String, BindingInfo>,
    ) -> Option<Type> {
        match &callee.kind {
            ExprKind::Name(name) => {
                if let Some(binding) = scope.get(name) {
                    match &binding.ty {
                        Type::Function { return_type, .. } | Type::Closure { return_type, .. } => {
                            return Some((**return_type).clone());
                        }
                        _ => {}
                    }
                }
                if let Some(function) = self.program.functions.get(name) {
                    return Some(function.signature.return_type.clone());
                }
                if let Some(function) = self.program.extern_functions.get(name) {
                    return Some(function.signature.return_type.clone());
                }
                if name == "TaskGroup" {
                    return Some(Type::named("TaskGroup"));
                }
                if let Some(class_info) = self.program.classes.get(name) {
                    return Some(self.analysis_class_type(name, class_info, Vec::new()));
                }
                match BuiltinFunction::from_name(name)? {
                    BuiltinFunction::Abs | BuiltinFunction::Min | BuiltinFunction::Max => args
                        .first()
                        .and_then(|arg| self.infer_expr_type(&arg.value, scope)),
                    BuiltinFunction::Sqrt => args
                        .first()
                        .and_then(|arg| self.infer_expr_type(&arg.value, scope)),
                    BuiltinFunction::Round => args.first().and_then(|arg| {
                        let ty = self.infer_expr_type(&arg.value, scope)?;
                        Some(
                            if matches!(
                                ty,
                                Type::Named(ref name, ref type_args)
                                    if type_args.is_empty()
                                        && matches!(name.as_str(), "float32" | "float64")
                            ) {
                                Type::named("int64")
                            } else {
                                ty
                            },
                        )
                    }),
                    BuiltinFunction::Divmod => args.first().and_then(|arg| {
                        let ty = self.infer_expr_type(&arg.value, scope)?;
                        Some(Type::Tuple(vec![ty.clone(), ty]))
                    }),
                    BuiltinFunction::Select => {
                        if args.iter().any(|argument| argument.name.is_some()) {
                            return None;
                        }
                        let mut queue_payload = None;
                        let mut task_result = None;
                        for argument in args {
                            match self.infer_expr_type(&argument.value, scope)? {
                                Type::Named(name, source_args)
                                    if name == "Queue" && source_args.len() == 1 =>
                                {
                                    if queue_payload
                                        .as_ref()
                                        .is_some_and(|expected| expected != &source_args[0])
                                    {
                                        return None;
                                    }
                                    queue_payload.get_or_insert_with(|| source_args[0].clone());
                                }
                                Type::Named(name, source_args)
                                    if name == "Task" && source_args.len() == 1 =>
                                {
                                    if task_result
                                        .as_ref()
                                        .is_some_and(|expected| expected != &source_args[0])
                                    {
                                        return None;
                                    }
                                    task_result.get_or_insert_with(|| source_args[0].clone());
                                }
                                Type::Named(name, source_args)
                                    if name == "Duration" && source_args.is_empty() => {}
                                _ => return None,
                            }
                        }
                        (!args.is_empty()).then(|| {
                            Type::Named(
                                "SelectOutcome".to_string(),
                                vec![
                                    queue_payload.unwrap_or(Type::Unit),
                                    task_result.unwrap_or(Type::Unit),
                                ],
                            )
                        })
                    }
                    BuiltinFunction::WaitAny => args.first().and_then(|arg| {
                        let task_list = self.infer_expr_type(&arg.value, scope)?;
                        match task_list {
                            Type::Named(vec_name, vec_args)
                                if vec_name == "list" && vec_args.len() == 1 =>
                            {
                                match &vec_args[0] {
                                    Type::Named(task_name, task_args)
                                        if task_name == "Task" && task_args.len() == 1 =>
                                    {
                                        Some(Type::Named(
                                            "WaitAny".to_string(),
                                            vec![task_args[0].clone()],
                                        ))
                                    }
                                    _ => None,
                                }
                            }
                            _ => None,
                        }
                    }),
                    BuiltinFunction::WaitAll => args.first().and_then(|arg| {
                        let task_list = self.infer_expr_type(&arg.value, scope)?;
                        match task_list {
                            Type::Named(vec_name, vec_args)
                                if vec_name == "list" && vec_args.len() == 1 =>
                            {
                                match &vec_args[0] {
                                    Type::Named(task_name, task_args)
                                        if task_name == "Task" && task_args.len() == 1 =>
                                    {
                                        Some(Type::Named(
                                            "WaitAll".to_string(),
                                            vec![task_args[0].clone()],
                                        ))
                                    }
                                    _ => None,
                                }
                            }
                            _ => None,
                        }
                    }),
                    _ => builtin_function_return_type(name),
                }
            }
            ExprKind::Member { object, field } => {
                if let ExprKind::Name(enum_name) = &object.kind {
                    if !scope.contains_key(enum_name) {
                        match (enum_name.as_str(), field.as_str()) {
                            ("Duration", "ms" | "seconds" | "minutes") => {
                                return Some(Type::named("Duration"));
                            }
                            ("str", "from_bytes") => {
                                return Some(Type::Named(
                                    "Result".to_string(),
                                    vec![Type::named("str"), Type::named("bytes.Error")],
                                ));
                            }
                            _ => {}
                        }
                    }
                    if enum_name == "Duration" {
                        return None;
                    }
                    if matches!(enum_name.as_str(), "Option" | "Result" | "SendError") {
                        return infer_builtin_variant_call(enum_name, field, args, |expr| {
                            self.infer_expr_type(expr, scope)
                        });
                    }
                    if self.program.enums.contains_key(enum_name) {
                        return Some(Type::named(enum_name));
                    }
                }
                let receiver_type = self.infer_expr_type(object, scope)?;
                if BuiltinMember::resolve(base_type_name(&receiver_type), field)
                    == Some(BuiltinMember::VecMap)
                {
                    let callback_type = args
                        .first()
                        .and_then(|argument| self.infer_expr_type(&argument.value, scope))?;
                    let return_type = match callback_type {
                        Type::Function { return_type, .. } | Type::Closure { return_type, .. } => {
                            return_type
                        }
                        _ => return None,
                    };
                    return Some(Type::Named("list".to_string(), vec![*return_type]));
                }
                if BuiltinMember::resolve(base_type_name(&receiver_type), field)
                    == Some(BuiltinMember::ArrayMap)
                {
                    let callback_type = args
                        .first()
                        .and_then(|argument| self.infer_expr_type(&argument.value, scope))?;
                    let return_type = match callback_type {
                        Type::Function { return_type, .. } | Type::Closure { return_type, .. } => {
                            return_type
                        }
                        _ => return None,
                    };
                    return Some(Type::Named("Array".to_string(), vec![*return_type]));
                }
                self.resolve_member_expr(object, field, scope)
                    .and_then(|member| member.ty)
                    .map(|ty| match ty {
                        Type::Function { return_type, .. } | Type::Closure { return_type, .. } => {
                            *return_type
                        }
                        ty => ty,
                    })
            }
            ExprKind::Specialize { expr, type_args } => match &expr.kind {
                ExprKind::Name(name)
                    if self.program.classes.contains_key(name)
                        || matches!(
                            name.as_str(),
                            "Queue" | "Array" | "list" | "set" | "dict" | "Task"
                        ) =>
                {
                    let args = type_args
                        .iter()
                        .map(|ty| self.lower_analysis_type_ref(ty))
                        .collect::<Vec<_>>();
                    Some(
                        self.program
                            .classes
                            .get(name)
                            .map(|class_info| {
                                self.analysis_class_type(name, class_info, args.clone())
                            })
                            .unwrap_or_else(|| Type::Named(name.clone(), args)),
                    )
                }
                _ => self
                    .infer_specialized_function_return_type(expr, type_args, scope)
                    .or_else(|| self.infer_call_type(expr, args, scope)),
            },
            _ => None,
        }
    }

    fn infer_specialized_function_return_type(
        &self,
        expr: &Expr,
        type_args: &[TypeRef],
        scope: &BTreeMap<String, BindingInfo>,
    ) -> Option<Type> {
        let function = match &expr.kind {
            ExprKind::Name(name) => self.program.functions.get(name)?,
            ExprKind::Member { object, field } => {
                let Type::Module(module_path) = self.infer_expr_type(object, scope)? else {
                    return None;
                };
                self.module_namespace(&module_path)?.functions.get(field)?
            }
            _ => return None,
        };
        if function.decl.type_params.len() != type_args.len() {
            return None;
        }
        let concrete_args = type_args
            .iter()
            .map(|ty| self.lower_analysis_type_ref(ty))
            .collect::<Vec<_>>();
        let substitutions = crate::sema::substitutions_from_decl_type_args(
            &function.decl.type_params,
            &concrete_args,
        );
        Some(crate::sema::substitute_type(
            &function.signature.return_type,
            &substitutions,
        ))
    }

    fn infer_iterable_binding_type(
        &self,
        iterable: &Expr,
        scope: &BTreeMap<String, BindingInfo>,
    ) -> Option<Type> {
        if matches!(
            &iterable.kind,
            ExprKind::Call { callee, .. } if matches!(&callee.kind, ExprKind::Name(name) if name == "range")
        ) {
            return Some(Type::named("int64"));
        }

        if let ExprKind::Call { callee, args } = &iterable.kind {
            if let ExprKind::Name(name) = &callee.kind {
                let builtin_loop_form = !self.program.functions.contains_key(name)
                    && !self.program.classes.contains_key(name)
                    && !self.program.enums.contains_key(name);
                if builtin_loop_form && name == "enumerate" && args.len() == 1 {
                    let element_ty = self.infer_lockstep_element_type(&args[0].value, scope)?;
                    return Some(Type::Tuple(vec![Type::named("int64"), element_ty]));
                }
                if builtin_loop_form && name == "zip" && args.len() == 2 {
                    return Some(Type::Tuple(
                        args.iter()
                            .map(|arg| self.infer_lockstep_element_type(&arg.value, scope))
                            .collect::<Option<Vec<_>>>()?,
                    ));
                }
            }
        }

        let iterable_ty = self.infer_expr_type(iterable, scope)?;
        match base_type_name(&iterable_ty) {
            "Queue" | "list" | "set" => iterable_ty.type_arguments().first().cloned(),
            _ => None,
        }
    }

    fn infer_lockstep_element_type(
        &self,
        iterable: &Expr,
        scope: &BTreeMap<String, BindingInfo>,
    ) -> Option<Type> {
        let iterable_ty = self.infer_expr_type(iterable, scope)?;
        matches!(base_type_name(&iterable_ty), "list" | "set")
            .then(|| iterable_ty.type_arguments().first().cloned())
            .flatten()
    }

    #[cfg(test)]
    fn match_binding_type(
        &self,
        scrutinee_type: Option<&Type>,
        enum_name: Option<&str>,
        variant_name: &str,
    ) -> Option<Type> {
        self.match_binding_types(scrutinee_type, enum_name, variant_name)
            .into_iter()
            .next()
    }

    fn match_binding_types(
        &self,
        scrutinee_type: Option<&Type>,
        enum_name: Option<&str>,
        variant_name: &str,
    ) -> Vec<Type> {
        if let Some(ty) = scrutinee_type {
            match (base_type_name(ty), variant_name) {
                ("Option", "Some") => {
                    return ty.type_arguments().first().cloned().into_iter().collect()
                }
                ("Result", "Ok") => {
                    return ty.type_arguments().first().cloned().into_iter().collect()
                }
                ("Result", "Err") => {
                    return ty.type_arguments().get(1).cloned().into_iter().collect()
                }
                ("SendError", "Closed" | "Cancelled") => {
                    return ty.type_arguments().first().cloned().into_iter().collect()
                }
                _ => {}
            }
        }

        let Some(enum_name) = enum_name.or_else(|| scrutinee_type.map(base_type_name)) else {
            return Vec::new();
        };
        let Some(info) = self.resolve_named_enum_info(enum_name) else {
            return Vec::new();
        };
        let Some(variant) = info.variants.get(variant_name) else {
            return Vec::new();
        };
        let substitutions = scrutinee_type
            .map(TypeExt::type_arguments)
            .unwrap_or_default()
            .iter()
            .cloned()
            .zip(info.decl.type_params.iter().cloned())
            .map(|(ty, name)| (name, ty))
            .collect();
        variant
            .payloads
            .iter()
            .map(|payload| crate::sema::substitute_type(&payload.ty, &substitutions))
            .collect()
    }

    fn push_occurrence(
        &mut self,
        range: AnalysisRange,
        hover: String,
        definition: Option<AnalysisRange>,
    ) {
        self.output.occurrences.push(AnalysisOccurrence {
            line: range.line,
            start_character: range.start_character,
            end_character: range.end_character,
            hover,
            definition,
        });
    }

    fn insert_scope_binding(
        &self,
        name: &str,
        ty: Type,
        line: usize,
        kind: &str,
        scope: &mut BTreeMap<String, BindingInfo>,
    ) {
        let definition = self
            .find_identifier_range(line, name)
            .unwrap_or(AnalysisRange {
                file_path: self.current_source_path(),
                line: line.saturating_sub(1),
                start_character: 0,
                end_character: name.len(),
            });
        let hover = format_value_hover(kind, name, &ty);
        scope.insert(
            name.to_string(),
            BindingInfo {
                ty,
                trait_bounds: Vec::new(),
                definition,
                hover,
            },
        );
    }

    fn insert_scope_target(
        &self,
        target: &crate::ast::BindingTarget,
        ty: &Type,
        line: usize,
        kind: &str,
        scope: &mut BTreeMap<String, BindingInfo>,
    ) {
        match (target, ty) {
            (crate::ast::BindingTarget::Name { name, .. }, ty) => {
                self.insert_scope_binding(name, ty.clone(), line, kind, scope)
            }
            (crate::ast::BindingTarget::Tuple { elements, .. }, Type::Tuple(element_types)) => {
                for (element, element_ty) in elements.iter().zip(element_types) {
                    self.insert_scope_target(element, element_ty, line, kind, scope);
                }
            }
            _ => {}
        }
    }

    fn insert_scope_target_exact(
        &self,
        target: &crate::ast::BindingTarget,
        ty: &Type,
        kind: &str,
        scope: &mut BTreeMap<String, BindingInfo>,
    ) {
        match (target, ty) {
            (crate::ast::BindingTarget::Name { name, span }, ty) => {
                let definition =
                    range_from_span_with_path(*span, name.len(), self.current_source_path());
                scope.insert(
                    name.clone(),
                    BindingInfo {
                        ty: ty.clone(),
                        trait_bounds: Vec::new(),
                        definition,
                        hover: format_value_hover(kind, name, ty),
                    },
                );
            }
            (crate::ast::BindingTarget::Tuple { elements, .. }, Type::Tuple(element_types)) => {
                for (element, element_ty) in elements.iter().zip(element_types) {
                    self.insert_scope_target_exact(element, element_ty, kind, scope);
                }
            }
            _ => {}
        }
    }

    fn find_identifier_range(&self, line_number: usize, name: &str) -> Option<AnalysisRange> {
        let line_index = line_number.checked_sub(1)?;
        let text = *self.source_lines.get(line_index)?;
        find_identifier_in_line(text, name).map(|(start, end)| AnalysisRange {
            file_path: self.current_source_path(),
            line: line_index,
            start_character: start,
            end_character: end,
        })
    }

    fn find_match_enum_range(&self, line_number: usize, enum_name: &str) -> Option<AnalysisRange> {
        let line_index = line_number.checked_sub(1)?;
        let text = *self.source_lines.get(line_index)?;
        text.find(enum_name).map(|start| AnalysisRange {
            file_path: self.current_source_path(),
            line: line_index,
            start_character: start,
            end_character: start + enum_name.len(),
        })
    }

    fn find_match_variant_range(
        &self,
        line_number: usize,
        variant: &VariantPattern,
    ) -> Option<AnalysisRange> {
        let line_index = line_number.checked_sub(1)?;
        let text = *self.source_lines.get(line_index)?;
        let token = variant
            .enum_name
            .as_ref()
            .map(|enum_name| format!("{}.{}", enum_name, variant.variant_name))
            .unwrap_or_else(|| variant.variant_name.clone());
        let start = text.find(&token)?;
        let variant_start = start + token.len().saturating_sub(variant.variant_name.len());
        Some(AnalysisRange {
            file_path: self.current_source_path(),
            line: line_index,
            start_character: variant_start,
            end_character: variant_start + variant.variant_name.len(),
        })
    }
}

fn infer_builtin_variant_call<F>(
    enum_name: &str,
    variant_name: &str,
    args: &[crate::ast::Argument],
    infer_arg: F,
) -> Option<Type>
where
    F: Fn(&Expr) -> Option<Type>,
{
    match (enum_name, variant_name) {
        ("Option", "Some") => Some(Type::Named(
            "Option".to_string(),
            vec![args
                .first()
                .and_then(|arg| infer_arg(&arg.value))
                .unwrap_or(Type::Unit)],
        )),
        ("Option", "None") => Some(Type::Named("Option".to_string(), vec![Type::Unit])),
        ("Result", "Ok") => Some(Type::Named(
            "Result".to_string(),
            vec![
                args.first()
                    .and_then(|arg| infer_arg(&arg.value))
                    .unwrap_or(Type::Unit),
                Type::Unit,
            ],
        )),
        ("Result", "Err") => Some(Type::Named(
            "Result".to_string(),
            vec![
                Type::Unit,
                args.first()
                    .and_then(|arg| infer_arg(&arg.value))
                    .unwrap_or(Type::Unit),
            ],
        )),
        ("SendError", "Closed" | "Cancelled") => Some(Type::Named(
            "SendError".to_string(),
            vec![args
                .first()
                .and_then(|arg| infer_arg(&arg.value))
                .unwrap_or(Type::Unit)],
        )),
        _ => None,
    }
}

fn symbols_from_module(module: &Module) -> Vec<AnalysisSymbol> {
    let mut symbols = module
        .constants
        .iter()
        .map(|constant| AnalysisSymbol {
            name: constant.name.clone(),
            kind: "constant".to_string(),
            detail: constant
                .annotation
                .as_ref()
                .map(|ty| lower_type_ref(ty).to_string())
                .unwrap_or_else(|| "inferred".to_string()),
            line: constant.span.line.saturating_sub(1),
            start_character: constant.span.column.saturating_sub(1),
            end_character: constant.span.column.saturating_sub(1) + constant.name.len(),
            children: Vec::new(),
        })
        .collect::<Vec<_>>();
    for item in &module.items {
        match item {
            Item::Class(class_decl) => {
                symbols.push(AnalysisSymbol {
                    name: class_decl.name.clone(),
                    kind: "class".to_string(),
                    detail: String::new(),
                    line: class_decl.span.line.saturating_sub(1),
                    start_character: class_decl.span.column.saturating_sub(1),
                    end_character: class_decl.span.column.saturating_sub(1) + class_decl.name.len(),
                    children: class_decl
                        .fields
                        .iter()
                        .map(|field| AnalysisSymbol {
                            name: field.name.clone(),
                            kind: "field".to_string(),
                            detail: lower_type_ref(&field.ty).to_string(),
                            line: field.span.line.saturating_sub(1),
                            start_character: field.span.column.saturating_sub(1),
                            end_character: field.span.column.saturating_sub(1) + field.name.len(),
                            children: Vec::new(),
                        })
                        .chain(class_decl.methods.iter().map(|method| AnalysisSymbol {
                            name: method.name.clone(),
                            kind: "method".to_string(),
                            detail: format_decl_return(method),
                            line: method.span.line.saturating_sub(1),
                            start_character: method.span.column.saturating_sub(1),
                            end_character: method.span.column.saturating_sub(1) + method.name.len(),
                            children: Vec::new(),
                        }))
                        .collect(),
                });
            }
            Item::Enum(enum_decl) => {
                symbols.push(AnalysisSymbol {
                    name: enum_decl.name.clone(),
                    kind: "enum".to_string(),
                    detail: String::new(),
                    line: enum_decl.span.line.saturating_sub(1),
                    start_character: enum_decl.span.column.saturating_sub(1),
                    end_character: enum_decl.span.column.saturating_sub(1) + enum_decl.name.len(),
                    children: enum_decl
                        .variants
                        .iter()
                        .map(|variant| AnalysisSymbol {
                            name: variant.name.clone(),
                            kind: "variant".to_string(),
                            detail: if variant.payloads.is_empty() {
                                String::new()
                            } else {
                                variant
                                    .payloads
                                    .iter()
                                    .map(|payload| lower_type_ref(&payload.ty).to_string())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            },
                            line: variant.span.line.saturating_sub(1),
                            start_character: variant.span.column.saturating_sub(1),
                            end_character: variant.span.column.saturating_sub(1)
                                + variant.name.len(),
                            children: Vec::new(),
                        })
                        .collect(),
                });
            }
            Item::Function(function_decl) => {
                symbols.push(AnalysisSymbol {
                    name: function_decl.name.clone(),
                    kind: "function".to_string(),
                    detail: format_decl_return(function_decl),
                    line: function_decl.span.line.saturating_sub(1),
                    start_character: function_decl.span.column.saturating_sub(1),
                    end_character: function_decl.span.column.saturating_sub(1)
                        + function_decl.name.len(),
                    children: Vec::new(),
                });
            }
            Item::ExternFunction(function_decl) => {
                symbols.push(AnalysisSymbol {
                    name: function_decl.name.clone(),
                    kind: "function".to_string(),
                    detail: format!(
                        "extern \"{}\" -> {}",
                        function_decl.abi,
                        lower_type_ref(&function_decl.return_type)
                    ),
                    line: function_decl.name_span.line.saturating_sub(1),
                    start_character: function_decl.name_span.column.saturating_sub(1),
                    end_character: function_decl.name_span.column.saturating_sub(1)
                        + function_decl.name.len(),
                    children: Vec::new(),
                });
            }
            Item::ExternOpaqueClass(class_decl) => {
                symbols.push(AnalysisSymbol {
                    name: class_decl.name.clone(),
                    kind: "class".to_string(),
                    detail: format!("extern \"{}\" opaque", class_decl.abi),
                    line: class_decl.name_span.line.saturating_sub(1),
                    start_character: class_decl.name_span.column.saturating_sub(1),
                    end_character: class_decl.name_span.column.saturating_sub(1)
                        + class_decl.name.len(),
                    children: Vec::new(),
                });
            }
            Item::Trait(trait_decl) => {
                symbols.push(AnalysisSymbol {
                    name: trait_decl.name.clone(),
                    kind: "trait".to_string(),
                    detail: String::new(),
                    line: trait_decl.span.line.saturating_sub(1),
                    start_character: trait_decl.span.column.saturating_sub(1),
                    end_character: trait_decl.span.column.saturating_sub(1) + trait_decl.name.len(),
                    children: trait_decl
                        .methods
                        .iter()
                        .map(|method| AnalysisSymbol {
                            name: method.name.clone(),
                            kind: "method".to_string(),
                            detail: format_decl_return(method),
                            line: method.span.line.saturating_sub(1),
                            start_character: method.span.column.saturating_sub(1),
                            end_character: method.span.column.saturating_sub(1) + method.name.len(),
                            children: Vec::new(),
                        })
                        .collect(),
                });
            }
            Item::Impl(_) => {}
        }
    }
    symbols
}

fn analysis_diagnostic(error: &Diagnostic) -> AnalysisDiagnostic {
    let (line, start_character) = error
        .span
        .map(|span| (span.line.saturating_sub(1), span.column.saturating_sub(1)))
        .unwrap_or((0, 0));
    AnalysisDiagnostic {
        code: error.code.clone(),
        line,
        start_character,
        end_character: start_character + 1,
        message: error.message.clone(),
        severity: 1,
        secondary_spans: error
            .secondary_spans
            .iter()
            .map(|secondary| AnalysisDiagnosticSpan {
                line: secondary.span.line.saturating_sub(1),
                start_character: secondary.span.column.saturating_sub(1),
                end_character: secondary.span.column,
                label: secondary.label.clone(),
            })
            .collect(),
        notes: error.notes.clone(),
        help: error.help.clone(),
        edits: error
            .edits
            .iter()
            .map(|edit| AnalysisDiagnosticEdit {
                line: edit.start.line.saturating_sub(1),
                start_character: edit.start.column.saturating_sub(1),
                end_character: edit.end.column.saturating_sub(1),
                replacement: edit.replacement.clone(),
                applicability: edit.applicability.clone(),
            })
            .collect(),
        call_frames: error
            .call_frames
            .iter()
            .map(|frame| AnalysisRuntimeCallFrame {
                function: frame.function.clone(),
                span: analysis_frame_span(&frame.span),
            })
            .collect(),
        task_ancestry: error
            .task_ancestry
            .iter()
            .map(|frame| AnalysisRuntimeTaskFrame {
                task_function: frame.task_function.clone(),
                task_entry_span: analysis_frame_span(&frame.task_entry_span),
                parent_function: frame.parent_function.clone(),
                spawn_span: analysis_frame_span(&frame.spawn_span),
            })
            .collect(),
    }
}

fn analysis_frame_span(span: &RuntimeSourceSpan) -> AnalysisFrameSpan {
    AnalysisFrameSpan {
        file_path: span.path.clone(),
        line: span.start.line.saturating_sub(1),
        start_character: span.start.column.saturating_sub(1),
        end_character: span.end.column.saturating_sub(1),
    }
}

fn range_from_span(span: Span, len: usize) -> AnalysisRange {
    AnalysisRange {
        file_path: None,
        line: span.line.saturating_sub(1),
        start_character: span.column.saturating_sub(1),
        end_character: span.column.saturating_sub(1) + len,
    }
}

fn range_from_span_with_path(span: Span, len: usize, file_path: Option<String>) -> AnalysisRange {
    AnalysisRange {
        file_path,
        line: span.line.saturating_sub(1),
        start_character: span.column.saturating_sub(1),
        end_character: span.column.saturating_sub(1) + len,
    }
}

fn find_identifier_in_line(line: &str, name: &str) -> Option<(usize, usize)> {
    let mut search_from = 0;
    while let Some(offset) = line[search_from..].find(name) {
        let start = search_from + offset;
        let end = start + name.len();
        let before_ok = start == 0
            || !line[..start]
                .chars()
                .next_back()
                .map(is_identifier_continue)
                .unwrap_or(false);
        let after_ok = end == line.len()
            || !line[end..]
                .chars()
                .next()
                .map(is_identifier_continue)
                .unwrap_or(false);
        if before_ok && after_ok {
            return Some((start, end));
        }
        search_from = end;
    }
    None
}

fn is_identifier_continue(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn lower_type_ref(ty: &TypeRef) -> Type {
    match &ty.kind {
        crate::ast::TypeRefKind::Tuple(elements) => {
            Type::Tuple(elements.iter().map(lower_type_ref).collect())
        }
        crate::ast::TypeRefKind::Function {
            params,
            return_type,
        } => Type::Function {
            params: params
                .iter()
                .map(|param| FunctionParamContract {
                    name: String::new(),
                    ty: lower_type_ref(&param.ty),
                    passing: resolve_param_passing(param.mode),
                    has_default: false,
                    default_erased: true,
                })
                .collect(),
            return_type: Box::new(lower_type_ref(return_type)),
        },
        crate::ast::TypeRefKind::Named { name, args } if name == "None" => Type::Unit,
        crate::ast::TypeRefKind::Named { name, args } => {
            let name = match name.as_str() {
                "str" => "str",
                "int" => "int64",
                name => name,
            };
            Type::Named(name.to_string(), args.iter().map(lower_type_ref).collect())
        }
    }
}

fn base_type_name(ty: &Type) -> &str {
    match ty {
        Type::Unit => "None",
        Type::Module(name) => name.as_str(),
        Type::TypeParam(name) => name.as_str(),
        Type::Tuple(_) => "tuple",
        Type::Function { .. } => "function",
        Type::Closure { .. } => "closure",
        Type::Named(name, _) => name.as_str(),
    }
}

fn expression_start_span(expr: &Expr) -> Span {
    let leftmost_child = match &expr.kind {
        ExprKind::Membership { value, .. } => Some(value.as_ref()),
        ExprKind::CompareChain { first, .. } => Some(first.as_ref()),
        ExprKind::Member { object, .. }
        | ExprKind::Specialize { expr: object, .. }
        | ExprKind::Cast { expr: object, .. }
        | ExprKind::Try(object)
        | ExprKind::Group(object)
        | ExprKind::Unary { expr: object, .. } => Some(object.as_ref()),
        ExprKind::Call { callee, .. } => Some(callee.as_ref()),
        ExprKind::Binary { left, .. } => Some(left.as_ref()),
        ExprKind::Conditional { then_expr, .. } => Some(then_expr.as_ref()),
        ExprKind::Index { object, .. } | ExprKind::Slice { object, .. } => Some(object.as_ref()),
        _ => None,
    };
    leftmost_child
        .map(|child| span_min(expr.span, expression_start_span(child)))
        .unwrap_or(expr.span)
}

fn span_min(left: Span, right: Span) -> Span {
    if (right.line, right.column) < (left.line, left.column) {
        right
    } else {
        left
    }
}

fn expression_contains_position(expr: &Expr, line: usize, character: usize) -> bool {
    let starts_before_position =
        expr.span.line < line || (expr.span.line == line && expr.span.column <= character + 1);
    starts_before_position && line <= expression_end_line(expr)
}

fn position_is_before_span(line: usize, character: usize, span: Span) -> bool {
    line < span.line || (line == span.line && character + 1 < span.column)
}

fn expression_end_line(expr: &Expr) -> usize {
    let child_end = match &expr.kind {
        ExprKind::Membership {
            value, container, ..
        } => expression_end_line(value).max(expression_end_line(container)),
        ExprKind::CompareChain { first, links } => links
            .iter()
            .map(|link| expression_end_line(&link.operand))
            .fold(expression_end_line(first), usize::max),
        ExprKind::Member { object, .. }
        | ExprKind::Specialize { expr: object, .. }
        | ExprKind::Cast { expr: object, .. }
        | ExprKind::Unary { expr: object, .. }
        | ExprKind::Try(object)
        | ExprKind::Group(object) => expression_end_line(object),
        ExprKind::Lambda { body, .. } => expression_end_line(body),
        ExprKind::Call { callee, args } => args
            .iter()
            .map(|arg| expression_end_line(&arg.value))
            .fold(expression_end_line(callee), usize::max),
        ExprKind::FString(parts) => parts
            .iter()
            .filter_map(|part| match part {
                crate::ast::FormatPart::Expr(part_expr)
                | crate::ast::FormatPart::Formatted {
                    expr: part_expr, ..
                } => Some(expression_end_line(part_expr)),
                crate::ast::FormatPart::Literal(_) => None,
            })
            .fold(expr.span.line, usize::max),
        ExprKind::Binary { left, right, .. } => {
            expression_end_line(left).max(expression_end_line(right))
        }
        ExprKind::Conditional {
            then_expr,
            condition,
            else_expr,
        } => expression_end_line(then_expr)
            .max(expression_end_line(condition))
            .max(expression_end_line(else_expr)),
        ExprKind::Tuple(elements) | ExprKind::List(elements) | ExprKind::Set(elements) => elements
            .iter()
            .map(expression_end_line)
            .fold(expr.span.line, usize::max),
        ExprKind::Map(entries) => entries
            .iter()
            .map(|entry| expression_end_line(&entry.key).max(expression_end_line(&entry.value)))
            .fold(expr.span.line, usize::max),
        ExprKind::Comprehension { output, clauses } => {
            let output_end = match output {
                ComprehensionOutput::List(value) | ComprehensionOutput::Set(value) => {
                    expression_end_line(value)
                }
                ComprehensionOutput::Map { key, value } => {
                    expression_end_line(key).max(expression_end_line(value))
                }
            };
            clauses.iter().fold(output_end, |end, clause| {
                clause
                    .filters
                    .iter()
                    .map(expression_end_line)
                    .fold(end.max(expression_end_line(&clause.iterable)), usize::max)
            })
        }
        ExprKind::Index { object, index } => {
            expression_end_line(object).max(expression_end_line(index))
        }
        ExprKind::Slice {
            object,
            start,
            end,
            colon_span,
        } => {
            let mut end_line = expression_end_line(object).max(colon_span.line);
            if let Some(start) = start {
                end_line = end_line.max(expression_end_line(start));
            }
            if let Some(end) = end {
                end_line = end_line.max(expression_end_line(end));
            }
            end_line
        }
        ExprKind::Match {
            scrutinee, arms, ..
        } => arms
            .iter()
            .map(|arm| {
                arm.guard
                    .as_ref()
                    .map(expression_end_line)
                    .unwrap_or(arm.span.line)
                    .max(expression_end_line(&arm.value))
            })
            .fold(expression_end_line(scrutinee), usize::max),
        ExprKind::Name(_)
        | ExprKind::Int(_)
        | ExprKind::DurationNanos(_)
        | ExprKind::BuiltinOmitted
        | ExprKind::Float(_)
        | ExprKind::Bool(_)
        | ExprKind::String(_) => expr.span.line,
    };
    expr.span.line.max(child_end)
}

fn analysis_is_integer_literal(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Int(_) => true,
        ExprKind::Group(inner) => analysis_is_integer_literal(inner),
        ExprKind::Unary {
            op: crate::ast::UnaryOp::Neg,
            expr: inner,
        } => matches!(inner.kind, ExprKind::Int(_)),
        _ => false,
    }
}

fn analysis_is_float_literal(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Float(_) => true,
        ExprKind::Group(inner) => analysis_is_float_literal(inner),
        _ => false,
    }
}

fn analysis_type_contains_unknown(ty: &Type) -> bool {
    matches!(ty, Type::Named(name, _) if name == "Unknown")
        || matches!(ty, Type::Named(_, args) if args.iter().any(analysis_type_contains_unknown))
        || matches!(ty, Type::Tuple(elements) if elements.iter().any(analysis_type_contains_unknown))
        || matches!(
            ty,
            Type::Function {
                params,
                return_type,
                ..
            } if params
                .iter()
                .any(|param| analysis_type_contains_unknown(&param.ty))
                || analysis_type_contains_unknown(return_type)
        )
        || matches!(
            ty,
            Type::Closure {
                params,
                return_type,
                captures,
                ..
            } if params
                .iter()
                .any(|param| analysis_type_contains_unknown(&param.ty))
                || analysis_type_contains_unknown(return_type)
                || captures
                    .iter()
                    .any(|capture| analysis_type_contains_unknown(&capture.ty))
        )
}

fn analysis_is_numeric_type(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Named(name, args)
            if args.is_empty()
                && matches!(
                    name.as_str(),
                    "int8"
                        | "int16"
                        | "int32"
                        | "int64"
                        | "int128"
                        | "intsize"
                        | "uint8"
                        | "uint16"
                        | "uint32"
                        | "uint64"
                        | "uint128"
                        | "uintsize"
                        | "float32"
                        | "float64"
                )
    )
}

fn analysis_is_float_type(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Named(name, args)
            if args.is_empty() && matches!(name.as_str(), "float32" | "float64")
    )
}

trait TypeExt {
    fn type_arguments(&self) -> &[Type];
}

impl TypeExt for Type {
    fn type_arguments(&self) -> &[Type] {
        match self {
            Type::Unit => &[],
            Type::Module(_) => &[],
            Type::TypeParam(_) => &[],
            Type::Tuple(elements) => elements.as_slice(),
            Type::Function { .. } => &[],
            Type::Closure { .. } => &[],
            Type::Named(_, args) => args.as_slice(),
        }
    }
}

fn format_value_hover(kind: &str, name: &str, ty: &Type) -> String {
    format!("```aura\n{} {}: {}\n```", kind, name, ty)
}

fn view_source_root(expr: &Expr) -> Option<&str> {
    match &expr.kind {
        ExprKind::Name(name) => Some(name),
        ExprKind::Group(inner)
        | ExprKind::Member { object: inner, .. }
        | ExprKind::Index { object: inner, .. } => view_source_root(inner),
        _ => None,
    }
}

fn render_view_source(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::Name(name) => Some(name.clone()),
        ExprKind::Group(inner) => render_view_source(inner),
        ExprKind::Member { object, field } => {
            Some(format!("{}.{}", render_view_source(object)?, field))
        }
        ExprKind::Index { object, index } => {
            let ExprKind::Int(index) = index.kind else {
                return None;
            };
            Some(format!("{}[{index}]", render_view_source(object)?))
        }
        _ => None,
    }
}

fn format_decl_return(function_decl: &FunctionDecl) -> String {
    let ty = lower_type_ref(&function_decl.return_type);
    match &function_decl.view_return {
        Some(view_return) => format!(
            "view {}{} from {}",
            if view_return.mutable { "mut " } else { "" },
            ty,
            view_return.origin
        ),
        None => ty.to_string(),
    }
}

fn format_param_hover(param: &Param, ty: &Type) -> String {
    let mode = match param.mode {
        ParamMode::Default => "",
        ParamMode::Own => "own ",
        ParamMode::BorrowMut => "mut ",
    };
    format!("```aura\nparam {}: {}{}\n```", param.name, mode, ty)
}

fn format_lambda_param_hover(param: &LambdaParam, ty: &Type) -> String {
    let mode = match param.mode {
        ParamMode::Default => "",
        ParamMode::Own => "own ",
        ParamMode::BorrowMut => "mut ",
    };
    format!("```aura\nparam {}: {}{}\n```", param.name, mode, ty)
}

fn format_param_decl(param: &Param) -> String {
    let mode = match param.mode {
        ParamMode::Default => String::new(),
        ParamMode::Own => "own ".to_string(),
        ParamMode::BorrowMut => "mut ".to_string(),
    };
    let default = if param.default.is_some() {
        " = ..."
    } else {
        ""
    };
    format!(
        "{}: {}{}{}",
        param.name,
        mode,
        lower_type_ref(&param.ty),
        default
    )
}

fn append_alias_target(
    hover: String,
    local_name: &str,
    module_name: &str,
    target_name: &str,
) -> String {
    if local_name == target_name {
        hover
    } else {
        format!("{hover}\n\nAlias `{local_name}` for `{module_name}.{target_name}`.")
    }
}

fn format_function_hover(function_decl: &FunctionDecl) -> String {
    let params = function_decl
        .params
        .iter()
        .map(format_param_decl)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "```aura\nfunction {}({}) -> {}\n```",
        function_decl.name,
        params,
        format_decl_return(function_decl)
    )
}

fn format_extern_function_hover(function_decl: &crate::ast::ExternFunctionDecl) -> String {
    let params = function_decl
        .params
        .iter()
        .map(format_param_decl)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "```aura\nextern \"{}\" function {}({}) -> {}\n```",
        function_decl.abi,
        function_decl.name,
        params,
        lower_type_ref(&function_decl.return_type)
    )
}

fn format_extern_opaque_hover(handle_decl: &crate::ast::ExternOpaqueClassDecl) -> String {
    format!(
        "```aura\nextern \"{}\" opaque class {}\n```",
        handle_decl.abi, handle_decl.name
    )
}

fn format_method_hover(method_decl: &FunctionDecl) -> String {
    let params = method_decl
        .receiver
        .map(canonical_receiver_spelling)
        .into_iter()
        .map(str::to_string)
        .chain(method_decl.params.iter().map(format_param_decl))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "```aura\nmethod {}({}) -> {}\n```",
        method_decl.name,
        params,
        format_decl_return(method_decl)
    )
}

fn format_class_hover(class_info: &ClassInfo) -> String {
    if let Some(constructor) = class_info.builtin_constructor() {
        return match constructor {
            BuiltinClassConstructor::RandomRng => format!(
                "```aura\nclass Rng(seed: int64)\n```\n{}",
                constructor.docs()
            ),
        };
    }
    let mut fields = class_info
        .fields
        .iter()
        .map(|(name, field)| format!("{}: {}", name, field.ty))
        .collect::<Vec<_>>();
    fields.sort();
    if fields.is_empty() {
        format!("```aura\nclass {}\n```", class_info.decl.name)
    } else {
        format!(
            "```aura\nclass {}\n{}\n```",
            class_info.decl.name,
            fields.join("\n")
        )
    }
}

fn format_class_detail(class_info: &ClassInfo) -> String {
    if let Some(constructor) = class_info.builtin_constructor() {
        return match constructor {
            BuiltinClassConstructor::RandomRng => "Rng(seed: int64)".to_string(),
        };
    }
    let fields = class_info
        .decl
        .fields
        .iter()
        .map(|field| {
            let default = if field.default.is_some() {
                " = ..."
            } else {
                ""
            };
            format!(
                "{}: own {}{}",
                field.name,
                lower_type_ref(&field.ty),
                default
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{}({})", class_info.decl.name, fields)
}

fn format_enum_hover_named(enum_name: &str) -> String {
    format!("```aura\nenum {enum_name}\n```")
}

fn builtin_enum_hover(detail: &str, docs: &str) -> String {
    format!("```aura\nenum {}\n```\n{}", detail, docs)
}

fn builtin_type_hover(detail: &str, docs: &str) -> String {
    format!("```aura\ntype {}\n```\n{}", detail, docs)
}

fn builtin_function_hover(detail: &str, docs: &str) -> String {
    format!("```aura\n{}\n```\n{}", detail, docs)
}

fn format_variant_hover(enum_name: &str, variant_name: &str, payload: Option<&Type>) -> String {
    format_variant_hover_payloads(
        enum_name,
        variant_name,
        payload.into_iter().map(|payload| format!("own {payload}")),
    )
}

fn format_variant_hover_payloads(
    enum_name: &str,
    variant_name: &str,
    payloads: impl IntoIterator<Item = String>,
) -> String {
    let payloads = payloads.into_iter().collect::<Vec<_>>();
    if payloads.is_empty() {
        format!("```aura\nvariant {} -> {}\n```", variant_name, enum_name)
    } else {
        format!(
            "```aura\nvariant {}({}) -> {}\n```",
            variant_name,
            payloads.join(", "),
            enum_name
        )
    }
}

fn format_enum_variant_payload(payload: &crate::sema::EnumPayloadFieldInfo) -> String {
    match payload.name.as_deref() {
        Some(name) => format!("{name}: own {}", payload.ty),
        None => format!("own {}", payload.ty),
    }
}

const KEYWORDS: &[&str] = &[
    "class", "enum", "trait", "def", "if", "elif", "else", "while", "for", "in", "match", "case",
    "with", "return", "assert", "try", "lambda", "public", "mut", "own", "indirect", "copy",
    "break", "continue", "pass",
];

struct CompletionMeta {
    name: &'static str,
    detail: &'static str,
}

const BUILTIN_ENUM_COMPLETIONS: &[CompletionMeta] = &[
    CompletionMeta {
        name: "Option",
        detail: "enum Option[T]",
    },
    CompletionMeta {
        name: "Result",
        detail: "enum Result[T, E]",
    },
    CompletionMeta {
        name: "SendError",
        detail: "enum SendError[T]",
    },
    CompletionMeta {
        name: "QueueReceive",
        detail: "enum QueueReceive[T]",
    },
    CompletionMeta {
        name: "TaskResult",
        detail: "enum TaskResult[T]",
    },
    CompletionMeta {
        name: "WaitAny",
        detail: "enum WaitAny[T]",
    },
    CompletionMeta {
        name: "WaitAll",
        detail: "enum WaitAll[T]",
    },
    CompletionMeta {
        name: "SelectOutcome",
        detail: "enum SelectOutcome[Q, T]",
    },
];

fn builtin_enum_variant_completions(base_name: &str) -> Vec<AnalysisCompletion> {
    match base_name {
        "Option" => vec![
            AnalysisCompletion {
                name: "Some".to_string(),
                kind: "variant".to_string(),
                detail: "Some(own T) -> Option".to_string(),
            },
            AnalysisCompletion {
                name: "None".to_string(),
                kind: "variant".to_string(),
                detail: "None -> Option".to_string(),
            },
        ],
        "Result" => vec![
            AnalysisCompletion {
                name: "Ok".to_string(),
                kind: "variant".to_string(),
                detail: "Ok(own T) -> Result".to_string(),
            },
            AnalysisCompletion {
                name: "Err".to_string(),
                kind: "variant".to_string(),
                detail: "Err(own E) -> Result".to_string(),
            },
        ],
        "SendError" => vec![
            AnalysisCompletion {
                name: "Closed".to_string(),
                kind: "variant".to_string(),
                detail: "Closed(own T) -> SendError".to_string(),
            },
            AnalysisCompletion {
                name: "Cancelled".to_string(),
                kind: "variant".to_string(),
                detail: "Cancelled(own T) -> SendError".to_string(),
            },
            AnalysisCompletion {
                name: "TimedOut".to_string(),
                kind: "variant".to_string(),
                detail: "TimedOut(own T) -> SendError".to_string(),
            },
            AnalysisCompletion {
                name: "Full".to_string(),
                kind: "variant".to_string(),
                detail: "Full(own T) -> SendError".to_string(),
            },
        ],
        "QueueReceive" => vec![
            AnalysisCompletion {
                name: "Item".to_string(),
                kind: "variant".to_string(),
                detail: "Item(own T) -> QueueReceive".to_string(),
            },
            AnalysisCompletion {
                name: "Closed".to_string(),
                kind: "variant".to_string(),
                detail: "Closed -> QueueReceive".to_string(),
            },
            AnalysisCompletion {
                name: "TimedOut".to_string(),
                kind: "variant".to_string(),
                detail: "TimedOut -> QueueReceive".to_string(),
            },
            AnalysisCompletion {
                name: "Cancelled".to_string(),
                kind: "variant".to_string(),
                detail: "Cancelled -> QueueReceive".to_string(),
            },
        ],
        "TaskResult" => vec![
            AnalysisCompletion {
                name: "Ready".to_string(),
                kind: "variant".to_string(),
                detail: "Ready(own T) -> TaskResult".to_string(),
            },
            AnalysisCompletion {
                name: "Error".to_string(),
                kind: "variant".to_string(),
                detail: "Error(own str) -> TaskResult".to_string(),
            },
            AnalysisCompletion {
                name: "TimedOut".to_string(),
                kind: "variant".to_string(),
                detail: "TimedOut -> TaskResult".to_string(),
            },
            AnalysisCompletion {
                name: "Cancelled".to_string(),
                kind: "variant".to_string(),
                detail: "Cancelled -> TaskResult".to_string(),
            },
        ],
        "WaitAny" => vec![
            AnalysisCompletion {
                name: "Ready".to_string(),
                kind: "variant".to_string(),
                detail: "Ready(own int64, own T) -> WaitAny".to_string(),
            },
            AnalysisCompletion {
                name: "Error".to_string(),
                kind: "variant".to_string(),
                detail: "Error(own int64, own str) -> WaitAny".to_string(),
            },
            AnalysisCompletion {
                name: "TimedOut".to_string(),
                kind: "variant".to_string(),
                detail: "TimedOut -> WaitAny".to_string(),
            },
            AnalysisCompletion {
                name: "Cancelled".to_string(),
                kind: "variant".to_string(),
                detail: "Cancelled -> WaitAny".to_string(),
            },
        ],
        "WaitAll" => vec![
            AnalysisCompletion {
                name: "Ready".to_string(),
                kind: "variant".to_string(),
                detail: "Ready(own list[T]) -> WaitAll".to_string(),
            },
            AnalysisCompletion {
                name: "Error".to_string(),
                kind: "variant".to_string(),
                detail: "Error(own int64, own str) -> WaitAll".to_string(),
            },
            AnalysisCompletion {
                name: "TimedOut".to_string(),
                kind: "variant".to_string(),
                detail: "TimedOut -> WaitAll".to_string(),
            },
            AnalysisCompletion {
                name: "Cancelled".to_string(),
                kind: "variant".to_string(),
                detail: "Cancelled -> WaitAll".to_string(),
            },
        ],
        "SelectOutcome" => vec![
            AnalysisCompletion {
                name: "Queue".to_string(),
                kind: "variant".to_string(),
                detail: "Queue(own int64, own QueueReceive[Q]) -> SelectOutcome".to_string(),
            },
            AnalysisCompletion {
                name: "Task".to_string(),
                kind: "variant".to_string(),
                detail: "Task(own int64, own TaskResult[T]) -> SelectOutcome".to_string(),
            },
            AnalysisCompletion {
                name: "Deadline".to_string(),
                kind: "variant".to_string(),
                detail: "Deadline(own int64) -> SelectOutcome".to_string(),
            },
            AnalysisCompletion {
                name: "Cancelled".to_string(),
                kind: "variant".to_string(),
                detail: "Cancelled -> SelectOutcome".to_string(),
            },
        ],
        _ => Vec::new(),
    }
}

fn builtin_member_completions(receiver_type: &Type) -> Vec<AnalysisCompletion> {
    let mut completions = Vec::new();
    match base_type_name(receiver_type) {
        "list" => {
            completions.extend([
                AnalysisCompletion {
                    name: "len".to_string(),
                    kind: "method".to_string(),
                    detail: "len() -> int64".to_string(),
                },
                AnalysisCompletion {
                    name: "is_empty".to_string(),
                    kind: "method".to_string(),
                    detail: "is_empty() -> bool".to_string(),
                },
                AnalysisCompletion {
                    name: "copy".to_string(),
                    kind: "method".to_string(),
                    detail: "copy() -> list[T]".to_string(),
                },
                AnalysisCompletion {
                    name: "append".to_string(),
                    kind: "method".to_string(),
                    detail: BuiltinMember::VecPush.detail().to_string(),
                },
                AnalysisCompletion {
                    name: "pop".to_string(),
                    kind: "method".to_string(),
                    detail: "pop(index: int64 = -1) -> T".to_string(),
                },
                AnalysisCompletion {
                    name: "get".to_string(),
                    kind: "method".to_string(),
                    detail: "get(index: int64) -> Option[T]".to_string(),
                },
                AnalysisCompletion {
                    name: "set".to_string(),
                    kind: "method".to_string(),
                    detail: BuiltinMember::VecSet.detail().to_string(),
                },
                AnalysisCompletion {
                    name: "remove".to_string(),
                    kind: "method".to_string(),
                    detail: "remove(value: T) -> None".to_string(),
                },
                AnalysisCompletion {
                    name: "swap".to_string(),
                    kind: "method".to_string(),
                    detail: "swap(first: int64, second: int64) -> None".to_string(),
                },
                AnalysisCompletion {
                    name: "insert".to_string(),
                    kind: "method".to_string(),
                    detail: BuiltinMember::VecInsert.detail().to_string(),
                },
                AnalysisCompletion {
                    name: "clear".to_string(),
                    kind: "method".to_string(),
                    detail: "clear() -> None".to_string(),
                },
                AnalysisCompletion {
                    name: "reverse".to_string(),
                    kind: "method".to_string(),
                    detail: "reverse() -> None".to_string(),
                },
                AnalysisCompletion {
                    name: "extend".to_string(),
                    kind: "method".to_string(),
                    detail: BuiltinMember::VecExtend.detail().to_string(),
                },
                AnalysisCompletion {
                    name: "sort".to_string(),
                    kind: "method".to_string(),
                    detail: BuiltinMember::VecSort.detail().to_string(),
                },
                AnalysisCompletion {
                    name: "map".to_string(),
                    kind: "method".to_string(),
                    detail: BuiltinMember::VecMap.detail().to_string(),
                },
                AnalysisCompletion {
                    name: "filter".to_string(),
                    kind: "method".to_string(),
                    detail: BuiltinMember::VecFilter.detail().to_string(),
                },
            ]);
        }
        "dict" => {
            completions.extend([
                AnalysisCompletion {
                    name: "items".to_string(),
                    kind: "method".to_string(),
                    detail: "items() -> list[(K, V)]".to_string(),
                },
                AnalysisCompletion {
                    name: "clear".to_string(),
                    kind: "method".to_string(),
                    detail: "clear() -> None".to_string(),
                },
                AnalysisCompletion {
                    name: "update".to_string(),
                    kind: "method".to_string(),
                    detail: BuiltinMember::MapExtend.detail().to_string(),
                },
            ]);
        }
        "set" => {
            completions.extend([
                AnalysisCompletion {
                    name: "is_empty".to_string(),
                    kind: "method".to_string(),
                    detail: "is_empty() -> bool".to_string(),
                },
                AnalysisCompletion {
                    name: "copy".to_string(),
                    kind: "method".to_string(),
                    detail: "copy() -> set[T]".to_string(),
                },
                AnalysisCompletion {
                    name: "add".to_string(),
                    kind: "method".to_string(),
                    detail: BuiltinMember::SetInsert.detail().to_string(),
                },
                AnalysisCompletion {
                    name: "remove".to_string(),
                    kind: "method".to_string(),
                    detail: "remove(value: T) -> None".to_string(),
                },
            ]);
        }
        _ => {}
    }

    for builtin in [
        BuiltinMember::IntegerWrappingAdd,
        BuiltinMember::IntegerWrappingSub,
        BuiltinMember::IntegerWrappingMul,
        BuiltinMember::IntegerSaturatingAdd,
        BuiltinMember::IntegerSaturatingSub,
        BuiltinMember::IntegerSaturatingMul,
        BuiltinMember::IntegerWrappingShl,
        BuiltinMember::IntegerWrappingShr,
        BuiltinMember::IntegerSaturatingShl,
        BuiltinMember::IntegerSaturatingShr,
        BuiltinMember::ArrayShape,
        BuiltinMember::ArrayLen,
        BuiltinMember::ArrayClone,
        BuiltinMember::ArrayGet,
        BuiltinMember::ArraySet,
        BuiltinMember::ArrayFill,
        BuiltinMember::ArrayMap,
        BuiltinMember::ArraySum,
        BuiltinMember::ArrayMin,
        BuiltinMember::ArrayMax,
        BuiltinMember::ArrayMean,
        BuiltinMember::ArrayWrappingAdd,
        BuiltinMember::ArrayWrappingSub,
        BuiltinMember::ArrayWrappingMul,
        BuiltinMember::ArraySaturatingAdd,
        BuiltinMember::ArraySaturatingSub,
        BuiltinMember::ArraySaturatingMul,
        BuiltinMember::FileReadAll,
        BuiltinMember::FileReadBytes,
        BuiltinMember::FileWriteAll,
        BuiltinMember::FileWriteBytes,
        BuiltinMember::FileFlush,
        BuiltinMember::FileClose,
        BuiltinMember::TcpListenerAccept,
        BuiltinMember::TcpListenerLocalAddr,
        BuiltinMember::TcpListenerClose,
        BuiltinMember::TcpStreamReadAll,
        BuiltinMember::TcpStreamReadLine,
        BuiltinMember::TcpStreamReadBytes,
        BuiltinMember::TcpStreamReadExact,
        BuiltinMember::TcpStreamWriteAll,
        BuiltinMember::TcpStreamWriteBytes,
        BuiltinMember::TcpStreamFlush,
        BuiltinMember::TcpStreamLocalAddr,
        BuiltinMember::TcpStreamPeerAddr,
        BuiltinMember::TcpStreamShutdownRead,
        BuiltinMember::TcpStreamShutdownWrite,
        BuiltinMember::TcpStreamShutdownBoth,
        BuiltinMember::TcpStreamClose,
        BuiltinMember::UdpSocketSendText,
        BuiltinMember::UdpSocketSendBytes,
        BuiltinMember::UdpSocketRecv,
        BuiltinMember::UdpSocketRecvFrom,
        BuiltinMember::UdpSocketLocalAddr,
        BuiltinMember::UdpSocketPeerAddr,
        BuiltinMember::UdpSocketClose,
        BuiltinMember::UdpDatagramAddress,
        BuiltinMember::UdpDatagramBytes,
        BuiltinMember::UdpDatagramText,
        BuiltinMember::HttpListenerAccept,
        BuiltinMember::HttpListenerLocalAddr,
        BuiltinMember::HttpListenerClose,
        BuiltinMember::HttpExchangeMethod,
        BuiltinMember::HttpExchangePath,
        BuiltinMember::HttpExchangeHeaders,
        BuiltinMember::HttpExchangeBodyText,
        BuiltinMember::HttpExchangeBodyBytes,
        BuiltinMember::HttpExchangeRespondText,
        BuiltinMember::HttpExchangeRespondBytes,
        BuiltinMember::HttpResponseStatus,
        BuiltinMember::HttpResponseReason,
        BuiltinMember::HttpResponseHeaders,
        BuiltinMember::HttpResponseText,
        BuiltinMember::HttpResponseBytes,
        BuiltinMember::WebSocketListenerAccept,
        BuiltinMember::WebSocketListenerLocalAddr,
        BuiltinMember::WebSocketSendText,
        BuiltinMember::WebSocketSendBytes,
        BuiltinMember::WebSocketRecvText,
        BuiltinMember::WebSocketRecvBytes,
        BuiltinMember::WebSocketClose,
        BuiltinMember::UnixListenerAccept,
        BuiltinMember::UnixListenerClose,
        BuiltinMember::UnixStreamReadLine,
        BuiltinMember::UnixStreamReadExact,
        BuiltinMember::UnixStreamWriteAll,
        BuiltinMember::UnixStreamClose,
        BuiltinMember::TlsListenerAccept,
        BuiltinMember::TlsListenerLocalAddr,
        BuiltinMember::TlsListenerClose,
        BuiltinMember::TlsStreamReadLine,
        BuiltinMember::TlsStreamReadExact,
        BuiltinMember::TlsStreamWriteAll,
        BuiltinMember::TlsStreamClose,
        BuiltinMember::ProcessChildStdin,
        BuiltinMember::ProcessChildStdout,
        BuiltinMember::ProcessChildStderr,
        BuiltinMember::ProcessChildWait,
        BuiltinMember::ProcessChildWaitOrNone,
        BuiltinMember::ProcessChildWaitOk,
        BuiltinMember::ProcessChildKill,
        BuiltinMember::ProcessChildTerminate,
        BuiltinMember::ProcessChildClose,
        BuiltinMember::ProcessPipeReadAll,
        BuiltinMember::ProcessPipeReadLine,
        BuiltinMember::ProcessPipeReadBytes,
        BuiltinMember::ProcessPipeWriteAll,
        BuiltinMember::ProcessPipeWriteBytes,
        BuiltinMember::ProcessPipeFlush,
        BuiltinMember::ProcessPipeClose,
        BuiltinMember::ProcessCompletedStatus,
        BuiltinMember::ProcessCompletedSuccess,
        BuiltinMember::ProcessCompletedStdout,
        BuiltinMember::ProcessCompletedStdoutBytes,
        BuiltinMember::ProcessCompletedStderr,
        BuiltinMember::ProcessCompletedStderrBytes,
        BuiltinMember::ProcessCompletedCheck,
        BuiltinMember::FloatSqrt,
        BuiltinMember::IntegerToFloat,
        BuiltinMember::DurationToMilliseconds,
        BuiltinMember::DurationToSeconds,
        BuiltinMember::StringLen,
        BuiltinMember::StringByteLen,
        BuiltinMember::StringContains,
        BuiltinMember::StringStartsWith,
        BuiltinMember::StringEndsWith,
        BuiltinMember::StringSplit,
        BuiltinMember::StringReplace,
        BuiltinMember::StringToLower,
        BuiltinMember::StringToUpper,
        BuiltinMember::StringJoin,
        BuiltinMember::StringStripPrefix,
        BuiltinMember::StringStripSuffix,
        BuiltinMember::StringTrim,
        BuiltinMember::StringToBytes,
        BuiltinMember::StringClone,
        BuiltinMember::ScalarToString,
        BuiltinMember::VecInsert,
        BuiltinMember::VecIndex,
        BuiltinMember::VecCount,
        BuiltinMember::VecReserve,
        BuiltinMember::VecClear,
        BuiltinMember::VecReverse,
        BuiltinMember::VecSort,
        BuiltinMember::VecMap,
        BuiltinMember::VecFilter,
        BuiltinMember::MapLen,
        BuiltinMember::MapIsEmpty,
        BuiltinMember::MapClone,
        BuiltinMember::MapGet,
        BuiltinMember::MapSet,
        BuiltinMember::MapRemove,
        BuiltinMember::MapContainsKey,
        BuiltinMember::MapKeys,
        BuiltinMember::MapValues,
        BuiltinMember::MapItems,
        BuiltinMember::MapClear,
        BuiltinMember::MapExtend,
        BuiltinMember::MapReserve,
        BuiltinMember::SetLen,
        BuiltinMember::SetIsEmpty,
        BuiltinMember::SetClone,
        BuiltinMember::SetContains,
        BuiltinMember::SetInsert,
        BuiltinMember::SetRemove,
        BuiltinMember::SetDiscard,
        BuiltinMember::SetClear,
        BuiltinMember::SetReserve,
        BuiltinMember::QueuePut,
        BuiltinMember::QueueTryPut,
        BuiltinMember::QueueGet,
        BuiltinMember::QueueGetOrNone,
        BuiltinMember::QueueGetOr,
        BuiltinMember::QueueClose,
        BuiltinMember::TaskResult,
        BuiltinMember::TaskResultOrNone,
        BuiltinMember::TaskResultOr,
        BuiltinMember::TaskGroupStart,
        BuiltinMember::TaskGroupStartSoon,
        BuiltinMember::TaskGroupStartWithStack,
        BuiltinMember::TaskGroupStartSoonWithStack,
        BuiltinMember::TaskGroupCancel,
        BuiltinMember::RngNextInt,
        BuiltinMember::RngNextFloat,
        BuiltinMember::RngShuffle,
    ] {
        if is_array_integer_arithmetic_member(builtin)
            && !matches!(
                receiver_type,
                Type::Named(name, args)
                    if name == "Array"
                        && args.len() == 1
                        && matches!(
                            &args[0],
                            Type::Named(dtype, dtype_args)
                                if dtype_args.is_empty()
                                    && matches!(dtype.as_str(), "int32" | "int64")
                        )
            )
        {
            continue;
        }
        if BuiltinMember::resolve(base_type_name(receiver_type), builtin.name()) == Some(builtin)
            && !completions
                .iter()
                .any(|completion| completion.name == builtin.name())
        {
            completions.push(AnalysisCompletion {
                name: builtin.name().to_string(),
                kind: "method".to_string(),
                detail: builtin.detail().to_string(),
            });
        }
    }

    completions
}

fn is_array_integer_arithmetic_member(member: BuiltinMember) -> bool {
    matches!(
        member,
        BuiltinMember::ArrayWrappingAdd
            | BuiltinMember::ArrayWrappingSub
            | BuiltinMember::ArrayWrappingMul
            | BuiltinMember::ArraySaturatingAdd
            | BuiltinMember::ArraySaturatingSub
            | BuiltinMember::ArraySaturatingMul
    )
}

fn builtin_associated_function_completions(type_name: &str) -> Vec<AnalysisCompletion> {
    ALL_BUILTIN_ASSOCIATED_FUNCTIONS
        .iter()
        .copied()
        .filter(|function| {
            BuiltinAssociatedFunction::resolve(type_name, function.name()) == Some(*function)
        })
        .map(|function| AnalysisCompletion {
            name: function.name().to_string(),
            kind: "function".to_string(),
            detail: function.detail().to_string(),
        })
        .collect()
}

fn builtin_specialized_associated_function_completions(
    type_name: &str,
    specialized_type: &str,
) -> Vec<AnalysisCompletion> {
    builtin_associated_function_completions(type_name)
        .into_iter()
        .map(|completion| {
            if completion.name == "with_capacity" {
                AnalysisCompletion {
                    detail: format!(
                        "with_capacity(minimum: int64) -> {}",
                        specialized_type.trim()
                    ),
                    ..completion
                }
            } else {
                completion
            }
        })
        .collect()
}

fn builtin_function_return_type(name: &str) -> Option<Type> {
    match BuiltinFunction::from_name(name)? {
        BuiltinFunction::Print => Some(Type::Unit),
        BuiltinFunction::Range => Some(Type::named("Range")),
        BuiltinFunction::Cancelled => Some(Type::named("bool")),
        BuiltinFunction::YieldNow => Some(Type::Unit),
        BuiltinFunction::Sleep => Some(Type::Unit),
        BuiltinFunction::Select => None,
        BuiltinFunction::WaitAny => None,
        BuiltinFunction::WaitAll => None,
        BuiltinFunction::Len => Some(Type::named("int64")),
        BuiltinFunction::Str => Some(Type::named("str")),
        BuiltinFunction::Abs => None,
        BuiltinFunction::Min => None,
        BuiltinFunction::Max => None,
        BuiltinFunction::Sqrt => None,
        BuiltinFunction::Round => None,
        BuiltinFunction::Divmod => None,
        BuiltinFunction::ParseInt32 => Some(Type::Named(
            "Result".to_string(),
            vec![Type::named("int32"), Type::named("str")],
        )),
        BuiltinFunction::ParseInt64 => Some(Type::Named(
            "Result".to_string(),
            vec![Type::named("int64"), Type::named("str")],
        )),
        BuiltinFunction::ParseFloat64 => Some(Type::Named(
            "Result".to_string(),
            vec![Type::named("float64"), Type::named("str")],
        )),
    }
}

fn format_function_detail(function_decl: &FunctionDecl) -> String {
    let params = function_decl
        .receiver
        .map(canonical_receiver_spelling)
        .into_iter()
        .map(str::to_string)
        .chain(function_decl.params.iter().map(format_param_decl))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{}({}) -> {}",
        function_decl.name,
        params,
        format_decl_return(function_decl)
    )
}

fn format_extern_function_detail(function_decl: &crate::ast::ExternFunctionDecl) -> String {
    let params = function_decl
        .params
        .iter()
        .map(format_param_decl)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "extern \"{}\" {}({}) -> {}",
        function_decl.abi,
        function_decl.name,
        params,
        lower_type_ref(&function_decl.return_type)
    )
}

fn format_extern_opaque_detail(handle_decl: &crate::ast::ExternOpaqueClassDecl) -> String {
    format!("extern \"{}\" opaque class", handle_decl.abi)
}

fn canonical_receiver_spelling(receiver: ReceiverKind) -> &'static str {
    match receiver {
        ReceiverKind::Value => "own self",
        ReceiverKind::Borrow => "self",
        ReceiverKind::BorrowMut => "mut self",
    }
}

fn callable_contains_line(stmts: &[Stmt], line: usize) -> bool {
    block_contains_line(stmts, line)
}

fn block_contains_line(stmts: &[Stmt], line: usize) -> bool {
    if stmts.is_empty() {
        return false;
    }
    let start = stmt_start_line(&stmts[0]);
    let end = stmts.iter().map(stmt_end_line).max().unwrap_or(start);
    start <= line && line <= end
}

fn stmt_start_line(stmt: &Stmt) -> usize {
    match stmt {
        Stmt::Assign(assign) => assign.span.line,
        Stmt::View(view) => view.span.line,
        Stmt::Destructure(destructure) => destructure.span.line,
        Stmt::Assert(assert_stmt) => assert_stmt.span.line,
        Stmt::Return(ret) => ret.span.line,
        Stmt::If(if_stmt) => if_stmt.span.line,
        Stmt::Match(match_stmt) => match_stmt.span.line,
        Stmt::For(for_stmt) => for_stmt.span.line,
        Stmt::With(with_stmt) => with_stmt.span.line,
        Stmt::While(while_stmt) => while_stmt.span.line,
        Stmt::Break(stmt) => stmt.span.line,
        Stmt::Continue(stmt) => stmt.span.line,
        Stmt::Pass(stmt) => stmt.span.line,
        Stmt::Expr(stmt) => stmt.span.line,
    }
}

fn stmt_end_line(stmt: &Stmt) -> usize {
    match stmt {
        Stmt::Assign(assign) => {
            let target_end = match &assign.target {
                AssignTarget::Name(_) => assign.span.line,
                AssignTarget::Member { object, .. } => expression_end_line(object),
                AssignTarget::Index { object, index } => {
                    expression_end_line(object).max(expression_end_line(index))
                }
            };
            assign
                .span
                .line
                .max(target_end)
                .max(expression_end_line(&assign.value))
        }
        Stmt::View(view) => view.span.line.max(expression_end_line(&view.source)),
        Stmt::Destructure(destructure) => destructure
            .span
            .line
            .max(expression_end_line(&destructure.value)),
        Stmt::Assert(assert_stmt) => assert_stmt
            .message
            .as_ref()
            .map(expression_end_line)
            .unwrap_or(assert_stmt.span.line)
            .max(assert_stmt.span.line)
            .max(expression_end_line(&assert_stmt.condition)),
        Stmt::Return(ret) => ret
            .value
            .as_ref()
            .map(expression_end_line)
            .unwrap_or(ret.span.line)
            .max(ret.span.line),
        Stmt::If(if_stmt) => {
            let mut end = if_stmt.span.line;
            for branch in &if_stmt.branches {
                end = end
                    .max(branch.span.line)
                    .max(expression_end_line(&branch.condition))
                    .max(
                        branch
                            .body
                            .iter()
                            .map(stmt_end_line)
                            .max()
                            .unwrap_or(branch.span.line),
                    );
            }
            if let Some(body) = &if_stmt.else_body {
                end = end.max(
                    body.iter()
                        .map(stmt_end_line)
                        .max()
                        .unwrap_or(if_stmt.span.line),
                );
            }
            end
        }
        Stmt::Match(match_stmt) => match_stmt
            .arms
            .iter()
            .map(|arm| {
                arm.body
                    .iter()
                    .map(stmt_end_line)
                    .max()
                    .unwrap_or(arm.span.line)
                    .max(arm.span.line)
            })
            .max()
            .unwrap_or(match_stmt.span.line)
            .max(match_stmt.span.line)
            .max(expression_end_line(&match_stmt.scrutinee)),
        Stmt::For(for_stmt) => for_stmt
            .body
            .iter()
            .map(stmt_end_line)
            .max()
            .unwrap_or(for_stmt.span.line)
            .max(for_stmt.span.line)
            .max(expression_end_line(&for_stmt.iterable)),
        Stmt::With(with_stmt) => with_stmt
            .body
            .iter()
            .map(stmt_end_line)
            .max()
            .unwrap_or(with_stmt.span.line)
            .max(with_stmt.span.line)
            .max(expression_end_line(&with_stmt.value)),
        Stmt::While(while_stmt) => while_stmt
            .body
            .iter()
            .map(stmt_end_line)
            .max()
            .unwrap_or(while_stmt.span.line)
            .max(while_stmt.span.line)
            .max(expression_end_line(&while_stmt.condition)),
        Stmt::Break(stmt) => stmt.span.line,
        Stmt::Continue(stmt) => stmt.span.line,
        Stmt::Pass(stmt) => stmt.span.line,
        Stmt::Expr(stmt) => stmt.span.line.max(expression_end_line(&stmt.expr)),
    }
}

fn extract_receiver_before_dot(line_text: &str, character: usize) -> Option<String> {
    extract_receiver_ending_before(line_text, character).map(|value| value.trim().to_string())
}

fn recover_checked_program_after_parse_error_with<F>(
    source: &str,
    error: &Diagnostic,
    check_program: &mut F,
) -> Option<Program>
where
    F: FnMut(&str) -> Result<Program>,
{
    if error.message.starts_with("unclosed delimiter") {
        return recover_checked_program_after_member_errors(source, check_program);
    }
    if !error.message.starts_with("expected member name") {
        return None;
    }
    let span = error.span?;
    recover_checked_program_after_position(
        source,
        span.line.saturating_sub(1),
        span.column.saturating_sub(1),
        check_program,
    )
}

fn recover_checked_program_after_position<F>(
    source: &str,
    line: usize,
    character: usize,
    check_program: &mut F,
) -> Option<Program>
where
    F: FnMut(&str) -> Result<Program>,
{
    let sanitized = sanitize_member_completion_source(source, line, character);
    if let Some(program) = recover_checked_program_after_member_errors(&sanitized, check_program) {
        return Some(program);
    }

    let fallback = replace_dangling_member_stmt_with_recovery_stmt(source, line);
    recover_checked_program_after_member_errors(&fallback, check_program)
}

fn recover_checked_program_after_member_errors<F>(
    source: &str,
    check_program: &mut F,
) -> Option<Program>
where
    F: FnMut(&str) -> Result<Program>,
{
    recover_checked_program_after_member_errors_with(
        source,
        check_program,
        replace_dangling_member_stmt_with_recovery_stmt,
    )
}

fn recover_checked_program_after_member_errors_with(
    source: &str,
    check_program: &mut dyn FnMut(&str) -> Result<Program>,
    replace_member_stmt: fn(&str, usize) -> String,
) -> Option<Program> {
    let mut candidate = source.to_string();
    for _ in 0..8 {
        if let Some(line) = first_dangling_member_line(&candidate) {
            let next = replace_member_stmt(&candidate, line);
            if next == candidate {
                return None;
            }
            candidate = next;
            continue;
        }

        match parser::parse(&candidate) {
            Ok(_) => match check_program(&candidate) {
                Ok(program) => return Some(program),
                Err(error) => {
                    let line = error.span.map(|span| span.line.saturating_sub(1))?;
                    let next = replace_member_stmt(&candidate, line);
                    if next == candidate {
                        return None;
                    }
                    candidate = next;
                }
            },
            Err(error) if error.message.starts_with("expected member name") => {
                let line = error.span.map(|span| span.line.saturating_sub(1))?;
                let next = replace_member_stmt(&candidate, line);
                if next == candidate {
                    return None;
                }
                candidate = next;
            }
            Err(_) => return None,
        }
    }
    None
}

fn first_dangling_member_line(source: &str) -> Option<usize> {
    source
        .lines()
        .enumerate()
        .find_map(|(line, text)| line_ends_with_dangling_member_dot(text).then_some(line))
}

fn line_ends_with_dangling_member_dot(line: &str) -> bool {
    if !line.trim_end().ends_with('.') {
        return false;
    }

    let mut quote = None;
    let mut escaped = false;
    for ch in line.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && quote.is_some() {
            escaped = true;
            continue;
        }
        match quote {
            Some(active) if ch == active => quote = None,
            Some(_) => {}
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch == '#' => return false,
            None => {}
        }
    }
    quote.is_none()
}

fn sanitize_member_completion_source(source: &str, line: usize, character: usize) -> String {
    let mut lines = source.lines().map(str::to_string).collect::<Vec<_>>();
    let Some(line_text) = lines.get_mut(line) else {
        return source.to_string();
    };
    let byte_index = line_text
        .char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(line_text.len()))
        .nth(character)
        .unwrap_or(line_text.len());
    if byte_index == 0 || byte_index > line_text.len() {
        return source.to_string();
    }

    let dot_index = line_text[..byte_index]
        .char_indices()
        .rev()
        .find_map(|(index, ch)| (ch == '.').then_some(index));
    let Some(dot_index) = dot_index else {
        return source.to_string();
    };

    line_text.remove(dot_index);
    lines.join("\n")
}

fn replace_dangling_member_stmt_with_recovery_stmt(source: &str, line: usize) -> String {
    let mut lines = source.lines().map(str::to_string).collect::<Vec<_>>();
    let start_line = unmatched_delimiter_statement_start_line(source, line).unwrap_or(line);
    let Some(line_text) = lines.get(start_line) else {
        return source.to_string();
    };
    let indent = line_text
        .chars()
        .take_while(|ch| ch.is_whitespace())
        .collect::<String>();
    let replacement = enclosing_function_return_placeholder(source, start_line)
        .map(|value| format!("{}{}", indent, value))
        .unwrap_or_else(|| format!("{}pass", indent));
    if start_line == line {
        lines[start_line] = replacement;
    } else {
        for line_text in lines.iter_mut().take(line).skip(start_line) {
            line_text.clear();
        }
        let Some(line_text) = lines.get_mut(line) else {
            return source.to_string();
        };
        *line_text = replacement;
    }
    lines.join("\n")
}

fn unmatched_delimiter_statement_start_line(source: &str, through_line: usize) -> Option<usize> {
    let mut delimiters = Vec::<(char, usize)>::new();
    for (line, text) in source.lines().enumerate().take(through_line + 1) {
        let mut quote = None;
        let mut escaped = false;
        for ch in text.chars() {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' && quote.is_some() {
                escaped = true;
                continue;
            }
            match quote {
                Some(active) if ch == active => quote = None,
                Some(_) => {}
                None if ch == '"' || ch == '\'' => quote = Some(ch),
                None if ch == '#' => break,
                None if matches!(ch, '(' | '[' | '{') => delimiters.push((ch, line)),
                None if matches!(ch, ')' | ']' | '}') => {
                    let expected = match ch {
                        ')' => '(',
                        ']' => '[',
                        '}' => '{',
                        _ => unreachable!(),
                    };
                    if matches!(delimiters.last(), Some((opener, _)) if *opener == expected) {
                        delimiters.pop();
                    }
                }
                None => {}
            }
        }
    }
    delimiters.first().map(|(_, line)| *line)
}

fn enclosing_function_return_placeholder(source: &str, line: usize) -> Option<String> {
    let lines = source.lines().collect::<Vec<_>>();
    let target_indent = lines
        .get(line)?
        .chars()
        .take_while(|ch| ch.is_whitespace())
        .count();

    for candidate in (0..line).rev() {
        let text = lines[candidate];
        let indent = text.chars().take_while(|ch| ch.is_whitespace()).count();
        if indent >= target_indent {
            continue;
        }
        let trimmed = text.trim_start();
        if !trimmed.starts_with("def ") && !trimmed.starts_with("public def ") {
            continue;
        }
        let return_type = trimmed
            .split_once("->")
            .and_then(|(_, rest)| rest.split_once(':').map(|(ty, _)| ty.trim()))
            .unwrap_or("None");
        return placeholder_stmt_for_return_type(return_type);
    }

    None
}

fn placeholder_stmt_for_return_type(return_type: &str) -> Option<String> {
    match return_type {
        "None" => Some("return".to_string()),
        "bool" => Some("return false".to_string()),
        "float32" | "float64" => Some("return 0.0".to_string()),
        "str" => Some("return \"\"".to_string()),
        "Duration" => Some("return 0ms".to_string()),
        "int" | "int8" | "int16" | "int32" | "int64" | "int128" | "intsize" | "uint8"
        | "uint16" | "uint32" | "uint64" | "uint128" | "uintsize" => Some("return 0".to_string()),
        ty if ty.starts_with("Option[") => Some("return Option.None".to_string()),
        _ => None,
    }
}

fn extract_receiver_ending_before(line_text: &str, end_index_exclusive: usize) -> Option<&str> {
    if line_text.is_empty() {
        return None;
    }

    let mut index = end_index_exclusive.min(line_text.len()).saturating_sub(1);
    let bytes = line_text.as_bytes();
    while index > 0
        && bytes
            .get(index)
            .copied()
            .unwrap_or_default()
            .is_ascii_whitespace()
    {
        index -= 1;
    }
    if bytes.get(index).copied() != Some(b'.') {
        return None;
    }

    if index == 0 {
        return None;
    }
    index -= 1;
    while index > 0
        && bytes
            .get(index)
            .copied()
            .unwrap_or_default()
            .is_ascii_whitespace()
    {
        index -= 1;
    }

    let end = index + 1;
    let start = find_receiver_start(line_text, index)?;
    Some(&line_text[start..end])
}

fn find_receiver_start(line_text: &str, index: usize) -> Option<usize> {
    let bytes = line_text.as_bytes();
    if bytes.get(index).copied() == Some(b']') {
        let opening = find_matching_open_delimiter(line_text, index)?;
        let base_end = previous_non_whitespace_index(bytes, opening);
        return base_end
            .and_then(|base_end| find_receiver_start(line_text, base_end))
            .or(Some(opening));
    }

    if bytes.get(index).copied() == Some(b')') {
        let opening = find_matching_open_delimiter(line_text, index)?;
        if let Some(base_end) = previous_non_whitespace_index(bytes, opening) {
            if receiver_token_can_precede_call(bytes[base_end]) {
                return find_receiver_start(line_text, base_end).or(Some(opening));
            }
        }
        return Some(opening);
    }

    if is_identifier_char(bytes.get(index).copied()? as char) {
        let mut cursor = index as isize;
        while cursor >= 0 {
            let ch = bytes[cursor as usize] as char;
            if is_identifier_char(ch) || ch == '.' {
                cursor -= 1;
                continue;
            }
            break;
        }
        let start = (cursor + 1) as usize;
        if bytes.get(start).copied() == Some(b'.') && cursor >= 0 {
            return find_receiver_start(line_text, cursor as usize).or(Some(start + 1));
        }
        return Some(start);
    }

    None
}

fn previous_non_whitespace_index(bytes: &[u8], before: usize) -> Option<usize> {
    (0..before)
        .rev()
        .find(|index| !bytes[*index].is_ascii_whitespace())
}

fn receiver_token_can_precede_call(byte: u8) -> bool {
    is_identifier_char(byte as char) || matches!(byte, b')' | b']')
}

fn find_matching_open_delimiter(line_text: &str, close_index: usize) -> Option<usize> {
    let bytes = line_text.as_bytes();
    let close = *bytes.get(close_index)?;
    let expected_open = match close {
        b')' => b'(',
        b']' => b'[',
        _ => return None,
    };
    let mut delimiters = Vec::new();
    let mut in_string = false;
    let mut escaped = false;

    for (index, byte) in bytes.iter().copied().enumerate().take(close_index + 1) {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }

        match byte {
            b'"' => in_string = true,
            b'(' | b'[' | b'{' => delimiters.push((byte, index)),
            b')' | b']' | b'}' => {
                let (open, opening_index) = delimiters.pop()?;
                if !matches!((open, byte), (b'(', b')') | (b'[', b']') | (b'{', b'}')) {
                    return None;
                }
                if index == close_index {
                    return (open == expected_open).then_some(opening_index);
                }
            }
            _ => {}
        }
    }
    None
}

fn is_identifier_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

#[cfg(test)]
#[path = "analysis_tests.rs"]
mod tests;
