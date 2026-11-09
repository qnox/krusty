use crate::integer_constant::IntegerConstant;
use crate::libraries::{GenericSig, SemanticPlatform};
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TyKind, TypeName};

use super::{infer_generic_return_bindings, semantic_arg_assignable};

#[derive(Clone, Debug)]
pub(crate) enum CallArgKind {
    /// A fully inferred non-lambda expression type.
    Typed(Ty),
    /// A spread argument (`*xs`); its type is the array being spread.
    Spread(Ty),
    /// A lambda literal whose function shape may still contain `Ty::Error` probes.
    LambdaLiteral(Ty),
    /// A callable reference retains both its nominal reflection type and exact function shape.
    CallableReference { nominal: Ty, function: Ty },
    /// A generic nested call whose result awaits an enclosing expected parameter type.
    ExpectedTypeCallable {
        provisional: Ty,
        generic_sig: std::sync::Arc<GenericSig>,
        /// Whether `provisional` still mentions a formal the nested call's own inputs left
        /// unsolved. A type parameter lexically in scope at the call is the enclosing
        /// declaration's fixed type, not an open one.
        provisional_open: bool,
    },
    /// A safely folded integer constant and its ordinary runtime type.
    IntegerLiteral { ty: Ty, constant: IntegerConstant },
    /// A collection literal whose construction waits for the selected parameter. `provisional` is
    /// its expectation-free reading (`List<T>`); `factory_classifiers` are the classifiers among
    /// the candidates' parameters that supply a literal construction (a standard factory or an
    /// `operator fun of`). Arrays always supply one.
    CollectionLiteral {
        provisional: Ty,
        factory_classifiers: std::sync::Arc<[TypeName]>,
    },
    /// A legal declaration-default slot produced by the shared argument mapper.
    OmittedDefault,
}

impl CallArgKind {
    /// This argument with every type it carries rewritten by `map`.
    pub(crate) fn map_types(self, map: impl Fn(Ty) -> Ty) -> Self {
        match self {
            Self::Typed(ty) => Self::Typed(map(ty)),
            Self::Spread(ty) => Self::Spread(map(ty)),
            Self::LambdaLiteral(ty) => Self::LambdaLiteral(map(ty)),
            Self::CallableReference { nominal, function } => Self::CallableReference {
                nominal: map(nominal),
                function: map(function),
            },
            Self::ExpectedTypeCallable {
                provisional,
                generic_sig,
                provisional_open,
            } => Self::ExpectedTypeCallable {
                provisional: map(provisional),
                generic_sig,
                provisional_open,
            },
            Self::IntegerLiteral { ty, constant } => Self::IntegerLiteral {
                ty: map(ty),
                constant,
            },
            Self::CollectionLiteral {
                provisional,
                factory_classifiers,
            } => Self::CollectionLiteral {
                provisional: map(provisional),
                factory_classifiers,
            },
            Self::OmittedDefault => Self::OmittedDefault,
        }
    }

    #[cfg(test)]
    pub(crate) fn integer_literal(ty: Ty, value: i32) -> Self {
        let constant = if ty.is_unsigned() {
            u64::try_from(value)
                .map(IntegerConstant::Unsigned)
                .unwrap_or(IntegerConstant::Signed(value))
        } else {
            IntegerConstant::Signed(value)
        };
        Self::integer_constant(ty, constant)
    }

    pub(crate) fn integer_constant(ty: Ty, constant: IntegerConstant) -> Self {
        Self::IntegerLiteral { ty, constant }
    }

    pub(crate) fn ty(&self) -> Ty {
        match self {
            Self::Typed(ty)
            | Self::Spread(ty)
            | Self::LambdaLiteral(ty)
            | Self::IntegerLiteral { ty, .. } => *ty,
            Self::CallableReference { nominal, .. } => *nominal,
            Self::ExpectedTypeCallable { provisional, .. }
            | Self::CollectionLiteral { provisional, .. } => *provisional,
            // This probe never becomes an expression type. Applicability recognizes the explicit
            // variant and generic inference skips it.
            Self::OmittedDefault => Ty::Error,
        }
    }

    pub(crate) fn function_type(&self) -> Option<Ty> {
        match self {
            Self::CallableReference { function, .. } => Some(*function),
            kind => kind.ty().is_fun().then(|| kind.ty()),
        }
    }

