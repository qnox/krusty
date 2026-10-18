//! The outcome of selecting a top-level call spelling: a callable, a value, a classifier's
//! `invoke`, or a construction together with the typealias that named the constructed classifier.

pub(super) enum SelectedTopLevelCall {
    Ambiguous,
    Callable {
        callable: Box<crate::libraries::LibraryCallable>,
        source: Option<(u32, u32)>,
        declaration: Option<crate::fir::DeclarationId>,
        parameter_by_argument: Box<[Option<u32>]>,
    },
    Value(Box<crate::libraries::PropertyInfo>),
    /// A classifier whose constructors do not apply: its associated `operator fun invoke`, then the
    /// `invoke` convention on the value it denotes (an object singleton or companion), if any.
    ClassifierInvoke(crate::types::TypeName, Option<crate::types::Ty>),
    Constructor {
        declaration: Box<crate::symbol_resolver::SelectedConstructorDeclaration>,
        alias: SelectedAlias,
        /// Exact source-argument to declaration-parameter mapping retained by the winning
        /// constructor candidate. Publication must not try to map the call again.
        parameter_by_argument: Box<[Option<u32>]>,
    },
    /// A fun-interface name applied to one function value (`I { … }`): no constructor, the result
    /// is the interface. Both constructions carry the alias the winning scope rung named, if any.
    SamConstructor(crate::types::TypeName, SelectedAlias),
}

pub(super) type SelectedAlias = Option<Box<crate::libraries::AliasExpansion>>;

/// Constructor arguments in the exact parameter order supplied to overload selection, together
/// with the source-slot mapping that produced that order. Publication consumes this retained map;
/// it never reruns argument mapping against the selected declaration.
pub(super) struct MappedConstructorArguments {
    pub(super) kinds: Vec<crate::symbol_resolver::CallArgKind>,
    pub(super) types: Vec<crate::types::Ty>,
    parameter_by_argument: Box<[Option<u32>]>,
}

impl MappedConstructorArguments {
    pub(super) fn mapped(
        kinds: Vec<crate::symbol_resolver::CallArgKind>,
        types: Vec<crate::types::Ty>,
        parameter_by_argument: Box<[Option<u32>]>,
    ) -> Self {
        Self {
            kinds,
            types,
            parameter_by_argument,
        }
    }

    pub(super) fn parameter_by_argument(&self) -> Box<[Option<u32>]> {
        self.parameter_by_argument.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Ty;

    #[test]
    fn selected_constructor_consumes_its_retained_argument_mapping() {
        let mapped = MappedConstructorArguments::mapped(
            vec![
                crate::symbol_resolver::CallArgKind::Typed(Ty::String),
                crate::symbol_resolver::CallArgKind::Typed(Ty::Int),
            ],
            vec![Ty::String, Ty::Int],
            vec![Some(1), Some(0)].into_boxed_slice(),
        );

        assert_eq!(
            mapped.parameter_by_argument(),
            vec![Some(1), Some(0)].into_boxed_slice(),
        );
    }
}
