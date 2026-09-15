use crate::types::Ty;

use super::SemanticPlatform;

/// A parsed generic signature in Kotlin's logical shape: formal type-parameter names, an optional
/// receiver, the value parameters, and the return. Providers normalize their source format into
/// this one declaration-owned contract before resolution consumes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenericSig {
    pub formals: Vec<String>,
    /// Declared upper bounds, parallel to [`Self::formals`]. An empty slot denotes Kotlin's
    /// implicit nullable `Any?` bound.
    pub formal_bounds: Vec<Vec<Ty>>,
    pub receiver: Option<Ty>,
    pub params: Vec<Ty>,
    pub ret: Ty,
    pub return_policy: GenericReturnPolicy,
}

/// Post-substitution policy for a generic callable's return. Keeping this on [`GenericSig`] gives
/// member, static, and top-level resolution one authoritative fact.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GenericReturnPolicy {
    #[default]
    Exact,
    /// An unannotated reference contract remains null-capable when its outer method type parameter
    /// binds to a primitive.
    FlexibleReference,
}

impl GenericSig {
    /// The first declared upper bound, or Kotlin's implicit `Any?` when none was written.
    pub fn primary_formal_bound(&self, index: usize) -> Ty {
        self.formal_bounds
            .get(index)
            .and_then(|bounds| bounds.first())
            .copied()
            .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")))
    }

    /// Rebuild the declaration's source parameters in the order used by a static realization:
    /// `(contexts..., extension receiver, values...)`.
    pub fn parameters_with_receiver(&self, context_count: usize) -> Box<[Ty]> {
        let mut parameters = self.params.clone();
        if let Some(receiver) = self.receiver {
            parameters.insert(context_count.min(parameters.len()), receiver);
        }
        parameters.into_boxed_slice()
    }

    pub fn apply_return_policy(&self, platform: &dyn SemanticPlatform, specialized: Ty) -> Ty {
        match self.return_policy {
            GenericReturnPolicy::Exact => specialized,
            GenericReturnPolicy::FlexibleReference => specialized
                .boxed_ref()
                .map(|boxed| platform.library_value_form(boxed))
                .unwrap_or(specialized),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signature(bounds: Vec<Vec<Ty>>) -> GenericSig {
        GenericSig {
            formals: vec!["T".to_owned()],
            formal_bounds: bounds,
            receiver: None,
            params: Vec::new(),
            ret: Ty::Unit,
            return_policy: GenericReturnPolicy::Exact,
        }
    }

    #[test]
    fn an_absent_bound_is_nullable_any_but_a_declared_bound_is_preserved() {
        assert_eq!(
            signature(vec![Vec::new()]).primary_formal_bound(0),
            Ty::nullable(Ty::obj("kotlin/Any"))
        );
        assert_eq!(
            signature(vec![vec![Ty::obj("sample/Bound")]]).primary_formal_bound(0),
            Ty::obj("sample/Bound")
        );
    }
}
