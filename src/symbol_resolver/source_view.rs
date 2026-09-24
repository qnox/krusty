//! The resolver's declaration federation, with memoization represented as a complete state.

use super::{FunctionImportScope, FunctionScopeRef, SymbolResolver};
use crate::libraries::{ResolvedSymbols, SemanticPlatform};
use crate::symbol_source::{
    CachedCompositeSource, CompositeSource, SymbolNamespace, SymbolQueryCache, SymbolSource,
};
use crate::types::{Ty, TypeName};

/// A resolver either reads its federation directly or through a caller-owned memo. Keeping the two
/// states explicit means a `CachedCompositeSource` can never exist without the cache its name and
/// invariants promise.
pub(super) enum ResolverSource<'a> {
    Direct(CompositeSource<'a>),
    Memoized(CachedCompositeSource<'a>),
}

impl<'a> ResolverSource<'a> {
    pub(super) fn direct(children: Vec<&'a dyn SymbolSource>) -> Self {
        Self::Direct(CompositeSource::new(children))
    }

    pub(super) fn memoize_in(self, cache: &'a SymbolQueryCache) -> Self {
        match self {
            Self::Direct(source) => {
                Self::Memoized(CachedCompositeSource::from_composite(source, cache))
            }
            Self::Memoized(_) => panic!("a resolver symbol source may be memoized only once"),
        }
    }
}

impl<'a> SymbolResolver<'a> {
    pub fn new(lib: &'a dyn SemanticPlatform) -> Self {
        Self {
            lib,
            src: ResolverSource::direct(vec![lib as &dyn SymbolSource]),
            module: None,
            fn_scope: None,
            lexical_classes: Vec::new(),
            access_package: None,
            access_file: None,
        }
    }

    /// A resolver whose top-level function resolution is restricted to `fn_scope`'s packages.
    pub fn new_scoped(lib: &'a dyn SemanticPlatform, fn_scope: &'a [TypeName]) -> Self {
        Self {
            lib,
            src: ResolverSource::direct(vec![lib as &dyn SymbolSource]),
            module: None,
            fn_scope: Some(FunctionScopeRef::Flat(fn_scope)),
            lexical_classes: Vec::new(),
            access_package: None,
            access_file: None,
        }
    }

    /// The primary resolver: symbol resolution federates the current `module` over the classpath `lib`.
    pub fn new_scoped_with_module(
        lib: &'a dyn SemanticPlatform,
        module: &'a dyn SymbolSource,
        fn_scope: &'a [TypeName],
    ) -> Self {
        Self {
            lib,
            src: ResolverSource::direct(vec![module, lib as &dyn SymbolSource]),
            module: Some(module),
            fn_scope: Some(FunctionScopeRef::Flat(fn_scope)),
            lexical_classes: Vec::new(),
            access_package: None,
            access_file: None,
        }
    }

    pub(crate) fn new_import_scoped_with_module(
        lib: &'a dyn SemanticPlatform,
        module: &'a dyn SymbolSource,
        fn_scope: &'a FunctionImportScope,
    ) -> Self {
        Self {
            lib,
            src: ResolverSource::direct(vec![module, lib as &dyn SymbolSource]),
            module: Some(module),
            fn_scope: Some(FunctionScopeRef::Imports(fn_scope)),
            lexical_classes: Vec::new(),
            access_package: None,
            access_file: None,
        }
    }

    /// Read declaration records through `cache`: a caller-owned memo of this resolver's own
    /// federation (the module over the platform) for one bounded source unit, during which neither
    /// changes. A checker resolves every call, receiver and supertype walk of its unit against the
    /// same few records; this is the composite provider cache every such query shares.
    pub(crate) fn with_symbol_query_cache(mut self, cache: &'a SymbolQueryCache) -> Self {
        self.src = self.src.memoize_in(cache);
        self
    }
}

impl SymbolSource for ResolverSource<'_> {
    fn package_exists(&self, parent: TypeName, name: &str) -> bool {
        match self {
            Self::Direct(source) => source.package_exists(parent, name),
            Self::Memoized(source) => source.package_exists(parent, name),
        }
    }

    fn symbols(&self, namespace: SymbolNamespace, name: &str) -> std::rc::Rc<ResolvedSymbols> {
        match self {
            Self::Direct(source) => source.symbols(namespace, name),
            Self::Memoized(source) => source.symbols(namespace, name),
        }
    }

    fn platform_flexible_upper_bound(&self, lower: Ty) -> Ty {
        match self {
            Self::Direct(source) => source.platform_flexible_upper_bound(lower),
            Self::Memoized(source) => source.platform_flexible_upper_bound(lower),
        }
    }

    fn generated_serializer_singleton(&self, classifier: TypeName) -> Option<TypeName> {
        match self {
            Self::Direct(source) => source.generated_serializer_singleton(classifier),
            Self::Memoized(source) => source.generated_serializer_singleton(classifier),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct CountingEmptySource {
        queries: Cell<usize>,
    }

    impl SymbolSource for CountingEmptySource {
        fn symbols(
            &self,
            _namespace: SymbolNamespace,
            _name: &str,
        ) -> std::rc::Rc<ResolvedSymbols> {
            self.queries.set(self.queries.get() + 1);
            std::rc::Rc::new(ResolvedSymbols::default())
        }
    }

    #[test]
    fn resolver_views_for_one_stable_source_unit_share_their_federated_record() {
        let source = CountingEmptySource {
            queries: Cell::new(0),
        };
        let cache = SymbolQueryCache::default();
        let namespace = SymbolNamespace::Package(TypeName::ROOT);
        let first = ResolverSource::direct(vec![&source]).memoize_in(&cache);
        let second = ResolverSource::direct(vec![&source]).memoize_in(&cache);

        let first_record = first.symbols(namespace, "missing");
        let second_record = second.symbols(namespace, "missing");

        assert!(std::rc::Rc::ptr_eq(&first_record, &second_record));
        assert_eq!(source.queries.get(), 1);
    }
}
