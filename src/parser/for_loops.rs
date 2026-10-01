//! `for` statements: the loop header, its range or iterable, and the destructuring a
//! `for ((a, b) in xs)` loop prepends to its body.

use super::*;

impl Parser<'_> {
    pub(super) fn parse_for(&mut self, start: Span, label: Option<String>) -> StmtId {
        self.bump(); // 'for'
        self.expect(TokenKind::LParen, "'('");
        // A destructuring loop variable — `for ((a, b) in pairs)` or `for ([a, b] in pairs)` —
        // desugars to a synthetic temp plus `val (a, b) = temp` prepended to the body (reusing the
        // `Stmt::Destructure` machinery; both forms lower to the same positional `componentN` calls,
        // so the bytecode matches kotlinc's either way).
        let close = if self.at(TokenKind::LParen) {
            Some(TokenKind::RParen)
        } else if self.at(TokenKind::LBracket) {
            // Always parse the brackets. Without the feature this is kotlinc's language-version
            // error, and the rest of the file — including declarations after the loop — stays bound.
            self.note_ungated_bracket_destructure();
            Some(TokenKind::RBracket)
        } else {
            None
        };
        let destructure: Option<DestructureEntries> = if let Some(close) = close {
            self.bump();
            let mut entries = Vec::new();
            // Parallel by-name source properties (`for ((a = prop) in …)`); `None` for a positional entry.
            let mut source_props: Vec<Option<String>> = Vec::new();
            let mut entry_types: Vec<Option<TypeRef>> = Vec::new();
            loop {
                // Full form (`for ([val a, val b] in …)`): each component carries its own
                // `val`/`var`. A full-form PAREN component binds by property name; the classic
                // keyword-less short form stays positional (unless the short-form flag is on).
                let is_var = self.at(TokenKind::KwVar);
                let had_kw = is_var || self.at(TokenKind::KwVal);
                if had_kw {
                    self.bump();
                }
                let ignored =
                    self.at(TokenKind::Ident) && self.text() == "_" && !self.escaped_ident();
                let n = self.ident_or_error("variable name");
                let mut entry_type = self.eat(TokenKind::Colon).then(|| self.parse_type());
                let source = if self.name_based_destructuring && self.eat(TokenKind::Eq) {
                    let src = self.ident_or_error("property name");
                    if self.eat(TokenKind::Colon) {
                        entry_type = Some(self.parse_type());
                    }
                    Some(src)
                } else if close == TokenKind::RParen && (self.short_form_destructuring || had_kw) {
                    Some(n.clone())
                } else {
                    None
                };
                entries.push(DestructureEntry {
                    name: n,
                    mutable: is_var,
                    ignored,
                });
                source_props.push(source);
                entry_types.push(entry_type);
                if !self.eat(TokenKind::Comma) {
                    break;
                }
                if self.at(close) {
                    break;
                }
            }
            self.expect(
                close,
                if close == TokenKind::RParen {
                    "')'"
                } else {
                    "']'"
                },
            );
            Some((entries, source_props, entry_types))
        } else {
            None
        };
        let destructured = destructure.is_some();
        let name = match &destructure {
            Some(_) => format!("$dest${}", start.lo),
            None => {
                let n = self.ident_or_error("loop variable");
                // An explicit loop-variable type — `for (i: Int in xs)`. The variable's type is the
                // iterable's element type; the annotation only widens it (`for (c: Char? in str)`), so
                // parse and discard it, mirroring the destructuring path above.
                if self.eat(TokenKind::Colon) {
                    let _ = self.parse_type();
                }
                n
            }
        };
        self.expect(TokenKind::KwIn, "'in'");
        // Parse the iterable / range start at additive precedence so the `..`/`until`/`downTo`
        // operator is left for the `for`-specific range handling below (not swallowed into a
        // `RangeTo` value expression).
        let rstart = self.parse_bp(9);
        // A parenthesized or control-flow range bound may end on its own line before the range
        // operator. Physical newlines are allowed here; an explicit semicolon still terminates the
        // iterable expression and is deliberately left visible.
        self.skip_plain_newlines();
        let kind = if self.eat(TokenKind::DotDot) {
            RangeKind::Through
        } else if self.eat(TokenKind::DotDotLt) {
            RangeKind::OpenEnd
        } else if self.at(TokenKind::Ident) && self.text() == "until" {
            self.bump();
            RangeKind::Until
        } else if self.at(TokenKind::Ident) && self.text() == "downTo" {
            self.bump();
            RangeKind::DownTo
        } else {
            // No range operator: a plain iterable. It may still carry trailing infix calls that the
            // bp-9 start didn't consume (`for (x in progression step 2)`, `… step 2 step 0`) — continue
            // them so the whole expression (e.g. `progression.step(2)`) becomes the ForEach iterable.
            let mut rstart = self.parse_for_trailing_infix(rstart);
            // The iterable start was parsed at additive precedence (bp 9) so the range operators above
            // stay visible. When it is a plain iterable (no range), lower-precedence operators the bp-9
            // start left behind still belong to it — notably an elvis `?:` (`for (v in foo() ?: continue)`).
            // Fold the Elvis chain here so the whole expression becomes the ForEach iterable. The
            // bp-9 range probe intentionally stopped just above the Elvis/infix boundary; the RHS
            // re-enters that boundary so chains stay right-associative and ranges bind tighter.
            while self.at(TokenKind::Question)
                && self
                    .t
                    .get(self.i + 1)
                    .is_some_and(|t| t.kind == TokenKind::Colon)
            {
                self.bump(); // '?'
                self.bump(); // ':'
                self.skip_newlines();
                let rhs = self.parse_bp(8);
                let lspan = self.file.expr_spans[rstart.0 as usize];
                let rspan = self.file.expr_spans[rhs.0 as usize];
                rstart = self.file.add_expr(
                    Expr::Elvis { lhs: rstart, rhs },
                    Span::new(lspan.lo, rspan.hi),
                );
            }
            self.expect(TokenKind::RParen, "')'");
            let body = self.parse_loop_body();
            let body = self.desugar_destructure_body(&name, destructure, body);
            // Iterate over `rstart`: the checker decides whether it is a counted progression.
            return self.finish_loop(
                Stmt::ForEach {
                    name,
                    iterable: rstart,
                    body,
                    label,
                },
                start,
                destructured,
            );
        };
        let rend = self.parse_bp(9);
        // A `..`/`until`/`downTo` range may be followed by ordinary infix calls (`step`, or any user
        // infix), possibly chained (`a..b step 2 step 3`). These are NOT special syntax — recognizing
        // them here by name would be the hardcode kotlinc avoids. Build the base range value and apply
        // any trailing infix generically; the result's TYPE (e.g. `IntProgression` from `step`) drives
        // the loop lowering. A bare range (no trailing infix) keeps the optimized counted `Stmt::For`.
        if self.at(TokenKind::Ident) && {
            let next = self.infix_operand_follows();
            !self.keyword_text_any(&["is", "as", "in"]) && next
        } {
            let lspan = self.file.expr_spans[rstart.0 as usize];
            let rspan = self.file.expr_spans[rend.0 as usize];
            let base_span = Span::new(lspan.lo, rspan.hi);
            // Preserve the source declaration kind: only `..`/`..<` are operators. `until` and
            // `downTo` are ordinary infix calls and must enter normal callable resolution.
            let base = match kind {
                RangeKind::Until | RangeKind::DownTo => {
                    let name = match kind {
                        RangeKind::Until => "until",
                        RangeKind::DownTo => "downTo",
                        _ => unreachable!(),
                    };
                    let callee = self.file.add_expr(
                        Expr::Member {
                            receiver: rstart,
                            name: name.to_string(),
                        },
                        base_span,
                    );
                    self.file.add_expr(
                        Expr::Call {
                            callee,
                            args: vec![rend],
                        },
                        base_span,
                    )
                }
                k => self.file.add_expr(
                    Expr::RangeTo {
                        lo: rstart,
                        hi: rend,
                        kind: k,
                    },
                    base_span,
                ),
            };
            let iterable = self.parse_for_trailing_infix(base);
            self.expect(TokenKind::RParen, "')'");
            let body = self.parse_loop_body();
            let body = self.desugar_destructure_body(&name, destructure, body);
            return self.finish_loop(
                Stmt::ForEach {
                    name,
                    iterable,
                    body,
                    label,
                },
                start,
                destructured,
            );
        }
        self.expect(TokenKind::RParen, "')'");
        let body = self.parse_loop_body();
        let body = self.desugar_destructure_body(&name, destructure, body);
        self.finish_loop(
            Stmt::For {
                name,
                range: ForRange {
                    start: rstart,
                    end: rend,
                    kind,
                },
                body,
                label,
            },
            start,
            destructured,
        )
    }

    /// Finish a `for` statement, recording whether its variable is a destructuring pattern.
    fn finish_loop(&mut self, statement: Stmt, start: Span, destructured: bool) -> StmtId {
        let statement = self.finish_stmt(statement, start);
        if destructured {
            self.file.destructuring.loops.insert(statement);
        }
        statement
    }

    /// For a destructuring `for ((a, b) in …)`, prepend `val (a, b) = <temp>` to the loop body so the
    /// component names are bound from the synthetic loop variable. A no-op when not destructuring.
    fn desugar_destructure_body(
        &mut self,
        temp: &str,
        destructure: Option<DestructureEntries>,
        body: ExprId,
    ) -> ExprId {
        let Some((entries, source_props, entry_types)) = destructure else {
            return body;
        };
        let sp = self.file.expr_spans[body.0 as usize];
        let temp_expr = self.file.add_expr(Expr::Name(temp.to_string()), sp);
        let dstmt = self.file.add_stmt(
            Stmt::Destructure {
                entries,
                init: temp_expr,
            },
            sp,
        );
        if source_props.iter().any(|s| s.is_some()) {
            self.file
                .destructuring
                .source_properties
                .insert(dstmt.0, source_props);
        }
        if entry_types.iter().any(Option::is_some) {
            self.file
                .destructuring
                .entry_types
                .insert(dstmt.0, entry_types);
        }
        match self.file.expr(body).clone() {
            Expr::Block { stmts, trailing } => {
                let mut s2 = vec![dstmt];
                s2.extend(stmts);
                self.file.add_expr(
                    Expr::Block {
                        stmts: s2,
                        trailing,
                    },
                    sp,
                )
            }
            _ => self.file.add_expr(
                Expr::Block {
                    stmts: vec![dstmt],
                    trailing: Some(body),
                },
                sp,
            ),
        }
    }
}
