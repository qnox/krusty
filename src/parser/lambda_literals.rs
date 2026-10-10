//! Lambda literals: the `{ [params ->] statements }` body, and the label kotlinc gives one.
//!
//! kotlinc's raw-FIR builder names a lambda literal after the innermost call it is written in
//! (`calleeNamesForLambda`): a lambda in a call's argument list, its trailing lambda, or an infix
//! call's operand takes the call's name, and a call whose callee is not a simple name is `invoke`.
//! A property initializer, an assignment and a non-infix binary operator enter a scope with no
//! name, so a lambda there has none; `if`, `when`, `try`, parentheses, casts and a lambda's own
//! body pass the enclosing name through. A label written on the literal replaces the call's.

use super::*;

/// The open naming scopes, innermost last, and each lambda literal parsed so far with the depth it
/// took its name at.
#[derive(Default)]
pub(super) struct LambdaLabelScopes {
    scopes: Vec<Option<String>>,
    lambdas: Vec<(ExprId, usize)>,
}

/// Where a binary operator's left operand began: the operator's scope covers that operand too,
/// though the parser meets the operator only after the operand.
#[derive(Clone, Copy)]
pub(super) struct LambdaLabelMark(usize);

/// A lambda whose name a binary operator already settled; an enclosing operator leaves it alone.
const SETTLED: usize = usize::MAX;

