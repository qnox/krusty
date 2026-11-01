//! Types and signatures proven publishable at the Pass-1 boundary.

use crate::types::Ty;

/// A type proven suitable for publication. The field is private: pending/error types and a call's
/// inference variables cannot be manufactured by users of the resolved module index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedTy(Ty);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnpublishableType {
    Pending,
    Error,
    /// A call's inference variable outlived that call's solver. Only the solver owns it, so no
    /// declaration, caller or backend may receive it, not even approximated by its bound.
    InferenceVariable,
}

impl ResolvedTy {
    pub fn new(ty: Ty) -> Result<Self, UnpublishableType> {
        let ty = ty.canonical_semantic();
        if ty.mentions_pending() {
            Err(UnpublishableType::Pending)
        } else if ty.mentions_error() {
            Err(UnpublishableType::Error)
        } else if ty.mentions_type_variable() {
            Err(UnpublishableType::InferenceVariable)
        } else {
            Ok(Self(ty))
        }
    }

    pub const fn get(self) -> Ty {
        self.0
    }
}

/// The declaration's exposed result and any distinct checking context for its own inferred body.
/// Both views have removed solver-local parameters; only the exposed view approximates anonymous
/// implementation types. Callers, metadata and ABI consume `published` exclusively.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeclarationResultTypes {
    pub published: ResolvedTy,
    pub inferred_body_result: Option<ResolvedTy>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedSignature {
    pub parameters: Box<[ResolvedTy]>,
    pub result: ResolvedTy,
    /// Declaration-owned body evidence before anonymous exposure, with solver-local parameters
    /// removed. This is checking context, never a caller-visible result or emitted ABI slot.
    pub(super) inferred_body_result: Option<ResolvedTy>,
    /// The typealias an inferred result was reached through, kotlinc's abbreviation attribute on
    /// the result type (`val text = StringBuilder()` has type `java.lang.StringBuilder`
    /// abbreviated as `kotlin.text.StringBuilder`). It never takes part in type identity: only
    /// `@Metadata` reads it, as the declaration's `abbreviatedType`.
    pub result_abbreviation: Option<ResolvedTypeAbbreviation>,
}

/// A typealias application that abbreviates a resolved type: the alias's qualified identity and
/// its type arguments, in the alias's own parameter order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedTypeAbbreviation {
    pub alias: crate::types::TypeName,
    pub arguments: Box<[Ty]>,
    /// Expanded-type argument indices from the inferred result root to the node abbreviated by
    /// `alias`. Constructor aliases abbreviate the root and therefore carry an empty path; a
    /// class literal through `Alias<T>` abbreviates the represented `KClass` argument at `[0]`.
    pub argument_path: Box<[u32]>,
}

impl ResolvedSignature {
    pub(crate) fn inferred_body_result(&self) -> Option<ResolvedTy> {
        self.inferred_body_result
    }

    pub fn new(
        parameters: impl IntoIterator<Item = Ty>,
        result: Ty,
    ) -> Result<Self, UnpublishableType> {
        let parameters = parameters
            .into_iter()
            .map(ResolvedTy::new)
            .collect::<Result<Vec<_>, _>>()?
            .into_boxed_slice();
        Ok(Self {
            parameters,
            result: ResolvedTy::new(result)?,
            inferred_body_result: None,
            result_abbreviation: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{ResolvedTy, UnpublishableType};
    use crate::fx_hash::HashMap;
    use crate::types::{ty_subst, Ty, TyParamName};

    fn variable() -> Ty {
        Ty::type_variable(
            TyParamName::new("\0call:T"),
            Ty::nullable(Ty::obj("kotlin/Any")),
        )
    }

    #[test]
    fn an_inference_variable_is_rejected_at_the_root_and_nested() {
        let rejected = UnpublishableType::InferenceVariable;
        for ty in [
            variable(),
            Ty::nullable(variable()),
            Ty::fun(vec![Ty::Int], variable()),
            Ty::fun(vec![variable()], Ty::Unit),
            Ty::obj_args("kotlin/collections/List", &[variable()]),
            Ty::obj_args(
                "kotlin/collections/Map",
                &[Ty::String, Ty::fun(Vec::new(), variable())],
            ),
        ] {
            assert_eq!(ResolvedTy::new(ty), Err(rejected), "{ty:?}");
        }
    }

    #[test]
    fn a_variable_bounded_by_an_ordinary_type_is_still_rejected() {
        let bounded = Ty::type_variable(TyParamName::new("\0call:T"), Ty::String);
        assert_eq!(
            ResolvedTy::new(Ty::obj_args("kotlin/collections/List", &[bounded])),
            Err(UnpublishableType::InferenceVariable)
        );
    }

    #[test]
    fn declaration_type_parameters_and_solved_calls_publish() {
        let declared = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
        let list_of_declared = Ty::obj_args("kotlin/collections/List", &[declared]);
        assert_eq!(ResolvedTy::new(declared).map(ResolvedTy::get), Ok(declared));
        assert_eq!(
            ResolvedTy::new(list_of_declared).map(ResolvedTy::get),
            Ok(list_of_declared)
        );

        let unsolved = Ty::obj_args("kotlin/collections/List", &[variable()]);
        let solved = ty_subst(
            unsolved,
            &HashMap::from_iter([(TyParamName::new("\0call:T"), Ty::String)]),
        );
        let expected = Ty::obj_args("kotlin/collections/List", &[Ty::String]);
        assert_eq!(solved, expected);
        assert_eq!(ResolvedTy::new(solved).map(ResolvedTy::get), Ok(expected));
    }
}
