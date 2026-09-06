use crate::libraries::GenericSig;
use crate::symbol_source::SymbolSource;
use crate::types::Ty;

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
    },
    /// A safely folded integer constant and its ordinary runtime type.
    IntegerLiteral { ty: Ty, value: i32 },
    /// A legal declaration-default slot produced by the shared argument mapper.
    OmittedDefault,
}

impl CallArgKind {
    pub(crate) fn integer_literal(ty: Ty, value: i32) -> Self {
        Self::IntegerLiteral { ty, value }
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

    pub(crate) fn function_type(&self) -> Option<Ty> {
        match self {
            Self::CallableReference { function, .. } => Some(*function),
            kind => matches!(kind.ty(), Ty::Fun(_)).then(|| kind.ty()),
        }
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
        if super::semantic_sam_signature(source, parameter).is_some() {
            self.function_type()
                .unwrap_or_else(|| self.type_for(parameter))
        } else {
            self.type_for(parameter)
        }
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

    pub(crate) fn adapts_integer_literal_to(&self, parameter: Ty) -> bool {
        let Self::IntegerLiteral { ty, value } = self else {
            return false;
        };
        match (*ty, parameter) {
            (Ty::Int, Ty::Byte) => i8::try_from(*value).is_ok(),
            (Ty::Int, Ty::Short) => i16::try_from(*value).is_ok(),
            (Ty::Int, Ty::Long) => true,
            (Ty::UInt, Ty::UByte) => u8::try_from(*value).is_ok(),
            (Ty::UInt, Ty::UShort) => u16::try_from(*value).is_ok(),
            (Ty::UInt, Ty::ULong) => *value >= 0,
            _ => false,
        }
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
        let Self::ExpectedTypeCallable { generic_sig, .. } = self else {
            return false;
        };
        let result_formals = generic_sig
            .formals
            .iter()
            .filter(|formal| {
                crate::types::ty_mentions_param(generic_sig.ret, std::slice::from_ref(*formal))
            })
            .collect::<Vec<_>>();
        let constrained_by_input = result_formals.iter().any(|formal| {
            generic_sig.receiver.is_some_and(|receiver| {
                crate::types::ty_mentions_param(receiver, std::slice::from_ref(*formal))
            }) || generic_sig.params.iter().any(|parameter| {
                crate::types::ty_mentions_param(*parameter, std::slice::from_ref(*formal))
            })
        });
        !constrained_by_input && self.binds_result_to(src, parameter)
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