impl Parser<'_> {
    /// Parse under a naming scope: `name` for a call, `None` for a scope that names nothing.
    pub(super) fn with_lambda_label<T>(
        &mut self,
        name: Option<String>,
        parse: impl FnOnce(&mut Self) -> T,
    ) -> T {
        self.lambda_label_scopes.scopes.push(name);
        let parsed = parse(self);
        self.lambda_label_scopes.scopes.pop();
        parsed
    }

    /// An initializer or assigned value: a lambda written there has no name.
    pub(super) fn parse_unlabelled_expr(&mut self) -> ExprId {
        self.with_lambda_label(None, Self::parse_expr)
    }

    /// The name a call gives the lambdas in its arguments.
    pub(super) fn lambda_label_of_callee(&self, callee: ExprId) -> String {
        if self.parenthesized_expressions.contains(&callee.0) {
            return "invoke".to_string();
        }
        match self.file.expr(callee) {
            Expr::Name(name)
            | Expr::Member { name, .. }
            | Expr::SafeCall {
                name, args: None, ..
            } => name.clone(),
            _ => "invoke".to_string(),
        }
    }

    /// The name a trailing lambda takes: the call it completes, or `invoke` when it calls the
    /// value of a finished call (`(f()) { … }`, `f { … } { … }`).
    pub(super) fn trailing_lambda_label(&self, callee: ExprId) -> String {
        let completes = !self.parenthesized_expressions.contains(&callee.0)
            && !self.file.call_has_trailing_lambda.contains(&callee.0);
        match self.file.expr(callee) {
            Expr::Call { callee: inner, .. } if completes => self.lambda_label_of_callee(*inner),
            Expr::SafeCall { name, .. } if completes => name.clone(),
            Expr::Call { .. } | Expr::SafeCall { .. } => "invoke".to_string(),
            _ => self.lambda_label_of_callee(callee),
        }
    }

    pub(super) fn lambda_label_mark(&self) -> LambdaLabelMark {
        LambdaLabelMark(self.lambda_label_scopes.lambdas.len())
    }

    /// Give the lambdas of a binary operator's already-parsed left operand the operator's name.
    pub(super) fn relabel_left_operand(&mut self, mark: LambdaLabelMark, name: Option<&str>) {
        let depth = self.lambda_label_scopes.scopes.len();
        for (lambda, taken_at) in &mut self.lambda_label_scopes.lambdas[mark.0..] {
            if *taken_at != depth {
                continue;
            }
            *taken_at = SETTLED;
            match name {
                Some(name) => self
                    .file
                    .lambda_call_labels
                    .insert(lambda.0, name.to_owned()),
                None => self.file.lambda_call_labels.remove(&lambda.0),
            };
        }
    }

    pub(super) fn parse_lambda(&mut self) -> ExprId {
        let lambda = self.parse_lambda_literal();
        let scopes = &self.lambda_label_scopes.scopes;
        if let Some(Some(name)) = scopes.last() {
            self.file.lambda_call_labels.insert(lambda.0, name.clone());
        }
        let depth = scopes.len();
        self.lambda_label_scopes.lambdas.push((lambda, depth));
        lambda
    }

    /// Parse a lambda literal `{ [param ->] stmts }` (single optional parameter; the body is a block).
    fn parse_lambda_literal(&mut self) -> ExprId {
        let start = self.tok().span;
        self.expect(TokenKind::LBrace, "'{'");
        self.skip_newlines();
        // Optional parameter list ending in `->`: `it ->`, `x: T ->`, `a, b ->` (types discarded; the
        // parameter types come from the declared function type via `check_lambda_with_types`). Detect
        // by scanning for a top-level `->` before the lambda's closing `}`.
        let has_params = self.lambda_arrow_before_close(self.i);
        // Parameter type annotations, parallel to `params` — kept (in a side-table) so a bare-value
        // lambda `{ x: Int -> … }` types its own parameters even without an expected function type.
        let mut param_types: Vec<Option<TypeRef>> = Vec::new();
        let mut param_spans: Vec<Span> = Vec::new();
        // A destructured lambda parameter `{ (a, b) -> … }` binds ONE (synthetic) parameter, then
        // `val (a, b) = <synthetic>` is prepended to the body — reusing the `Stmt::Destructure`
        // machinery. Collected here, spliced after the body statements are parsed.
        // (synthetic param name, destructured entries `(name, is_var)`, span, parenthesized short
        // form) per `(a, b)` param.
        type LambdaDestructure = (
            String,
            Vec<DestructureEntry>,
            Vec<Option<DestructureProperty>>,
            Vec<Option<TypeRef>>,
            Span,
            bool,
        );
        let mut destructures: Vec<LambdaDestructure> = Vec::new();
        // How each parameter was written, parallel to `params`.
        let mut roles: Vec<LambdaParameterRole> = Vec::new();
        let params = if has_params {
            let mut ps = Vec::new();
            loop {
                self.skip_newlines();
                if self.at(TokenKind::LParen) {
                    let sp = self.tok().span;
                    self.bump();
                    let mut entries = Vec::new();
                    let mut source_props: Vec<Option<DestructureProperty>> = Vec::new();
                    let mut entry_types: Vec<Option<TypeRef>> = Vec::new();
                    // `{ (a, b) -> … }` is the parenthesized short form until an entry carries
                    // `val`/`var`.
                    let mut parenthesized_short = true;
                    loop {
                        // Full form (`{ (val a, val b) -> … }`): each component carries its own
                        // `val`/`var` and binds by property name. Keyword-less short form is positional
                        // unless the short-form flag is on.
                        let is_var = self.at(TokenKind::KwVar);
                        let had_kw = is_var || self.at(TokenKind::KwVal);
                        if had_kw {
                            self.bump();
                            parenthesized_short = false;
                        }
                        let ignored = self.at(TokenKind::Ident)
                            && self.text() == "_"
                            && !self.escaped_ident();
                        let name_span = self.syntactic_ident_span(self.tok());
                        let n = self.ident_or_error("variable name");
                        let mut entry_type = self.eat(TokenKind::Colon).then(|| self.parse_type());
                        // By-name entry (`(a = prop) ->`) or short-form (`(a, b) ->` binds by own name).
                        let implicit = self.short_form_destructuring || had_kw;
                        let source = self.destructure_property(&n, implicit, &mut entry_type);
                        entries.push(DestructureEntry {
                            name: n,
                            name_span,
                            mutable: is_var,
                            ignored,
                        });
                        source_props.push(source);
                        entry_types.push(entry_type);
                        if !self.eat(TokenKind::Comma) {
                            break;
                        }
                        if self.at(TokenKind::RParen) {
                            break; // trailing comma
                        }
                    }
                    self.expect(TokenKind::RParen, "')'");
                    // `(a, b): T ->` declares the destructured parameter's own type.
                    let synth = format!("$dstr{}", destructures.len());
                    ps.push(synth.clone());
                    param_spans.push(sp);
                    param_types.push(self.eat(TokenKind::Colon).then(|| self.parse_type()));
                    roles.push(LambdaParameterRole::Destructured);
                    destructures.push((
                        synth,
                        entries,
                        source_props,
                        entry_types,
                        sp,
                        parenthesized_short,
                    ));
                } else if self.at(TokenKind::LBracket) {
                    self.gate_bracket_destructuring();
                    // The short-form bracket destructuring `{ [a, b] -> … }` (NameBasedDestructuring) —
                    // identical to the `(a, b)` form, just with `[ ]`.
                    let sp = self.tok().span;
                    self.bump();
                    let mut entries = Vec::new();
                    let mut entry_types: Vec<Option<TypeRef>> = Vec::new();
                    loop {
                        // Full-form bracket (`{ [val a, val b] -> … }`) — component keyword optional,
                        // positional either way.
                        let is_var = self.at(TokenKind::KwVar);
                        if is_var || self.at(TokenKind::KwVal) {
                            self.bump();
                        }
                        let ignored = self.at(TokenKind::Ident)
                            && self.text() == "_"
                            && !self.escaped_ident();
                        let name_span = self.syntactic_ident_span(self.tok());
                        let n = self.ident_or_error("variable name");
                        let entry_type = self.eat(TokenKind::Colon).then(|| self.parse_type());
                        entries.push(DestructureEntry {
                            name: n,
                            name_span,
                            mutable: is_var,
                            ignored,
                        });
                        entry_types.push(entry_type);
                        if !self.eat(TokenKind::Comma) {
                            break;
                        }
                        if self.at(TokenKind::RBracket) {
                            break; // trailing comma
                        }
                    }
                    self.expect(TokenKind::RBracket, "']'");
                    let synth = format!("$dstr{}", destructures.len());
                    ps.push(synth.clone());
                    param_spans.push(sp);
                    param_types.push(self.eat(TokenKind::Colon).then(|| self.parse_type()));
                    roles.push(LambdaParameterRole::Destructured);
                    // The `[a, b]` bracket form is positional (`componentN`), never by-name.
                    let source_props = vec![None; entries.len()];
                    destructures.push((synth, entries, source_props, entry_types, sp, false));
                } else if self.at(TokenKind::Ident) {
                    let parameter_span = self.tok().span;
                    roles.push(if self.text() == "_" && !self.escaped_ident() {
                        LambdaParameterRole::Unused
                    } else {
                        LambdaParameterRole::Named
                    });
                    ps.push(self.text().to_string());
                    param_spans.push(parameter_span);
                    self.bump();
                    if self.at(TokenKind::Colon) {
                        self.bump();
                        param_types.push(Some(self.parse_type()));
                    } else {
                        param_types.push(None);
                    }
                }
                if self.at(TokenKind::Comma) {
                    self.bump();
                    continue;
                }
                break;
            }
            self.expect(TokenKind::Arrow, "'->'");
            ps
        } else {
            Vec::new()
        };
        let mut stmts = Vec::new();
        let saved_block_value = self.block_trailing_is_value;
        self.block_trailing_is_value = true;
        loop {
            self.skip_newlines();
            if self.at(TokenKind::RBrace) || self.at(TokenKind::Eof) {
                break;
            }
            stmts.push(self.parse_stmt());
        }
        self.block_trailing_is_value = saved_block_value;
        // Prepend `val (a, b) = <synthetic-param>` for each destructured parameter (reversed so the
        // first parameter's binding ends up first).
        for (synth, entries, source_props, entry_types, sp, parenthesized_short) in
            destructures.into_iter().rev()
        {
            let init = self.file.add_expr(Expr::Name(synth), sp);
            let d = self.file.add_stmt(Stmt::Destructure { entries, init }, sp);
            self.file.destructuring.lambda_parameters.insert(d);
            if parenthesized_short {
                self.file.destructuring.parenthesized_short_form.insert(d);
            }
            if source_props.iter().any(|s| s.is_some()) {
                self.file
                    .destructuring
                    .source_properties
                    .insert(d.0, source_props);
            }
            if entry_types.iter().any(Option::is_some) {
                self.file.destructuring.entry_types.insert(d.0, entry_types);
            }
            stmts.insert(0, d);
        }
        let end = self.tok().span;
        self.expect(TokenKind::RBrace, "'}'");
        let mut trailing = None;
        if let Some(&last) = stmts.last() {
            if let Stmt::Expr(e) = self.file.stmt(last) {
                trailing = Some(*e);
                stmts.pop();
            }
        }
        let body = self
            .file
            .add_expr(Expr::Block { stmts, trailing }, Span::new(start.lo, end.hi));
        let lam = self
            .file
            .add_expr(Expr::Lambda { params, body }, Span::new(start.lo, end.hi));
        if has_params {
            self.file.lambda_explicit_arrows.insert(lam.0);
        }
        if param_types.iter().any(|t| t.is_some()) {
            self.file.lambda_param_types.insert(lam.0, param_types);
        }
        if !param_spans.is_empty() {
            self.file.lambda_param_spans.insert(lam.0, param_spans);
        }
        if roles.iter().any(|role| *role != LambdaParameterRole::Named) {
            self.file.lambda_parameter_roles.insert(lam.0, roles);
        }
        lam
    }
}
