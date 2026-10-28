//! The statement parser: the depth-guarded statement entry and the dispatch from a statement's
//! leading tokens to a declaration, a loop, an assignment, an increment, or an expression
//! statement. An increment keeps its expression value immediately before the closing brace of a
//! value-consuming block and as the bare body of an `if`/`when` branch; everywhere else it takes
//! the statement path.

use super::*;

impl Parser<'_> {
    /// Depth-guarded entry for the statement parser. Nested statement structures — block bodies
    /// (`while (…) { while (…) { … } }`), local declarations — recurse `parse_stmt` →
    /// `parse_block_expr` → `parse_stmt` without ever passing through `parse_bp`, so they need
    /// their own bounded, stack-grown funnel. Past [`EXPR_DEPTH_LIMIT`] the statement degrades to
    /// an error statement with a diagnostic and a balanced skip — degrade, never crash — leaving
    /// one closer for each enclosing block frame.
    pub(super) fn parse_stmt(&mut self) -> StmtId {
        self.with_nesting_guard(
            ParserNesting::Statement,
            |parser, span| {
                let expression = parser
                    .file
                    .add_expr(Expr::Name("<error>".to_string()), span);
                parser.finish_stmt(Stmt::Expr(expression), span)
            },
            Self::parse_stmt_inner,
        )
    }

    fn parse_stmt_inner(&mut self) -> StmtId {
        let bare_branch_value = std::mem::take(&mut self.bare_branch_value);
        // Labels may prefix declarations as well as loops/lambdas. Keep the declaration as its real
        // statement node and retain the label in the file's sparse source metadata.
        if self.at(TokenKind::Ident)
            && self
                .t
                .get(self.i + 1)
                .is_some_and(|token| token.kind == TokenKind::At)
        {
            let after = self.after_plain_newlines(self.i + 2);
            let declaration_follows = self.t.get(after).is_some_and(|token| {
                matches!(
                    token.kind,
                    TokenKind::KwVal | TokenKind::KwVar | TokenKind::KwFun
                ) || self.token_keyword_text(*token, "typealias")
            });
            if declaration_follows {
                let label = self.text().to_string();
                let label_span = self.tok().span;
                self.bump(); // label
                self.bump(); // '@'
                self.skip_plain_newlines();
                let statement = self.parse_stmt();
                self.file
                    .statement_labels
                    .insert(statement, (label, label_span));
                return statement;
            }
        }
        // Labeled loop: `l1@ while(…)` / `l1@ for(…)` / `l1@ do {…}`. Capture the label and thread it
        // onto the loop so `break@l1`/`continue@l1` can target it.
        let mut loop_label: Option<String> = None;
        if self.at(TokenKind::Ident) {
            let next1 = self.t.get(self.i + 1);
            let mut loop_token = self.i + 2;
            while self
                .t
                .get(loop_token)
                .is_some_and(|token| token.kind == TokenKind::Newline)
            {
                loop_token += 1;
            }
            let is_label = next1.is_some_and(|t| t.kind == TokenKind::At)
                && self.t.get(loop_token).is_some_and(|t| {
                    matches!(
                        t.kind,
                        TokenKind::KwWhile | TokenKind::KwFor | TokenKind::KwDo
                    )
                });
            if is_label {
                loop_label = Some(self.text().to_string());
                self.bump(); // label name
                self.bump(); // '@'
                self.skip_newlines();
            }
        }
        // A modifier-prefixed LOCAL function (`tailrec fun f(…)`, `suspend fun g(…)`) is a
        // declaration statement — without the prefix scan the leading soft keyword parses as an
        // expression name and reports itself unresolved.
        if matches!(self.kind(), TokenKind::Ident | TokenKind::At)
            && self.local_declaration_after_prefix() == Some(TokenKind::KwFun)
        {
            let start = self.tok().span;
            let mods = self.parse_member_decl_prefix();
            let function = self.parse_fun(&mods);
            return self.finish_stmt(Stmt::LocalFun(function), start);
        }
        // Statement annotations have no codegen representation, but `@Suppress` and opt-in
        // acceptance are scoped frontend policies. Retain what they read on the transient statement.
        if self.at(TokenKind::At) {
            let mut annotations = Vec::new();
            while self.at(TokenKind::At) {
                let (annotation, arguments) = self.parse_annotation();
                if let Some(annotation) = annotation {
                    annotations.push(UseSiteAnnotation {
                        annotation,
                        arguments,
                    });
                }
                self.skip_newlines();
            }
            // The annotated statement is the one a bare branch body holds.
            self.bare_branch_value = bare_branch_value;
            let statement = self.parse_stmt();
            if !annotations.is_empty() {
                self.file
                    .statement_annotations
                    .insert(statement, annotations);
            }
            return statement;
        }
        if self.at(TokenKind::Ident)
            && self.keyword_text("lateinit")
            && self
                .t
                .get(self.i + 1)
                .is_some_and(|token| token.kind == TokenKind::KwVar)
        {
            let start = self.tok().span;
            self.bump();
            self.bump();
            let name = self.ident_or_error("variable name");
            self.expect(TokenKind::Colon, "':'");
            let ty = self.parse_type();
            return self.finish_stmt(Stmt::LocalLateinit { name, ty }, start);
        }
        if self.at(TokenKind::Ident)
            && self.keyword_text("context")
            && self.local_declaration_after_prefix() == Some(TokenKind::KwFun)
        {
            let start = self.tok().span;
            let mods = self.parse_member_decl_prefix();
            let function = self.parse_fun(&mods);
            return self.finish_stmt(Stmt::LocalFun(function), start);
        }
        let start = self.tok().span;
        match self.kind() {
            TokenKind::KwVal | TokenKind::KwVar => self.parse_local_binding(start),
            TokenKind::KwReturn => self.parse_return_statement(start),
            TokenKind::Ident if self.keyword_text("break") => {
                self.bump();
                let label = self.parse_loop_label_ref();
                self.finish_stmt(Stmt::Break(label), start)
            }
            TokenKind::Ident if self.keyword_text("continue") => {
                self.bump();
                let label = self.parse_loop_label_ref();
                self.finish_stmt(Stmt::Continue(label), start)
            }
            TokenKind::KwWhile => {
                self.bump();
                let cond = self.parse_parenthesized_condition();
                let body = self.parse_loop_body();
                self.finish_stmt(
                    Stmt::While {
                        cond,
                        body,
                        label: loop_label,
                    },
                    start,
                )
            }
            TokenKind::KwDo => {
                self.bump();
                // An EMPTY do-while body (`do while (cond);`): the `do` is immediately followed by the
                // `while` keyword, so there is no body statement. (A non-do loop body may itself be a
                // `while` loop, so this empty-body shortcut is do-while-specific.)
                let body_span = self.tok().span;
                self.skip_newlines();
                let body = if self.at(TokenKind::KwWhile) {
                    self.file.add_expr(
                        Expr::Block {
                            stmts: Vec::new(),
                            trailing: None,
                        },
                        body_span,
                    )
                } else {
                    self.parse_loop_body()
                };
                self.skip_newlines();
                self.expect(TokenKind::KwWhile, "'while'");
                let cond = self.parse_parenthesized_condition();
                self.finish_stmt(
                    Stmt::DoWhile {
                        body,
                        cond,
                        label: loop_label,
                    },
                    start,
                )
            }
            TokenKind::KwFor => self.parse_for(start, loop_label),
            TokenKind::KwFun
                if !self
                    .t
                    .get(self.i + 1)
                    .is_some_and(|token| token.kind == TokenKind::LParen) =>
            {
                let function = self.parse_fun(&[]);
                self.finish_stmt(Stmt::LocalFun(function), start)
            }
            TokenKind::Ident if self.keyword_text("typealias") => {
                let alias = self.parse_type_alias_syntax();
                self.finish_stmt(Stmt::LocalTypeAlias(alias), start)
            }
            // Local class declaration inside a function body (`class`/`data class`/`enum class`/
            // `sealed class`/`annotation class`/`interface Name`, optionally `open`/`abstract`/… prefixed).
            // Consume leading modifiers/annotations (as the top-level path does), then apply `open`/
            // `abstract` to the parsed decl. (`object` is omitted — a bare `object` may start an
            // anonymous-object EXPRESSION, which stays on the expression path.)
            _ if self.looks_like_local_type_decl() => {
                let mods = self.skip_decl_prefix();
                let is_sealed = mods.iter().any(|m| m == "sealed");
                let is_open = is_sealed || mods.iter().any(|m| m == "open");
                let is_abstract = is_sealed || mods.iter().any(|m| m == "abstract");
                // A nested type inside this local class is hoisted to `file.decls` DURING the parse
                // below, named by the path from the local class (`Local.Inner`). `hoist_local_classes`
                // later qualifies the local class itself, and these have to move with it — so record
                // exactly which declarations belong to it rather than guessing from a name prefix.
                let lexical_type_parameter_captures = self.current_lexical_type_params();
                let nested_start = self.file.decls.len();
                let mut d = self.parse_nested_type_decl();
                d.lexical_type_parameter_captures = lexical_type_parameter_captures;
                // Preserves the prior behavior: this path applied open/abstract but left `is_sealed`
                // at its default `false` (so a local `sealed` class never reported `is_sealed`).
                d.modality = modality_of(is_open, is_abstract, false);
                d.final_modifier = mods.iter().any(|modifier| modifier == "final");
                let nested: Vec<crate::ast::DeclId> = self.file.decls[nested_start..].to_vec();
                let classifier = d.name.clone();
                let stmt = self.finish_stmt(Stmt::LocalClass(d), start);
                if !nested.is_empty() {
                    self.file.local_class_nested.insert(stmt, nested);
                }
                self.lexical_type_parameters
                    .shadow_with_classifier(classifier);
                stmt
            }
            // Full-form destructuring (`+NameBasedDestructuring`): `(val a, val b) = e` /
            // `[val a, val b] = e`, where each component carries its OWN `val`/`var` (unlike the
            // leading-keyword short form `val (a, b) = e`). Paren form is name-based (binds by
            // property name); bracket form is positional (`componentN`). Disambiguated from a plain
            // parenthesized-expression statement by the `val`/`var` right after the opener.
            _ if self.at_full_form_destructure() => self.parse_full_form_destructure(start),
            _ => {
                let e = self.parse_expr();
                // Increment/decrement *statement* (`target++` / `++target`): a name remains an
                // `Expr::IncDec`, while member/index parsing already built its single-evaluation
                // result block. In statement position re-route the name to `Stmt::IncDec` and prune
                // only the unused result of an access block. Immediately before a closing brace of a
                // VALUE-CONSUMING block, or as the bare body of an `if`/`when` branch, keep the
                // expression value. The enclosing parser context marks lambda/if/when/try value
                // blocks; function/accessor/init/constructor/loop/finally blocks stay on this
                // statement path. This avoids both a lambda-only fork and accidentally treating a
                // Unit function's trailing implicit property increment as a local-variable
                // expression.
                // Peek WITHOUT consuming: only a `}` (after newlines) makes this the body's last
                // statement — consuming the newlines here would shift the statement's spans.
                let after_nl = (self.i..self.t.len())
                    .find(|&i| self.t[i].kind != TokenKind::Newline)
                    .map(|i| self.t[i].kind);
                // A script's last statement is its result, a value as well.
                let keeps_value = bare_branch_value
                    || (after_nl == Some(TokenKind::RBrace) && self.block_trailing_is_value)
                    || (after_nl == Some(TokenKind::Eof) && self.file.is_script);
                if !keeps_value {
                    if let Expr::IncDec {
                        target,
                        dec,
                        prefix,
                    } = self.file.expr(e).clone()
                    {
                        let op_span = self.file.expr_spans[e.0 as usize];
                        return self.incdec_target(target, dec, prefix, op_span, start);
                    }
                    // Access increments were expanded before statement context was known. Retain the
                    // single-evaluation update block but prune its unused expression result.
                    self.discard_incdec_access_value(e);
                }
                // assignment: `name = value` or `receiver.name = value`.
                if self.at(TokenKind::Eq) {
                    let target_span = self.assignment_target_span(e);
                    match self.file.expr(e).clone() {
                        Expr::Name(n) => {
                            let operator = self.bump().span; // '='
                            self.skip_newlines();
                            let value = self.parse_unlabelled_expr();
                            self.file.value_operator_spans.insert(value.0, operator);
                            let statement = self.finish_assignment_stmt(
                                Stmt::Assign { name: n, value },
                                start,
                                target_span,
                            );
                            self.file.assignment_target_reads.insert(statement.0, e);
                            return statement;
                        }
                        Expr::Member { receiver, name } => {
                            let operator = self.bump().span; // '='
                            self.skip_newlines();
                            let value = self.parse_unlabelled_expr();
                            self.file.value_operator_spans.insert(value.0, operator);
                            let statement = self.finish_assignment_stmt(
                                Stmt::AssignMember {
                                    receiver,
                                    name,
                                    value,
                                    safe: false,
                                },
                                start,
                                target_span,
                            );
                            self.file.assignment_target_reads.insert(statement.0, e);
                            return statement;
                        }
                        Expr::Index { array, indices } => {
                            let operator = self.bump().span; // '='
                            self.skip_newlines();
                            let value = self.parse_unlabelled_expr();
                            self.file.value_operator_spans.insert(value.0, operator);
                            return self.finish_assignment_stmt(
                                Stmt::AssignIndex {
                                    array,
                                    indices,
                                    value,
                                },
                                start,
                                target_span,
                            );
                        }
                        Expr::SafeCall {
                            receiver,
                            name,
                            args: None,
                        } if !self.parenthesized_expressions.contains(&e.0) => {
                            let operator = self.bump().span; // '='
                            self.skip_newlines();
                            let value = self.parse_unlabelled_expr();
                            self.file.value_operator_spans.insert(value.0, operator);
                            return self.finish_assignment_stmt(
                                Stmt::AssignMember {
                                    receiver,
                                    name,
                                    value,
                                    safe: true,
                                },
                                start,
                                target_span,
                            );
                        }
                        Expr::SafeIndex { .. } => {
                            return self.finish_safe_index_assignment(e, start, target_span);
                        }
                        _ => self
                            .diags
                            .error(self.tok().span, "invalid assignment target"),
                    }
                }
                // compound assignment: `target op= value` → `target = target op value`.
                if let Some(op) = compound_op(self.kind()) {
                    let op_span = self.tok().span;
                    let target_span = self.assignment_target_span(e);
                    match self.file.expr(e).clone() {
                        Expr::Name(n) => {
                            self.bump();
                            self.skip_newlines();
                            let rhs = self.parse_unlabelled_expr();
                            let lhs = self.file.add_compound_assignment_read(e, op_span);
                            let value = self.file.add_expr(
                                Expr::Binary {
                                    op,
                                    lhs,
                                    rhs,
                                    operator_span: op_span,
                                },
                                op_span,
                            );
                            return self.finish_assignment_stmt(
                                Stmt::Assign { name: n, value },
                                start,
                                target_span,
                            );
                        }
                        Expr::Member { receiver, name } => {
                            self.bump();
                            self.skip_newlines();
                            let rhs = self.parse_unlabelled_expr();
                            let lhs = self.file.add_compound_assignment_read(e, op_span);
                            let value = self.file.add_expr(
                                Expr::Binary {
                                    op,
                                    lhs,
                                    rhs,
                                    operator_span: op_span,
                                },
                                op_span,
                            );
                            return self.finish_assignment_stmt(
                                Stmt::AssignMember {
                                    receiver,
                                    name,
                                    value,
                                    safe: false,
                                },
                                start,
                                target_span,
                            );
                        }
                        Expr::SafeCall {
                            receiver,
                            name,
                            args: None,
                        } if !self.parenthesized_expressions.contains(&e.0) => {
                            self.bump();
                            self.skip_newlines();
                            let rhs = self.parse_unlabelled_expr();
                            // The compound operator applies to the non-null member, inside the
                            // safe-call guard. A safe-call lhs would select the nullable extension.
                            let name_span = self.file.exact_member_name_spans.get(&e.0).copied();
                            let lhs = self.file.add_expr(
                                Expr::Member {
                                    receiver,
                                    name: name.clone(),
                                },
                                target_span,
                            );
                            if let Some(name_span) = name_span {
                                self.file.exact_member_name_spans.insert(lhs.0, name_span);
                            }
                            let value = self.file.add_expr(
                                Expr::Binary {
                                    op,
                                    lhs,
                                    rhs,
                                    operator_span: op_span,
                                },
                                Span::new(target_span.lo, self.file.expr_spans[rhs.0 as usize].hi),
                            );
                            return self.finish_assignment_stmt(
                                Stmt::AssignMember {
                                    receiver,
                                    name,
                                    value,
                                    safe: true,
                                },
                                start,
                                target_span,
                            );
                        }
                        Expr::SafeIndex { .. } => {
                            return self.finish_compound_safe_index_assignment(
                                e,
                                op,
                                op_span,
                                start,
                                target_span,
                            );
                        }
                        Expr::Index { array, indices } => {
                            self.bump();
                            self.skip_newlines();
                            let rhs = self.parse_unlabelled_expr();
                            let lhs = self.file.add_expr(
                                Expr::Index {
                                    array,
                                    indices: indices.clone(),
                                },
                                op_span,
                            );
                            let value = self.file.add_expr(
                                Expr::Binary {
                                    op,
                                    lhs,
                                    rhs,
                                    operator_span: op_span,
                                },
                                op_span,
                            );
                            return self.finish_assignment_stmt(
                                Stmt::AssignIndex {
                                    array,
                                    indices,
                                    value,
                                },
                                start,
                                target_span,
                            );
                        }
                        // Any other target is not assignable, and `target op= value` on it is
                        // Kotlin's operator-call form (`m[k]!! += x` is `plusAssign` on the
                        // value, as is `(m[k]!!) += x`); the checker decides whether the value's
                        // type offers that operator. Rejecting the syntax here dropped the whole
                        // declaring file from a real project's test source set.
                        _ => {
                            self.bump();
                            self.skip_newlines();
                            let value = self.parse_unlabelled_expr();
                            return self.finish_assignment_stmt(
                                Stmt::CompoundAssign {
                                    target: e,
                                    value,
                                    op,
                                    operator_span: op_span,
                                },
                                start,
                                target_span,
                            );
                        }
                    }
                }
                self.finish_stmt(Stmt::Expr(e), start)
            }
        }
    }

    /// A `{…}` block restores statement-terminating line breaks inside any enclosing parentheses.
    pub(super) fn parse_block_expr(&mut self, trailing_is_value: bool) -> ExprId {
        self.with_newlines_significant(|parser| parser.parse_block_body(trailing_is_value))
    }

    fn parse_block_body(&mut self, trailing_is_value: bool) -> ExprId {
        let start = self.tok().span;
        self.expect(TokenKind::LBrace, "'{'");
        let saved_block_value = self.block_trailing_is_value;
        let classifier_shadow_count = self.lexical_type_parameters.shadow_count();
        self.block_trailing_is_value = trailing_is_value;
        let mut stmts = Vec::new();
        loop {
            self.skip_newlines();
            if self.at(TokenKind::RBrace) || self.at(TokenKind::Eof) {
                break;
            }
            stmts.push(self.parse_stmt());
        }
        self.block_trailing_is_value = saved_block_value;
        self.lexical_type_parameters
            .truncate_shadows(classifier_shadow_count);
        let end = self.tok().span;
        self.expect(TokenKind::RBrace, "'}'");
        // A trailing bare expression is the block's value.
        let mut trailing = None;
        if let Some(&last) = stmts.last() {
            if let Stmt::Expr(e) = self.file.stmt(last) {
                let e = *e;
                trailing = Some(e);
                stmts.pop();
                // The statement's annotations (`@Suppress("NAME") call()`) annotate the value.
                if let Some(mut annotations) = self.file.statement_annotations.remove(&last) {
                    if let Some(own) = self.file.expression_annotations.remove(&e) {
                        annotations.extend(own);
                    }
                    self.file.expression_annotations.insert(e, annotations);
                }
            }
        }
        self.file
            .add_expr(Expr::Block { stmts, trailing }, Span::new(start.lo, end.hi))
    }
}
