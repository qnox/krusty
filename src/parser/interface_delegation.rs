//! Interface delegation clauses (`: Iface by delegate`) in a class header.

use super::*;

impl Parser<'_> {
    /// Class delegation: `: Iface by delegate`, after the supertype `interface` was consumed.
    /// Preserves both the simple-name field form and a general delegate expression;
    /// representation support belongs to later phases.
    pub(super) fn parse_interface_delegation(
        &mut self,
        supertype: Option<u32>,
        interface: String,
        has_primitive_type_argument: bool,
    ) -> Option<InterfaceDelegation> {
        if !(self.at(TokenKind::Ident) && self.keyword_text("by")) {
            return None;
        }
        self.bump();
        // A following `{` opens the CLASS BODY, not a lambda on the delegate call.
        let saved = self.no_trailing_lambda;
        self.no_trailing_lambda = true;
        let value = self.parse_expr();
        self.no_trailing_lambda = saved;
        // Parentheses leave no node, so `by d` and `by (d)` both name the delegate directly; any
        // other shape (`by Impl()`, `by a.b`, …) is an EXPRESSION delegate.
        let bare_name = match self.file.expr(value) {
            Expr::Name(name) => Some(name.clone()),
            _ => None,
        };
        Some(InterfaceDelegation {
            supertype,
            interface,
            value,
            bare_name,
            has_primitive_type_argument,
        })
    }
}
