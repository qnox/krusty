//! A class's or enum class's primary-constructor parameter list. Both declarations share the
//! `classParameters` grammar, so one production parses each parameter's annotations, modifiers,
//! `val`/`var`, type, and default. Besides the parameters, it keeps the closing `)`'s offset:
//! kotlinc maps the `$default` constructor's `return` there.

use super::*;

/// The parsed `(params)` of a primary constructor.
pub(super) struct PrimaryConstructorParameters {
    pub(super) parameters: Vec<PropParam>,
    /// The offset of the closing `)`.
    pub(super) close_lo: u32,
    /// The span from the opening `(` through the closing `)`, when the `)` is present.
    pub(super) span: Option<Span>,
}

impl Parser<'_> {
    /// Parse `(params)` positioned on the `(`.
    pub(super) fn parse_primary_constructor_parameters(&mut self) -> PrimaryConstructorParameters {
        let open = self.tok().span.lo;
        self.bump(); // '('
        let mut parameters = Vec::new();
        self.skip_semicolons();
        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            parameters.push(self.parse_primary_constructor_parameter());
            self.skip_semicolons();
            if !self.eat(TokenKind::Comma) {
                break;
            }
            self.skip_semicolons();
        }
        let close = self.tok().span;
        let span = self
            .expect(TokenKind::RParen, "')'")
            .then(|| Span::new(open, close.hi));
        PrimaryConstructorParameters {
            parameters,
            close_lo: close.lo,
            span,
        }
    }

    fn parse_primary_constructor_parameter(&mut self) -> PropParam {
        let declaration_start = self.tok().span.lo;
        let mut annotations = Vec::new();
        let mut annotation_args = Vec::new();
        let mut modifiers = Vec::new();
        self.begin_declaration_prefix();
        if self.at(TokenKind::At)
            || (self.at(TokenKind::Ident) && self.at_modifier() && self.text() != "value")
        {
            modifiers = self.skip_decl_prefix(); // `private val x`, `@Anno val y`, `vararg xs`
            annotations = self.take_pending_annotations();
            annotation_args = self.take_pending_annotation_args();
        }
        self.record_declaration_prefix(declaration_start);
        let is_vararg = modifiers.iter().any(|m| m == "vararg");
        let (is_property, is_var) = match self.kind() {
            TokenKind::KwVal => {
                self.bump();
                (true, false)
            }
            TokenKind::KwVar => {
                self.bump();
                (true, true)
            }
            _ => (false, false), // a plain constructor parameter (not a property)
        };
        if is_property {
            self.record_external_property_parameter(&modifiers);
        }
        let span = self.tok().span;
        let name = self.ident_or_error("parameter name");
        self.expect(TokenKind::Colon, "':'");
        self.skip_semicolons(); // a wrapped declaration puts the type on the next line (`val x:\n  T`)
        let ty = self.parse_vararg_aware_type(is_vararg);
        let default = if self.eat(TokenKind::Eq) {
            self.skip_semicolons();
            Some(self.parse_expr())
        } else {
            None
        };
        let declaration_end = self.t[self.i.saturating_sub(1)].span.hi;
        let has = |modifier: &str| modifiers.iter().any(|m| m == modifier);
        PropParam {
            name,
            ty,
            span,
            declaration_span: Span::new(declaration_start, declaration_end),
            decl_line: 0,
            decl_start_line: 0,
            is_vararg,
            is_var,
            is_property,
            is_script_parameter: false,
            is_actual: has("actual"),
            is_override: has("override"),
            has_open_modifier: has("open"),
            is_final: has("final"),
            is_open: !has("final") && (has("open") || has("override")),
            visibility: visibility_of(&modifiers),
            has_visibility_modifier: has_visibility_modifier(&modifiers),
            default,
            annotations,
            annotation_args,
        }
    }
}