    /// Whether this argument can be passed to a parameter of type `parameter` when overload
    /// applicability is decided: an omitted default always, an unchecked lambda literal when the
    /// parameter can host a function value, a function-shaped argument by SAM conversion, and
    /// otherwise the argument's type for that parameter (an integer literal adapts) by
    /// assignability.
    pub(crate) fn fits_parameter(
        &self,
        lib: &dyn SemanticPlatform,
        src: &dyn SymbolSource,
        parameter: Ty,
    ) -> bool {
        if self.is_omitted_default() || self.constructible_as(parameter) {
            return true;
        }
        if self.is_lambda_literal() && self.ty() == Ty::Error {
            return super::untyped_lambda_pertinent(lib, src, parameter);
        }
        let function = self.function_type().unwrap_or_else(|| self.ty());
        let sam = (self.is_lambda_literal() || self.function_type().is_some())
            && super::sam_arg_matches(lib, src, parameter, function);
        sam || super::arg_fits_source(lib, src, &parameter, &self.type_for(parameter))
    }

    pub(crate) fn type_for(&self, parameter: Ty) -> Ty {
        if self.is_omitted_default() {
            return parameter;
        }
        let concrete = parameter.non_null();
        if self.adapts_integer_literal_to(concrete) {
            return concrete;
        }
        if let TyKind::TyParam(_, bound) | TyKind::Var(_, bound) = parameter.non_null().kind() {
            if self.adapts_integer_literal_to(bound) {
                return bound;
            }
        }
        if parameter.non_null().is_fun() {
            self.function_type().unwrap_or_else(|| self.ty())
        } else {
            self.ty()
        }
    }

    /// Semantic input contributed by this argument to generic call inference.
    ///
    /// A callable reference has a nominal reflection type for ordinary assignability, but a
    /// functional-interface parameter is constrained by the reference's exact function shape.
    /// Keeping this distinction here prevents each top-level/member/static selection path from
    /// independently deciding whether to feed `KFunctionN` or `(P) -> R` into the generic solver.
    pub(crate) fn inference_type(&self, source: &dyn SymbolSource, parameter: Ty) -> Ty {
        if let Some(sam) = super::semantic_sam_signature(source, parameter) {
            // A solved application is the exact interface: `Worker<TokenA>` does not instantiate
            // `Worker<TokenB>`. An unsolved `Worker<T>` has no application yet, so any
            // instantiation of that classifier is this value's constraint rather than its
            // function supertype.
            let nominal = self.ty();
            let already = if parameter.mentions_ty_param() {
                self.nominal_implements_classifier(source, sam.internal)
            } else {
                !matches!(
                    nominal.non_null().kind(),
                    TyKind::Error | TyKind::Pending | TyKind::Fun(_)
                ) && !nominal.mentions_pending()
                    && semantic_arg_assignable(source, &parameter, &nominal)
            };
            if already {
                return nominal;
            }
            self.function_type()
                .unwrap_or_else(|| self.type_for(parameter))
        } else {
            self.type_for(parameter)
        }
    }

    /// Whether this argument's nominal type is the fun interface `target`, or a subtype of it.
    fn nominal_implements_classifier(&self, source: &dyn SymbolSource, target: TypeName) -> bool {
        let start = self.ty().non_null();
        if matches!(
            start.kind(),
            TyKind::Error | TyKind::Pending | TyKind::Fun(_)
        ) || start.mentions_pending()
        {
            return false;
        }
        let mut pending = vec![start];
        let mut seen = Vec::new();
        while let Some(ty) = pending.pop() {
            let ty = ty.non_null();
            if seen.contains(&ty) {
                continue;
            }
            seen.push(ty);
            if let TyKind::Obj(name, _) = ty.kind() {
                if name == target {
                    return true;
                }
            }
            pending.extend(super::direct_supertypes(source, ty));
        }
        false
    }

    pub(crate) fn is_spread(&self) -> bool {
        matches!(self, Self::Spread(_))
    }

    pub(crate) fn is_lambda_literal(&self) -> bool {
        matches!(self, Self::LambdaLiteral(_))
    }

    /// This argument as overload applicability sees it before any lambda body is analyzed: a
    /// checked lambda literal keeps its inputs, and its result becomes an open probe.
    pub(crate) fn with_postponed_lambda_result(&self) -> Self {
        let Self::LambdaLiteral(ty) = self else {
            return self.clone();
        };
        let TyKind::Fun(signature) = ty.kind() else {
            return self.clone();
        };
        Self::LambdaLiteral(Ty::fun_with_shape(
            signature.params.clone(),
            Ty::Error,
            signature.context_count,
            signature.has_receiver,
            signature.suspend,
        ))
    }

