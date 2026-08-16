use crate::ast::{
    Argument, AssertStmt, AssignStmt, AssignTarget, BinaryOp, BindingPattern, BindingTarget,
    BreakStmt, ClassDecl, CompareLink, CompareOp, ComprehensionClause, ComprehensionOutput,
    ConstantDecl, ContinueStmt, DestructureStmt, EnumDecl, EnumPayloadFieldDecl, EnumVariantDecl,
    Expr, ExprKind, ExprStmt, ExternFunctionDecl, ExternOpaqueClassDecl, FieldDecl, ForStmt,
    FormatPart, FunctionDecl, FunctionTypeParam, IfBranch, IfStmt, ImplDecl, ImportDecl,
    ImportKind, ImportName, Item, LambdaCapture, LambdaParam, LiteralPattern, LiteralPatternKind,
    MapEntryExpr, MatchArm, MatchExprArm, MatchStmt, Module, Param, ParamMode, Pattern,
    ReceiverKind, ReturnStmt, Stmt, TraitDecl, TuplePattern, TypeRef, TypeRefKind, UnaryOp,
    VariantPattern, ViewKind, ViewReturn, ViewStmt, WhileStmt, WithStmt,
};
use crate::diag::{Diagnostic, Result, Span};
use crate::integer::IntegerValue;
use crate::lexer::{lex, Token, TokenKind};
use crate::limits::RECURSION_LIMIT;

type TypeParamBounds = std::collections::BTreeMap<String, Vec<TypeRef>>;
type ParsedTypeParams = (Vec<String>, TypeParamBounds);

fn parse_error(span: Span, message: impl Into<String>) -> Diagnostic {
    Diagnostic::coded_at("AU1101", span, message)
}

fn pattern_span(pattern: &Pattern) -> Span {
    match pattern {
        Pattern::Or(pattern) => pattern.span,
        Pattern::Variant(pattern) => pattern.span,
        Pattern::Tuple(pattern) => pattern.span,
        Pattern::Binding(pattern) => pattern.span,
        Pattern::Literal(pattern) => pattern.span,
        Pattern::Wildcard(span) => *span,
    }
}

pub fn parse(source: &str) -> Result<Module> {
    let tokens = lex(source)?;
    Parser::new(tokens).parse_module()
}

pub fn parse_expression(source: &str) -> Result<Expr> {
    parse_expression_with_recursion_depth(source, 0)
}

fn parse_expression_with_recursion_depth(source: &str, recursion_depth: usize) -> Result<Expr> {
    let tokens = lex(source)?;
    let mut parser = Parser::with_recursion_depth(tokens, recursion_depth);
    parser.skip_newlines();
    let expr = parser.parse_expr()?;
    parser.skip_newlines();
    if !parser.at_eof() {
        return Err(parser.error_here("unexpected trailing tokens after expression"));
    }
    Ok(expr)
}

