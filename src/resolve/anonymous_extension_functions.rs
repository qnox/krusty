//! Checking of an anonymous extension function, `context(c: C) fun R.(v: V) = …`.
//!
//! The declaration writes its receiver and parameters in its own syntax, so its shape comes from the
//! declaration rather than from an expected function type. Its context parameters are named leading
//! parameters (the parser prepends them to the value parameters); its extension receiver follows
//! them, matching kotlinc's `context(C) R.(V) -> T` function type.

use super::*;

impl Checker<'_> {
    /// Type an anonymous extension function whose receiver type reference is `receiver_ref`. Feeds
    /// the declared shape into the ordinary receiver-lambda checker: it owns implicit `this`, member
    /// lookup, the function-type receiver bit, and lowerer's receiver binding metadata.
    pub(super) fn check_anonymous_extension_function(
        &mut self,
        scope: &CheckerScope<'_>,
        e: ExprId,
        receiver_ref: &TypeRef,
        implicit_label: Option<&str>,
    ) -> Ty {
        let receiver = self.type_ref_ty(scope, receiver_ref);
        let declared = self
            .file
            .lambda_param_types
            .get(&e.0)
            .cloned()
            .unwrap_or_default();
        let parameter_types = declared
            .iter()
            .map(|parameter| {
                parameter
                    .as_ref()
                    .map(|ty| self.type_ref_ty(scope, ty))
                    .unwrap_or_else(|| Ty::obj("kotlin/Any"))
            })
            .collect::<Vec<_>>();
        let context_count = self
            .file
            .anon_fun_context_count
            .get(&e.0)
            .copied()
            .unwrap_or(0) as usize;
        let (context_types, value_types) =
            parameter_types.split_at(context_count.min(parameter_types.len()));
        self.check_lambda_with_implicit_receivers_and_return_labeled(
            scope,
            e,
            LambdaShape {
                context_types,
                extension_receiver: Some(receiver),
                value_types,
            },
            implicit_label,
            LambdaCheckMode {
                suspend: false,
                coerce_return_to_unit: false,
                result_constraint: LambdaResultConstraint::Open,
            },
        )
    }
}