    pub(crate) fn is_integer_literal(&self) -> bool {
        matches!(self, Self::IntegerLiteral { .. })
    }

    pub(crate) fn is_expected_type_callable(&self) -> bool {
        matches!(self, Self::ExpectedTypeCallable { .. })
    }

    pub(crate) fn is_omitted_default(&self) -> bool {
        matches!(self, Self::OmittedDefault)
    }

    /// Whether a collection literal can be built for `parameter`. Its provisional `List<T>` is
    /// not applicability evidence against a parameter that supplies its own construction, just
    /// as an unchecked lambda's placeholder shape is not.
    pub(crate) fn constructible_as(&self, parameter: Ty) -> bool {
        let Self::CollectionLiteral {
            factory_classifiers,
            ..
        } = self
        else {
            return false;
        };
        let parameter = parameter.non_null();
        parameter.array_elem().is_some()
            || parameter
                .kotlin_class_internal()
                .is_some_and(|classifier| factory_classifiers.contains(&classifier))
    }

    /// Whether a nested generic call's RESULT is fixed by its own inputs — a formal in its return
    /// type that its receiver or a parameter also mentions — rather than only by what an enclosing
    /// expectation supplies.
    ///
    /// `xs.map { B(it) }` qualifies: `R` appears in the transform parameter, so the provisional
    /// `List<B>` is real evidence about the argument and not a placeholder. `ArrayList()` and
    /// `emptySet()` do not: their result variables have no input to come from, so their provisional
    /// reads as the declared bound (`ArrayList<Any>`) or stays free (`Set<T>`), and treating either
    /// as evidence discards the element type the expectation would have supplied.
    pub(crate) fn result_is_input_constrained(&self) -> bool {
        let Self::ExpectedTypeCallable {
            generic_sig,
            provisional_open,
            ..
        } = self
        else {
            return false;
        };
        nested_result_is_input_constrained(*provisional_open, generic_sig)
    }

    /// Whether this argument has a final semantic type that may constrain a call's declaration
    /// variables. An unresolved lambda is shaped by the candidate first; a rechecked/materialized
    /// lambda participates like every other typed expression.
    ///
    pub(crate) fn contributes_type_to_inference(&self) -> bool {
        if self.is_omitted_default() {
            return false;
        }
        // `listOf(value)` and `xs.map { B(it) }` already fixed their result. That result is a
        // constraint on the enclosing call. `emptySet()` and `ArrayList()` did not: only the
        // enclosing expectation may finish them.
        if self.is_expected_type_callable() {
            return self.result_is_input_constrained();
        }
        !self.is_lambda_literal() || (!self.ty().mentions_error() && !self.ty().mentions_pending())
    }

    /// A value argument whose type is known before the enclosing candidate is chosen.
    ///
    /// An unresolved lambda is still shaped by the parameter, while a materialized lambda is
    /// ordinary evidence. A nested call contributes only when its own inputs fixed the result; a
    /// result-only producer stays postponed.
    pub(crate) fn supplies_fixed_argument_type(&self) -> bool {
        self.contributes_type_to_inference()
    }

    pub(crate) fn adapts_integer_literal_to(&self, parameter: Ty) -> bool {
        let Self::IntegerLiteral { ty, constant } = self else {
            return false;
        };
        let family = matches!(
            (*ty, parameter),
            (Ty::Int, Ty::Byte | Ty::Short | Ty::Long)
                | (Ty::UInt, Ty::UByte | Ty::UShort | Ty::ULong)
        );
        family && constant.fits(parameter)
    }

    pub(crate) fn adapts_signed_integer_literal_to_unsigned(&self, parameter: Ty) -> bool {
        matches!(self, Self::IntegerLiteral { ty: Ty::Int, .. })
            && matches!(
                parameter.non_null(),
                Ty::UByte | Ty::UShort | Ty::UInt | Ty::ULong
            )
    }

