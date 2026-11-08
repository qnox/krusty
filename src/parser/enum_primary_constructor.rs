//! An enum class's primary-constructor parameter list. Besides the parameters, it keeps the
//! closing `)`'s offset: kotlinc maps the `$default` constructor's `return` there.

use super::*;

impl Parser<'_> {
    /// Parse `(params)` positioned on the `(`, returning the parameters and the offset of the
    /// closing `)`.
    pub(super) fn parse_enum_primary_constructor_parameters(&mut self) -> (Vec<PropParam>, u32) {
        self.bump(); // '('
        let mut props = Vec::new();
        self.skip_semicolons();
        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            let declaration_start = self.tok().span.lo;
            self.begin_declaration_prefix();
            let epmods = self.skip_decl_prefix();
            self.record_declaration_prefix(declaration_start);
            let is_vararg = epmods.iter().any(|m| m == "vararg");
            let is_property = self.at(TokenKind::KwVal) || self.at(TokenKind::KwVar);
            let is_var = self.at(TokenKind::KwVar);
            if is_property {
                self.record_external_property_parameter(&epmods);
                self.bump();
            }
            let pspan = self.tok().span;
            let pname = self.ident_or_error("parameter name");
            self.expect(TokenKind::Colon, "':'");
            self.skip_semicolons(); // a wrapped declaration puts the type on the next line (`val x:\n  T`)
            let ty = self.parse_vararg_aware_type(is_vararg);
            // A default value (`enum class C(val x: Int = 1)`) — same as a regular class ctor param;
            // each enum entry that omits the argument gets it at its construction site.
            let default = if self.eat(TokenKind::Eq) {
                self.skip_semicolons();
                Some(self.parse_expr())
            } else {
                None
            };
            let declaration_end = self.t[self.i.saturating_sub(1)].span.hi;
            props.push(PropParam {
                name: pname,
                ty,
                span: pspan,
                declaration_span: Span::new(declaration_start, declaration_end),
                decl_line: 0,
                decl_start_line: 0,
                is_vararg,
                is_var,
                is_property,
                is_script_parameter: false,
                is_actual: epmods.iter().any(|modifier| modifier == "actual"),
                is_override: epmods.iter().any(|modifier| modifier == "override"),
                has_open_modifier: epmods.iter().any(|modifier| modifier == "open"),
                is_final: epmods.iter().any(|modifier| modifier == "final"),
                is_open: !epmods.iter().any(|modifier| modifier == "final")
                    && epmods
                        .iter()
                        .any(|modifier| modifier == "open" || modifier == "override"),
                visibility: visibility_of(&epmods),
                has_visibility_modifier: has_visibility_modifier(&epmods),
                default,
                annotations: Vec::new(),
                annotation_args: Vec::new(),
            });
            self.skip_semicolons();
            if !self.eat(TokenKind::Comma) {
                break;
            }
            self.skip_semicolons();
        }
        // As for a class, kotlinc maps the primary constructor `$default`'s `return` to `)`.
        let close = self.tok().span.lo;
        self.expect(TokenKind::RParen, "')'");
        (props, close)
    }
}
