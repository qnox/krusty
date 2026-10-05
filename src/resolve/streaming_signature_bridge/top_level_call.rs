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
    Constructor(
        Box<crate::symbol_resolver::SelectedConstructorDeclaration>,
        SelectedAlias,
    ),
    /// A fun-interface name applied to one function value (`I { … }`): no constructor, the result
    /// is the interface. Both constructions carry the alias the winning scope rung named, if any.
    SamConstructor(crate::types::TypeName, SelectedAlias),
}

pub(super) type SelectedAlias = Option<Box<crate::libraries::AliasExpansion>>;