    pub(super) fn binds_result_to(&self, src: &dyn SymbolSource, parameter: Ty) -> bool {
        let Self::ExpectedTypeCallable {
            provisional,
            generic_sig,
            provisional_open,
        } = self
        else {
            return false;
        };
        // An input-constrained result (`id(x)`, `listOf(value)`) carries lower bounds from the
        // nested call's own arguments, read back from its provisional type. The parameter may
        // widen such a binding (`mutableListOf(1)` as `MutableList<Number>`) but never narrow it:
        // `id(x)` with `x: String?` cannot become a `String`, so that candidate is inapplicable.
        let admits = |actual: Ty, bound: Ty| {
            actual == bound || semantic_arg_assignable(src, &bound, &actual)
        };
        let infer = |signature: &GenericSig| {
            infer_generic_return_bindings(signature, parameter, admits).is_some_and(|bindings| {
                expected_result_keeps_call_evidence(
                    ProvisionalResult {
                        ty: *provisional,
                        open: *provisional_open,
                    },
                    generic_sig,
                    parameter,
                    &bindings,
                )
            })
        };
        if infer(generic_sig) {
            return true;
        }
        // An expected parameter may constrain a generic construction through an applied
        // supertype (`ArrayList<T>` consumed as `Iterable<Int>`). Applicability must admit that
        // contextual result before the checker can re-run the nested construction with the
        // selected parameter; direct result unification alone sees different classifiers.
        let Some(applied) = crate::assignable::applied_supertype(
            &super::SourceOracle(src),
            generic_sig.ret,
            parameter,
        ) else {
            return false;
        };
        let mut projected = generic_sig.as_ref().clone();
        projected.ret = applied;
        infer(&projected)
    }

    /// Whether expected-result inference is the only possible source of the nested call's result
    /// variables. A result-only producer such as `emptyList<T>()` qualifies; `listOf(value)` and
    /// `map(transform)` do not, because rebinding their input-constrained result would discard real
    /// argument evidence during enclosing overload selection.
    pub(super) fn binds_unconstrained_result_to(
        &self,
        src: &dyn SymbolSource,
        parameter: Ty,
    ) -> bool {
        if !self.is_expected_type_callable() {
            return false;
        }
        !self.result_is_input_constrained() && self.binds_result_to(src, parameter)
    }

    /// Whether a constructor parameter may complete this nested call without replacing a
    /// concrete result established by the nested call's own inputs.
    ///
    /// A concrete expectation cannot rebind `listOf(1, 2)` from `List<Int>` to
    /// `Collection<String>`. A still-symbolic parameter such as `List<P>` may finish the
    /// result. Result-only producers remain freely contextual.
    pub(crate) fn may_bind_result_during_overload_selection(&self, parameter: Ty) -> bool {
        !self.result_is_input_constrained() || parameter.mentions_ty_param()
    }
}

/// Whether a nested generic call's RESULT is fixed by its own inputs (a formal in its return type
/// that its receiver or a parameter also mentions) rather than only by what an enclosing
/// expectation supplies. `provisional` is the call's type checked without an expectation.
///
/// `xs.map { B(it) }` qualifies: `R` appears in the transform parameter, so the provisional
/// `List<B>` is real evidence about the argument and not a placeholder. `ArrayList()` and
/// `emptySet()` do not: their result variables have no input to come from, so their provisional
/// reads as the declared bound (`ArrayList<Any>`) or stays free (`Set<T>`), and treating either
/// as evidence discards the element type the expectation would have supplied.
pub(crate) fn nested_result_is_input_constrained(
    provisional_open: bool,
    generic_sig: &GenericSig,
) -> bool {
    // A provisional that still mentions an open formal is not determined: `"OK" to
    // emptySet()` reads as `Pair<String, B>` because its own argument supplied nothing for
    // `B`. Only the enclosing expectation can finish it, so it is not evidence about this
    // call — which is what letting it through regressed. `Box(listOf(r))` with `r: R` of the
    // enclosing `fun <R>` is determined: `R` is that declaration's own type.
    if provisional_open {
        return false;
    }
    generic_sig
        .formals
        .iter()
        .filter(|formal| {
            crate::types::ty_mentions_param(generic_sig.ret, std::slice::from_ref(*formal))
        })
        .any(|formal| {
            generic_sig.receiver.is_some_and(|receiver| {
                crate::types::ty_mentions_param(receiver, std::slice::from_ref(formal))
            }) || generic_sig.params.iter().any(|parameter| {
                crate::types::ty_mentions_param(*parameter, std::slice::from_ref(formal))
            })
        })
}

