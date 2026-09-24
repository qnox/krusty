//! What a typealias constructor call fixes about the classifier it constructs.
//!
//! A call through a generic alias that omits the alias's type arguments (`LinkedHashMap(map)`, where
//! `typealias LinkedHashMap<K, V> = java.util.LinkedHashMap<K, V>`) expands to the alias target with
//! the alias's own formals still open. Those formals are type variables of this call, exactly like
//! the constructed classifier's formals in a direct `java.util.LinkedHashMap(map)` call: the value
//! arguments and the expected type decide them. Only the target arguments that do not mention an
//! open formal (`String` in `typealias Keyed<V> = Entry<String, V>`) are fixed by the alias itself.

use super::*;

impl Checker<'_> {
    /// The target application a constructor call through the alias `name` denotes, or `None` when
    /// no alias of that spelling is in scope. Omitted alias arguments stay open in the returned
    /// expansion and are recorded for the call; constructor selection reads them back through
    /// [`Self::alias_constructor_fixed_application`].
    pub(super) fn scoped_source_alias_call_ty(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        name: &str,
        expected: Option<Ty>,
    ) -> Option<Ty> {
        let (formals, expansion) = if let Some(alias) = scope.type_alias(name) {
            (alias.formals, alias.expansion)
        } else if let Some(alias) = self.qualified_body_local_type_alias(scope, name) {
            (alias.formals, alias.expansion)
        } else {
            let identity = self.scoped_source_alias_identity(scope, name)?;
            self.source_alias_expansion(identity)?
        };
        let arguments = self
            .file
            .call_type_args
            .get(&call.0)
            .cloned()
            .unwrap_or_default();
        // Constructor syntax may omit a generic alias's arguments and infer them from value
        // arguments (`AliasedCell(MyClass())`). This early probe exists only to recognize aliases
        // of compiler-defined arrays; diagnosing the empty argument list here rejects an ordinary
        // alias before constructor inference sees it. Preserve the declaration-owned expansion with
        // its open alias formals. If it is not an array, the caller falls through to normal
        // constructor selection; if it is, the array constructor consumes the same semantic shape.
        if arguments.is_empty() && !formals.is_empty() {
            let signature = GenericSig {
                formals: formals.clone(),
                formal_bounds: vec![vec![Ty::nullable(Ty::obj("kotlin/Any"))]; formals.len()],
                receiver: None,
                params: Vec::new(),
                ret: expansion,
                return_policy: GenericReturnPolicy::Exact,
            };
            self.contextual_constructor_signatures
                .insert(call, signature.clone());
            // Omitted alias arguments participate in the constructor call's expected-type
            // inference. Keep the alias declaration's own variables as the shape side of
            // unification: `Alias()`, where `Alias<Y> = Pair<Y, Concrete>`, under an expected
            // `Pair<X, T>` becomes `Pair<X, Concrete>`. Treating the bare expansion as already
            // applied leaves the unrelated declaration name `Y` in the checked expression and
            // later compares it nominally with `X` instead of sharing the call constraint.
            let bindings = expected
                .filter(|expected| *expected != Ty::Error)
                .and_then(|expected| {
                    let source = self.fed_source();
                    crate::symbol_resolver::infer_generic_return_bindings_from_symbols(
                        &source,
                        &signature,
                        expected.non_null(),
                        |actual, bound| self.receiver_is_assignable(actual, bound),
                    )
                })
                .unwrap_or_default();
            return Some(crate::symbol_resolver::ty_subst_keep_unbound(
                expansion, &bindings,
            ));
        }
        Some(self.alias_application_ty(
            scope,
            formals,
            expansion,
            name,
            &arguments,
            self.call_callee_name_span(call),
        ))
    }

    /// The facet of an alias constructor application that constructor selection may treat as
    /// written type arguments. Every target argument that still mentions one of the alias's open
    /// formals is an inference position of this call and is reported as `Ty::Error`, the marker an
    /// `_` placeholder uses. An application whose alias arguments were written, or were completed by
    /// the expected type, is returned unchanged.
    pub(super) fn alias_constructor_fixed_application(
        &self,
        call: ExprId,
        applied: Option<Ty>,
    ) -> Option<Ty> {
        let applied = applied?;
        let Some(open) = self
            .contextual_constructor_signatures
            .get(&call)
            .map(|signature| signature.formals.as_slice())
        else {
            return Some(applied);
        };
        let Ty::Obj(owner, arguments) = applied else {
            return Some(applied);
        };
        if !arguments
            .iter()
            .any(|argument| ty_mentions_param(*argument, open))
        {
            return Some(applied);
        }
        let fixed = arguments
            .iter()
            .map(|&argument| {
                if ty_mentions_param(argument, open) {
                    Ty::Error
                } else {
                    argument
                }
            })
            .collect::<Vec<_>>();
        Some(Ty::obj_args_name(owner, &fixed))
    }

    /// The result constraint of an alias constructor call. A fixed facet with inference positions
    /// does not constrain the result; the call's own expected type does.
    pub(super) fn alias_constructor_result_constraint(
        fixed: Option<Ty>,
        expected: Option<Ty>,
    ) -> Option<Ty> {
        fixed
            .filter(|applied| !applied.mentions_error())
            .or(expected)
    }
}