struct Parser {
    tokens: Vec<Token>,
    index: usize,
    recursion_depth: usize,
    current_function_returns_view: bool,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self::with_recursion_depth(tokens, 0)
    }

    fn with_recursion_depth(tokens: Vec<Token>, recursion_depth: usize) -> Self {
        Self {
            tokens,
            index: 0,
            recursion_depth,
            current_function_returns_view: false,
        }
    }

    fn enter_recursion(&mut self, kind: &str) -> Result<()> {
        if self.recursion_depth >= RECURSION_LIMIT {
            return Err(self.error_here(format!(
                "{} nesting exceeds the supported recursion limit of {}",
                kind, RECURSION_LIMIT
            )));
        }
        self.recursion_depth += 1;
        Ok(())
    }

    fn exit_recursion(&mut self) {
        self.recursion_depth = self.recursion_depth.saturating_sub(1);
    }

    fn check_expression_chain_limit(&self, count: usize) -> Result<()> {
        if count >= RECURSION_LIMIT {
            Err(self.error_here(format!(
                "expression chain exceeds the supported recursion limit of {}",
                RECURSION_LIMIT
            )))
        } else {
            Ok(())
        }
    }

    fn parse_module(&mut self) -> Result<Module> {
        let mut imports = Vec::new();
        let mut constants = Vec::new();
        let mut items = Vec::new();
        let mut top_level_stmts = Vec::new();
        let mut top_level_local_names = std::collections::BTreeSet::new();
        self.skip_newlines();

        while !self.at_eof() {
            if self.at_keyword_import() || self.at_from_import_start() {
                imports.push(self.parse_import()?);
            } else if self.at_module_constant_start()
                && !matches!(
                    self.current_kind(),
                    TokenKind::Identifier(name) if top_level_local_names.contains(name)
                )
            {
                constants.push(self.parse_module_constant()?);
            } else if self.at_simple(&TokenKind::KwPublic)
                || self.at_copy_class_start()
                || self.at_keyword_class()
                || self.at_keyword_enum()
                || self.at_keyword_extern()
                || self.at_keyword_def()
                || self.at_keyword_trait()
                || self.at_keyword_impl()
            {
                items.push(self.parse_item()?);
            } else {
                let statement = self.parse_stmt()?;
                if let Stmt::Assign(AssignStmt {
                    mutable: true,
                    target: AssignTarget::Name(name),
                    ..
                }) = &statement
                {
                    top_level_local_names.insert(name.clone());
                }
                top_level_stmts.push(statement);
            }
            self.skip_newlines();
        }

        Ok(Module {
            imports,
            constants,
            items,
            top_level_stmts,
        })
    }

    fn at_module_constant_start(&self) -> bool {
        match self.current_kind() {
            TokenKind::Identifier(_) => {
                matches!(self.peek_kind(1), Some(TokenKind::Equal | TokenKind::Colon))
            }
            TokenKind::KwPublic => matches!(self.peek_kind(1), Some(TokenKind::Identifier(_))),
            _ => false,
        }
    }

    fn parse_module_constant(&mut self) -> Result<ConstantDecl> {
        let public = self.eat_simple(&TokenKind::KwPublic).is_some();
        let (name, span) = self.expect_identifier_with_span()?;
        let annotation = if self.eat_simple(&TokenKind::Colon).is_some() {
            Some(self.parse_type()?)
        } else {
            None
        };
        self.expect_simple(TokenKind::Equal)?;
        let value = self.parse_expr()?;
        self.expect_statement_terminator()?;
        Ok(ConstantDecl {
            public,
            name,
            annotation,
            value,
            span,
        })
    }

    fn parse_item(&mut self) -> Result<Item> {
        let public = self.eat_simple(&TokenKind::KwPublic).is_some();
        if self.at_copy_class_start() || self.at_keyword_class() {
            Ok(Item::Class(self.parse_class(public)?))
        } else if self.at_keyword_enum() {
            Ok(Item::Enum(self.parse_enum(public)?))
        } else if self.at_keyword_extern() {
            self.parse_extern_item(public)
        } else if self.at_keyword_def() {
            Ok(Item::Function(self.parse_function(public)?))
        } else if self.at_keyword_trait() {
            Ok(Item::Trait(self.parse_trait(public)?))
        } else if self.at_keyword_impl() {
            if public {
                return Err(self.error_here("`public` is not allowed on `impl` blocks"));
            }
            Ok(Item::Impl(self.parse_impl()?))
        } else {
            Err(self.error_here("expected `class`, `enum`, `extern`, `def`, `trait`, or `impl`"))
        }
    }

    fn parse_extern_item(&mut self, public: bool) -> Result<Item> {
        let span = self.expect_keyword(TokenKind::KwExtern)?.span;
        let abi_token = self.bump();
        let abi = match abi_token.kind {
            TokenKind::StringLiteral(abi) if abi == "C" => abi,
            TokenKind::StringLiteral(_) => {
                return Err(parse_error(
                    abi_token.span,
                    "FFI v0 supports only `extern \"C\"` declarations",
                ));
            }
            _ => {
                return Err(parse_error(
                    abi_token.span,
                    "expected ABI string `\"C\"` after `extern`",
                ));
            }
        };

        if self.eat_simple(&TokenKind::KwOpaque).is_some() {
            self.expect_keyword(TokenKind::KwClass)?;
            let (name, name_span) = self.expect_identifier_with_span()?;
            if self.at_simple(&TokenKind::LBracket) {
                return Err(self
                    .error_here("FFI v0 opaque handle declarations cannot have type parameters"));
            }
            if self.at_simple(&TokenKind::Colon) {
                return Err(self.error_here(
                    "`extern` opaque classes have no Aura body; remove `:` and the indented block",
                ));
            }
            self.expect_newline()?;
            return Ok(Item::ExternOpaqueClass(ExternOpaqueClassDecl {
                public,
                abi,
                name,
                name_span,
                span,
            }));
        }

        self.expect_keyword(TokenKind::KwDef)?;
        let (name, name_span) = self.expect_identifier_with_span()?;
        if self.at_simple(&TokenKind::LBracket) {
            return Err(self.error_here("FFI v0 `extern` declarations cannot have type parameters"));
        }
        self.reject_reserved_ffi_signature_syntax()?;
        self.expect_simple(TokenKind::LParen)?;
        let (_, params) = self.parse_params(false)?;
        self.expect_simple(TokenKind::RParen)?;
        if params.iter().any(|param| param.default.is_some()) {
            return Err(parse_error(
                params
                    .iter()
                    .find(|param| param.default.is_some())
                    .map(|param| param.span)
                    .unwrap_or(span),
                "`extern` function parameters cannot have default values",
            ));
        }
        if !self.at_simple(&TokenKind::Arrow) {
            return Err(parse_error(
                self.current_span(),
                "`extern` function declarations require an explicit return type; write `-> None` when the function returns no value",
            ));
        }
        let (return_type, view_return) = self.parse_return_annotation(span)?;
        if view_return.is_some() {
            return Err(parse_error(
                span,
                "`extern` functions cannot return Aura views",
            ));
        }
        if self.at_simple(&TokenKind::Colon) {
            return Err(self.error_here(
                "`extern` function declarations have no Aura body; remove `:` and the indented block",
            ));
        }
        self.expect_newline()?;

        Ok(Item::ExternFunction(ExternFunctionDecl {
            public,
            abi,
            name,
            name_span,
            params,
            return_type,
            span,
        }))
    }

    fn reject_reserved_ffi_signature_syntax(&self) -> Result<()> {
        let mut index = self.index;
        while let Some(kind) = self.peek_kind_at(index) {
            if matches!(kind, TokenKind::Newline | TokenKind::Eof) {
                break;
            }
            if matches!(kind, TokenKind::Dot)
                && matches!(self.peek_kind_at(index + 1), Some(TokenKind::Dot))
                && matches!(self.peek_kind_at(index + 2), Some(TokenKind::Dot))
            {
                return Err(parse_error(
                    self.tokens[index].span,
                    "FFI v0 does not support variadic declarations; declare fixed parameters explicitly",
                ));
            }
            if matches!(kind, TokenKind::KwDef) {
                return Err(parse_error(
                    self.tokens[index].span,
                    "FFI v0 does not support callback parameters or returns",
                ));
            }
            if matches!(kind, TokenKind::Star) {
                return Err(parse_error(
                    self.tokens[index].span,
                    "FFI v0 does not expose raw pointer syntax; use a supported byte/string view or opaque handle",
                ));
            }
            index += 1;
        }
        Ok(())
    }

    fn parse_import(&mut self) -> Result<ImportDecl> {
        if self.at_keyword_import() {
            let span = self.expect_keyword(TokenKind::KwImport)?.span;
            let path = self.parse_identifier_path()?;
            let alias = if self.eat_simple(&TokenKind::KwAs).is_some() {
                Some(self.parse_import_alias()?)
            } else {
                None
            };
            self.expect_newline()?;
            return Ok(ImportDecl {
                kind: ImportKind::Module { path, alias },
                span,
            });
        }

        let span = self.expect_keyword(TokenKind::KwFrom)?.span;
        let module_path = self.parse_identifier_path()?;
        self.expect_keyword(TokenKind::KwImport)?;
        let mut names = Vec::new();
        let mut targets = std::collections::BTreeSet::new();
        let mut local_names = std::collections::BTreeSet::new();
        loop {
            let (name, name_span) = self.expect_identifier_with_span()?;
            if !targets.insert(name.clone()) {
                return Err(Diagnostic::coded_at(
                    "AU2999",
                    name_span,
                    format!("import target `{name}` appears more than once in this declaration"),
                ));
            }
            let alias = if self.eat_simple(&TokenKind::KwAs).is_some() {
                Some(self.parse_import_alias()?)
            } else {
                None
            };
            let local_name = alias.as_deref().unwrap_or(&name);
            if !local_names.insert(local_name.to_string()) {
                return Err(Diagnostic::coded_at(
                    "AU2999",
                    name_span,
                    format!("duplicate import binding `{local_name}`"),
                ));
            }
            names.push(ImportName {
                name,
                alias,
                span: name_span,
            });
            if self.eat_simple(&TokenKind::Comma).is_none() {
                break;
            }
        }
        self.expect_newline()?;
        Ok(ImportDecl {
            kind: ImportKind::From { module_path, names },
            span,
        })
    }

    fn parse_import_alias(&mut self) -> Result<String> {
        let token = self.bump();
        match token.kind {
            TokenKind::Identifier(name) if name == "_" => Err(Diagnostic::coded_at(
                "AU2999",
                token.span,
                "`_` cannot be used as an import alias",
            )),
            TokenKind::Identifier(name) => Ok(name),
            TokenKind::Newline | TokenKind::Eof => {
                Err(parse_error(token.span, "expected identifier after `as`"))
            }
            _ => Err(Diagnostic::coded_at(
                "AU2999",
                token.span,
                "reserved words cannot be used as import aliases",
            )),
        }
    }

    fn parse_class(&mut self, public: bool) -> Result<ClassDecl> {
        let copy = if matches!(self.current_kind(), TokenKind::Identifier(name) if name == "copy") {
            self.bump();
            true
        } else {
            false
        };
        let span = self.expect_keyword(TokenKind::KwClass)?.span;
        let name = self.expect_identifier()?;
        let (type_params, type_param_bounds) = self.parse_optional_type_params(true)?;
        self.expect_simple(TokenKind::Colon)?;
        self.expect_newline()?;
        self.expect_simple(TokenKind::Indent)?;

        let mut fields = Vec::new();
        let mut methods = Vec::new();
        while !self.at_simple(&TokenKind::Dedent) && !self.at_eof() {
            if self.at_simple(&TokenKind::Newline) {
                self.bump();
                continue;
            }
            if self.at_simple(&TokenKind::KwPass) {
                self.parse_pass_stmt()?;
                continue;
            }
            let method_public = self.at_simple(&TokenKind::KwPublic)
                && matches!(self.peek_kind_at(self.index + 1), Some(TokenKind::KwDef));
            if method_public {
                self.bump();
            }
            if self.at_keyword_def() {
                methods.push(self.parse_function_with_receiver(true, method_public)?);
            } else {
                fields.push(self.parse_field()?);
            }
        }

        self.expect_simple(TokenKind::Dedent)?;

        Ok(ClassDecl {
            public,
            copy,
            name,
            type_params,
            type_param_bounds,
            fields,
            methods,
            span,
        })
    }

    fn parse_enum(&mut self, public: bool) -> Result<EnumDecl> {
        let span = self.expect_keyword(TokenKind::KwEnum)?.span;
        let name = self.expect_identifier()?;
        let (type_params, type_param_bounds) = self.parse_optional_type_params(true)?;
        self.expect_simple(TokenKind::Colon)?;
        self.expect_newline()?;
        self.expect_simple(TokenKind::Indent)?;

        let mut variants = Vec::new();
        while !self.at_simple(&TokenKind::Dedent) && !self.at_eof() {
            if self.at_simple(&TokenKind::Newline) {
                self.bump();
                continue;
            }
            variants.push(self.parse_enum_variant()?);
        }

        self.expect_simple(TokenKind::Dedent)?;

        Ok(EnumDecl {
            public,
            name,
            type_params,
            type_param_bounds,
            variants,
            span,
        })
    }

    fn parse_enum_variant(&mut self) -> Result<EnumVariantDecl> {
        let span = self.current_span();
        let name = self.expect_identifier()?;
        let (payloads, named_payloads) = if self.eat_simple(&TokenKind::LParen).is_some() {
            let mut payloads = Vec::new();
            let mut saw_named = false;
            let mut saw_unnamed = false;
            loop {
                let field_span = self.current_span();
                let (field_name, field_ty) =
                    if matches!(self.current_kind(), TokenKind::Identifier(_))
                        && matches!(self.peek_kind(1), Some(TokenKind::Colon))
                    {
                        let field_name = self.expect_identifier()?;
                        self.expect_simple(TokenKind::Colon)?;
                        let field_ty = self.parse_type()?;
                        saw_named = true;
                        (Some(field_name), field_ty)
                    } else {
                        let field_ty = self.parse_type()?;
                        saw_unnamed = true;
                        (None, field_ty)
                    };
                payloads.push(EnumPayloadFieldDecl {
                    name: field_name,
                    ty: field_ty,
                    span: field_span,
                });
                if self.eat_simple(&TokenKind::Comma).is_none() {
                    break;
                }
            }
            self.expect_simple(TokenKind::RParen)?;
            if saw_named && saw_unnamed {
                return Err(parse_error(
                    span,
                    "enum variant payloads must be either all named or all positional",
                ));
            }
            (payloads, saw_named)
        } else {
            (Vec::new(), false)
        };
        self.expect_newline()?;
        Ok(EnumVariantDecl {
            name,
            payloads,
            named_payloads,
            span,
        })
    }

    fn parse_field(&mut self) -> Result<FieldDecl> {
        let public = self.eat_simple(&TokenKind::KwPublic).is_some();
        let span = self.current_span();
        let name = self.expect_identifier()?;
        self.expect_simple(TokenKind::Colon)?;
        let ty = self.parse_type()?;
        let default = if self.eat_simple(&TokenKind::Equal).is_some() {
            Some(self.parse_expr()?)
        } else {
            None
        };
        self.expect_newline()?;

        Ok(FieldDecl {
            public,
            name,
            ty,
            default,
            span,
        })
    }

    fn parse_function(&mut self, public: bool) -> Result<FunctionDecl> {
        self.parse_function_with_receiver(false, public)
    }

    fn parse_function_with_receiver(
        &mut self,
        allow_receiver: bool,
        public: bool,
    ) -> Result<FunctionDecl> {
        let span = self.expect_keyword(TokenKind::KwDef)?.span;
        let name = self.expect_identifier()?;
        let (type_params, type_param_bounds) = self.parse_optional_type_params(true)?;
        self.expect_simple(TokenKind::LParen)?;
        let (receiver, params) = self.parse_params(allow_receiver)?;
        self.expect_simple(TokenKind::RParen)?;
        let (return_type, view_return) = self.parse_return_annotation(span)?;
        self.expect_simple(TokenKind::Colon)?;
        self.expect_newline()?;
        let previous_view_return = self.current_function_returns_view;
        self.current_function_returns_view = view_return.is_some();
        let body = self.parse_block();
        self.current_function_returns_view = previous_view_return;
        let body = body?;

        Ok(FunctionDecl {
            public,
            name,
            type_params,
            type_param_bounds,
            receiver,
            params,
            return_type,
            view_return,
            body,
            span,
        })
    }

    fn parse_trait(&mut self, public: bool) -> Result<TraitDecl> {
        let span = self.expect_keyword(TokenKind::KwTrait)?.span;
        let name = self.expect_identifier()?;
        let (type_params, _) = self.parse_optional_type_params(false)?;
        self.expect_simple(TokenKind::Colon)?;
        let mut supertraits = Vec::new();
        if !self.at_simple(&TokenKind::Newline) {
            loop {
                supertraits.push(self.parse_type()?);
                if self.eat_simple(&TokenKind::Comma).is_none() {
                    break;
                }
            }
            self.expect_simple(TokenKind::Colon)?;
        }
        self.expect_newline()?;
        self.expect_simple(TokenKind::Indent)?;

        let mut methods = Vec::new();
        while !self.at_simple(&TokenKind::Dedent) && !self.at_eof() {
            if self.at_simple(&TokenKind::Newline) {
                self.bump();
                continue;
            }
            if self.at_simple(&TokenKind::KwPass) {
                self.parse_pass_stmt()?;
                continue;
            }
            methods.push(self.parse_trait_method()?);
        }

        self.expect_simple(TokenKind::Dedent)?;
        Ok(TraitDecl {
            public,
            name,
            type_params,
            supertraits,
            methods,
            span,
        })
    }

    fn parse_impl(&mut self) -> Result<ImplDecl> {
        let span = self.expect_keyword(TokenKind::KwImpl)?.span;
        let (type_params, type_param_bounds) = self.parse_optional_type_params(true)?;
        let trait_name = self.expect_identifier()?;
        let mut trait_args = Vec::new();
        if self.eat_simple(&TokenKind::LBracket).is_some() {
            loop {
                trait_args.push(self.parse_type()?);
                if self.eat_simple(&TokenKind::Comma).is_none() {
                    break;
                }
            }
            self.expect_simple(TokenKind::RBracket)?;
        }
        self.expect_keyword(TokenKind::KwFor)?;
        let for_type = self.parse_type()?;
        self.expect_simple(TokenKind::Colon)?;
        self.expect_newline()?;
        self.expect_simple(TokenKind::Indent)?;

        let mut methods = Vec::new();
        while !self.at_simple(&TokenKind::Dedent) && !self.at_eof() {
            if self.at_simple(&TokenKind::Newline) {
                self.bump();
                continue;
            }
            if self.at_simple(&TokenKind::KwPass) {
                self.parse_pass_stmt()?;
                continue;
            }
            methods.push(self.parse_function_with_receiver(true, false)?);
        }

        self.expect_simple(TokenKind::Dedent)?;
        Ok(ImplDecl {
            type_params,
            type_param_bounds,
            trait_name,
            trait_args,
            for_type,
            methods,
            span,
        })
    }

    fn parse_trait_method(&mut self) -> Result<FunctionDecl> {
        let span = self.expect_keyword(TokenKind::KwDef)?.span;
        let name = self.expect_identifier()?;
        let (type_params, type_param_bounds) = self.parse_optional_type_params(true)?;
        self.expect_simple(TokenKind::LParen)?;
        let (receiver, params) = self.parse_params(true)?;
        self.expect_simple(TokenKind::RParen)?;
        let (return_type, view_return) = self.parse_return_annotation(span)?;
        let body = if self.eat_simple(&TokenKind::Colon).is_some() {
            self.expect_newline()?;
            let previous_view_return = self.current_function_returns_view;
            self.current_function_returns_view = view_return.is_some();
            let body = self.parse_block();
            self.current_function_returns_view = previous_view_return;
            body?
        } else {
            self.expect_newline()?;
            Vec::new()
        };
        Ok(FunctionDecl {
            public: false,
            name,
            type_params,
            type_param_bounds,
            receiver,
            params,
            return_type,
            view_return,
            body,
            span,
        })
    }

    fn parse_return_annotation(&mut self, span: Span) -> Result<(TypeRef, Option<ViewReturn>)> {
        if self.eat_simple(&TokenKind::Arrow).is_none() {
            return Ok((TypeRef::named("None", Vec::new(), false, span), None));
        }

        if matches!(self.current_kind(), TokenKind::Identifier(name) if name == "view")
            && matches!(
                self.peek_kind(1),
                Some(
                    TokenKind::KwMut
                        | TokenKind::KwIndirect
                        | TokenKind::KwDef
                        | TokenKind::Identifier(_)
                        | TokenKind::LParen
                )
            )
        {
            let view_span = self.bump().span;
            let mutable = self.eat_simple(&TokenKind::KwMut).is_some();
            let return_type = self.parse_type()?;
            if self.eat_simple(&TokenKind::KwFrom).is_none() {
                return Err(parse_error(
                    self.current_span(),
                    "a view return type requires `from` and one receiver or parameter origin",
                ));
            }
            let origin = self.expect_identifier()?;
            return Ok((
                return_type,
                Some(ViewReturn {
                    mutable,
                    origin,
                    span: view_span,
                }),
            ));
        }

        Ok((self.parse_type()?, None))
    }

    fn parse_optional_type_params(&mut self, allow_bounds: bool) -> Result<ParsedTypeParams> {
        let mut type_params = Vec::new();
        let mut bounds = TypeParamBounds::new();
        if self.eat_simple(&TokenKind::LBracket).is_none() {
            return Ok((type_params, bounds));
        }

        loop {
            let name = self.expect_identifier()?;
            let mut param_bounds = Vec::new();
            if allow_bounds && self.eat_simple(&TokenKind::Colon).is_some() {
                loop {
                    param_bounds.push(self.parse_type()?);
                    if self.eat_simple(&TokenKind::Plus).is_none() {
                        break;
                    }
                }
            }
            type_params.push(name);
            if !param_bounds.is_empty() {
                bounds.insert(type_params.last().unwrap().clone(), param_bounds);
            }
            if self.eat_simple(&TokenKind::Comma).is_none() {
                break;
            }
        }

        self.expect_simple(TokenKind::RBracket)?;
        Ok((type_params, bounds))
    }

    fn parse_params(&mut self, allow_receiver: bool) -> Result<(Option<ReceiverKind>, Vec<Param>)> {
        let mut receiver = None;
        let mut params = Vec::new();

        if self.at_simple(&TokenKind::RParen) {
            return Ok((receiver, params));
        }

        loop {
            if self.at_simple(&TokenKind::Star) {
                return Err(Diagnostic::coded_at(
                    "AU1101",
                    self.current_span(),
                    "keyword-only parameters are not part of Aura 0.3's structural callable model",
                ));
            }
            if allow_receiver && receiver.is_none() {
                if self.at_typed_receiver_start() {
                    return Err(Diagnostic::coded_at(
                        "AU3004",
                        self.current_span(),
                        "`self: Type` is not a method receiver; use `self` for shared access, `own self` to consume, or `mut self` to mutate",
                    ));
                }

                if self.at_mut_receiver_start() {
                    if !params.is_empty() {
                        return Err(parse_error(
                            self.current_span(),
                            "method receiver must be the first parameter",
                        ));
                    }
                    self.bump();
                    self.expect_identifier()?;
                    receiver = Some(ReceiverKind::BorrowMut);
                    if self.eat_simple(&TokenKind::Comma).is_none() {
                        break;
                    }
                    continue;
                }

                if self.at_own_receiver_start() {
                    if !params.is_empty() {
                        return Err(parse_error(
                            self.current_span(),
                            "method receiver must be the first parameter",
                        ));
                    }
                    self.bump();
                    self.expect_identifier()?;
                    receiver = Some(ReceiverKind::Value);
                    if self.eat_simple(&TokenKind::Comma).is_none() {
                        break;
                    }
                    continue;
                }

                if self.at_value_receiver_start() {
                    if !params.is_empty() {
                        return Err(parse_error(
                            self.current_span(),
                            "method receiver must be the first parameter",
                        ));
                    }
                    self.bump();
                    receiver = Some(ReceiverKind::Borrow);
                    if self.eat_simple(&TokenKind::Comma).is_none() {
                        break;
                    }
                    continue;
                }
            }

            let span = self.current_span();
            if self.at_simple(&TokenKind::KwOwn) {
                return Err(parse_error(
                    self.current_span(),
                    "ordinary owned parameters must be written as `name: own Type`",
                ));
            }
            let mut mode = ParamMode::Default;
            let name = self.expect_identifier()?;
            self.expect_simple(TokenKind::Colon)?;
            if self.eat_simple(&TokenKind::KwOwn).is_some() {
                mode = ParamMode::Own;
            } else if self.eat_simple(&TokenKind::KwMut).is_some() {
                mode = ParamMode::BorrowMut;
            }
            let ty = self.parse_type()?;
            let default = if self.eat_simple(&TokenKind::Equal).is_some() {
                Some(self.parse_expr()?)
            } else {
                None
            };
            params.push(Param {
                name,
                mode,
                ty,
                default,
                span,
            });

            if self.eat_simple(&TokenKind::Comma).is_none() {
                break;
            }
        }

        Ok((receiver, params))
    }

    fn at_mut_receiver_start(&self) -> bool {
        matches!(
            (
                self.current_kind(),
                self.peek_kind_at(self.index + 1),
                self.peek_kind_at(self.index + 2),
            ),
            (TokenKind::KwMut, Some(TokenKind::Identifier(name)), next)
                if name == "self" && !matches!(next, Some(TokenKind::Colon))
        )
    }

    fn at_own_receiver_start(&self) -> bool {
        matches!(
            (
                self.current_kind(),
                self.peek_kind_at(self.index + 1),
                self.peek_kind_at(self.index + 2),
            ),
            (TokenKind::KwOwn, Some(TokenKind::Identifier(name)), next)
                if name == "self" && !matches!(next, Some(TokenKind::Colon))
        )
    }

    fn at_typed_receiver_start(&self) -> bool {
        matches!(
            (self.current_kind(), self.peek_kind_at(self.index + 1)),
            (TokenKind::Identifier(name), Some(TokenKind::Colon)) if name == "self"
        )
    }

    fn at_value_receiver_start(&self) -> bool {
        matches!(
            (self.current_kind(), self.peek_kind_at(self.index + 1)),
            (TokenKind::Identifier(name), next) if name == "self" && !matches!(next, Some(TokenKind::Colon))
        )
    }

    fn parse_stmt(&mut self) -> Result<Stmt> {
        self.enter_recursion("statement")?;
        let result = self.parse_stmt_inner();
        self.exit_recursion();
        result
    }

    fn parse_stmt_inner(&mut self) -> Result<Stmt> {
        if self.at_simple(&TokenKind::KwTry) && matches!(self.peek_kind(1), Some(TokenKind::Colon))
        {
            Err(Diagnostic::coded_at(
                "AU2005",
                self.current_span(),
                "`try`/`except` is not supported; use `Result` with `match` today",
            ))
        } else if self.at_simple(&TokenKind::KwReturn) {
            self.parse_return_stmt()
        } else if self.at_simple(&TokenKind::KwAssert) {
            self.parse_assert_stmt()
        } else if self.at_simple(&TokenKind::KwPass) {
            self.parse_pass_stmt()
        } else if self.at_simple(&TokenKind::KwIf) {
            self.parse_if_stmt()
        } else if self.at_simple(&TokenKind::KwMatch) {
            self.parse_match_stmt()
        } else if self.at_simple(&TokenKind::KwFor) {
            self.parse_for_stmt()
        } else if self.at_simple(&TokenKind::KwWith) {
            self.parse_with_stmt()
        } else if self.at_simple(&TokenKind::KwWhile) {
            self.parse_while_stmt()
        } else if self.at_simple(&TokenKind::KwBreak) {
            self.parse_break_stmt()
        } else if self.at_simple(&TokenKind::KwContinue) {
            self.parse_continue_stmt()
        } else if self.is_view_binding_stmt() {
            self.parse_view_stmt()
        } else if self.is_destructure_assignment_stmt() {
            self.parse_destructure_stmt()
        } else if self.is_assignment_stmt() {
            self.parse_assign_stmt()
        } else {
            self.parse_expr_stmt()
        }
    }

    fn parse_return_stmt(&mut self) -> Result<Stmt> {
        let span = self.expect_keyword(TokenKind::KwReturn)?.span;
        let view = if self.current_function_returns_view
            && matches!(self.current_kind(), TokenKind::Identifier(name) if name == "view")
            && !matches!(self.peek_kind(1), Some(TokenKind::Newline | TokenKind::Eof))
        {
            self.bump();
            Some(if self.eat_simple(&TokenKind::KwMut).is_some() {
                ViewKind::Mutable
            } else {
                ViewKind::Shared
            })
        } else {
            None
        };
        let value = if self.at_simple(&TokenKind::Newline) {
            None
        } else {
            Some(self.parse_expr()?)
        };
        self.expect_statement_terminator()?;
        Ok(Stmt::Return(ReturnStmt { value, view, span }))
    }

    fn is_view_binding_stmt(&self) -> bool {
        if !matches!(self.current_kind(), TokenKind::Identifier(name) if name == "view") {
            return false;
        }
        matches!(
            (self.peek_kind(1), self.peek_kind(2), self.peek_kind(3)),
            (Some(TokenKind::Identifier(_)), Some(TokenKind::Equal), _)
                | (
                    Some(TokenKind::KwMut),
                    Some(TokenKind::Identifier(_)),
                    Some(TokenKind::Equal)
                )
        )
    }

    fn parse_view_stmt(&mut self) -> Result<Stmt> {
        let span = self.bump().span;
        let mutable = self.eat_simple(&TokenKind::KwMut).is_some();
        let name = self.expect_identifier()?;
        self.expect_simple(TokenKind::Equal)?;
        let source = self.parse_expr()?;
        self.expect_statement_terminator()?;
        Ok(Stmt::View(ViewStmt {
            name,
            mutable,
            source,
            span,
        }))
    }

    fn parse_assert_stmt(&mut self) -> Result<Stmt> {
        let span = self.expect_keyword(TokenKind::KwAssert)?.span;
        let condition = self.parse_non_tuple_expr()?;
        let message = if self.eat_simple(&TokenKind::Comma).is_some() {
            Some(self.parse_non_tuple_expr()?)
        } else {
            None
        };
        self.expect_statement_terminator()?;
        Ok(Stmt::Assert(AssertStmt {
            condition,
            message,
            span,
        }))
    }

    fn parse_assign_stmt(&mut self) -> Result<Stmt> {
        let mutable = self.eat_simple(&TokenKind::KwMut).is_some();
        let span = self.current_span();
        let target = self.parse_assign_target()?;
        let annotation = if matches!(target, AssignTarget::Name(_))
            && self.eat_simple(&TokenKind::Colon).is_some()
        {
            Some(self.parse_type()?)
        } else {
            None
        };
        let op = self.parse_assignment_operator()?;
        let value = self.parse_expr()?;
        self.expect_statement_terminator()?;

        Ok(Stmt::Assign(AssignStmt {
            mutable,
            target,
            annotation,
            op,
            value,
            span,
        }))
    }

    fn parse_destructure_stmt(&mut self) -> Result<Stmt> {
        if self.eat_simple(&TokenKind::KwMut).is_some() {
            return Err(parse_error(
                self.current_span(),
                "`mut` destructuring is not supported yet; bind the tuple first and mutate named values explicitly",
            ));
        }

        let span = self.current_span();
        let target = self.parse_binding_target_sequence(false)?;
        self.reject_duplicate_binding_names(&target)?;

        if !self.at_simple(&TokenKind::Equal) {
            if self.is_assignment_operator_kind(Some(self.current_kind())) {
                return Err(parse_error(
                    self.current_span(),
                    "destructuring only supports plain `=`; compound assignment requires a single assignable place",
                ));
            }
            return Err(parse_error(
                self.current_span(),
                "expected `=` after destructuring target",
            ));
        }
        self.bump();

        let value = self.parse_expr()?;
        self.expect_statement_terminator()?;
        Ok(Stmt::Destructure(DestructureStmt {
            target,
            value,
            span,
        }))
    }

    fn parse_if_stmt(&mut self) -> Result<Stmt> {
        let span = self.expect_keyword(TokenKind::KwIf)?.span;
        let mut branches = Vec::new();
        let condition = self.parse_expr()?;
        self.expect_simple(TokenKind::Colon)?;
        self.expect_newline()?;
        let body = self.parse_block()?;
        branches.push(IfBranch {
            condition,
            body,
            span,
        });

        while self.at_simple(&TokenKind::KwElif) {
            let branch_span = self.expect_keyword(TokenKind::KwElif)?.span;
            let condition = self.parse_expr()?;
            self.expect_simple(TokenKind::Colon)?;
            self.expect_newline()?;
            let body = self.parse_block()?;
            branches.push(IfBranch {
                condition,
                body,
                span: branch_span,
            });
        }

        let else_body = if self.at_simple(&TokenKind::KwElse) {
            self.bump();
            self.expect_simple(TokenKind::Colon)?;
            self.expect_newline()?;
            Some(self.parse_block()?)
        } else {
            None
        };

        Ok(Stmt::If(IfStmt {
            branches,
            else_body,
            span,
        }))
    }

    fn parse_match_stmt(&mut self) -> Result<Stmt> {
        let span = self.expect_keyword(TokenKind::KwMatch)?.span;
        let capability = self.parse_match_capability()?;
        let scrutinee = self.parse_expr()?;
        self.expect_simple(TokenKind::Colon)?;
        self.expect_newline()?;
        self.expect_simple(TokenKind::Indent)?;

        let mut arms = Vec::new();
        while !self.at_simple(&TokenKind::Dedent) && !self.at_eof() {
            if self.at_simple(&TokenKind::Newline) {
                self.bump();
                continue;
            }
            arms.push(self.parse_match_arm()?);
        }

        self.expect_simple(TokenKind::Dedent)?;

        Ok(Stmt::Match(MatchStmt {
            scrutinee,
            capability,
            arms,
            span,
        }))
    }

    fn parse_for_stmt(&mut self) -> Result<Stmt> {
        let span = self.expect_keyword(TokenKind::KwFor)?.span;
        let target = self.parse_binding_target_sequence(false)?;
        self.reject_duplicate_binding_names(&target)?;
        self.expect_simple(TokenKind::KwIn)?;
        let borrow_mode = self.parse_optional_for_mode()?;
        let iterable = self.parse_expr()?;
        self.expect_simple(TokenKind::Colon)?;
        self.expect_newline()?;
        let body = self.parse_block()?;
        Ok(Stmt::For(ForStmt {
            target,
            iterable,
            borrow_mode,
            body,
            span,
        }))
    }

    fn parse_with_stmt(&mut self) -> Result<Stmt> {
        let span = self.expect_keyword(TokenKind::KwWith)?.span;
        let (binding, value) = if matches!(self.current_kind(), TokenKind::Identifier(_))
            && matches!(self.peek_kind(1), Some(TokenKind::Equal))
        {
            let binding = self.expect_identifier()?;
            self.expect_simple(TokenKind::Equal)?;
            let value = self.parse_expr()?;
            (binding, value)
        } else {
            let value = self.parse_expr()?;
            self.expect_simple(TokenKind::KwAs)?;
            let binding = self.expect_identifier()?;
            (binding, value)
        };
        self.expect_simple(TokenKind::Colon)?;
        self.expect_newline()?;
        let body = self.parse_block()?;
        Ok(Stmt::With(WithStmt {
            binding,
            value,
            body,
            span,
        }))
    }

    fn parse_match_arm(&mut self) -> Result<MatchArm> {
        let span = self.expect_keyword(TokenKind::KwCase)?.span;
        let pattern = self.parse_pattern()?;
        let guard = if self.eat_simple(&TokenKind::KwIf).is_some() {
            Some(self.parse_expr()?)
        } else {
            None
        };
        self.expect_simple(TokenKind::Colon)?;
        self.expect_newline()?;
        let body = self.parse_block()?;
        Ok(MatchArm {
            pattern,
            guard,
            body,
            span,
        })
    }

    fn parse_match_expr_arm(&mut self) -> Result<MatchExprArm> {
        let span = self.expect_keyword(TokenKind::KwCase)?.span;
        let pattern = self.parse_pattern()?;
        let guard = if self.eat_simple(&TokenKind::KwIf).is_some() {
            Some(self.parse_expr()?)
        } else {
            None
        };
        self.expect_simple(TokenKind::Colon)?;
        let value = if self.at_simple(&TokenKind::Newline) {
            self.expect_newline()?;
            self.expect_simple(TokenKind::Indent)?;
            let value = self.parse_expr()?;
            self.expect_statement_terminator()?;
            self.expect_simple(TokenKind::Dedent)?;
            value
        } else {
            let value = self.parse_expr()?;
            self.expect_match_expr_arm_terminator()?;
            value
        };
        Ok(MatchExprArm {
            pattern,
            guard,
            value,
            span,
        })
    }

    fn parse_pattern(&mut self) -> Result<Pattern> {
        self.enter_recursion("pattern")?;
        let result = self.parse_or_pattern();
        self.exit_recursion();
        result
    }

    fn parse_or_pattern(&mut self) -> Result<Pattern> {
        let first = self.parse_pattern_inner()?;
        if self.eat_simple(&TokenKind::Pipe).is_none() {
            return Ok(first);
        }
        let span = pattern_span(&first);
        let mut alternatives = vec![first];
        loop {
            if matches!(
                self.current_kind(),
                TokenKind::Colon | TokenKind::Comma | TokenKind::RParen | TokenKind::KwIf
            ) {
                return Err(parse_error(
                    self.current_span(),
                    "or-pattern requires a pattern after `|`",
                ));
            }
            alternatives.push(self.parse_pattern_inner()?);
            if self.eat_simple(&TokenKind::Pipe).is_none() {
                break;
            }
        }
        Ok(Pattern::Or(crate::ast::OrPattern { alternatives, span }))
    }

    fn parse_pattern_inner(&mut self) -> Result<Pattern> {
        let span = self.current_span();
        if self.eat_simple(&TokenKind::LParen).is_some() {
            if self.at_simple(&TokenKind::RParen) {
                return Err(parse_error(span, "empty tuple patterns are not supported"));
            }

            let first = self.parse_pattern()?;
            if self.eat_simple(&TokenKind::Comma).is_none() {
                self.expect_simple(TokenKind::RParen)?;
                return Ok(first);
            }

            let mut elements = vec![first];
            if self.eat_simple(&TokenKind::RParen).is_some() {
                return Ok(Pattern::Tuple(TuplePattern { elements, span }));
            }

            loop {
                elements.push(self.parse_pattern()?);
                if self.eat_simple(&TokenKind::Comma).is_none() {
                    break;
                }
                if self.at_simple(&TokenKind::RParen) {
                    return Err(parse_error(
                        self.current_span(),
                        "trailing commas are only allowed for singleton tuple patterns",
                    ));
                }
            }
            self.expect_simple(TokenKind::RParen)?;
            return Ok(Pattern::Tuple(TuplePattern { elements, span }));
        }

        if matches!(self.current_kind(), TokenKind::Identifier(name) if name == "_") {
            self.bump();
            return Ok(Pattern::Wildcard(span));
        }
        match self.current_kind().clone() {
            TokenKind::BoolLiteral(value) => {
                self.bump();
                return Ok(Pattern::Literal(LiteralPattern {
                    kind: LiteralPatternKind::Bool(value),
                    span,
                }));
            }
            TokenKind::StringLiteral(value) => {
                self.bump();
                return Ok(Pattern::Literal(LiteralPattern {
                    kind: LiteralPatternKind::String(value),
                    span,
                }));
            }
            TokenKind::FloatLiteral(value) => {
                self.bump();
                return Ok(Pattern::Literal(LiteralPattern {
                    kind: LiteralPatternKind::Float(value),
                    span,
                }));
            }
            TokenKind::IntLiteral(value) => {
                self.bump();
                return Ok(Pattern::Literal(LiteralPattern {
                    kind: LiteralPatternKind::Int(IntegerValue::from_literal(value)),
                    span,
                }));
            }
            TokenKind::Minus => {
                let minus = self.bump();
                let kind = match self.current_kind().clone() {
                    TokenKind::IntLiteral(value) => {
                        self.bump();
                        let negative = match IntegerValue::from_literal(value).checked_neg() {
                            Some(value) => value,
                            None => {
                                return Err(parse_error(
                                    minus.span,
                                    "negative integer literal in pattern is outside the supported range",
                                ));
                            }
                        };
                        LiteralPatternKind::Int(negative)
                    }
                    TokenKind::FloatLiteral(value) => {
                        self.bump();
                        LiteralPatternKind::Float(-value)
                    }
                    _ => {
                        return Err(parse_error(
                            minus.span,
                            "match patterns currently support enum variants, `_`, and boolean/string/integer/float literals",
                        ));
                    }
                };
                return Ok(Pattern::Literal(LiteralPattern {
                    kind,
                    span: minus.span,
                }));
            }
            _ => {}
        }
        if !matches!(self.current_kind(), TokenKind::Identifier(_)) {
            return Err(parse_error(
                span,
                "match patterns currently support enum variants, `_`, and boolean/string/integer/float literals",
            ));
        }
        let mut segments = vec![self.expect_identifier()?];
        while self.eat_simple(&TokenKind::Dot).is_some() {
            segments.push(self.expect_identifier()?);
        }
        if segments.len() == 1
            && !matches!(self.current_kind(), TokenKind::LParen)
            && segments[0]
                .chars()
                .next()
                .is_some_and(|ch| ch == '_' || ch.is_ascii_lowercase())
        {
            return Ok(Pattern::Binding(BindingPattern {
                name: segments.remove(0),
                span,
            }));
        }
        let variant_name = segments
            .pop()
            .expect("pattern should contain a variant segment");
        let enum_name = if segments.is_empty() {
            None
        } else {
            Some(segments.join("."))
        };
        let subpatterns = if self.eat_simple(&TokenKind::LParen).is_some() {
            let mut subpatterns = Vec::new();
            if !self.at_simple(&TokenKind::RParen) {
                loop {
                    if matches!(self.current_kind(), TokenKind::Identifier(_))
                        && matches!(self.peek_kind(1), Some(TokenKind::Equal))
                    {
                        return Err(Diagnostic::coded_at(
                            "AU2999",
                            self.current_span(),
                            "class patterns are not supported; destructure the class before matching or match a separate enum tag",
                        ));
                    }
                    subpatterns.push(self.parse_pattern()?);
                    if self.eat_simple(&TokenKind::Comma).is_none() {
                        break;
                    }
                }
            }
            self.expect_simple(TokenKind::RParen)?;
            subpatterns
        } else {
            Vec::new()
        };
        Ok(Pattern::Variant(VariantPattern {
            enum_name,
            variant_name,
            subpatterns,
            span,
        }))
    }

    fn parse_while_stmt(&mut self) -> Result<Stmt> {
        let span = self.expect_keyword(TokenKind::KwWhile)?.span;
        let condition = self.parse_expr()?;
        self.expect_simple(TokenKind::Colon)?;
        self.expect_newline()?;
        let body = self.parse_block()?;
        Ok(Stmt::While(WhileStmt {
            condition,
            body,
            span,
        }))
    }

    fn parse_break_stmt(&mut self) -> Result<Stmt> {
        let span = self.expect_keyword(TokenKind::KwBreak)?.span;
        self.expect_newline()?;
        Ok(Stmt::Break(BreakStmt { span }))
    }

    fn parse_continue_stmt(&mut self) -> Result<Stmt> {
        let span = self.expect_keyword(TokenKind::KwContinue)?.span;
        self.expect_newline()?;
        Ok(Stmt::Continue(ContinueStmt { span }))
    }

    fn parse_pass_stmt(&mut self) -> Result<Stmt> {
        let span = self.expect_keyword(TokenKind::KwPass)?.span;
        self.expect_newline()?;
        Ok(Stmt::Pass(crate::ast::PassStmt { span }))
    }

    fn parse_expr_stmt(&mut self) -> Result<Stmt> {
        let span = self.current_span();
        let expr = self.parse_expr()?;
        self.expect_statement_terminator()?;
        Ok(Stmt::Expr(ExprStmt { expr, span }))
    }

    fn parse_block(&mut self) -> Result<Vec<Stmt>> {
        self.expect_simple(TokenKind::Indent)?;

        let mut body = Vec::new();
        while !self.at_simple(&TokenKind::Dedent) && !self.at_eof() {
            if self.at_simple(&TokenKind::Newline) {
                self.bump();
                continue;
            }
            body.push(self.parse_stmt()?);
        }

        self.expect_simple(TokenKind::Dedent)?;
        Ok(body)
    }

    fn parse_type(&mut self) -> Result<TypeRef> {
        self.enter_recursion("type")?;
        let result = self.parse_type_inner();
        self.exit_recursion();
        result
    }

    fn parse_type_inner(&mut self) -> Result<TypeRef> {
        let span = self.current_span();
        let capability = match self.current_kind() {
            TokenKind::KwMut => Some("mut"),
            TokenKind::KwOwn => Some("own"),
            _ => None,
        };
        if let Some(capability) = capability {
            return Err(parse_error(
                span,
                format!(
                    "`{capability}` is not valid in a type position; capability modifiers belong only on parameters and receivers or on supported `for` and `match` selectors (`mut` also declares mutable local bindings)"
                ),
            ));
        }

        let indirect = self.eat_simple(&TokenKind::KwIndirect).is_some();
        let mut ty = if self.eat_simple(&TokenKind::KwDef).is_some() {
            if indirect {
                return Err(parse_error(
                    span,
                    "`indirect` is not valid on function types; function values already use pointer-like representation",
                ));
            }
            self.expect_simple(TokenKind::LParen)?;
            let mut params = Vec::new();
            if self.eat_simple(&TokenKind::RParen).is_none() {
                loop {
                    let param_span = self.current_span();
                    let mode = if self.eat_simple(&TokenKind::KwMut).is_some() {
                        ParamMode::BorrowMut
                    } else if self.eat_simple(&TokenKind::KwOwn).is_some() {
                        ParamMode::Own
                    } else {
                        ParamMode::Default
                    };
                    if matches!(self.current_kind(), TokenKind::KwMut | TokenKind::KwOwn) {
                        return Err(parse_error(
                            self.current_span(),
                            "function type parameters accept only one capability modifier",
                        ));
                    }
                    if matches!(
                        self.current_kind(),
                        TokenKind::Comma | TokenKind::RParen | TokenKind::Arrow
                    ) {
                        let message = if mode == ParamMode::Default {
                            "expected a function parameter type"
                        } else {
                            "expected a type after the function parameter capability"
                        };
                        return Err(parse_error(self.current_span(), message));
                    }
                    let ty = self.parse_type()?;
                    if self.at_simple(&TokenKind::Colon) {
                        return Err(parse_error(
                            self.current_span(),
                            "function type parameters contain types only; remove the parameter name",
                        ));
                    }
                    if self.at_simple(&TokenKind::Equal) {
                        return Err(parse_error(
                            self.current_span(),
                            "function type parameters cannot declare default values",
                        ));
                    }
                    params.push(FunctionTypeParam::new(mode, ty, param_span));
                    if self.eat_simple(&TokenKind::Comma).is_none() {
                        break;
                    }
                }
                self.expect_simple(TokenKind::RParen)?;
            }
            if self.eat_simple(&TokenKind::Arrow).is_none() {
                return Err(parse_error(
                    self.current_span(),
                    "expected `->` and a return type after function type parameters",
                ));
            }
            let return_type = self.parse_type()?;
            TypeRef::function_with_params(params, return_type, span)
        } else if self.eat_simple(&TokenKind::LParen).is_some() {
            if indirect {
                return Err(parse_error(
                    span,
                    "`indirect` applies only to named types, not tuple types",
                ));
            }
            if self.at_simple(&TokenKind::RParen) {
                return Err(parse_error(span, "empty tuple types are not supported"));
            }

            let first = self.parse_type()?;
            if self.eat_simple(&TokenKind::Comma).is_none() {
                return Err(parse_error(
                    self.current_span(),
                    "tuple types need a comma; write `(T,)` for a singleton tuple type or `T` for the type itself",
                ));
            }

            let mut elements = vec![first];
            if self.eat_simple(&TokenKind::RParen).is_none() {
                loop {
                    elements.push(self.parse_type()?);
                    if self.eat_simple(&TokenKind::Comma).is_none() {
                        break;
                    }
                    if self.at_simple(&TokenKind::RParen) {
                        return Err(parse_error(
                            self.current_span(),
                            "trailing commas are only allowed for singleton tuple types",
                        ));
                    }
                }
                self.expect_simple(TokenKind::RParen)?;
            }
            TypeRef::tuple(elements, false, span)
        } else {
            let name = self.parse_identifier_path()?.join(".");
            let mut args = Vec::new();

            if self.eat_simple(&TokenKind::LBracket).is_some() {
                loop {
                    args.push(self.parse_type()?);
                    if self.eat_simple(&TokenKind::Comma).is_none() {
                        break;
                    }
                }
                self.expect_simple(TokenKind::RBracket)?;
            }

            TypeRef::named(name, args, indirect, span)
        };
        if self.eat_simple(&TokenKind::Question).is_some() {
            ty = TypeRef::named("Option", vec![ty], indirect, span);
        }

        Ok(ty)
    }

    fn parse_identifier_path(&mut self) -> Result<Vec<String>> {
        let mut path = vec![self.expect_identifier()?];
        while self.eat_simple(&TokenKind::Dot).is_some() {
            path.push(self.expect_identifier()?);
        }
        Ok(path)
    }

    fn parse_expr(&mut self) -> Result<Expr> {
        self.enter_recursion("expression")?;
        let result = self.parse_non_tuple_expr_inner();
        self.exit_recursion();
        result
    }

    /// Parse an expression whose outermost form cannot consume a statement-level
    /// comma. Tuple expressions will be layered above this entry point so
    /// comma-delimited statement syntax can keep an explicit boundary.
    fn parse_non_tuple_expr(&mut self) -> Result<Expr> {
        self.enter_recursion("expression")?;
        let result = self.parse_non_tuple_expr_inner();
        self.exit_recursion();
        result
    }

    fn parse_non_tuple_expr_inner(&mut self) -> Result<Expr> {
        if matches!(self.current_kind(), TokenKind::Identifier(name) if name == "lambda") {
            self.parse_lambda()
        } else {
            self.parse_conditional()
        }
    }

    fn parse_lambda(&mut self) -> Result<Expr> {
        let token = self.bump();
        debug_assert!(matches!(
            token.kind,
            TokenKind::Identifier(ref name) if name == "lambda"
        ));

        let captures = if self.eat_simple(&TokenKind::LBracket).is_some() {
            let mut captures = Vec::new();
            if self.eat_simple(&TokenKind::RBracket).is_none() {
                loop {
                    let mode = if self.eat_simple(&TokenKind::KwOwn).is_some() {
                        ParamMode::Own
                    } else if self.eat_simple(&TokenKind::KwMut).is_some() {
                        ParamMode::BorrowMut
                    } else {
                        ParamMode::Default
                    };
                    let capture_span = self.current_span();
                    let name = self.expect_identifier()?;
                    if captures
                        .iter()
                        .any(|capture: &LambdaCapture| capture.name == name)
                    {
                        return Err(parse_error(
                            capture_span,
                            format!("duplicate lambda capture `{name}`"),
                        ));
                    }
                    captures.push(LambdaCapture {
                        name,
                        mode,
                        span: capture_span,
                    });
                    if self.eat_simple(&TokenKind::Comma).is_none() {
                        break;
                    }
                    if self.at_simple(&TokenKind::RBracket) {
                        return Err(parse_error(
                            self.current_span(),
                            "expected a capture name after `,` in lambda capture list",
                        ));
                    }
                }
                self.expect_simple(TokenKind::RBracket)?;
            }
            Some(captures)
        } else {
            None
        };

        let mut params: Vec<LambdaParam> = Vec::new();
        if !self.at_simple(&TokenKind::Colon) {
            loop {
                let mode = if self.eat_simple(&TokenKind::KwOwn).is_some() {
                    ParamMode::Own
                } else if self.eat_simple(&TokenKind::KwMut).is_some() {
                    ParamMode::BorrowMut
                } else {
                    ParamMode::Default
                };
                let param_span = self.current_span();
                let name = match self.current_kind() {
                    TokenKind::Identifier(name) if name != "lambda" => name.clone(),
                    TokenKind::KwFrom => "from".to_string(),
                    _ => {
                        return Err(parse_error(
                            param_span,
                            "expected a parameter name in lambda parameter list",
                        ))
                    }
                };
                self.bump();

                if self.at_simple(&TokenKind::Equal) {
                    return Err(parse_error(
                        self.current_span(),
                        "lambda parameters cannot have defaults; pass values explicitly when calling the closure",
                    ));
                }
                if params.iter().any(|param| param.name == name) {
                    return Err(parse_error(
                        param_span,
                        format!("duplicate lambda parameter `{name}`"),
                    ));
                }
                params.push(LambdaParam {
                    name,
                    mode,
                    span: param_span,
                });

                if self.eat_simple(&TokenKind::Comma).is_none() {
                    break;
                }
                if self.at_simple(&TokenKind::Colon) {
                    return Err(parse_error(
                        self.current_span(),
                        "expected a parameter name after `,` in lambda parameter list",
                    ));
                }
            }
        }

        if self.eat_simple(&TokenKind::Colon).is_none() {
            return Err(parse_error(
                self.current_span(),
                "expected `:` after lambda parameter list",
            ));
        }
        if self.at_simple(&TokenKind::Newline) || self.at_eof() {
            return Err(parse_error(
                self.current_span(),
                "lambda body must be a single expression after `:`; use a named `def` for multi-statement logic",
            ));
        }

        let body = self.parse_non_tuple_expr()?;
        if self.at_simple(&TokenKind::Colon) && looks_like_lambda_type_annotation(&body) {
            return Err(parse_error(
                self.current_span(),
                "lambda parameter types are inferred from context; write `lambda value: expression` without a parameter type",
            ));
        }
        Ok(Expr {
            kind: ExprKind::Lambda {
                captures,
                params,
                body: Box::new(body),
            },
            span: token.span,
        })
    }

    fn parse_conditional(&mut self) -> Result<Expr> {
        let then_expr = self.parse_or()?;
        let Some(if_token) = self.eat_simple(&TokenKind::KwIf) else {
            return Ok(then_expr);
        };

        self.enter_recursion("conditional expression")?;
        let result = (|| {
            let condition = self.parse_or()?;
            if self.eat_simple(&TokenKind::KwElse).is_none() {
                return Err(parse_error(
                    if_token.span,
                    "conditional expression requires `else` and an alternative value",
                ));
            }
            let else_expr = self.parse_non_tuple_expr()?;
            let span = then_expr.span;
            Ok(Expr {
                kind: ExprKind::Conditional {
                    then_expr: Box::new(then_expr),
                    condition: Box::new(condition),
                    else_expr: Box::new(else_expr),
                },
                span,
            })
        })();
        self.exit_recursion();
        result
    }

    fn parse_or(&mut self) -> Result<Expr> {
        let mut expr = self.parse_and()?;
        let mut chain_len = 0usize;

        while self.eat_simple(&TokenKind::KwOr).is_some() {
            chain_len += 1;
            self.check_expression_chain_limit(chain_len)?;
            let right = self.parse_and()?;
            let span = expr.span;
            expr = Expr {
                kind: ExprKind::Binary {
                    op: BinaryOp::Or,
                    left: Box::new(expr),
                    right: Box::new(right),
                },
                span,
            };
        }

        Ok(expr)
    }

    fn parse_and(&mut self) -> Result<Expr> {
        let mut expr = self.parse_not()?;
        let mut chain_len = 0usize;

        while self.eat_simple(&TokenKind::KwAnd).is_some() {
            chain_len += 1;
            self.check_expression_chain_limit(chain_len)?;
            let right = self.parse_not()?;
            let span = expr.span;
            expr = Expr {
                kind: ExprKind::Binary {
                    op: BinaryOp::And,
                    left: Box::new(expr),
                    right: Box::new(right),
                },
                span,
            };
        }

        Ok(expr)
    }

    fn parse_not(&mut self) -> Result<Expr> {
        let mut operator_spans = Vec::new();
        while let Some(token) = self.eat_simple(&TokenKind::KwNot) {
            operator_spans.push(token.span);
            self.check_expression_chain_limit(operator_spans.len())?;
        }

        let mut value = self.parse_comparison_chain()?;
        while let Some(span) = operator_spans.pop() {
            value = Expr {
                kind: ExprKind::Unary {
                    op: UnaryOp::Not,
                    expr: Box::new(value),
                },
                span,
            };
        }

        Ok(value)
    }

    /// Parses the Python-shaped comparison level. Equality, ordering, and
    /// membership share one precedence and chain left to right, so
    /// `a < b <= c` is one chain rather than a comparison of a comparison.
    fn parse_comparison_chain(&mut self) -> Result<Expr> {
        let first = self.parse_binary_precedence(0)?;
        let mut links: Vec<CompareLink> = Vec::new();

        loop {
            if let Some(token) = self.eat_simple(&TokenKind::KwIs) {
                return Err(Diagnostic::coded_at(
                    "AU2005",
                    token.span,
                    "`is` is not supported; use `== None` or `match` for optional values",
                ));
            }
            let Some((op, op_span)) = self.eat_comparison_operator() else {
                break;
            };
            self.check_expression_chain_limit(links.len().saturating_add(1))?;
            let operand = self.parse_binary_precedence(0)?;
            links.push(CompareLink {
                op,
                op_span,
                operand,
            });
        }

        if links.is_empty() {
            return Ok(first);
        }
        let span = first.span;
        if links.len() == 1 {
            let CompareLink {
                op,
                op_span,
                operand,
            } = links.remove(0);
            let kind = match op.as_binary_op() {
                Some(op) => ExprKind::Binary {
                    op,
                    left: Box::new(first),
                    right: Box::new(operand),
                },
                None => ExprKind::Membership {
                    value: Box::new(first),
                    container: Box::new(operand),
                    negated: op == CompareOp::NotIn,
                    operator_span: op_span,
                },
            };
            return Ok(Expr { kind, span });
        }

        Ok(Expr {
            kind: ExprKind::CompareChain {
                first: Box::new(first),
                links,
            },
            span,
        })
    }

    /// Parses the complete left-associative arithmetic and bitwise ladder in
    /// one precedence-climbing frame. Keeping the ladder iterative avoids
    /// multiplying parser stack use by every precedence level inside nested
    /// grouping expressions.
    fn parse_binary_precedence(&mut self, minimum_precedence: u8) -> Result<Expr> {
        let mut expr = self.parse_prefix()?;
        let mut chain_len = 0usize;
        while let Some((op, precedence)) = self.current_binary_precedence() {
            if precedence < minimum_precedence {
                break;
            }
            self.bump();
            chain_len += 1;
            self.check_expression_chain_limit(chain_len)?;
            let right = self.parse_binary_precedence(precedence.saturating_add(1))?;
            let span = expr.span;
            expr = Expr {
                kind: ExprKind::Binary {
                    op,
                    left: Box::new(expr),
                    right: Box::new(right),
                },
                span,
            };
        }
        Ok(expr)
    }

    fn current_binary_precedence(&self) -> Option<(BinaryOp, u8)> {
        Some(match self.current_kind() {
            TokenKind::Pipe => (BinaryOp::BitOr, 0),
            TokenKind::Caret => (BinaryOp::BitXor, 1),
            TokenKind::Ampersand => (BinaryOp::BitAnd, 2),
            TokenKind::ShiftLeft => (BinaryOp::Shl, 3),
            TokenKind::ShiftRight => (BinaryOp::Shr, 3),
            TokenKind::Plus => (BinaryOp::Add, 4),
            TokenKind::Minus => (BinaryOp::Sub, 4),
            TokenKind::Star => (BinaryOp::Mul, 5),
            TokenKind::Slash => (BinaryOp::Div, 5),
            TokenKind::DoubleSlash => (BinaryOp::FloorDiv, 5),
            TokenKind::Percent => (BinaryOp::Mod, 5),
            _ => return None,
        })
    }

    /// Consumes one comparison operator, including the two-token `not in`.
    fn eat_comparison_operator(&mut self) -> Option<(CompareOp, Span)> {
        if self.at_simple(&TokenKind::KwNot) && matches!(self.peek_kind(1), Some(TokenKind::KwIn)) {
            let span = self.current_span();
            self.bump();
            self.bump();
            return Some((CompareOp::NotIn, span));
        }
        for (kind, op) in [
            (TokenKind::EqEq, CompareOp::Eq),
            (TokenKind::NotEq, CompareOp::NotEq),
            (TokenKind::LessEq, CompareOp::LessEq),
            (TokenKind::GreaterEq, CompareOp::GreaterEq),
            (TokenKind::Less, CompareOp::Less),
            (TokenKind::Greater, CompareOp::Greater),
            (TokenKind::KwIn, CompareOp::In),
        ] {
            if let Some(token) = self.eat_simple(&kind) {
                return Some((op, token.span));
            }
        }
        None
    }

    fn parse_prefix(&mut self) -> Result<Expr> {
        self.enter_recursion("expression")?;
        let result = self.parse_prefix_inner();
        self.exit_recursion();
        result
    }

    fn parse_prefix_inner(&mut self) -> Result<Expr> {
        if let Some(token) = self.eat_simple(&TokenKind::KwMatch) {
            let capability = self.parse_match_capability()?;
            let scrutinee = self.parse_expr()?;
            self.expect_simple(TokenKind::Colon)?;
            self.expect_newline()?;
            self.expect_simple(TokenKind::Indent)?;
            let mut arms = Vec::new();
            while !self.at_match_expr_end() && !self.at_eof() {
                if self.at_simple(&TokenKind::Newline) {
                    self.bump();
                    continue;
                }
                arms.push(self.parse_match_expr_arm()?);
            }
            if !self.at_simple(&TokenKind::Dedent) {
                return Err(self.error_here("expected end of match expression"));
            }
            self.expect_simple(TokenKind::Dedent)?;
            return Ok(Expr {
                kind: ExprKind::Match {
                    scrutinee: Box::new(scrutinee),
                    capability,
                    arms,
                },
                span: token.span,
            });
        }

        if let Some(token) = self.eat_simple(&TokenKind::KwTry) {
            let value = self.parse_prefix()?;
            return Ok(Expr {
                kind: ExprKind::Try(Box::new(value)),
                span: token.span,
            });
        }

        if let Some(token) = self.eat_simple(&TokenKind::Minus) {
            let value = self.parse_prefix()?;
            return Ok(Expr {
                kind: ExprKind::Unary {
                    op: UnaryOp::Neg,
                    expr: Box::new(value),
                },
                span: token.span,
            });
        }

        if let Some(token) = self.eat_simple(&TokenKind::Tilde) {
            let value = self.parse_prefix()?;
            return Ok(Expr {
                kind: ExprKind::Unary {
                    op: UnaryOp::BitNot,
                    expr: Box::new(value),
                },
                span: token.span,
            });
        }

        self.parse_power()
    }

    fn parse_power(&mut self) -> Result<Expr> {
        let left = self.parse_postfix()?;
        if self.eat_simple(&TokenKind::DoubleStar).is_none() {
            return Ok(left);
        }
        let right = self.parse_prefix()?;
        let span = left.span;
        Ok(Expr {
            kind: ExprKind::Binary {
                op: BinaryOp::Pow,
                left: Box::new(left),
                right: Box::new(right),
            },
            span,
        })
    }

    fn parse_postfix(&mut self) -> Result<Expr> {
        let mut expr = self.parse_primary()?;
        let mut chain_len = 0usize;

        loop {
            if self.at_simple(&TokenKind::LBracket) && self.starts_specialization_suffix(&expr) {
                chain_len += 1;
                self.check_expression_chain_limit(chain_len)?;
                self.bump();
                let mut type_args = Vec::new();
                loop {
                    type_args.push(self.parse_type()?);
                    if self.eat_simple(&TokenKind::Comma).is_none() {
                        break;
                    }
                }
                self.expect_simple(TokenKind::RBracket)?;
                let span = expr.span;
                expr = Expr {
                    kind: ExprKind::Specialize {
                        expr: Box::new(expr),
                        type_args,
                    },
                    span,
                };
                continue;
            }

            if self.eat_simple(&TokenKind::LBracket).is_some() {
                chain_len += 1;
                self.check_expression_chain_limit(chain_len)?;
                if let Some(colon) = self.eat_simple(&TokenKind::Colon) {
                    if self.at_simple(&TokenKind::Colon) {
                        return Err(self.slice_step_error());
                    }
                    let end = if self.at_simple(&TokenKind::RBracket) {
                        None
                    } else {
                        Some(Box::new(self.parse_expr()?))
                    };
                    if self.at_simple(&TokenKind::Colon) {
                        return Err(self.slice_step_error());
                    }
                    self.expect_simple(TokenKind::RBracket)?;
                    let span = expr.span;
                    expr = Expr {
                        kind: ExprKind::Slice {
                            object: Box::new(expr),
                            start: None,
                            end,
                            colon_span: colon.span,
                        },
                        span,
                    };
                    continue;
                }
                let first = self.parse_expr()?;
                if let Some(colon) = self.eat_simple(&TokenKind::Colon) {
                    if self.at_simple(&TokenKind::Colon) {
                        return Err(self.slice_step_error());
                    }
                    let end = if self.at_simple(&TokenKind::RBracket) {
                        None
                    } else {
                        Some(Box::new(self.parse_expr()?))
                    };
                    if self.at_simple(&TokenKind::Colon) {
                        return Err(self.slice_step_error());
                    }
                    self.expect_simple(TokenKind::RBracket)?;
                    let span = expr.span;
                    expr = Expr {
                        kind: ExprKind::Slice {
                            object: Box::new(expr),
                            start: Some(Box::new(first)),
                            end,
                            colon_span: colon.span,
                        },
                        span,
                    };
                    continue;
                }
                let index = if self.eat_simple(&TokenKind::Comma).is_some() {
                    let mut elements = vec![first];
                    loop {
                        elements.push(self.parse_expr()?);
                        if self.eat_simple(&TokenKind::Comma).is_none() {
                            break;
                        }
                    }
                    Expr {
                        span: expr.span,
                        kind: ExprKind::Tuple(elements),
                    }
                } else {
                    first
                };
                self.expect_simple(TokenKind::RBracket)?;
                let span = expr.span;
                expr = Expr {
                    kind: ExprKind::Index {
                        object: Box::new(expr),
                        index: Box::new(index),
                    },
                    span,
                };
                continue;
            }

            if self.eat_simple(&TokenKind::Dot).is_some() {
                chain_len += 1;
                self.check_expression_chain_limit(chain_len)?;
                let field_span = self.current_span();
                let field = self.expect_member_name()?;
                expr = Expr {
                    kind: ExprKind::Member {
                        object: Box::new(expr),
                        field,
                    },
                    span: field_span,
                };
                continue;
            }

            if self.eat_simple(&TokenKind::LParen).is_some() {
                chain_len += 1;
                self.check_expression_chain_limit(chain_len)?;
                let args = self.parse_args()?;
                self.expect_simple(TokenKind::RParen)?;
                let span = expr.span;
                expr = Expr {
                    kind: ExprKind::Call {
                        callee: Box::new(expr),
                        args,
                    },
                    span,
                };
                continue;
            }

            if self.at_simple(&TokenKind::KwAs) && self.next_starts_numeric_cast_type() {
                chain_len += 1;
                self.check_expression_chain_limit(chain_len)?;
                self.bump();
                let ty = self.parse_type()?;
                let span = expr.span;
                expr = Expr {
                    kind: ExprKind::Cast {
                        expr: Box::new(expr),
                        ty,
                    },
                    span,
                };
                continue;
            }

            break;
        }

        Ok(expr)
    }

    fn next_starts_numeric_cast_type(&self) -> bool {
        matches!(self.peek_kind(1), Some(TokenKind::KwMut | TokenKind::KwOwn))
            || matches!(
                self.peek_kind(1),
                Some(TokenKind::Identifier(name))
                    if matches!(
                        name.as_str(),
                        "int"
                            | "int8"
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

    fn parse_primary(&mut self) -> Result<Expr> {
        let token = self.bump();

        match token.kind {
            TokenKind::Identifier(name) => Ok(Expr {
                kind: ExprKind::Name(name),
                span: token.span,
            }),
            TokenKind::KwFrom => Ok(Expr {
                kind: ExprKind::Name("from".to_string()),
                span: token.span,
            }),
            TokenKind::IntLiteral(value) => Ok(Expr {
                kind: ExprKind::Int(value),
                span: token.span,
            }),
            TokenKind::DurationLiteral(value) => Ok(Expr {
                kind: ExprKind::DurationNanos(value),
                span: token.span,
            }),
            TokenKind::FloatLiteral(value) => Ok(Expr {
                kind: ExprKind::Float(value),
                span: token.span,
            }),
            TokenKind::BoolLiteral(value) => Ok(Expr {
                kind: ExprKind::Bool(value),
                span: token.span,
            }),
            TokenKind::StringLiteral(value) => Ok(Expr {
                kind: ExprKind::String(value),
                span: token.span,
            }),
            TokenKind::FStringLiteral(value) => Ok(Expr {
                kind: ExprKind::FString(self.parse_format_parts(&value, token.span)?),
                span: token.span,
            }),
            TokenKind::LParen => {
                if self.at_simple(&TokenKind::RParen) {
                    return Err(parse_error(
                        token.span,
                        "empty tuple literals are not supported; use `None` for unit",
                    ));
                }

                let first = self.parse_non_tuple_expr()?;
                if self.at_simple(&TokenKind::KwFor) {
                    return Err(self.generator_expression_error());
                }
                if self.eat_simple(&TokenKind::Comma).is_none() {
                    self.expect_simple(TokenKind::RParen)?;
                    return Ok(Expr {
                        kind: ExprKind::Group(Box::new(first)),
                        span: token.span,
                    });
                }

                let mut elements = vec![first];
                if self.eat_simple(&TokenKind::RParen).is_some() {
                    return Ok(Expr {
                        kind: ExprKind::Tuple(elements),
                        span: token.span,
                    });
                }

                loop {
                    elements.push(self.parse_non_tuple_expr()?);
                    if self.eat_simple(&TokenKind::Comma).is_none() {
                        break;
                    }
                    if self.at_simple(&TokenKind::RParen) {
                        return Err(parse_error(
                            self.current_span(),
                            "trailing commas are only allowed for singleton tuple literals",
                        ));
                    }
                }
                self.expect_simple(TokenKind::RParen)?;
                Ok(Expr {
                    kind: ExprKind::Tuple(elements),
                    span: token.span,
                })
            }
            TokenKind::LBracket => {
                if self.at_simple(&TokenKind::RBracket) {
                    self.bump();
                    return Ok(Expr {
                        kind: ExprKind::List(Vec::new()),
                        span: token.span,
                    });
                }

                let first = self.parse_expr()?;
                if self.at_simple(&TokenKind::KwFor) {
                    let clauses = self.parse_comprehension_clauses(&TokenKind::RBracket)?;
                    self.expect_simple(TokenKind::RBracket)?;
                    return Ok(Expr {
                        kind: ExprKind::Comprehension {
                            output: ComprehensionOutput::List(Box::new(first)),
                            clauses,
                        },
                        span: token.span,
                    });
                }

                let mut elements = vec![first];
                while self.eat_simple(&TokenKind::Comma).is_some() {
                    elements.push(self.parse_expr()?);
                    if self.at_simple(&TokenKind::KwFor) {
                        return Err(self.mixed_comprehension_literal_error());
                    }
                }
                self.expect_simple(TokenKind::RBracket)?;
                Ok(Expr {
                    kind: ExprKind::List(elements),
                    span: token.span,
                })
            }
            TokenKind::LBrace => {
                if self.at_simple(&TokenKind::RBrace) {
                    self.bump();
                    return Ok(Expr {
                        kind: ExprKind::Map(Vec::new()),
                        span: token.span,
                    });
                }

                let first = self.parse_expr()?;
                if self.eat_simple(&TokenKind::Colon).is_none() {
                    if self.at_simple(&TokenKind::KwFor) {
                        let clauses = self.parse_comprehension_clauses(&TokenKind::RBrace)?;
                        self.expect_simple(TokenKind::RBrace)?;
                        return Ok(Expr {
                            kind: ExprKind::Comprehension {
                                output: ComprehensionOutput::Set(Box::new(first)),
                                clauses,
                            },
                            span: token.span,
                        });
                    }

                    let mut elements = vec![first];
                    while self.eat_simple(&TokenKind::Comma).is_some() {
                        elements.push(self.parse_expr()?);
                        if self.at_simple(&TokenKind::KwFor) {
                            return Err(self.mixed_comprehension_literal_error());
                        }
                    }
                    self.expect_simple(TokenKind::RBrace)?;
                    return Ok(Expr {
                        kind: ExprKind::Set(elements),
                        span: token.span,
                    });
                }

                let value = self.parse_expr()?;
                if self.at_simple(&TokenKind::KwFor) {
                    let clauses = self.parse_comprehension_clauses(&TokenKind::RBrace)?;
                    self.expect_simple(TokenKind::RBrace)?;
                    return Ok(Expr {
                        kind: ExprKind::Comprehension {
                            output: ComprehensionOutput::Map {
                                key: Box::new(first),
                                value: Box::new(value),
                            },
                            clauses,
                        },
                        span: token.span,
                    });
                }

                let mut entries = vec![MapEntryExpr { key: first, value }];
                while self.eat_simple(&TokenKind::Comma).is_some() {
                    let key = self.parse_expr()?;
                    self.expect_simple(TokenKind::Colon)?;
                    let value = self.parse_expr()?;
                    entries.push(MapEntryExpr { key, value });
                    if self.at_simple(&TokenKind::KwFor) {
                        return Err(self.mixed_comprehension_literal_error());
                    }
                }
                self.expect_simple(TokenKind::RBrace)?;
                Ok(Expr {
                    kind: ExprKind::Map(entries),
                    span: token.span,
                })
            }
            TokenKind::KwMut => Err(parse_error(
                token.span,
                "`mut` cannot prefix a call argument or other expression; pass the value directly because the callee parameter declares shared, mutable, or owned access. Capability modifiers belong only on parameters and receivers or on supported `for` and `match` selectors (`mut` also declares mutable local bindings)",
            )),
            TokenKind::KwOwn => Err(parse_error(
                token.span,
                "`own` cannot prefix a call argument or other expression; pass the value directly because the callee parameter declares shared, mutable, or owned access. Capability modifiers belong only on parameters and receivers or on supported `for` and `match` selectors (`mut` also declares mutable local bindings)",
            )),
            other => Err(parse_error(
                token.span,
                format!("unexpected token in expression: {:?}", other),
            )),
        }
    }

    fn parse_comprehension_clauses(
        &mut self,
        closing: &TokenKind,
    ) -> Result<Vec<ComprehensionClause>> {
        let mut clauses = Vec::new();
        let mut chain_len = 0usize;

        while let Some(for_token) = self.eat_simple(&TokenKind::KwFor) {
            chain_len += 1;
            self.check_expression_chain_limit(chain_len)?;
            if self.at_simple(&TokenKind::KwMut) || self.at_simple(&TokenKind::KwOwn) {
                return Err(self.comprehension_capability_error());
            }
            if self.at_simple(&TokenKind::KwIn) {
                return Err(parse_error(
                    self.current_span(),
                    "expected a binding target after `for` in comprehension",
                ));
            }

            let target = self.parse_binding_target_sequence(false)?;
            self.reject_duplicate_binding_names(&target)?;
            if self.eat_simple(&TokenKind::KwIn).is_none() {
                return Err(parse_error(
                    self.current_span(),
                    "expected `in` after comprehension target",
                ));
            }
            if self.at_simple(&TokenKind::KwMut) || self.at_simple(&TokenKind::KwOwn) {
                return Err(self.comprehension_capability_error());
            }
            if self.at_comprehension_component_boundary(closing) {
                return Err(parse_error(
                    self.current_span(),
                    "expected an iterable expression after `in` in comprehension",
                ));
            }

            let iterable = self.parse_comprehension_component()?;
            let mut filters = Vec::new();
            while self.eat_simple(&TokenKind::KwIf).is_some() {
                chain_len += 1;
                self.check_expression_chain_limit(chain_len)?;
                if self.at_comprehension_component_boundary(closing) {
                    return Err(parse_error(
                        self.current_span(),
                        "expected a filter expression after `if` in comprehension",
                    ));
                }
                filters.push(self.parse_comprehension_component()?);
            }

            clauses.push(ComprehensionClause {
                target,
                iterable,
                filters,
                span: for_token.span,
            });
        }

        if self.at_simple(&TokenKind::Comma) {
            if self.peek_kind(1).is_some_and(|kind| kind == closing) {
                return Err(parse_error(
                    self.current_span(),
                    "comprehensions do not allow trailing commas",
                ));
            }
            return Err(self.mixed_comprehension_literal_error());
        }
        if !self.at_simple(closing) {
            return Err(parse_error(
                self.current_span(),
                "expected `for`, `if`, or the end of the comprehension",
            ));
        }

        Ok(clauses)
    }

    /// Clause iterables and filters stop before a comprehension-level `if`.
    /// A conditional expression remains available when explicitly grouped.
    fn parse_comprehension_component(&mut self) -> Result<Expr> {
        if matches!(self.current_kind(), TokenKind::Identifier(name) if name == "lambda") {
            self.parse_lambda()
        } else {
            self.parse_or()
        }
    }

    fn at_comprehension_component_boundary(&self, closing: &TokenKind) -> bool {
        self.at_simple(closing)
            || self.at_simple(&TokenKind::Comma)
            || self.at_simple(&TokenKind::KwFor)
            || self.at_simple(&TokenKind::KwIf)
            || self.at_eof()
    }

    fn comprehension_capability_error(&self) -> Diagnostic {
        parse_error(
            self.current_span(),
            "comprehensions use bare iteration; remove `mut` or `own` and write `for name in values`",
        )
    }

    fn mixed_comprehension_literal_error(&self) -> Diagnostic {
        parse_error(
            self.current_span(),
            "cannot mix literal entries with a comprehension; remove the comma-separated literal entries or use an explicit loop",
        )
    }

    fn generator_expression_error(&self) -> Diagnostic {
        Diagnostic::coded_at(
            "AU2005",
            self.current_span(),
            "generator expressions are unavailable; use an eager owned list comprehension or an explicit loop",
        )
    }

    fn slice_step_error(&self) -> Diagnostic {
        Diagnostic::coded_at(
            "AU2005",
            self.current_span(),
            "slice steps are unavailable; use an explicit loop to select a stride",
        )
    }

    fn slice_assignment_error(&self) -> Diagnostic {
        Diagnostic::coded_at(
            "AU2005",
            self.current_span(),
            "slice assignment is unavailable because slices are owned copies; mutate the source by index or build a new value",
        )
    }

    fn parse_args(&mut self) -> Result<Vec<Argument>> {
        let mut args = Vec::new();

        if self.at_simple(&TokenKind::RParen) {
            return Ok(args);
        }

        loop {
            let span = self.current_span();
            let contextual_name = match self.current_kind() {
                TokenKind::Identifier(name) => Some(name.clone()),
                TokenKind::KwFrom => Some("from".to_string()),
                _ => None,
            };
            let argument = if let Some(name) = contextual_name {
                if matches!(self.peek_kind(1), Some(TokenKind::Equal)) {
                    self.bump();
                    self.bump();
                    let value = self.parse_expr()?;
                    Argument {
                        name: Some(name),
                        value,
                        span,
                    }
                } else {
                    let value = self.parse_expr()?;
                    Argument {
                        name: None,
                        value,
                        span,
                    }
                }
            } else {
                let value = self.parse_expr()?;
                Argument {
                    name: None,
                    value,
                    span,
                }
            };

            if self.at_simple(&TokenKind::KwFor) {
                return Err(self.generator_expression_error());
            }
            args.push(argument);
            if self.eat_simple(&TokenKind::Comma).is_none() {
                break;
            }
        }

        Ok(args)
    }

    fn is_assignment_stmt(&self) -> bool {
        let mut idx = self.index;
        if matches!(self.peek_kind_at(idx), Some(TokenKind::KwMut)) {
            idx += 1;
        }

        if !self.is_contextual_identifier_at(idx) {
            return false;
        }
        idx += 1;
        let mut saw_suffix = false;

        loop {
            if matches!(self.peek_kind_at(idx), Some(TokenKind::Dot))
                && self.is_contextual_identifier_at(idx + 1)
            {
                saw_suffix = true;
                idx += 2;
                continue;
            }

            if matches!(self.peek_kind_at(idx), Some(TokenKind::LBracket)) {
                let Some(next_idx) = self.skip_bracketed_tokens(idx) else {
                    return false;
                };
                saw_suffix = true;
                idx = next_idx;
                continue;
            }

            break;
        }

        if self.is_assignment_operator_kind(self.peek_kind_at(idx)) {
            return true;
        }

        if !saw_suffix && matches!(self.peek_kind_at(idx), Some(TokenKind::Colon)) {
            idx += 1;
            idx = self.skip_type_tokens(idx);
            return self.is_assignment_operator_kind(self.peek_kind_at(idx));
        }

        false
    }

    fn is_destructure_assignment_stmt(&self) -> bool {
        let mut idx = self.index;
        if matches!(self.peek_kind_at(idx), Some(TokenKind::KwMut)) {
            idx += 1;
        }

        let mut paren_depth = 0usize;
        let mut bracket_depth = 0usize;
        let mut brace_depth = 0usize;
        let mut saw_comma = false;
        loop {
            match self.peek_kind_at(idx) {
                Some(TokenKind::LParen) => paren_depth += 1,
                Some(TokenKind::RParen) => {
                    if paren_depth == 0 {
                        return false;
                    }
                    paren_depth -= 1;
                }
                Some(TokenKind::LBracket) => bracket_depth += 1,
                Some(TokenKind::RBracket) => {
                    if bracket_depth == 0 {
                        return false;
                    }
                    bracket_depth -= 1;
                }
                Some(TokenKind::LBrace) => brace_depth += 1,
                Some(TokenKind::RBrace) => {
                    if brace_depth == 0 {
                        return false;
                    }
                    brace_depth -= 1;
                }
                Some(TokenKind::Colon)
                    if paren_depth == 0 && bracket_depth == 0 && brace_depth == 0 && !saw_comma =>
                {
                    return false;
                }
                Some(TokenKind::Comma) if bracket_depth == 0 && brace_depth == 0 => {
                    saw_comma = true;
                }
                kind if paren_depth == 0
                    && bracket_depth == 0
                    && brace_depth == 0
                    && self.is_assignment_operator_kind(kind) =>
                {
                    return saw_comma;
                }
                Some(TokenKind::Newline | TokenKind::Eof) | None => return false,
                Some(_) => {}
            }
            idx += 1;
        }
    }

    fn parse_binding_target_sequence(
        &mut self,
        allow_unparenthesized_singleton: bool,
    ) -> Result<BindingTarget> {
        let span = self.current_span();
        let first = self.parse_binding_target_atom()?;
        if self.eat_simple(&TokenKind::Comma).is_none() {
            return Ok(first);
        }

        let mut elements = vec![first];
        if self.binding_target_sequence_terminator() {
            if allow_unparenthesized_singleton {
                return Ok(BindingTarget::Tuple { elements, span });
            }
            return Err(parse_error(
                self.current_span(),
                "an unparenthesized destructuring target cannot end with a comma; write `(name,)` for a singleton tuple target",
            ));
        }

        loop {
            elements.push(self.parse_binding_target_atom()?);
            if self.eat_simple(&TokenKind::Comma).is_none() {
                break;
            }
            if self.binding_target_sequence_terminator() {
                return Err(parse_error(
                    self.current_span(),
                    "trailing commas are only allowed for singleton tuple targets",
                ));
            }
        }

        Ok(BindingTarget::Tuple { elements, span })
    }

    fn parse_binding_target_atom(&mut self) -> Result<BindingTarget> {
        let span = self.current_span();
        if self.eat_simple(&TokenKind::LParen).is_some() {
            if self.at_simple(&TokenKind::RParen) {
                return Err(parse_error(
                    span,
                    "empty tuple binding targets are not supported",
                ));
            }
            let target = self.parse_binding_target_sequence(true)?;
            self.expect_simple(TokenKind::RParen)?;
            return Ok(target);
        }

        let name = self.expect_identifier().map_err(|_| {
            parse_error(
                span,
                "binding targets must be names or recursively nested tuple targets",
            )
        })?;
        Ok(BindingTarget::Name { name, span })
    }

    fn binding_target_sequence_terminator(&self) -> bool {
        self.at_simple(&TokenKind::RParen)
            || self.at_simple(&TokenKind::KwIn)
            || self.is_assignment_operator_kind(Some(self.current_kind()))
    }

    fn reject_duplicate_binding_names(&self, target: &BindingTarget) -> Result<()> {
        fn visit(
            target: &BindingTarget,
            names: &mut std::collections::BTreeSet<String>,
        ) -> Result<()> {
            match target {
                BindingTarget::Name { name, span } => {
                    if !names.insert(name.clone()) {
                        return Err(parse_error(
                            *span,
                            format!("duplicate binding target `{name}`"),
                        ));
                    }
                }
                BindingTarget::Tuple { elements, .. } => {
                    for element in elements {
                        visit(element, names)?;
                    }
                }
            }
            Ok(())
        }

        visit(target, &mut std::collections::BTreeSet::new())
    }

    fn parse_assign_target(&mut self) -> Result<AssignTarget> {
        let span = self.current_span();
        let name = self.expect_identifier()?;
        let mut target = AssignTarget::Name(name);

        loop {
            if self.eat_simple(&TokenKind::Dot).is_some() {
                let field = self.expect_member_name()?;
                let object = assign_target_to_expr(target, span);
                target = AssignTarget::Member {
                    object: Box::new(object),
                    field,
                };
                continue;
            }

            if self.eat_simple(&TokenKind::LBracket).is_some() {
                if self.at_simple(&TokenKind::Colon) {
                    return Err(self.slice_assignment_error());
                }
                let first = self.parse_expr()?;
                if self.at_simple(&TokenKind::Colon) {
                    return Err(self.slice_assignment_error());
                }
                let index = if self.eat_simple(&TokenKind::Comma).is_some() {
                    let mut elements = vec![first];
                    loop {
                        elements.push(self.parse_expr()?);
                        if self.at_simple(&TokenKind::Colon) {
                            return Err(self.slice_assignment_error());
                        }
                        if self.eat_simple(&TokenKind::Comma).is_none() {
                            break;
                        }
                    }
                    Expr {
                        span,
                        kind: ExprKind::Tuple(elements),
                    }
                } else {
                    first
                };
                self.expect_simple(TokenKind::RBracket)?;
                let object = assign_target_to_expr(target, span);
                target = AssignTarget::Index {
                    object: Box::new(object),
                    index: Box::new(index),
                };
                continue;
            }

            break;
        }

        Ok(target)
    }

    fn parse_assignment_operator(&mut self) -> Result<Option<BinaryOp>> {
        let token = self.bump();
        match token.kind {
            TokenKind::Equal => Ok(None),
            TokenKind::PlusEqual => Ok(Some(BinaryOp::Add)),
            TokenKind::MinusEqual => Ok(Some(BinaryOp::Sub)),
            TokenKind::StarEqual => Ok(Some(BinaryOp::Mul)),
            TokenKind::DoubleStarEqual => Ok(Some(BinaryOp::Pow)),
            TokenKind::SlashEqual => Ok(Some(BinaryOp::Div)),
            TokenKind::DoubleSlashEqual => Ok(Some(BinaryOp::FloorDiv)),
            TokenKind::PercentEqual => Ok(Some(BinaryOp::Mod)),
            TokenKind::AmpersandEqual => Ok(Some(BinaryOp::BitAnd)),
            TokenKind::PipeEqual => Ok(Some(BinaryOp::BitOr)),
            TokenKind::CaretEqual => Ok(Some(BinaryOp::BitXor)),
            TokenKind::ShiftLeftEqual => Ok(Some(BinaryOp::Shl)),
            TokenKind::ShiftRightEqual => Ok(Some(BinaryOp::Shr)),
            other => Err(parse_error(
                token.span,
                format!("expected assignment operator, found {:?}", other),
            )),
        }
    }

    fn is_assignment_operator_kind(&self, kind: Option<&TokenKind>) -> bool {
        matches!(
            kind,
            Some(
                TokenKind::Equal
                    | TokenKind::PlusEqual
                    | TokenKind::MinusEqual
                    | TokenKind::StarEqual
                    | TokenKind::DoubleStarEqual
                    | TokenKind::SlashEqual
                    | TokenKind::DoubleSlashEqual
                    | TokenKind::PercentEqual
                    | TokenKind::AmpersandEqual
                    | TokenKind::PipeEqual
                    | TokenKind::CaretEqual
                    | TokenKind::ShiftLeftEqual
                    | TokenKind::ShiftRightEqual
            )
        )
    }

    fn skip_type_tokens(&self, mut idx: usize) -> usize {
        while matches!(
            self.peek_kind_at(idx),
            Some(TokenKind::KwMut | TokenKind::KwOwn)
        ) {
            idx += 1;
        }

        if matches!(self.peek_kind_at(idx), Some(TokenKind::KwIndirect)) {
            idx += 1;
        }

        if matches!(self.peek_kind_at(idx), Some(TokenKind::KwDef)) {
            idx += 1;
            if !matches!(self.peek_kind_at(idx), Some(TokenKind::LParen)) {
                return idx;
            }
            idx += 1;
            if !matches!(self.peek_kind_at(idx), Some(TokenKind::RParen)) {
                loop {
                    let next = self.skip_type_tokens(idx);
                    if next == idx {
                        return idx;
                    }
                    idx = next;
                    if matches!(self.peek_kind_at(idx), Some(TokenKind::Colon)) {
                        let named_type_start = idx + 1;
                        let next = self.skip_type_tokens(named_type_start);
                        if next == named_type_start {
                            return idx;
                        }
                        idx = next;
                    }
                    if matches!(self.peek_kind_at(idx), Some(TokenKind::Equal)) {
                        idx += 1;
                        let mut delimiter_depth = 0usize;
                        loop {
                            match self.peek_kind_at(idx) {
                                Some(
                                    TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace,
                                ) => delimiter_depth += 1,
                                Some(TokenKind::RParen) if delimiter_depth == 0 => break,
                                Some(
                                    TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace,
                                ) => delimiter_depth = delimiter_depth.saturating_sub(1),
                                Some(TokenKind::Comma) if delimiter_depth == 0 => break,
                                Some(TokenKind::Newline | TokenKind::Eof) | None => return idx,
                                Some(_) => {}
                            }
                            idx += 1;
                        }
                    }
                    if matches!(self.peek_kind_at(idx), Some(TokenKind::Comma)) {
                        idx += 1;
                        continue;
                    }
                    break;
                }
            }
            if !matches!(self.peek_kind_at(idx), Some(TokenKind::RParen)) {
                return idx;
            }
            idx += 1;
            if !matches!(self.peek_kind_at(idx), Some(TokenKind::Arrow)) {
                return idx;
            }
            return self.skip_type_tokens(idx + 1);
        }

        if matches!(self.peek_kind_at(idx), Some(TokenKind::LParen)) {
            let mut depth = 0usize;
            loop {
                match self.peek_kind_at(idx) {
                    Some(TokenKind::LParen) => depth += 1,
                    Some(TokenKind::RParen) => {
                        depth = depth.saturating_sub(1);
                        idx += 1;
                        if depth == 0 {
                            break;
                        }
                        continue;
                    }
                    Some(TokenKind::Newline | TokenKind::Eof) | None => return idx,
                    Some(_) => {}
                }
                idx += 1;
            }
            if matches!(self.peek_kind_at(idx), Some(TokenKind::Question)) {
                idx += 1;
            }
            return idx;
        }

        if !self.is_contextual_identifier_at(idx) {
            return idx;
        }
        idx += 1;
        while matches!(self.peek_kind_at(idx), Some(TokenKind::Dot))
            && self.is_contextual_identifier_at(idx + 1)
        {
            idx += 2;
        }

        while matches!(self.peek_kind_at(idx), Some(TokenKind::LBracket)) {
            let mut depth = 0usize;
            loop {
                match self.peek_kind_at(idx) {
                    Some(TokenKind::LBracket) => depth += 1,
                    Some(TokenKind::RBracket) => {
                        depth = depth.saturating_sub(1);
                        idx += 1;
                        if depth == 0 {
                            break;
                        }
                        continue;
                    }
                    Some(_) => {}
                    None => return idx,
                }
                idx += 1;
            }
        }

        if matches!(self.peek_kind_at(idx), Some(TokenKind::Question)) {
            idx += 1;
        }

        idx
    }

    fn parse_optional_for_mode(&mut self) -> Result<Option<ReceiverKind>> {
        if self.eat_simple(&TokenKind::KwOwn).is_some() {
            return Ok(Some(ReceiverKind::Value));
        }
        if self.eat_simple(&TokenKind::KwMut).is_some() {
            return Ok(Some(ReceiverKind::BorrowMut));
        }
        Ok(None)
    }

    fn parse_match_capability(&mut self) -> Result<ReceiverKind> {
        if self.eat_simple(&TokenKind::KwOwn).is_some() {
            return Ok(ReceiverKind::Value);
        }
        if self.eat_simple(&TokenKind::KwMut).is_some() {
            return Ok(ReceiverKind::BorrowMut);
        }
        Ok(ReceiverKind::Borrow)
    }

    fn starts_specialization_suffix(&self, expr: &Expr) -> bool {
        let mut idx = self.index + 1;
        loop {
            let next = self.skip_type_tokens(idx);
            if next == idx {
                return false;
            }
            idx = next;
            if matches!(self.peek_kind_at(idx), Some(TokenKind::Comma)) {
                idx += 1;
                continue;
            }
            break;
        }

        if !matches!(self.peek_kind_at(idx), Some(TokenKind::RBracket)) {
            return false;
        }

        match self.peek_kind_at(idx + 1) {
            Some(TokenKind::LParen) => {
                matches!(expr.kind, ExprKind::Name(_) | ExprKind::Member { .. })
            }
            Some(TokenKind::Dot) => specialization_target_name(expr)
                .map(is_static_specialization_target_name)
                .unwrap_or(false),
            _ => false,
        }
    }

    fn skip_bracketed_tokens(&self, start_idx: usize) -> Option<usize> {
        let mut idx = start_idx;
        let mut depth = 0usize;
        loop {
            match self.peek_kind_at(idx) {
                Some(TokenKind::LBracket) => depth += 1,
                Some(TokenKind::RBracket) => {
                    depth = depth.saturating_sub(1);
                    idx += 1;
                    if depth == 0 {
                        return Some(idx);
                    }
                    continue;
                }
                Some(_) => {}
                None => return None,
            }
            idx += 1;
        }
    }

    fn at_copy_class_start(&self) -> bool {
        matches!(
            (self.current_kind(), self.peek_kind_at(self.index + 1)),
            (TokenKind::Identifier(name), Some(TokenKind::KwClass)) if name == "copy"
        )
    }

    fn parse_format_parts(&mut self, value: &str, span: Span) -> Result<Vec<FormatPart>> {
        let mut parts = Vec::new();
        let chars = value.char_indices().collect::<Vec<_>>();
        let mut index = 0usize;
        let mut literal = String::new();

        while index < chars.len() {
            let (offset, ch) = chars[index];
            if ch == '{' && matches!(chars.get(index + 1), Some((_, '{'))) {
                literal.push('{');
                index += 2;
                continue;
            }
            if ch == '}' && matches!(chars.get(index + 1), Some((_, '}'))) {
                literal.push('}');
                index += 2;
                continue;
            }
            if ch != '{' {
                literal.push(ch);
                index += 1;
                continue;
            }

            if !literal.is_empty() {
                parts.push(FormatPart::Literal(std::mem::take(&mut literal)));
            }

            let expr_start = offset + ch.len_utf8();
            index += 1;
            let mut expr_end = None;
            let mut brace_depth = 0usize;
            let mut string_quote = None;
            let mut triple_quote = None;
            let mut escaped = false;
            while index < chars.len() {
                let (candidate_offset, candidate) = chars[index];
                if let Some(quote) = triple_quote {
                    if candidate == quote
                        && matches!(chars.get(index + 1), Some((_, next)) if *next == quote)
                        && matches!(chars.get(index + 2), Some((_, next)) if *next == quote)
                    {
                        triple_quote = None;
                        index += 3;
                    } else {
                        index += 1;
                    }
                    continue;
                }
                if let Some(quote) = string_quote {
                    if escaped {
                        escaped = false;
                    } else if candidate == '\\' {
                        escaped = true;
                    } else if candidate == quote {
                        string_quote = None;
                    }
                    index += 1;
                    continue;
                }
                match candidate {
                    '"' | '\''
                        if matches!(chars.get(index + 1), Some((_, next)) if *next == candidate)
                            && matches!(chars.get(index + 2), Some((_, next)) if *next == candidate) =>
                    {
                        triple_quote = Some(candidate);
                        index += 3;
                        continue;
                    }
                    '"' | '\'' => string_quote = Some(candidate),
                    '{' => brace_depth += 1,
                    '}' if brace_depth == 0 => {
                        expr_end = Some(candidate_offset);
                        break;
                    }
                    '}' => brace_depth -= 1,
                    _ => {}
                }
                index += 1;
            }

            let Some(expr_end) = expr_end else {
                return Err(parse_error(span, "unterminated f-string interpolation"));
            };
            let raw_expr_text = &value[expr_start..expr_end];
            let leading_ws = raw_expr_text.len() - raw_expr_text.trim_start().len();
            let expr_text = raw_expr_text.trim();
            if expr_text.is_empty() {
                return Err(parse_error(span, "f-string interpolation cannot be empty"));
            }
            let complete_parse =
                parse_expression_with_recursion_depth(expr_text, self.recursion_depth);
            let (mut expr, format_spec) = match complete_parse {
                Ok(expr) => (expr, None),
                Err(complete_error) => {
                    let mut parsed = None;
                    for colon in top_level_format_colons(expr_text).into_iter().rev() {
                        let expression_source = expr_text[..colon].trim_end();
                        if expression_source.is_empty() {
                            continue;
                        }
                        if let Ok(expr) = parse_expression_with_recursion_depth(
                            expression_source,
                            self.recursion_depth,
                        ) {
                            let spec = &expr_text[colon + 1..];
                            if spec.contains(['{', '}']) {
                                return Err(parse_error(
                                    Span::new(span.line, span.column + expr_start + colon + 2),
                                    "f-string format specifications cannot contain nested replacement fields",
                                ));
                            }
                            parsed = Some((expr, colon, spec.to_string()));
                            break;
                        }
                    }
                    let Some((expr, colon, spec)) = parsed else {
                        return Err(parse_error(
                            span,
                            format!(
                                "invalid f-string interpolation `{}`: {}",
                                expr_text, complete_error
                            ),
                        ));
                    };
                    (expr, Some((colon, spec)))
                }
            };
            let column_offset = span.column + expr_start + leading_ws + 1;
            offset_expr_span(&mut expr, span.line, column_offset);
            if let Some((colon, spec)) = format_spec {
                parts.push(FormatPart::Formatted {
                    expr,
                    spec,
                    spec_span: Span::new(span.line, span.column + expr_start + colon + 2),
                });
            } else {
                parts.push(FormatPart::Expr(expr));
            }
            index += 1;
        }

        if !literal.is_empty() {
            parts.push(FormatPart::Literal(literal));
        }

        Ok(parts)
    }

    fn skip_newlines(&mut self) {
        while self.at_simple(&TokenKind::Newline) {
            self.bump();
        }
    }

    fn expect_keyword(&mut self, kind: TokenKind) -> Result<Token> {
        let token = self.bump();
        if token.kind == kind {
            Ok(token)
        } else {
            Err(parse_error(
                token.span,
                format!("expected {:?}, found {:?}", kind, token.kind),
            ))
        }
    }

    fn expect_simple(&mut self, kind: TokenKind) -> Result<Token> {
        self.expect_keyword(kind)
    }

    fn expect_newline(&mut self) -> Result<Token> {
        self.expect_simple(TokenKind::Newline)
    }

    fn expect_statement_terminator(&mut self) -> Result<()> {
        if self.eat_simple(&TokenKind::Newline).is_some()
            || self.at_simple(&TokenKind::Dedent)
            || matches!(
                self.tokens
                    .get(self.index.saturating_sub(1))
                    .map(|token| &token.kind),
                Some(TokenKind::Dedent)
            )
            || self.at_eof()
        {
            Ok(())
        } else {
            Err(self.error_here("expected Newline"))
        }
    }

    fn expect_match_expr_arm_terminator(&mut self) -> Result<()> {
        if self.eat_simple(&TokenKind::Newline).is_some()
            || self.at_simple(&TokenKind::Dedent)
            || matches!(
                self.tokens
                    .get(self.index.saturating_sub(1))
                    .map(|token| &token.kind),
                Some(TokenKind::Dedent)
            )
            || self.at_eof()
        {
            Ok(())
        } else {
            Err(self.error_here("expected Newline"))
        }
    }

    fn expect_identifier(&mut self) -> Result<String> {
        self.expect_identifier_with_span().map(|(name, _)| name)
    }

    fn expect_identifier_with_span(&mut self) -> Result<(String, Span)> {
        let token = self.bump();
        let span = token.span;
        match token.kind {
            TokenKind::Identifier(name) => Ok((name, span)),
            TokenKind::KwFrom => Ok(("from".to_string(), span)),
            _ => Err(parse_error(span, "expected identifier")),
        }
    }

    fn expect_member_name(&mut self) -> Result<String> {
        let token = self.bump();
        match token.kind {
            TokenKind::Identifier(name) => Ok(name),
            TokenKind::KwFrom => Ok("from".to_string()),
            other => Err(parse_error(
                token.span,
                format!("expected member name, found {:?}", other),
            )),
        }
    }

    fn eat_simple(&mut self, kind: &TokenKind) -> Option<Token> {
        if self.at_simple(kind) {
            Some(self.bump())
        } else {
            None
        }
    }

    fn at_simple(&self, kind: &TokenKind) -> bool {
        self.current_kind() == kind
    }

    fn at_keyword_class(&self) -> bool {
        self.at_simple(&TokenKind::KwClass)
    }

    fn at_keyword_enum(&self) -> bool {
        self.at_simple(&TokenKind::KwEnum)
    }

    fn at_keyword_def(&self) -> bool {
        self.at_simple(&TokenKind::KwDef)
    }

    fn at_keyword_extern(&self) -> bool {
        self.at_simple(&TokenKind::KwExtern)
    }

    fn at_keyword_trait(&self) -> bool {
        self.at_simple(&TokenKind::KwTrait)
    }

    fn at_keyword_impl(&self) -> bool {
        self.at_simple(&TokenKind::KwImpl)
    }

    fn at_keyword_import(&self) -> bool {
        self.at_simple(&TokenKind::KwImport)
    }

    fn at_keyword_from(&self) -> bool {
        self.at_simple(&TokenKind::KwFrom)
    }

    fn at_from_import_start(&self) -> bool {
        if !self.at_keyword_from() {
            return false;
        }

        let mut index = self.index + 1;
        if !self.is_contextual_identifier_at(index) {
            return false;
        }
        index += 1;
        while matches!(self.peek_kind_at(index), Some(TokenKind::Dot))
            && self.is_contextual_identifier_at(index + 1)
        {
            index += 2;
        }
        matches!(self.peek_kind_at(index), Some(TokenKind::KwImport))
    }

    fn is_contextual_identifier_at(&self, index: usize) -> bool {
        matches!(
            self.peek_kind_at(index),
            Some(TokenKind::Identifier(_) | TokenKind::KwFrom)
        )
    }

    fn at_eof(&self) -> bool {
        self.at_simple(&TokenKind::Eof)
    }

    fn current_kind(&self) -> &TokenKind {
        &self.tokens[self.index].kind
    }

    fn current_span(&self) -> Span {
        self.tokens[self.index].span
    }

    fn peek_kind(&self, offset: usize) -> Option<&TokenKind> {
        self.tokens
            .get(self.index + offset)
            .map(|token| &token.kind)
    }

    fn peek_kind_at(&self, index: usize) -> Option<&TokenKind> {
        self.tokens.get(index).map(|token| &token.kind)
    }

    fn bump(&mut self) -> Token {
        let token = self.tokens[self.index].clone();
        self.index += 1;
        token
    }

    fn at_match_expr_end(&self) -> bool {
        self.at_simple(&TokenKind::Dedent)
    }

    fn error_here(&self, message: impl Into<String>) -> Diagnostic {
        parse_error(self.current_span(), message)
    }
}

/// Finds colons that are outside every nested expression delimiter. Parsing
/// the complete interpolation is attempted first, so slice and dictionary
/// colons remain expression syntax whenever the complete text is valid Aura.
fn top_level_format_colons(source: &str) -> Vec<usize> {
    let mut colons = Vec::new();
    let mut delimiters = Vec::new();
    let mut quote = None;
    let mut escaped = false;
    for (offset, ch) in source.char_indices() {
        if let Some(active_quote) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == active_quote {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '(' | '[' | '{' => delimiters.push(ch),
            ')' | ']' | '}' => {
                delimiters.pop();
            }
            ':' if delimiters.is_empty() => colons.push(offset),
            _ => {}
        }
    }
    colons
}

fn specialization_target_name(expr: &Expr) -> Option<&str> {
    match &expr.kind {
        ExprKind::Name(name) => Some(name.as_str()),
        ExprKind::Member { field, .. } => Some(field.as_str()),
        _ => None,
    }
}

fn is_static_specialization_target_name(name: &str) -> bool {
    matches!(name, "list" | "dict" | "set")
        || name
            .chars()
            .next()
            .map(|ch| ch.is_ascii_uppercase())
            .unwrap_or(false)
}

fn assign_target_to_expr(target: AssignTarget, span: Span) -> Expr {
    match target {
        AssignTarget::Name(name) => Expr {
            kind: ExprKind::Name(name),
            span,
        },
        AssignTarget::Member { object, field } => Expr {
            kind: ExprKind::Member { object, field },
            span,
        },
        AssignTarget::Index { object, index } => Expr {
            kind: ExprKind::Index { object, index },
            span,
        },
    }
}

fn offset_expr_span(expr: &mut Expr, line: usize, column_offset: usize) {
    expr.span.line = line;
    expr.span.column += column_offset;

    match &mut expr.kind {
        ExprKind::Unary { expr: inner, .. } | ExprKind::Try(inner) | ExprKind::Group(inner) => {
            offset_expr_span(inner, line, column_offset)
        }
        ExprKind::Cast { expr: inner, .. } => offset_expr_span(inner, line, column_offset),
        ExprKind::Binary { left, right, .. } => {
            offset_expr_span(left, line, column_offset);
            offset_expr_span(right, line, column_offset);
        }
        ExprKind::Conditional {
            then_expr,
            condition,
            else_expr,
        } => {
            offset_expr_span(then_expr, line, column_offset);
            offset_expr_span(condition, line, column_offset);
            offset_expr_span(else_expr, line, column_offset);
        }
        ExprKind::Lambda { params, body, .. } => {
            for param in params {
                param.span.line = line;
                param.span.column += column_offset;
            }
            offset_expr_span(body, line, column_offset);
        }
        ExprKind::Membership {
            value,
            container,
            operator_span,
            ..
        } => {
            operator_span.line = line;
            operator_span.column += column_offset;
            offset_expr_span(value, line, column_offset);
            offset_expr_span(container, line, column_offset);
        }
        ExprKind::CompareChain { first, links } => {
            offset_expr_span(first, line, column_offset);
            for link in links {
                link.op_span.line = line;
                link.op_span.column += column_offset;
                offset_expr_span(&mut link.operand, line, column_offset);
            }
        }
        ExprKind::Tuple(elements) | ExprKind::List(elements) => {
            for element in elements {
                offset_expr_span(element, line, column_offset);
            }
        }
        ExprKind::Set(elements) => {
            for element in elements {
                offset_expr_span(element, line, column_offset);
            }
        }
        ExprKind::Map(entries) => {
            for entry in entries {
                offset_expr_span(&mut entry.key, line, column_offset);
                offset_expr_span(&mut entry.value, line, column_offset);
            }
        }
        ExprKind::Comprehension { output, clauses } => {
            match output {
                ComprehensionOutput::List(element) | ComprehensionOutput::Set(element) => {
                    offset_expr_span(element, line, column_offset);
                }
                ComprehensionOutput::Map { key, value } => {
                    offset_expr_span(key, line, column_offset);
                    offset_expr_span(value, line, column_offset);
                }
            }
            for clause in clauses {
                clause.span.line = line;
                clause.span.column += column_offset;
                offset_binding_target_span(&mut clause.target, line, column_offset);
                offset_expr_span(&mut clause.iterable, line, column_offset);
                for filter in &mut clause.filters {
                    offset_expr_span(filter, line, column_offset);
                }
            }
        }
        ExprKind::Call { callee, args } => {
            offset_expr_span(callee, line, column_offset);
            for argument in args {
                argument.span.line = line;
                argument.span.column += column_offset;
                offset_expr_span(&mut argument.value, line, column_offset);
            }
        }
        ExprKind::Specialize {
            expr: inner,
            type_args,
        } => {
            offset_expr_span(inner, line, column_offset);
            for type_arg in type_args {
                offset_type_ref_span(type_arg, line, column_offset);
            }
        }
        ExprKind::Member { object, .. } => offset_expr_span(object, line, column_offset),
        ExprKind::Index { object, index } => {
            offset_expr_span(object, line, column_offset);
            offset_expr_span(index, line, column_offset);
        }
        ExprKind::Slice {
            object,
            start,
            end,
            colon_span,
        } => {
            offset_expr_span(object, line, column_offset);
            if let Some(start) = start {
                offset_expr_span(start, line, column_offset);
            }
            if let Some(end) = end {
                offset_expr_span(end, line, column_offset);
            }
            colon_span.line = line;
            colon_span.column += column_offset;
        }
        ExprKind::Match {
            scrutinee, arms, ..
        } => {
            offset_expr_span(scrutinee, line, column_offset);
            for arm in arms {
                arm.span.line = line;
                arm.span.column += column_offset;
                offset_expr_span(&mut arm.value, line, column_offset);
            }
        }
        ExprKind::Name(_)
        | ExprKind::Int(_)
        | ExprKind::DurationNanos(_)
        | ExprKind::BuiltinOmitted
        | ExprKind::Float(_)
        | ExprKind::Bool(_)
        | ExprKind::String(_) => {}
        ExprKind::FString(parts) => {
            for part in parts {
                match part {
                    FormatPart::Expr(inner) | FormatPart::Formatted { expr: inner, .. } => {
                        offset_expr_span(inner, line, column_offset);
                    }
                    FormatPart::Literal(_) => {}
                }
            }
        }
    }
}

fn offset_binding_target_span(target: &mut BindingTarget, line: usize, column_offset: usize) {
    match target {
        BindingTarget::Name { span, .. } => {
            span.line = line;
            span.column += column_offset;
        }
        BindingTarget::Tuple { elements, span } => {
            span.line = line;
            span.column += column_offset;
            for element in elements {
                offset_binding_target_span(element, line, column_offset);
            }
        }
    }
}

fn looks_like_lambda_type_annotation(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Name(name) => {
            name.chars().next().is_some_and(char::is_uppercase)
                || matches!(
                    name.as_str(),
                    "bool"
                        | "int"
                        | "int8"
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
        }
        ExprKind::Specialize { expr, .. } | ExprKind::Group(expr) => {
            looks_like_lambda_type_annotation(expr)
        }
        ExprKind::Index { object, index } => {
            matches!(
                &object.kind,
                ExprKind::Name(name) if name.chars().next().is_some_and(char::is_uppercase)
            ) && looks_like_lambda_type_annotation(index)
        }
        ExprKind::Slice { .. } => false,
        ExprKind::Tuple(elements) => elements.iter().all(looks_like_lambda_type_annotation),
        _ => false,
    }
}

fn offset_type_ref_span(type_ref: &mut TypeRef, line: usize, column_offset: usize) {
    type_ref.span.line = line;
    type_ref.span.column += column_offset;
    match &mut type_ref.kind {
        TypeRefKind::Named { args, .. } | TypeRefKind::Tuple(args) => {
            for arg in args {
                offset_type_ref_span(arg, line, column_offset);
            }
        }
        TypeRefKind::Function {
            params,
            return_type,
        } => {
            for param in params {
                param.span.line = line;
                param.span.column += column_offset;
                offset_type_ref_span(&mut param.ty, line, column_offset);
            }
            offset_type_ref_span(return_type, line, column_offset);
        }
    }
}

#[cfg(test)]
#[path = "parser_tests.rs"]
mod tests;