/// Whether binding a nested call's result to an enclosing expectation keeps the nullability the
/// call itself established.
///
/// A declared nullable result (`fun <R> f(c: Column<R>): Column<R>?`) stays nullable under every
/// binding, so a non-null expectation cannot accept it. An input-constrained result also keeps a
/// nullable lower bound its own arguments put on a result formal, read back from the call's
/// expectation-free `provisional` type: `id(x)` with `x: String?` cannot become a `String`, so a
/// `String` parameter does not accept it. Only nullability is read back. A provisional's classifier
/// may still be a placeholder when the call's own inputs are contextual (`forward(produce())`),
/// and the expectation may widen a binding anyway (`mutableListOf(1)` as `MutableList<Number>`).
/// A nested call's expectation-free result type, and whether it still mentions an open formal.
#[derive(Clone, Copy)]
pub(crate) struct ProvisionalResult {
    pub(crate) ty: Ty,
    pub(crate) open: bool,
}

pub(crate) fn expected_result_keeps_call_evidence(
    provisional: ProvisionalResult,
    generic_sig: &GenericSig,
    expected: Ty,
    expected_bindings: &super::GSigBinds,
) -> bool {
    let admits_null = |ty: Ty| ty.admits_null() || ty.upper_bound_admits_null();
    if generic_sig.ret.is_nullable() && !admits_null(expected) {
        return false;
    }
    if !nested_result_is_input_constrained(provisional.open, generic_sig) {
        return true;
    }
    let mut input_bindings = super::GSigBinds::default();
    super::unify_inferred_ty(generic_sig.ret, provisional.ty, &mut input_bindings);
    input_bindings.iter().all(|(formal, input)| {
        !input.admits_null()
            || expected_bindings
                .get(formal)
                .is_none_or(|expected| admits_null(*expected))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_collection_literal_is_constructible_as_an_array_or_a_factory_classifier_only() {
        let literal = CallArgKind::CollectionLiteral {
            provisional: Ty::obj("kotlin/collections/List"),
            factory_classifiers: vec![crate::types::type_name("Bag")].into(),
        };
        assert!(literal.constructible_as(Ty::obj("kotlin/IntArray")));
        assert!(literal.constructible_as(Ty::nullable(Ty::obj("Bag"))));
        assert!(!literal.constructible_as(Ty::obj("Box")));
        assert!(!literal.constructible_as(Ty::String));
        assert!(!CallArgKind::Typed(Ty::obj("kotlin/collections/List"))
            .constructible_as(Ty::obj("kotlin/IntArray")));
    }

    fn expected_type_call(provisional: Ty, params: Vec<Ty>) -> CallArgKind {
        let formal = Ty::ty_param("T", Ty::obj("kotlin/Any"));
        CallArgKind::ExpectedTypeCallable {
            provisional,
            provisional_open: provisional.mentions_ty_param(),
            generic_sig: std::sync::Arc::new(GenericSig {
                formals: vec![crate::types::TyParamName::new("T")],
                formal_bounds: vec![Vec::new()],
                receiver: None,
                params,
                ret: Ty::obj_args("fixture/Box", &[formal]),
                return_policy: Default::default(),
            }),
        }
    }

    #[test]
    fn integer_literal_uses_concrete_contextual_parameter_type() {
        let literal = CallArgKind::integer_literal(Ty::Int, 1);
        assert_eq!(literal.type_for(Ty::Byte), Ty::Byte);
        assert_eq!(literal.type_for(Ty::nullable(Ty::Short)), Ty::Short);
    }

    #[test]
    fn out_of_range_integer_literal_keeps_ordinary_type() {
        let literal = CallArgKind::integer_literal(Ty::Int, 256);
        assert_eq!(literal.type_for(Ty::Byte), Ty::Int);
    }

    #[test]
    fn input_constrained_result_carries_a_recorded_provisional_shape() {
        let formal = Ty::ty_param("T", Ty::obj("kotlin/Any"));
        let call = expected_type_call(Ty::obj_args("fixture/Box", &[Ty::Int]), vec![formal]);

        assert!(call.result_is_input_constrained());
    }

    #[test]
    fn result_only_producer_has_no_recorded_provisional_shape() {
        let call = expected_type_call(
            Ty::obj_args("fixture/Box", &[Ty::ty_param("T", Ty::obj("kotlin/Any"))]),
            Vec::new(),
        );

        assert!(!call.result_is_input_constrained());
    }

    #[test]
    fn only_a_materialized_lambda_supplies_a_fixed_argument_type() {
        assert!(!CallArgKind::LambdaLiteral(Ty::Error).supplies_fixed_argument_type());
        assert!(
            CallArgKind::LambdaLiteral(Ty::fun(Vec::new(), Ty::Int)).supplies_fixed_argument_type()
        );
    }
}
