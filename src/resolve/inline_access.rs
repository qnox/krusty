//! Public-API inline access diagnostics.
//!
//! The checker consumes resolved declaration visibility and annotation identities. JVM accessor
//! representation is a backend concern and does not participate in this decision.

use super::*;

impl Checker<'_> {
    /// Public or protected visibility, or `@PublishedApi` on an `internal` declaration. The
    /// annotation is the resolved classifier identity already stored on the candidate, never the
    /// source spelling.
    pub(super) fn is_public_api_for_inline_access(
        visibility: Visibility,
        annotations: &[TypeName],
    ) -> bool {
        visibility.is_public_api()
            || annotations
                .iter()
                .any(|annotation| *annotation == crate::types::wk::published_api())
    }

    pub(super) fn declaration_is_public_api_inline(
        &self,
        scope: &CheckerScope<'_>,
        function: &FunDecl,
    ) -> bool {
        let annotations = function
            .annotations
            .iter()
            .filter_map(|annotation| self.annotation_identity_in_scope(scope, annotation))
            .collect::<Vec<_>>();
        function.is_inline()
            && Self::is_public_api_for_inline_access(function.visibility, &annotations)
    }

    pub(super) fn enter_public_api_inline(
        &mut self,
        scope: &CheckerScope<'_>,
        function: &FunDecl,
    ) -> bool {
        let entered = self.declaration_is_public_api_inline(scope, function);
        if entered {
            self.public_api_inline_depth += 1;
        }
        entered
    }

    pub(super) fn leave_public_api_inline(&mut self, entered: bool) {
        if entered {
            self.public_api_inline_depth -= 1;
        }
    }

    /// A public-API `inline` function publishes its body into every caller. A non-public-API
    /// callee is rejected at the reference. `@PublishedApi internal` is public API for this
    /// check. An inline callee names the transitive form, because its own body would be
    /// published too.
    pub(super) fn reject_non_public_api_from_public_inline(
        &mut self,
        call: ExprId,
        callee_is_public_api: bool,
        inline: InlineKind,
    ) {
        if self.public_api_inline_depth == 0 || callee_is_public_api {
            return;
        }
        if !self.public_inline_access_calls.insert(call) {
            return;
        }
        let message = if inline.can_inline() {
            "public-API inline function cannot access non-public-API inline function as it could transitively access non-public-API declarations."
        } else {
            "public-API inline function cannot access non-public-API function."
        };
        self.diags
            .error(self.call_callee_name_span(call), message.to_string());
    }
}
