//! Expression parsing: Pratt operators, primaries, postfixes, control-flow expressions, and branch bodies.

use super::*;

impl Parser<'_> {
    fn parse_call_type_args(&mut self) -> Vec<TypeRef> {
        if self.at(TokenKind::Lt) && self.lookahead_is_type_args_call() {
            self.parse_type_args()
        } else {
            Vec::new()
        }
    }

    // ---- expressions (Pratt) ----
    pub(super) fn parse_expr(&mut self) -> ExprId {
        self.parse_bp(0)
    }

    /// Depth-guarded entry for the binary-expression parser. Ordinary expression recursion funnels
    /// through here — `parse_expr` → `parse_bp`, and nested operands
    /// (parenthesized/bracketed expressions, call arguments, prefix operands, branch bodies)
    /// re-enter via `parse_expr`. Annotation-value structures recurse outside `parse_bp`, so their
    /// entry explicitly selects the same [`ParserNesting::Expression`] lane. A left-leaning binary
    /// chain (`a && b && c`) iterates in `parse_bp_inner`'s loop and does not grow the depth; only
    /// genuinely nested expressions (`((((x))))`, `f(f(f(x)))`) do. Past [`EXPR_DEPTH_LIMIT`] the
    /// expression degrades to an error expression with a diagnostic — degrade, never crash —
    /// mirroring the checker's and lowering's `expr_depth` guards.
    pub(super) fn parse_bp(&mut self, min_bp: u8) -> ExprId {
        self.with_nesting_guard(
            ParserNesting::Expression,
            |parser, span| {
                parser
                    .file
                    .add_expr(Expr::Name("<error>".to_string()), span)
            },
            |parser| parser.parse_bp_inner(min_bp),
        )
    }

    fn parse_bp_inner(&mut self, min_bp: u8) -> ExprId {
        let mut lhs = self.parse_prefix();
        loop {
            // A newline before `||`/`&&`/`?:` is a line continuation, not a terminator — consume it so
            // the operator below (or the enclosing elvis loop, for `?:`) sees it on this logical line.
            self.skip_newlines_before_continuation_op();
            // Elvis binds more tightly than equality/comparison/named checks and more loosely than
            // infix/range/additive expressions. Keeping it in the Pratt loop is essential:
            // `a ?: fail() != b` is `(a ?: fail()) != b`, not `a ?: (fail() != b)`.
            // Parsing the RHS at the same boundary makes an Elvis chain right-associative while
            // still admitting infix/range expressions on either side.
            if min_bp <= 8
                && self.at(TokenKind::Question)
                && self
                    .t
                    .get(self.i + 1)
                    .is_some_and(|token| token.kind == TokenKind::Colon)
            {
                let lspan = self.file.expr_spans[lhs.0 as usize];
                self.bump(); // '?'
                self.bump(); // ':'
                self.skip_newlines();
                let rhs = self.parse_bp(8);
                let rspan = self.file.expr_spans[rhs.0 as usize];
                lhs = self
                    .file
                    .add_expr(Expr::Elvis { lhs, rhs }, Span::new(lspan.lo, rspan.hi));
                continue;
            }
            // Prefix operators bind more tightly than casts.
            if min_bp <= BP_CAST && self.at(TokenKind::Ident) && self.keyword_text("as") {
                let lspan = self.file.expr_spans[lhs.0 as usize];
                self.bump(); // 'as'
                let nullable = self.eat_type_nullable();
                let ty = self.parse_type();
                let end = self.t[self.i.saturating_sub(1)].span;
                lhs = self.file.add_expr(
                    Expr::As {
                        operand: lhs,
                        ty,
                        nullable,
                    },
                    Span::new(lspan.lo, end.hi),
                );
                continue;
            }
            // `is` / `!is` type test — a "named check" at comparison precedence (binding power 7).
            if min_bp <= 7 {
                let negated = if self.at(TokenKind::Ident) && self.keyword_text("is") {
                    Some(false)
                } else if self.at(TokenKind::Not)
                    && self
                        .t
                        .get(self.i + 1)
                        .is_some_and(|t| self.token_keyword_text(*t, "is"))
                {
                    Some(true)
                } else {
                    None
                };
                if let Some(negated) = negated {
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    if negated {
                        self.bump(); // '!'
                    }
                    self.bump(); // 'is'
                    let ty = self.parse_type();
                    let end = self.t[self.i.saturating_sub(1)].span;
                    lhs = self.file.add_expr(
                        Expr::Is {
                            operand: lhs,
                            ty,
                            negated,
                        },
                        Span::new(lspan.lo, end.hi),
                    );
                    continue;
                }
            }
            // `in` / `!in` membership — a "named check" at comparison precedence (bp 7). A range RHS
            // (`a..b`, `a until b`, `a downTo b`) becomes `Expr::InRange`; any other RHS becomes
            // `container.contains(value)` (`!in` wraps it in `!`).
            if min_bp <= 7 {
                let in_negated = if self.at(TokenKind::KwIn) {
                    Some(false)
                } else if self.at(TokenKind::Not)
                    && self
                        .t
                        .get(self.i + 1)
                        .is_some_and(|t| t.kind == TokenKind::KwIn)
                {
                    Some(true)
                } else {
                    None
                };
                if let Some(negated) = in_negated {
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    if negated {
                        self.bump(); // '!'
                    }
                    self.bump(); // 'in'
                    self.skip_newlines();
                    let rstart = self.parse_bp(9); // the range start binds tighter than `in` (and `..`)
                    let kind = if self.eat(TokenKind::DotDot) {
                        Some(RangeKind::Through)
                    } else if self.eat(TokenKind::DotDotLt) {
                        Some(RangeKind::OpenEnd)
                    } else if self.at(TokenKind::Ident) && self.text() == "until" {
                        self.bump();
                        Some(RangeKind::Until)
                    } else if self.at(TokenKind::Ident) && self.text() == "downTo" {
                        self.bump();
                        Some(RangeKind::DownTo)
                    } else {
                        None
                    };
                    match kind {
                        Some(kind) => {
                            let rend = self.parse_bp(9);
                            let end = self.file.expr_spans[rend.0 as usize];
                            lhs = self.file.add_expr(
                                Expr::InRange {
                                    value: lhs,
                                    start: rstart,
                                    end: rend,
                                    kind,
                                    negated,
                                },
                                Span::new(lspan.lo, end.hi),
                            );
                        }
                        None => {
                            // `value in container` → `container.contains(value)`.
                            let cspan = self.file.expr_spans[rstart.0 as usize];
                            let callee = self.file.add_expr(
                                Expr::Member {
                                    receiver: rstart,
                                    name: "contains".to_string(),
                                },
                                Span::new(lspan.lo, cspan.hi),
                            );
                            let call = self.file.add_expr(
                                Expr::Call {
                                    callee,
                                    args: vec![lhs],
                                },
                                Span::new(lspan.lo, cspan.hi),
                            );
                            lhs = if negated {
                                self.file.add_expr(
                                    Expr::Unary {
                                        op: UnOp::Not,
                                        operand: call,
                                    },
                                    Span::new(lspan.lo, cspan.hi),
                                )
                            } else {
                                call
                            };
                        }
                    }
                    continue;
                }
            }
            // Range operators `a..b` (`rangeTo`) and `a..<b` (`rangeUntil`) as a *value*. These are
            // the only true range *operators* — `until`/`downTo`/`step` are ordinary stdlib infix
            // functions and flow through the infix-function path below. Binds tighter than infix
            // functions (so `a..b step c` is `(a..b).step(c)`) and looser than additive (operands at
            // bp 9). Builds `Expr::RangeTo`; the `for`/`in` forms are handled separately above.
            if min_bp <= 8 {
                let rkind = if self.at(TokenKind::DotDot) {
                    Some(RangeKind::Through)
                } else if self.at(TokenKind::DotDotLt) {
                    Some(RangeKind::OpenEnd)
                } else {
                    None
                };
                if let Some(kind) = rkind {
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    self.bump(); // '..' / '..<'
                    self.skip_newlines();
                    let hi = self.parse_bp(9);
                    let rspan = self.file.expr_spans[hi.0 as usize];
                    lhs = self.file.add_expr(
                        Expr::RangeTo { lo: lhs, hi, kind },
                        Span::new(lspan.lo, rspan.hi),
                    );
                    continue;
                }
            }
            // Infix function call `a foo b` → `a.foo(b)`: a simple identifier between two operands.
            // Binds tighter than comparison (bp 7) and looser than additive (bp 9) — Kotlin's
            // `infixFunctionCall`. Resolution checks `foo` is actually an `infix`/member function.
            if min_bp <= 8 && self.at(TokenKind::Ident) {
                let name = self.text();
                // Exclude the real soft keywords only. `until`/`downTo`/`step` are ordinary stdlib
                // infix functions and parse as such here (`a until b` → `a.until(b)`); the `for`/`in`
                // forms recognize them separately before reaching this point.
                let is_soft_kw = matches!(name, "is" | "as" | "in");
                let next_starts_expr = self.infix_operand_follows();
                if !is_soft_kw && next_starts_expr {
                    let name = name.to_string();
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    self.bump(); // infix function name
                    self.skip_newlines();
                    let rhs = self.parse_bp(9); // operand binds at additive precedence or tighter
                    let rspan = self.file.expr_spans[rhs.0 as usize];
                    let callee = self.file.add_expr(
                        Expr::Member {
                            receiver: lhs,
                            name,
                        },
                        Span::new(lspan.lo, rspan.hi),
                    );
                    lhs = self.file.add_expr(
                        Expr::Call {
                            callee,
                            args: vec![rhs],
                        },
                        Span::new(lspan.lo, rspan.hi),
                    );
                    self.file.infix_calls.insert(lhs.0);
                    continue;
                }
            }
            let op = match infix_op(self.kind()) {
                Some(o) => o,
                None => break,
            };
            let (lbp, rbp) = infix_bp(op);
            if lbp < min_bp {
                break;
            }
            let op_span = self.tok().span;
            self.bump();
            self.skip_newlines();
            let rhs = self.parse_bp(rbp);
            let lspan = self.file.expr_spans[lhs.0 as usize];
            let rspan = self.file.expr_spans[rhs.0 as usize];
            lhs = self.file.add_expr(
                Expr::Binary {
                    op,
                    lhs,
                    rhs,
                    operator_span: op_span,
                },
                Span::new(lspan.lo, rspan.hi),
            );
        }
        lhs
    }

    fn parse_prefix(&mut self) -> ExprId {
        let start = self.tok().span;
        // Named context parameters on an anonymous function expression:
        // `context(x: C) fun () = …`. `context(...)` remains an ordinary call unless the balanced
        // parameter list is followed by `fun`.
        if self.at(TokenKind::Ident)
            && self.keyword_text("context")
            && self
                .t
                .get(self.i + 1)
                .is_some_and(|token| token.kind == TokenKind::LParen)
        {
            if let Some(mut after) = self.context_parameter_list_end_at(self.i + 1) {
                while self
                    .t
                    .get(after)
                    .is_some_and(|token| token.kind == TokenKind::Newline)
                {
                    after += 1;
                }
                if self
                    .t
                    .get(after)
                    .is_some_and(|token| token.kind == TokenKind::KwFun)
                {
                    self.bump(); // `context`
                    let context_params = self.parse_param_list();
                    self.skip_newlines();
                    return self.parse_anon_fun(context_params);
                }
            }
        }
        // Expression annotations decorate the following expression and do not change its value:
        // `@Ann fun() { … }`, `@Ann { … }`, `@Ann value`. Parse their arguments through the
        // ordinary annotation grammar, then continue with the same prefix-expression parser so
        // annotations compose with labels, unary operators, and anonymous functions.
        if self.at(TokenKind::At) {
            while self.at(TokenKind::At) {
                self.parse_annotation();
                self.skip_newlines();
            }
            return self.parse_prefix();
        }
        // A jump operand is a full expression, including an elvis chain.
        if self.at(TokenKind::Ident) && self.keyword_text("throw") {
            self.bump(); // 'throw'
            let operand = self.parse_expr();
            let end = self.file.expr_spans[operand.0 as usize];
            return self
                .file
                .add_expr(Expr::Throw { operand }, Span::new(start.lo, end.hi));
        }
        // `break`/`continue` (with an optional `@label`) in EXPRESSION position (`m[k] ?: continue`, a
        // `when` arm). Soft keywords (Ident), like `throw`; bottom type `Nothing`. A statement-position
        // `break`/`continue` is handled earlier in `parse_stmt` (→ `Stmt::Break`/`Continue`).
        if self.at(TokenKind::Ident)
            && (self.keyword_text("break") || self.keyword_text("continue"))
        {
            let is_break = self.keyword_text("break");
            let mut end = self.tok().span; // the `break`/`continue` token
            self.bump(); // 'break' / 'continue'
            let label = if self.at(TokenKind::At) {
                self.bump();
                if self.at(TokenKind::Ident) {
                    let l = self.text().to_string();
                    end = self.tok().span; // the label token
                    self.bump();
                    Some(l)
                } else {
                    None
                }
            } else {
                None
            };
            let sp = Span::new(start.lo, end.hi);
            let e = if is_break {
                Expr::Break { label }
            } else {
                Expr::Continue { label }
            };
            return self.file.add_expr(e, sp);
        }
        // `return`/`return@label value` in expression position (`x ?: return null`).
        if self.at(TokenKind::KwReturn) {
            self.bump(); // 'return'
            let label = if self.at(TokenKind::At) {
                self.bump();
                if self.at(TokenKind::Ident) {
                    let l = self.text().to_string();
                    self.bump();
                    Some(l)
                } else {
                    None
                }
            } else {
                None
            };
            // A value follows unless the next token closes the expression context.
            let value = if matches!(
                self.kind(),
                TokenKind::Newline
                    | TokenKind::RBrace
                    | TokenKind::RParen
                    | TokenKind::RBracket
                    | TokenKind::Comma
                    | TokenKind::Eof
            ) {
                None
            } else {
                Some(self.parse_expr())
            };
            let end = value
                .map(|v| self.file.expr_spans[v.0 as usize])
                .unwrap_or(start);
            return self
                .file
                .add_expr(Expr::Return { value, label }, Span::new(start.lo, end.hi));
        }
        // Labeled expression prefix: `label@ <expr>` (`l1@ "s"`, `x@ (1L + 2)`, `l@ { … }`). A label
        // names the following expression as a target for a non-local `return@label`/`break@label`; on a
        // plain expression it is a semantic no-op. Detected as `Ident` immediately followed by `@` — the
        // keyword-labels (`break@`, `continue@`, `return@`) are consumed by their handlers above, so any
        // `Ident @` reaching here is an expression label. Consume it and parse the labeled expression
        // (recurse so stacked labels `a@ b@ e` and a following unary/primary all flow through normally).
        // `this`/`super` are also `Ident`s but `this@Outer`/`super@Base` is a labeled RECEIVER, not a
        // labeled expression — leave those for `parse_primary` to bind the `@label` to the receiver.
        if self.at(TokenKind::Ident)
            && !self.keyword_text_any(&["this", "super"])
            && self
                .t
                .get(self.i + 1)
                .is_some_and(|t| t.kind == TokenKind::At)
        {
            // Consume the WHOLE label chain (`a@ b@ e`) iteratively before the single re-entry:
            // per-label self-recursion here would be an unguarded, un-grown stack path — a long
            // machine-generated label chain must degrade like any other deep nesting, and after
            // this loop the re-entry cannot reach this branch again (the next token is no label).
            let mut labels = Vec::new();
            while self.at(TokenKind::Ident)
                && !self.keyword_text_any(&["this", "super"])
                && self
                    .t
                    .get(self.i + 1)
                    .is_some_and(|t| t.kind == TokenKind::At)
            {
                labels.push(self.text().to_string());
                self.bump(); // label name
                self.bump(); // '@'
            }
            let labelled = self.parse_prefix();
            self.record_lambda_labels(labelled, labels);
            return labelled;
        }
        let unop = match self.kind() {
            TokenKind::Minus => Some(UnOp::Neg),
            TokenKind::Not => Some(UnOp::Not),
            TokenKind::Plus => Some(UnOp::Plus),
            _ => None,
        };
        if let Some(op) = unop {
            self.bump();
            // Kotlin: `-2147483648` is `Int.MIN_VALUE` (an `Int`), even though the bare literal
            // `2147483648` overflows `Int` and is otherwise a `Long`. Fold this one case so the
            // negation keeps `Int` type (a `when (x: Int)` branch / `val i: Int = -2147483648`).
            if matches!(op, UnOp::Neg)
                && self.at(TokenKind::IntLit)
                && parse_int_literal(self.text()) == 2147483648
            {
                let lit_span = self.tok().span;
                self.bump();
                return self.file.add_expr(
                    Expr::IntLit(i32::MIN as i64),
                    Span::new(start.lo, lit_span.hi),
                );
            }
            let operand = self.parse_bp(BP_PREFIX);
            let end = self.file.expr_spans[operand.0 as usize];
            return self
                .file
                .add_expr(Expr::Unary { op, operand }, Span::new(start.lo, end.hi));
        }
        // Prefix `++target` / `--target` as a value (the new value). Statement position is intercepted
        // in `parse_stmt` before reaching here, so this fires only when used as a value.
        if self.at(TokenKind::PlusPlus) || self.at(TokenKind::MinusMinus) {
            let dec = self.at(TokenKind::MinusMinus);
            self.bump();
            let target = self.parse_bp(BP_PREFIX);
            let end = self.file.expr_spans[target.0 as usize];
            let expression = self.file.add_expr(
                Expr::IncDec {
                    target,
                    dec,
                    prefix: true,
                },
                Span::new(start.lo, end.hi),
            );
            return if matches!(self.file.expr(target), Expr::Name(_)) {
                expression
            } else {
                self.incdec_access_value_expr(expression, target, dec, true, start)
            };
        }
        let primary = self.parse_primary();
        self.parse_postfix(primary)
    }

    /// Whether the cursor sits on a chain of `label@` prefixes ending in `{` — a LABELLED TRAILING
    /// LAMBDA. `this`/`super` are excluded: `this@Outer` is a labelled RECEIVER, not a label prefix.
    fn at_labelled_trailing_lambda(&self) -> bool {
        let mut i = self.i;
        let mut seen = false;
        while self.t.get(i).is_some_and(|t| t.kind == TokenKind::Ident)
            && self.t.get(i + 1).is_some_and(|t| t.kind == TokenKind::At)
            && !matches!(self.t[i].text(self.src), "this" | "super")
        {
            seen = true;
            i += 2;
        }
        seen && self.t.get(i).is_some_and(|t| t.kind == TokenKind::LBrace)
    }

    /// Record the explicit label of `expr` when it is a lambda literal. On any other expression a
    /// label is a semantic no-op; on a lambda it REPLACES the implicit callee-name label that a
    /// `return@…` inside the body targets. A chain (`a@ b@ { … }`) keeps the INNERMOST label — the one
    /// written closest to the lambda — since the table holds one label per lambda.
    fn record_lambda_labels(&mut self, expr: ExprId, labels: Vec<String>) {
        if labels.is_empty() || !matches!(self.file.expr(expr), Expr::Lambda { .. }) {
            return;
        }
        for label in labels {
            self.file.lambda_labels.insert(expr.0, label);
        }
    }

    fn parse_postfix(&mut self, mut lhs: ExprId) -> ExprId {
        // Explicit type arguments parsed just before a call paren (`foo<Int>(…)`), attached to the
        // call once it is built so a constructor instantiation (`ArrayList<Int>()`) keeps its args.
        let mut pending_targs: Vec<TypeRef> = Vec::new();
        // Labels consumed just before a trailing lambda (`run outer@{ … }`), attached to the lambda
        // once it is parsed so a `return@outer` inside it finds its target.
        let mut pending_lambda_labels: Vec<String> = Vec::new();
        loop {
            // A postfix chain may continue on a following line: Kotlin treats a newline before `.` or
            // `?.` as part of the selector chain, not a statement terminator (`x\n  .foo()\n  .bar()`).
            // Peek past the newline(s); if a member access follows, consume them and continue —
            // otherwise stop (the expression ended). `::`/`{` deliberately do NOT continue across a
            // newline (callable-ref / trailing-lambda are same-line only).
            if self.at(TokenKind::Newline) {
                let mut j = self.i;
                while self.t.get(j).is_some_and(|t| t.kind == TokenKind::Newline) {
                    j += 1;
                }
                let continues = match self.t.get(j).map(|t| t.kind) {
                    Some(TokenKind::Dot) => true,
                    Some(TokenKind::Question) => {
                        self.t.get(j + 1).is_some_and(|t| t.kind == TokenKind::Dot)
                    }
                    _ => false,
                };
                if !continues {
                    break;
                }
                self.skip_newlines();
            }
            match self.kind() {
                // Postfix `target++` / `target--` as a value (the old value). In statement position
                // `parse_stmt` re-routes the resulting `IncDec` to the statement path.
                TokenKind::PlusPlus | TokenKind::MinusMinus => {
                    let dec = self.at(TokenKind::MinusMinus);
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    let end = self.tok().span;
                    self.bump();
                    let target = lhs;
                    let expression = self.file.add_expr(
                        Expr::IncDec {
                            target,
                            dec,
                            prefix: false,
                        },
                        Span::new(lspan.lo, end.hi),
                    );
                    lhs = if matches!(self.file.expr(target), Expr::Name(_)) {
                        expression
                    } else {
                        self.incdec_access_value_expr(expression, target, dec, false, lspan)
                    };
                }
                // `!!` not-null assertion in postfix position = two consecutive `Not` tokens.
                TokenKind::Not
                    if self
                        .t
                        .get(self.i + 1)
                        .is_some_and(|t| t.kind == TokenKind::Not) =>
                {
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    self.bump();
                    let end = self.tok().span;
                    self.bump();
                    lhs = self
                        .file
                        .add_expr(Expr::NotNull { operand: lhs }, Span::new(lspan.lo, end.hi));
                }
                // `?.` safe call: `recv?.name` or `recv?.name(args)`.
                TokenKind::Question
                    if self
                        .t
                        .get(self.i + 1)
                        .is_some_and(|t| t.kind == TokenKind::Dot) =>
                {
                    self.bump(); // '?'
                    self.bump(); // '.'
                    let name_token = self.tok();
                    let name_span = self.syntactic_ident_span(name_token);
                    let name = self.ident_or_error("member name");
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    let type_args = self.parse_call_type_args();
                    let mut safe_names: Vec<Option<String>> = Vec::new();
                    let mut safe_name_spans: Vec<Option<Span>> = Vec::new();
                    let args = if self.at(TokenKind::LParen) {
                        self.bump();
                        let (args, names, name_spans) = self.parse_call_argument_list();
                        self.expect(TokenKind::RParen, "')'");
                        safe_names = names;
                        safe_name_spans = name_spans;
                        Some(args)
                    } else {
                        None
                    };
                    let end = self.t[self.i.saturating_sub(1)].span;
                    let needs_exact_name_span = name_span != name_token.span || args.is_some();
                    let safe_call = self.file.add_expr(
                        Expr::SafeCall {
                            receiver: lhs,
                            name,
                            args,
                        },
                        Span::new(lspan.lo, end.hi),
                    );
                    if needs_exact_name_span {
                        self.file
                            .exact_member_name_spans
                            .insert(safe_call.0, name_span);
                    }
                    if !type_args.is_empty() {
                        self.file.call_type_args.insert(safe_call.0, type_args);
                    }
                    if safe_names.iter().any(|n| n.is_some()) {
                        self.file.call_arg_names.insert(safe_call.0, safe_names);
                        self.file
                            .call_arg_name_spans
                            .insert(safe_call.0, safe_name_spans);
                    }
                    lhs = safe_call;
                }
                // `Recv?::name` / `Recv?::class` — a callable reference / class literal on a NULLABLE
                // receiver type. The expression still names `Recv`; retain nullability sparsely on the
                // reference node so semantic selection sees `Recv?` rather than silently narrowing it.
                TokenKind::Question
                    if self
                        .t
                        .get(self.i + 1)
                        .is_some_and(|t| t.kind == TokenKind::ColonColon) =>
                {
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    self.bump(); // '?'
                    self.bump(); // '::'
                    let name = if self.at(TokenKind::Ident) {
                        let n = self.text().to_string();
                        self.bump();
                        n
                    } else if self.at(TokenKind::KwClass) {
                        self.bump();
                        "class".to_string()
                    } else {
                        "<error>".to_string()
                    };
                    let end = self.t[self.i.saturating_sub(1)].span;
                    // Type arguments erase from the JVM owner but remain part of the semantic
                    // receiver (`Pair<String, Int>?::first` returns `String`). They belong to the
                    // receiver segment, not to the referenced member.
                    if !pending_targs.is_empty() {
                        self.file
                            .call_type_args
                            .insert(lhs.0, std::mem::take(&mut pending_targs));
                    }
                    let reference = self.file.add_expr(
                        Expr::CallableRef {
                            receiver: Some(lhs),
                            name,
                        },
                        Span::new(lspan.lo, end.hi),
                    );
                    self.file
                        .nullable_callable_ref_receivers
                        .insert(reference.0);
                    lhs = reference;
                }
                TokenKind::Dot => {
                    let dot_span = self.bump().span;
                    if self.eat(TokenKind::LParen) {
                        let callable = self.parse_expr();
                        let end = self.tok().span;
                        self.expect(TokenKind::RParen, "')'");
                        let lspan = self.file.expr_spans[lhs.0 as usize];
                        lhs = self.file.add_expr(
                            Expr::ExtensionAccess {
                                receiver: lhs,
                                callable,
                            },
                            Span::new(lspan.lo, end.hi),
                        );
                        continue;
                    }
                    let name_token = self.tok();
                    let name_span = self.syntactic_ident_span(name_token);
                    let name = self.ident_or_error("member name");
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    let end = self.t[self.i.saturating_sub(1)].span;
                    // In a classifier/callable-reference LHS, type arguments belong to the
                    // classifier segment immediately to their LEFT:
                    // `Outer<A>.Inner<B>::member`. Keeping them in one pending slot until the final
                    // `::` loses `Outer<A>` as soon as `<B>` is parsed. Attach each completed
                    // segment now; an ordinary generic call still consumes its pending arguments
                    // in the `(` / trailing-lambda branches below.
                    if !pending_targs.is_empty() {
                        self.file
                            .call_type_args
                            .insert(lhs.0, std::mem::take(&mut pending_targs));
                    }
                    let member = self.file.add_expr(
                        Expr::Member {
                            receiver: lhs,
                            name,
                        },
                        Span::new(lspan.lo, end.hi),
                    );
                    if name_span != name_token.span {
                        self.file
                            .exact_member_name_spans
                            .insert(member.0, name_span);
                    }
                    if dot_span.hi != name_span.lo {
                        self.file
                            .non_adjacent_member_dot_spans
                            .push((member.0, dot_span));
                    }
                    lhs = member;
                }
                // `expr::name` or `Expr::class` — bound callable reference / class literal.
                TokenKind::ColonColon => {
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    self.bump(); // '::'
                    let name = if self.at(TokenKind::Ident) {
                        let n = self.text().to_string();
                        self.bump();
                        n
                    } else if self.at(TokenKind::KwClass) {
                        self.bump();
                        "class".to_string()
                    } else {
                        "<error>".to_string()
                    };
                    let end = self.t[self.i.saturating_sub(1)].span;
                    // Complete the receiver classifier segment before constructing the reference.
                    // The arguments describe `A<T>` in `A<T>::member`, not the referenced member;
                    // retaining them on the receiver also preserves every applied segment in
                    // `Outer<A>.Inner<B>::member`.
                    if !pending_targs.is_empty() {
                        self.file
                            .call_type_args
                            .insert(lhs.0, std::mem::take(&mut pending_targs));
                    }
                    let reference = self.file.add_expr(
                        Expr::CallableRef {
                            receiver: Some(lhs),
                            name,
                        },
                        Span::new(lspan.lo, end.hi),
                    );
                    lhs = reference;
                }
                TokenKind::LParen => {
                    let open_paren = self.bump().span;
                    let (args, names, name_spans) = self.parse_call_argument_list();
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    let end = self.tok().span;
                    self.expect(TokenKind::RParen, "')'");
                    let is_empty = args.is_empty();
                    let call = self.file.add_expr(
                        Expr::Call { callee: lhs, args },
                        Span::new(lspan.lo, end.hi),
                    );
                    if is_empty {
                        self.file
                            .empty_call_open_paren_spans
                            .insert(call.0, open_paren);
                    }
                    if names.iter().any(|n| n.is_some()) {
                        self.file.call_arg_names.insert(call.0, names);
                        self.file.call_arg_name_spans.insert(call.0, name_spans);
                    }
                    if !pending_targs.is_empty() {
                        self.file
                            .call_type_args
                            .insert(call.0, std::mem::take(&mut pending_targs));
                    }
                    lhs = call;
                }
                // Trailing lambda: `expr { … }` / `recv.m(args) { … }` → append the lambda as the
                // last call argument (same line only, to avoid swallowing an unrelated block).
                TokenKind::LBrace if self.no_trailing_lambda => break,
                // A trailing lambda may carry an explicit LABEL (`run outer@{ … }`, `xs.sumOf s@{ … }`)
                // naming it as the target of a `return@outer` inside. Consume the label chain here so
                // the `{` still attaches to the call; without it the callee stayed a bare name and the
                // labelled block became a separate statement ("unresolved reference 'run'").
                TokenKind::Ident
                    if !self.no_trailing_lambda && self.at_labelled_trailing_lambda() =>
                {
                    while self.at(TokenKind::Ident)
                        && self
                            .t
                            .get(self.i + 1)
                            .is_some_and(|t| t.kind == TokenKind::At)
                    {
                        pending_lambda_labels.push(self.text().to_string());
                        self.bump(); // label name
                        self.bump(); // '@'
                    }
                }
                TokenKind::LBrace => {
                    let lambda = self.parse_lambda();
                    self.record_lambda_labels(lambda, std::mem::take(&mut pending_lambda_labels));
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    let end = self.t[self.i.saturating_sub(1)].span;
                    let old = lhs;
                    // Parentheses terminate the inner call's trailing-argument eligibility. The
                    // same is true once one trailing lambda has already been attached: a following
                    // lambda invokes that call's result (`f { ... } { ... }`).
                    let append_to_existing_call = !self.parenthesized_expressions.contains(&old.0)
                        && !self.file.call_has_trailing_lambda.contains(&old.0)
                        && matches!(
                            self.file.expr(old),
                            Expr::Call { .. } | Expr::SafeCall { .. }
                        );
                    let has_named_parenthesized_args =
                        append_to_existing_call && self.file.call_arg_names.contains_key(&old.0);
                    let close_paren_end = match (self.file.expr(old), has_named_parenthesized_args)
                    {
                        (Expr::Call { .. } | Expr::SafeCall { args: Some(_), .. }, true) => {
                            Some(self.file.expr_spans[old.0 as usize].hi)
                        }
                        _ => None,
                    };
                    let rebuilt_safe_call_name_span = if append_to_existing_call {
                        if let Expr::SafeCall { name, .. } = self.file.expr(old) {
                            self.file
                                .exact_member_name_spans
                                .get(&old.0)
                                .copied()
                                .or_else(|| {
                                    let span = self.file.expr_spans[old.0 as usize];
                                    Some(Span::new(
                                        span.hi.saturating_sub(name.len() as u32),
                                        span.hi,
                                    ))
                                })
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    lhs = if append_to_existing_call {
                        match self.file.expr(old).clone() {
                            Expr::Call { callee, mut args } => {
                                args.push(lambda);
                                self.file.add_expr(
                                    Expr::Call { callee, args },
                                    Span::new(lspan.lo, end.hi),
                                )
                            }
                            // `recv?.scopeFn { … }` — the trailing lambda is the safe call's
                            // argument, not an invocation of its result. Attach it after any `(…)`
                            // arguments.
                            Expr::SafeCall {
                                receiver,
                                name,
                                args,
                            } => {
                                let mut arguments = args.unwrap_or_default();
                                arguments.push(lambda);
                                self.file.add_expr(
                                    Expr::SafeCall {
                                        receiver,
                                        name,
                                        args: Some(arguments),
                                    },
                                    Span::new(lspan.lo, end.hi),
                                )
                            }
                            _ => unreachable!("append target was checked above"),
                        }
                    } else {
                        self.file.add_expr(
                            Expr::Call {
                                callee: old,
                                args: vec![lambda],
                            },
                            Span::new(lspan.lo, end.hi),
                        )
                    };
                    if append_to_existing_call {
                        // Carry call-owned metadata only when the old call was rebuilt. For an
                        // invocation of a parenthesized result, `old` remains a live inner call and
                        // must retain its own names, spans, and explicit type arguments.
                        if let Some(mut names) = self.file.call_arg_names.remove(&old.0) {
                            names.push(None);
                            self.file.call_arg_names.insert(lhs.0, names);
                        }
                        if let Some(mut spans) = self.file.call_arg_name_spans.remove(&old.0) {
                            spans.push(None);
                            self.file.call_arg_name_spans.insert(lhs.0, spans);
                        }
                        self.file.empty_call_open_paren_spans.remove(&old.0);
                        if let Some(span) = rebuilt_safe_call_name_span {
                            self.file.exact_member_name_spans.remove(&old.0);
                            self.file.exact_member_name_spans.insert(lhs.0, span);
                        }
                        self.file.call_has_trailing_lambda.remove(&old.0);
                        if let Some(type_arguments) = self.file.call_type_args.remove(&old.0) {
                            self.file.call_type_args.insert(lhs.0, type_arguments);
                        }
                    }
                    // Mark this call as having a SYNTACTIC trailing lambda so default-omission lowering
                    // binds it to the callee's LAST parameter (preceding gaps take defaults).
                    self.file.call_has_trailing_lambda.insert(lhs.0);
                    if let Some(close_paren_end) = close_paren_end {
                        self.file
                            .trailing_call_close_paren_ends
                            .insert(lhs.0, close_paren_end);
                    }
                    // The no-parentheses `f<T>{…}` form still has pending type arguments. Type
                    // arguments already owned by a rebuilt call were moved above; an inner
                    // parenthesized call deliberately keeps its own entry.
                    if !pending_targs.is_empty() {
                        self.file
                            .call_type_args
                            .insert(lhs.0, std::mem::take(&mut pending_targs));
                    }
                }
                // `array[index]` element access, or `receiver[i, j, …]` — a multi-index `get` operator.
                TokenKind::LBracket => {
                    self.bump();
                    self.skip_newlines();
                    let mut indices = vec![self.parse_expr()];
                    self.skip_newlines();
                    while self.eat(TokenKind::Comma) {
                        self.skip_newlines();
                        if self.at(TokenKind::RBracket) {
                            break; // tolerate a trailing comma
                        }
                        indices.push(self.parse_expr());
                        self.skip_newlines();
                    }
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    let end = self.tok().span;
                    self.expect(TokenKind::RBracket, "']'");
                    let span = Span::new(lspan.lo, end.hi);
                    lhs = self.file.add_expr(
                        Expr::Index {
                            array: lhs,
                            indices,
                        },
                        span,
                    );
                }
                // `expr<TypeArgs>(args)` — generic call with explicit type arguments.
                // Disambiguate from `a < b > c` (two comparisons) by checking whether a balanced
                // `>` is immediately followed by `(`, `{`, or `.` (call-like context).
                TokenKind::Lt if self.lookahead_is_type_args_call() => {
                    // Capture the explicit type arguments for the call that follows.
                    pending_targs = self.parse_type_args();
                }
                _ => break,
            }
        }
        lhs
    }

    fn parse_primary(&mut self) -> ExprId {
        let span = self.tok().span;
        // `try { … } catch (e: T) { … }` — a soft keyword followed by a block.
        if self.at(TokenKind::Ident)
            && self.keyword_text("try")
            && self
                .t
                .get(self.i + 1)
                .is_some_and(|t| t.kind == TokenKind::LBrace)
        {
            return self.parse_try();
        }
        match self.kind() {
            // Collection-literal `[a, b, …]`. Keep the ordinary call operand layout so generic
            // expression walkers remain exhaustive; the sparse marker below tells semantic checking
            // to ignore this provisional callee and select the language-defined target factory.
            TokenKind::LBracket => {
                self.bump(); // '['
                self.skip_newlines();
                let mut args = Vec::new();
                while !self.at(TokenKind::RBracket) && !self.at(TokenKind::Eof) {
                    args.push(self.parse_expr());
                    self.skip_newlines();
                    if self.at(TokenKind::Comma) {
                        self.bump();
                        self.skip_newlines();
                    } else {
                        break;
                    }
                }
                let end = self.tok().span;
                self.expect(TokenKind::RBracket, "']'");
                let fname = if args.is_empty() {
                    "emptyArray"
                } else {
                    "arrayOf"
                };
                let callee = self.file.add_expr(Expr::Name(fname.to_string()), span);
                let call = self
                    .file
                    .add_expr(Expr::Call { callee, args }, Span::new(span.lo, end.hi));
                self.file.collection_literal_calls.insert(call.0);
                call
            }
            TokenKind::IntLit => {
                let v = parse_int_literal(self.text());
                self.bump();
                // Values outside the i32 range are Long literals in Kotlin (no L suffix needed).
                if v > i32::MAX as i64 || v < i32::MIN as i64 {
                    self.file.add_expr(Expr::LongLit(v), span)
                } else {
                    self.file.add_expr(Expr::IntLit(v), span)
                }
            }
            TokenKind::LongLit => {
                let t = self.text();
                let v = parse_int_literal(&t[..t.len() - 1]); // strip trailing `L`
                self.bump();
                self.file.add_expr(Expr::LongLit(v), span)
            }
            TokenKind::UIntLit => {
                let v = parse_unsigned_literal_bits(self.text()); // suffix stripped inside
                self.bump();
                // A `U`-suffixed literal (no `L`) is `UInt` if it fits, else `ULong` (Kotlin's rule):
                // e.g. `0xffff_ffff_ffffU` exceeds `UInt.MAX` so it's a `ULong`.
                if v > u32::MAX as u64 {
                    self.file.add_expr(Expr::ULongLit(v as i64), span)
                } else {
                    self.file.add_expr(Expr::UIntLit(v as i64), span)
                }
            }
            TokenKind::ULongLit => {
                let v = parse_unsigned_literal_bits(self.text()) as i64;
                self.bump();
                self.file.add_expr(Expr::ULongLit(v), span)
            }
            TokenKind::DoubleLit => {
                let v = self.text().parse::<f64>().unwrap_or(0.0);
                self.bump();
                self.file.add_expr(Expr::DoubleLit(v), span)
            }
            TokenKind::FloatLit => {
                // strip the trailing `f`/`F` suffix
                let t = self.text();
                let v = t[..t.len() - 1].parse::<f32>().unwrap_or(0.0);
                self.bump();
                self.file.add_expr(Expr::FloatLit(v), span)
            }
            TokenKind::StringLit => {
                let raw = self.text();
                let v = unquote(raw);
                self.bump();
                self.file.add_expr(Expr::StringLit(v), span)
            }
            TokenKind::CharLit => {
                let raw = self.text();
                let c = unquote_char(raw);
                self.bump();
                self.file.add_expr(Expr::CharLit(c), span)
            }
            TokenKind::TemplateStart | TokenKind::RawTemplateStart => self.parse_template(),
            TokenKind::KwTrue => {
                self.bump();
                self.file.add_expr(Expr::BoolLit(true), span)
            }
            TokenKind::KwFalse => {
                self.bump();
                self.file.add_expr(Expr::BoolLit(false), span)
            }
            TokenKind::KwNull => {
                self.bump();
                self.file.add_expr(Expr::NullLit, span)
            }
            // An anonymous object expression `object : Super(args)? { … }` (in value position).
            TokenKind::Ident
                if self.keyword_text("object")
                    && self.t.get(self.i + 1).is_some_and(|t| {
                        matches!(t.kind, TokenKind::Colon | TokenKind::LBrace)
                    }) =>
            {
                self.parse_anon_object(span)
            }
            // A suspending lambda literal `suspend { … }` (in value position): the `suspend`
            // modifier marks the following lambda — recorded in a side-table so the checker types
            // it as a `suspend (…) -> …` function type and the lowerer builds a SuspendLambda
            // state machine for it (otherwise it misparses as a call to a fn named `suspend`).
            TokenKind::Ident
                if self.keyword_text("suspend")
                    && self
                        .t
                        .get(self.i + 1)
                        .is_some_and(|t| t.kind == TokenKind::LBrace) =>
            {
                self.bump(); // 'suspend'
                let lambda = self.parse_lambda();
                self.file.suspend_lambdas.insert(lambda.0);
                lambda
            }
            TokenKind::Ident => {
                let mut n = self.text().to_string();
                self.bump();
                // A TYPED super (`super<Base>.foo()`): the `<Base>` type qualifier selects WHICH
                // supertype's method to dispatch to. Encode it on the name (`super<Base>`) so the
                // checker/lowerer pick that interface's default; may be followed by a `@label`.
                if n == "super" && self.at(TokenKind::Lt) {
                    self.bump(); // '<'
                    let ty = self.parse_qualified_name();
                    let simple = ty.rsplit('.').next().unwrap_or(&ty).to_string();
                    self.expect(TokenKind::Gt, "'>'");
                    n = format!("super<{simple}>");
                }
                // A LABELED `this`/`super` (`this@Outer`, `super@Base`): the `@label` qualifies which
                // enclosing receiver / supertype it denotes. Capture it on the name (`this@Outer`) so the
                // checker/lowerer can resolve the label; a bare `this`/`super` stays unchanged.
                if (n == "this" || n == "super" || n.starts_with("super<"))
                    && self.at(TokenKind::At)
                    && self
                        .t
                        .get(self.i + 1)
                        .is_some_and(|t| t.kind == TokenKind::Ident)
                {
                    self.bump(); // '@'
                    let label = self.text().to_string();
                    self.bump(); // label
                    return self.file.add_expr(Expr::Name(format!("{n}@{label}")), span);
                }
                self.file.add_expr(Expr::Name(n), span)
            }
            TokenKind::LParen => {
                self.bump();
                self.skip_newlines();
                let e = self.parse_expr();
                self.skip_newlines();
                self.expect(TokenKind::RParen, "')'");
                self.parenthesized_expressions.insert(e.0);
                e
            }
            TokenKind::KwIf => self.parse_if(),
            TokenKind::KwWhen => self.parse_when(),
            TokenKind::LBrace => self.parse_lambda(),
            // Anonymous function expression: `fun (params): T = expr` / `fun (params): T { … }`. It
            // desugars to a lambda carrying explicit parameter types; a bare `return` in the block body
            // returns from the anonymous function, exactly as it does from a lambda compiled to its own
            // `invoke`. The receiver form `fun R.(…)` is not desugared here yet.
            TokenKind::KwFun => self.parse_anon_fun(Vec::new()),
            // `::name` — top-level callable reference / class literal without a receiver.
            TokenKind::ColonColon => {
                self.bump(); // '::'
                let name_token = self.tok();
                let name_span = self.syntactic_ident_span(name_token);
                let name = if self.at(TokenKind::Ident) {
                    let n = self.text().to_string();
                    self.bump();
                    n
                } else if self.at(TokenKind::KwClass) {
                    self.bump();
                    "class".to_string()
                } else {
                    "<error>".to_string()
                };
                let end = self.t[self.i.saturating_sub(1)].span;
                let reference = self.file.add_expr(
                    Expr::CallableRef {
                        receiver: None,
                        name,
                    },
                    Span::new(span.lo, end.hi),
                );
                self.file
                    .exact_member_name_spans
                    .insert(reference.0, name_span);
                reference
            }
            _ => {
                self.diags.error(span, "expected an expression");
                self.bump();
                self.file.add_expr(Expr::Name("<error>".to_string()), span)
            }
        }
    }

    fn parse_if(&mut self) -> ExprId {
        let start = self.tok().span;
        self.bump(); // 'if'
        self.expect(TokenKind::LParen, "'('");
        // The condition may start (and end) on a fresh line: `if(\n  a && b\n)`. Skip newlines around it.
        self.skip_newlines();
        let cond = self.parse_expr();
        self.skip_newlines();
        self.expect(TokenKind::RParen, "')'");
        self.skip_newlines();
        let then_branch = self.parse_branch(true);
        // optional else (may be on the next line)
        let save = self.i;
        self.skip_newlines();
        // An `if` used as a `when`-branch body without its own else (`when { x -> if (c) a; else -> b }`)
        // must NOT swallow the `when`'s `else` entry: an `else` immediately followed by `->` is a when
        // entry, not this `if`'s else (a real if-else branch never begins with `->`).
        let else_is_when_entry = self.at(TokenKind::KwElse) && {
            let mut j = self.i + 1;
            while self.t.get(j).is_some_and(|t| t.kind == TokenKind::Newline) {
                j += 1;
            }
            self.t.get(j).is_some_and(|t| t.kind == TokenKind::Arrow)
        };
        let else_branch = if !else_is_when_entry && self.eat(TokenKind::KwElse) {
            self.skip_newlines();
            Some(self.parse_branch(true))
        } else {
            self.i = save;
            None
        };
        let end = self.t[self.i.saturating_sub(1)].span;
        self.file.add_expr(
            Expr::If {
                cond,
                then_branch,
                else_branch,
            },
            Span::new(start.lo, end.hi),
        )
    }

    /// Parse a string template: `TemplateStart (StrChunk | Dollar Ident | Dollar { expr })* TemplateEnd`.
    fn parse_template(&mut self) -> ExprId {
        let start = self.tok().span;
        if self.text().starts_with('$') && !self.multi_dollar_interpolation {
            self.diags.error(
                start,
                "multi-dollar string interpolation is disabled by the language feature set",
            );
        }
        // A raw (triple-quoted) template's chunks are verbatim — no escape processing.
        let raw = self.kind() == TokenKind::RawTemplateStart;
        self.bump(); // TemplateStart / RawTemplateStart
        let mut parts = Vec::new();
        loop {
            match self.kind() {
                TokenKind::StrChunk => {
                    let text = self.text();
                    let piece = if raw {
                        KtString::from(text)
                    } else {
                        unescape_chunk(text)
                    };
                    parts.push(TemplatePart::Str(piece));
                    self.bump();
                }
                TokenKind::Dollar => {
                    self.bump();
                    if self.eat(TokenKind::LBrace) {
                        // `"${" NL* expression NL* "}"` — line breaks may surround the
                        // interpolated expression (a multiline lambda inside a raw template is
                        // the common shape). Plain newlines only: an explicit `;` still ends the
                        // expression and errors at the expected `}`.
                        self.skip_plain_newlines();
                        let e = self.parse_expr();
                        self.skip_plain_newlines();
                        self.expect(TokenKind::RBrace, "'}'");
                        parts.push(TemplatePart::Expr(e));
                    } else if self.at(TokenKind::Ident) {
                        let sp = self.tok().span;
                        let n = self.text().to_string();
                        self.bump();
                        let e = self.file.add_expr(Expr::Name(n), sp);
                        parts.push(TemplatePart::Expr(e));
                    }
                }
                TokenKind::TemplateEnd => {
                    self.bump();
                    break;
                }
                TokenKind::Eof => break,
                _ => {
                    self.bump(); // recover
                }
            }
        }
        let end = self.t[self.i.saturating_sub(1)].span;
        self.file
            .add_expr(Expr::Template(parts), Span::new(start.lo, end.hi))
    }

    /// `try { … } catch (e: T) { … } …` — krusty supports one or more `catch` clauses; `finally` is
    /// rejected (it needs duplicated-block / catch-all-rethrow lowering not yet implemented).
    fn parse_try(&mut self) -> ExprId {
        let start = self.tok().span;
        self.bump(); // 'try'
        self.skip_newlines();
        let body = self.parse_block_expr(true);
        let mut catches = Vec::new();
        let mut finally = None;
        loop {
            let save = self.i;
            self.skip_newlines();
            if self.at(TokenKind::Ident) && self.keyword_text("catch") {
                self.bump(); // 'catch'
                self.expect(TokenKind::LParen, "'('");
                // The parameter may sit on its own line(s) inside the parens (`catch (\n e: E\n)`),
                // so skip newlines around each part exactly as an ordinary parameter list allows.
                self.skip_newlines();
                // Annotations on the catch parameter (`catch (@Marker e: E)`): consume and discard —
                // a catch parameter is never referenced by annotation, so its markers carry no codegen.
                let param_start = self.tok().span;
                while self.at(TokenKind::At) {
                    self.parse_annotation();
                    self.skip_newlines();
                }
                let name = self.ident_or_error("catch parameter name");
                self.skip_newlines();
                self.expect(TokenKind::Colon, "':'");
                self.skip_newlines();
                let ty = self.parse_type();
                self.skip_newlines();
                // A trailing comma is allowed (`catch (e: E,)`), matching Kotlin's parameter lists.
                if self.eat(TokenKind::Comma) {
                    self.skip_newlines();
                }
                self.expect(TokenKind::RParen, "')'");
                self.skip_newlines();
                let cbody = self.parse_block_expr(true);
                let param_span = Span::new(param_start.lo, ty.span.hi);
                catches.push(CatchClause {
                    name,
                    ty,
                    body: cbody,
                    param_span,
                });
            } else if self.at(TokenKind::Ident) && self.keyword_text("finally") {
                self.bump(); // 'finally'
                self.skip_newlines();
                finally = Some(self.parse_block_expr(false));
                break; // `finally` is always last
            } else {
                self.i = save;
                break;
            }
        }
        if catches.is_empty() && finally.is_none() {
            self.diags
                .error(start, "expected 'catch' or 'finally' after try body");
        }
        let end = self.t[self.i.saturating_sub(1)].span;
        self.file.add_expr(
            Expr::Try {
                body,
                catches,
                finally,
            },
            Span::new(start.lo, end.hi),
        )
    }

    fn parse_when(&mut self) -> ExprId {
        let start = self.tok().span;
        self.bump(); // 'when'
                     // `when (val v = e) { … }` — a subject variable. Desugar to `{ val v = e; when (v) { … } }`:
                     // parse the binding here, use a `Name(v)` reference as the subject, then wrap the whole `when`
                     // in a block holding the `val` so every downstream path (smart-casts, `is` arms) sees a local.
        let mut subject_var: Option<(StmtId, ExprId)> = None;
        let subject = if self.eat(TokenKind::LParen) {
            // The subject (or subject-variable binding) may start on a fresh line: `when(\n  val v = e\n)`.
            self.skip_newlines();
            if self.at(TokenKind::KwVal) || self.at(TokenKind::KwVar) {
                let vstart = self.tok().span;
                let is_var = self.at(TokenKind::KwVar);
                self.bump(); // 'val' / 'var'
                let name = self.ident_or_error("variable name");
                let ty = if self.eat(TokenKind::Colon) {
                    self.skip_plain_newlines();
                    Some(self.parse_type())
                } else {
                    None
                };
                self.skip_plain_newlines_before(TokenKind::Eq);
                self.expect(TokenKind::Eq, "'='");
                // A `when` subject initializer may start on the next line.
                self.skip_newlines();
                let init = self.parse_expr();
                self.skip_newlines();
                self.expect(TokenKind::RParen, "')'");
                let sp = Span::new(vstart.lo, self.file.expr_spans[init.0 as usize].hi);
                let stmt = self.file.add_stmt(
                    Stmt::Local {
                        is_var,
                        name: name.clone(),
                        ty,
                        init,
                    },
                    sp,
                );
                let nm = self.file.add_expr(Expr::Name(name), sp);
                subject_var = Some((stmt, nm));
                Some(nm)
            } else {
                let e = self.parse_expr();
                self.skip_newlines();
                self.expect(TokenKind::RParen, "')'");
                Some(e)
            }
        } else {
            None
        };
        self.skip_newlines();
        self.expect(TokenKind::LBrace, "'{'");
        let mut arms = Vec::new();
        loop {
            self.skip_newlines();
            if self.at(TokenKind::RBrace) || self.at(TokenKind::Eof) {
                break;
            }
            let mut conditions = Vec::new();
            if self.eat(TokenKind::KwElse) {
                // else arm — no conditions
            } else {
                conditions.push(self.parse_when_condition(subject));
                while self.eat(TokenKind::Comma) {
                    self.skip_newlines();
                    if subject.is_some() && self.at(TokenKind::Arrow) {
                        break;
                    }
                    conditions.push(self.parse_when_condition(subject));
                }
            }
            let guard = if self.eat(TokenKind::KwIf) {
                if !self.when_guards {
                    self.diags.error(
                        self.t[self.i.saturating_sub(1)].span,
                        "when guards are disabled by the language feature set",
                    );
                }
                Some(self.parse_expr())
            } else {
                None
            };
            self.expect(TokenKind::Arrow, "'->'");
            self.skip_newlines();
            let body = self.parse_branch(true);
            arms.push(WhenArm {
                conditions,
                guard,
                body,
            });
        }
        let end = self.tok().span;
        self.expect(TokenKind::RBrace, "'}'");
        let span = Span::new(start.lo, end.hi);
        let when_expr = self.file.add_expr(Expr::When { subject, arms }, span);
        match subject_var {
            Some((stmt, _)) => self.file.add_expr(
                Expr::Block {
                    stmts: vec![stmt],
                    trailing: Some(when_expr),
                },
                span,
            ),
            None => when_expr,
        }
    }

    /// A single `when`-arm condition. In the subject form, `is T` / `!is T` becomes a type test
    /// against the subject (`Expr::Is` whose operand is the subject expression); otherwise a value
    /// matched by `==`.
    fn parse_when_condition(&mut self, subject: Option<ExprId>) -> WhenCondition {
        let negated = if self.at(TokenKind::Ident) && self.keyword_text("is") {
            Some(false)
        } else if self.at(TokenKind::Not)
            && self
                .t
                .get(self.i + 1)
                .is_some_and(|t| self.token_keyword_text(*t, "is"))
        {
            Some(true)
        } else {
            None
        };
        if let (Some(negated), Some(subj)) = (negated, subject) {
            let start = self.tok().span;
            if negated {
                self.bump(); // '!'
            }
            self.bump(); // 'is'
            let ty = self.parse_type();
            let end = self.t[self.i.saturating_sub(1)].span;
            return WhenCondition::Predicate(self.file.add_expr(
                Expr::Is {
                    operand: subj,
                    ty,
                    negated,
                },
                Span::new(start.lo, end.hi),
            ));
        }
        // `when (x) { in range -> … }` / `!in` — a membership condition on the subject (`x in range`),
        // mirroring the infix `in`/`!in` operator: a range RHS → `InRange`, any other RHS → `contains`.
        let in_negated = if self.at(TokenKind::KwIn) {
            Some(false)
        } else if self.at(TokenKind::Not)
            && self
                .t
                .get(self.i + 1)
                .is_some_and(|t| t.kind == TokenKind::KwIn)
        {
            Some(true)
        } else {
            None
        };
        if let (Some(negated), Some(subj)) = (in_negated, subject) {
            let start = self.tok().span;
            if negated {
                self.bump(); // '!'
            }
            self.bump(); // 'in'
            self.skip_newlines();
            let rstart = self.parse_bp(9);
            let kind = if self.eat(TokenKind::DotDot) {
                Some(RangeKind::Through)
            } else if self.eat(TokenKind::DotDotLt) {
                Some(RangeKind::OpenEnd)
            } else if self.at(TokenKind::Ident) && self.text() == "until" {
                self.bump();
                Some(RangeKind::Until)
            } else if self.at(TokenKind::Ident) && self.text() == "downTo" {
                self.bump();
                Some(RangeKind::DownTo)
            } else {
                None
            };
            let expression = match kind {
                Some(kind) => {
                    let rend = self.parse_bp(9);
                    let end = self.file.expr_spans[rend.0 as usize];
                    self.file.add_expr(
                        Expr::InRange {
                            value: subj,
                            start: rstart,
                            end: rend,
                            kind,
                            negated,
                        },
                        Span::new(start.lo, end.hi),
                    )
                }
                None => {
                    let cspan = self.file.expr_spans[rstart.0 as usize];
                    let callee = self.file.add_expr(
                        Expr::Member {
                            receiver: rstart,
                            name: "contains".to_string(),
                        },
                        Span::new(start.lo, cspan.hi),
                    );
                    let call = self.file.add_expr(
                        Expr::Call {
                            callee,
                            args: vec![subj],
                        },
                        Span::new(start.lo, cspan.hi),
                    );
                    if negated {
                        self.file.add_expr(
                            Expr::Unary {
                                op: UnOp::Not,
                                operand: call,
                            },
                            Span::new(start.lo, cspan.hi),
                        )
                    } else {
                        call
                    }
                }
            };
            return WhenCondition::Predicate(expression);
        }
        let expression = self.parse_expr();
        if subject.is_some() {
            WhenCondition::SubjectEquals(expression)
        } else {
            WhenCondition::Predicate(expression)
        }
    }

    /// A branch/body of `if`/`when`/`for`: a block, or a single statement. A bare expression keeps
    /// its value (exposed as the wrapping block's trailing value); a real statement (`return`,
    /// assignment, `s += i`, …) yields a Unit-valued block.
    /// At a `{`, whether it opens a LAMBDA (a top-level `->` precedes the matching `}`) rather than a
    /// block. Used to disambiguate a lambda branch body from a statement block.
    fn at_lambda_brace(&self) -> bool {
        self.lambda_arrow_before_close(self.i + 1)
    }

    /// Whether the supertype entry at the current position is a FUNCTION TYPE (`() -> R`, `(A) -> R`,
    /// `Recv.() -> R`, `suspend (A) -> R`) rather than a class/interface name — detected by a depth-0
    /// `->` before the entry's terminator (`,`, `{`, `by`/`where`, or end). A regular supertype (even
    /// a generic `Base<A>` or a base-class call `Base(args)`) has no top-level `->` (a lambda argument's
    /// arrow sits inside `(`/`{`, at depth > 0), so this cleanly distinguishes the two.
    pub(super) fn at_function_type_supertype(&self) -> bool {
        let mut j = self.i;
        let mut depth = 0i32;
        loop {
            match self.t.get(j).map(|t| t.kind) {
                None => return false,
                Some(TokenKind::Arrow) if depth == 0 => return true,
                Some(TokenKind::LBrace) if depth == 0 => return false, // class body
                // A closing `}` at depth 0 is the END of the enclosing class body — the supertype
                // clause is over. Stop here rather than decrementing into negative depth and scanning
                // on into the NEXT top-level declaration, where an unrelated `->` (a `when` arm or a
                // lambda) would be misread as this supertype's function-type arrow.
                Some(TokenKind::RBrace) if depth == 0 => return false,
                Some(TokenKind::Comma) if depth == 0 => return false, // next supertype
                Some(TokenKind::Newline) if depth == 0 => {
                    if self.t[j].text(self.src) == ";" {
                        return false;
                    }
                    let next = self.after_plain_newlines(j);
                    if self.token_starts_declaration_at(next) {
                        // A bodyless declaration is complete here. Do not scan into a following
                        // declaration whose own function type or lambda happens to contain `->`.
                        return false;
                    }
                }
                // Track generic-argument brackets too (`<` is a single `Lt`, `>` a single `Gt`), so a
                // function type used as a type ARGUMENT — `Base<() -> R>` — keeps its `->` at depth > 0
                // and is NOT mistaken for a function-type supertype.
                Some(
                    TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace | TokenKind::Lt,
                ) => depth += 1,
                Some(
                    TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace | TokenKind::Gt,
                ) => depth -= 1,
                // `by` (delegation) / `where` (constraints) terminate a plain-name supertype entry.
                Some(TokenKind::Ident)
                    if depth == 0 && matches!(self.t[j].text(self.src), "by" | "where") =>
                {
                    return false
                }
                _ => {}
            }
            j += 1;
        }
    }

    /// Whether the balanced parenthesized group starting at token index `at` (which must be `(`) is
    /// immediately followed by `->` — i.e. it is a function type's parameter list, not an annotation
    /// argument list. Used to disambiguate `@Ann("a") String` (annotation args) from
    /// `@Composable () -> Unit` (function type).
    pub(super) fn paren_group_precedes_arrow(&self, at: usize) -> bool {
        let mut j = at;
        let mut depth = 0i32;
        loop {
            match self.t.get(j).map(|t| t.kind) {
                None => return false,
                Some(TokenKind::LParen) => depth += 1,
                Some(TokenKind::RParen) => {
                    depth -= 1;
                    if depth == 0 {
                        return self
                            .t
                            .get(j + 1)
                            .is_some_and(|t| t.kind == TokenKind::Arrow);
                    }
                }
                _ => {}
            }
            j += 1;
        }
    }

    /// Whether a top-level `->` (a lambda's parameter arrow) precedes the matching `}`, scanning from
    /// token index `from`. Distinguishes a lambda `{ p -> … }` from a statement block. A lambda's
    /// parameter list (everything before its arrow) is only names, `:`, commas, destructuring parens,
    /// and parameter TYPES — never a `val`/`var`/`=`/statement keyword. Hitting one at depth 0 before
    /// any arrow means the arrow belongs to a nested function TYPE inside a statement
    /// (`{ val u: (Int) -> Unit = … }`), so this is a BLOCK, not a lambda.
    pub(super) fn lambda_arrow_before_close(&self, from: usize) -> bool {
        let mut j = from;
        let mut depth = 0i32;
        loop {
            match self.t.get(j).map(|t| t.kind) {
                None => return false,
                Some(
                    TokenKind::KwVal
                    | TokenKind::KwVar
                    | TokenKind::KwReturn
                    | TokenKind::KwWhile
                    | TokenKind::KwFor
                    | TokenKind::KwDo
                    | TokenKind::KwIf
                    | TokenKind::KwWhen
                    | TokenKind::KwClass
                    | TokenKind::KwFun
                    | TokenKind::KwImport
                    | TokenKind::KwPackage
                    | TokenKind::Eq,
                ) if depth == 0 => return false,
                // An `object` expression (`{ object : () -> Unit { … } }`): `object` is a hard keyword,
                // never a lambda parameter name, so a `->` reached inside its supertype/body belongs to a
                // function-type supertype — the brace is a BLOCK, not a lambda.
                Some(TokenKind::Ident) if depth == 0 && self.t[j].text(self.src) == "object" => {
                    return false
                }
                Some(TokenKind::Arrow) if depth == 0 => return true,
                Some(TokenKind::RBrace) if depth == 0 => return false,
                Some(TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace) => depth += 1,
                Some(TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace) => depth -= 1,
                _ => {}
            }
            j += 1;
        }
    }

    pub(super) fn parse_branch(&mut self, trailing_is_value: bool) -> ExprId {
        if self.at(TokenKind::LBrace) {
            // A branch body `{ … }` is a BLOCK — unless it is a LAMBDA (`when (x) { … -> { _ -> body } }`
            // returning a function type), detected by a top-level `->` before the closing `}`.
            if self.at_lambda_brace() {
                return self.parse_lambda();
            }
            return self.parse_block_expr(trailing_is_value);
        }
        // A bare branch is an EXPRESSION position. A modifier SOFT KEYWORD that ends the line
        // (`… else value`) is a name closing the branch, and the declaration on the next line
        // belongs to the enclosing scope — but the statement parser's modifier-prefix scan looks
        // ACROSS newlines for a declaration keyword (it has to: `suspend\nfun local()` inside a
        // block is one declaration) and would swallow that next declaration into this branch.
        // The branch then had a declaration's `Unit` value, so `if (c) "s" else value` typed as
        // `Any` and the enclosing function reported a return-type mismatch pointing at a span that
        // covered the following declaration.
        if self.at_modifier()
            && self
                .t
                .get(self.i + 1)
                .is_some_and(|token| token.kind == TokenKind::Newline)
        {
            return self.parse_expr();
        }
        let start = self.tok().span;
        let s = self.parse_stmt();
        // A bare expression branch stays a bare expression (its value is the branch value);
        // a real statement (`return`, assignment, `s += i`, …) becomes a Unit-valued block.
        if let Stmt::Expr(e) = self.file.stmt(s) {
            return *e;
        }
        let end = self.t[self.i.saturating_sub(1)].span;
        self.file.add_expr(
            Expr::Block {
                stmts: vec![s],
                trailing: None,
            },
            Span::new(start.lo, end.hi),
        )
    }
}
