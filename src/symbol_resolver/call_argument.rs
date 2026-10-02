use crate::integer_constant::IntegerConstant;
use crate::libraries::{GenericSig, SemanticPlatform};
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TypeName};

use super::{infer_generic_return_bindings, semantic_arg_assignable, GSigBinds};

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
    },
    /// A safely folded integer constant and its ordinary runtime type.
    IntegerLiteral { ty: Ty, constant: IntegerConstant },
    /// A legal declaration-default slot produced by the shared argument mapper.
    OmittedDefault,
}

impl CallArgKind {
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
            Self::ExpectedTypeCallable { provisional, .. } => *provisional,
            // This probe never becomes an expression type. Applicability recognizes the explicit
            // variant and generic inference skips it.
            Self::OmittedDefault => Ty::Error,
        }
    }

    pub(crate) fn substitute_types(&self, bindings: &GSigBinds) -> Self {
        let substitute = |ty| super::ty_subst_keep_unbound(ty, bindings);
        match self {
            Self::Typed(ty) => Self::Typed(substitute(*ty)),
            Self::Spread(ty) => Self::Spread(substitute(*ty)),
            Self::LambdaLiteral(ty) => Self::LambdaLiteral(substitute(*ty)),
            Self::CallableReference { nominal, function } => Self::CallableReference {
                nominal: substitute(*nominal),
                function: substitute(*function),
            },
            Self::ExpectedTypeCallable {
                provisional,
                generic_sig,
            } => Self::ExpectedTypeCallable {
                provisional: substitute(*provisional),
                generic_sig: generic_sig.clone(),
            },
            Self::IntegerLiteral { ty, constant } => Self::IntegerLiteral {
                ty: substitute(*ty),
                constant: *constant,
            },
            Self::OmittedDefault => Self::OmittedDefault,
        }
    }

    pub(crate) fn function_type(&self) -> Option<Ty> {
        match self {
            Self::CallableReference { function, .. } => Some(*function),
            kind => matches!(kind.ty(), Ty::Fun(_)).then(|| kind.ty()),
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
        if self.is_omitted_default() {
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
        if let Ty::TyParam(_, bound) = parameter.non_null() {
            if self.adapts_integer_literal_to(*bound) {
                return *bound;
            }
        }
        if matches!(parameter.non_null(), Ty::Fun(_)) {
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
            // A value that already implements the fun interface constrains that
            // interface. Its function supertype must not instantiate the interface.
            if self.nominal_implements_classifier(source, sam.internal) {
                return self.ty();
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
        if matches!(start, Ty::Error | Ty::Pending | Ty::Fun(_)) || start.mentions_pending() {
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
            if let Ty::Obj(name, _) = ty {
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

    pub(crate) fn is_integer_literal(&self) -> bool {
        matches!(self, Self::IntegerLiteral { .. })
    }

    pub(crate) fn is_expected_type_callable(&self) -> bool {
        matches!(self, Self::ExpectedTypeCallable { .. })
    }

    pub(crate) fn is_omitted_default(&self) -> bool {
        matches!(self, Self::OmittedDefault)
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
            provisional,
            generic_sig,
        } = self
        else {
            return false;
        };
        // A provisional that still mentions a type parameter is not determined: `"OK" to
        // emptySet()` reads as `Pair<String, B>` because its own argument supplied nothing for
        // `B`. Only the enclosing expectation can finish it, so it is not evidence about this
        // call — which is what letting it through regressed.
        if provisional.mentions_ty_param() {
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

    /// Whether this argument has a final semantic type that may constrain a call's declaration
    /// variables. An unresolved lambda is shaped by the candidate first; a rechecked/materialized
    /// lambda participates like every other typed expression.
    ///
    pub(crate) fn contributes_type_to_inference(&self) -> bool {
        !self.is_expected_type_callable()
            && !self.is_omitted_default()
            && (!self.is_lambda_literal()
                || (!self.ty().mentions_error() && !self.ty().mentions_pending()))
    }

    pub(crate) fn adapts_integer_literal_to(&self, parameter: Ty) -> bool {
        let Self::IntegerLiteral { ty, constant } = self else {
            return false;
        };
        let family = match (*ty, parameter) {
            (Ty::Int, Ty::Byte | Ty::Short | Ty::Long) => true,
            (Ty::UInt, Ty::UByte | Ty::UShort | Ty::ULong) => true,
            _ => false,
        };
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
        let Self::ExpectedTypeCallable { generic_sig, .. } = self else {
            return false;
        };
        let infer = |signature: &GenericSig| {
            infer_generic_return_bindings(signature, parameter, |actual, bound| {
                actual == bound || semantic_arg_assignable(src, &bound, &actual)
            })
            .is_some()
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
