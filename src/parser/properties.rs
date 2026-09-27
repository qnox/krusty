//! Property declarations and their accessor syntax.

use super::*;

impl Parser<'_> {
    pub(super) fn parse_top_property_c(
        &mut self,
        is_lateinit: bool,
        _abstract_ok: bool,
        is_const: bool,
        is_abstract: bool,
    ) -> PropDecl {
        let annotations = self.take_pending_annotations();
        let annotation_args = self.take_pending_annotation_args();
        let context_params = std::mem::take(&mut self.pending_context_params);
        let start = self.tok().span;
        let declaration_start = match self.member_declaration_prefix.take() {
            Some((prefix_start, prefix_end)) if prefix_end == self.i => prefix_start,
            _ => start.lo,
        };
        let is_var = self.at(TokenKind::KwVar);
        self.bump(); // val/var
                     // Optional generic type parameters on an extension property (`val <T> T.foo: T`) —
                     // erased, but retained so they scope over the receiver, type, and accessor bodies.
        let (type_params, _tp_non_null, reified_type_params, mut type_param_bounds, _) =
            if self.at(TokenKind::Lt) {
                self.parse_type_params(start.lo)
            } else {
                Default::default()
            };
        let lexical_type_param_lens =
            self.push_lexical_type_params(&type_params, &type_param_bounds);
        while self.at(TokenKind::At) {
            let _ = self.parse_annotation(); // receiver use-site annotation
            self.skip_plain_newlines();
        }
        let (receiver, name) = self.parse_receiver_and_declaration_name("property name");
        let name_span = self.declaration_name_span;
        let ty = if self.eat(TokenKind::Colon) {
            // `… (':' NL* type)?` — the type may start on the next line, which is where a formatter
            // puts a long generic one. An explicit `;` still ends the declaration.
            self.skip_plain_newlines();
            Some(self.parse_type())
        } else {
            None
        };
        let init_operator = self.eat_initializer_eq();
        let mut init = if init_operator.is_some() {
            self.skip_newlines();
            Some(self.parse_expr())
        } else {
            None
        };
        if let (Some(operator), Some(init)) = (init_operator, init) {
            self.file.value_operator_spans.insert(init.0, operator);
        }
        // `val x: T by <expr>` — a delegated property (in place of `= init`). Reads/writes route through
        // the delegate's `getValue`/`setValue` operators.
        let delegate_start = self.i;
        self.skip_plain_newlines();
        let mut delegate_by_span = None;
        let delegate = if init.is_none() && self.at(TokenKind::Ident) && self.keyword_text("by") {
            // The `by` keyword owns every diagnostic about the delegate as a delegate — kotlinc
            // anchors "cannot serve as a delegate" here, not on the expression or the property.
            delegate_by_span = Some(self.tok().span);
            self.bump(); // 'by'
            self.skip_newlines();
            Some(self.parse_expr())
        } else {
            self.i = delegate_start;
            None
        };
        type_param_bounds.extend(self.parse_where_clause(&type_params, &name));
        // Optional custom accessors: `get() = expr` / `get() { … }` and/or `[private] set(v) { … }`
        // / `private set`. Either order; at most one of each. An accessor begins with `get`/`set`
        // (optionally preceded by a visibility modifier) — anything else ends the property.
        let mut getter: Option<FunBody> = None;
        let mut getter_declared = false;
        let mut getter_span: Option<Span> = None;
        let mut getter_inline = false;
        let mut getter_ty: Option<TypeRef> = None;
        let mut setter: Option<PropAccessor> = None;
        let mut explicit_backing_field = None;
        let mut accessor_external = false;
        'accessors: loop {
            let save = self.i;
            self.skip_newlines();
            let mut visibility = None;
            let mut is_inline = false;
            let mut is_external = false;
            loop {
                self.skip_newlines();
                if self.at(TokenKind::At) {
                    let Some(end) = self.annotation_end_at(self.i) else {
                        self.i = save;
                        break 'accessors;
                    };
                    self.i = end;
                    continue;
                }
                if self.at(TokenKind::Ident)
                    && self.keyword_text_any(&[
                        "private",
                        "protected",
                        "internal",
                        "public",
                        "inline",
                        "external",
                    ])
                {
                    for (keyword, declared) in [
                        ("private", Visibility::Private),
                        ("protected", Visibility::Protected),
                        ("internal", Visibility::Internal),
                        ("public", Visibility::Public),
                    ] {
                        if self.keyword_text(keyword) {
                            visibility = Some(declared);
                        }
                    }
                    is_inline |= self.keyword_text("inline");
                    is_external |= self.keyword_text("external");
                    self.bump();
                    continue;
                }
                break;
            }
            if self.explicit_backing_fields
                && visibility != Some(Visibility::Private)
                && explicit_backing_field.is_none()
                && self.at(TokenKind::Ident)
                && self.keyword_text("field")
                && self
                    .t
                    .get(self.i + 1)
                    .is_some_and(|t| matches!(t.kind, TokenKind::Colon | TokenKind::Eq))
            {
                self.bump();
                let field_ty = if self.eat(TokenKind::Colon) {
                    Some(self.parse_type())
                } else {
                    None
                };
                let field_init_operator = self.eat_span(TokenKind::Eq);
                let field_init = if field_init_operator.is_some() {
                    self.skip_newlines();
                    Some(self.parse_expr())
                } else {
                    None
                };
                if init.is_some() && field_init.is_some() {
                    self.diags.error(
                        self.tok().span,
                        "krusty: a property with an explicit backing field cannot also have its own initializer",
                    );
                } else if let Some(field_init) = field_init {
                    if let Some(operator) = field_init_operator {
                        self.file
                            .value_operator_spans
                            .insert(field_init.0, operator);
                    }
                    init = Some(field_init);
                }
                explicit_backing_field = Some(ExplicitBackingField { ty: field_ty });
                continue;
            }
            if !self.at(TokenKind::Ident) || !self.keyword_text_any(&["get", "set"]) {
                self.i = save; // not an accessor — restore (incl. any consumed newlines/modifier)
                break;
            }
            let is_get = self.keyword_text("get");
            let accessor_keyword = self.tok().span;
            self.bump(); // 'get' / 'set'
            accessor_external |= is_external;
            if is_get {
                getter_declared = true;
                getter_inline = is_inline;
                // A custom getter is `get() = expr` / `get() { … }`. A bare `get` or a `get()` with
                // no body is the (redundant) explicit DEFAULT getter — consume its optional `()` and
                // leave `getter` unset (the property keeps its default field accessor).
                let had_parens = self.eat_accessor_parens(false).is_some();
                // The accessor header — keyword through its parameter list, before any declared
                // return type or body. A diagnostic about the accessor points at this, not at the
                // property, so it is captured while the tokens are in hand.
                getter_span = Some(Span::new(
                    accessor_keyword.lo,
                    self.t[self.i.saturating_sub(1)].span.hi,
                ));
                getter_ty = self.eat(TokenKind::Colon).then(|| self.parse_type());
                if self.at(TokenKind::Eq) || self.at(TokenKind::LBrace) {
                    getter = Some(self.parse_accessor_body());
                } else if had_parens {
                    // `get()` with parens but no body is invalid (a bare `get` — no parens — is the
                    // default accessor).
                    self.diags.error(
                        self.tok().span,
                        "expected '=' or '{' for a property getter".to_string(),
                    );
                }
                let _ = visibility; // getter visibility not modeled (rare); ignored
            } else {
                // setter: optional `(param)` then optional body; `private set` has neither.
                let param = self.parse_setter_param();
                let accessor_span = Span::new(
                    accessor_keyword.lo,
                    self.t[self.i.saturating_sub(1)].span.hi,
                );
                let body = if self.eat(TokenKind::Eq) {
                    self.skip_newlines();
                    Some(FunBody::Expr(self.parse_expr()))
                } else if self.at(TokenKind::LBrace) {
                    Some(FunBody::Block(self.parse_block_expr(false)))
                } else {
                    None // default-bodied setter (e.g. `private set`)
                };
                setter = Some(PropAccessor {
                    param,
                    span: accessor_span,
                    body,
                    visibility,
                    is_inline,
                });
            }
        }
        // Initializer requirements are semantic. The syntax layer retains a missing initializer for
        // abstract/expect/external members, deferred initialization, delegated/default-accessor
        // combinations, and invalid neighboring forms alike; signature collection/checking decides
        // which declaration contexts permit it.
        let end = self.t[self.i.saturating_sub(1)].span;
        let getter_reads_field = getter
            .as_ref()
            .and_then(|g| match g {
                FunBody::Expr(e) | FunBody::Block(e) => Some(*e),
                FunBody::None => None,
            })
            .is_some_and(|e| self.expr_reads_field(e));
        self.pop_lexical_type_params(lexical_type_param_lens);
        PropDecl {
            is_open: false,
            name,
            annotations,
            annotation_args,
            context_params,
            decl_line: 0,
            visibility: Visibility::Public,
            type_params,
            type_param_bounds,
            reified_type_params,
            receiver,
            ty,
            is_var,
            is_companion_extension: false,
            is_companion_block_member: false,
            is_override: false,
            is_lateinit,
            is_external: accessor_external,
            is_expect: false,
            is_actual: false,
            getter,
            getter_declared,
            getter_span,
            getter_inline,
            getter_ty,
            getter_reads_field,
            setter,
            is_const,
            is_abstract,
            delegate,
            delegate_by_span,
            explicit_backing_field,
            init,
            span: Span::new(start.lo, end.hi),
            declaration_span: Span::new(declaration_start, end.hi),
            name_span,
        }
    }
}
