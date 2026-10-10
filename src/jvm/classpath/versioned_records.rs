//! API-version-partitioned semantic record caches.
//!
//! Parsed class bytes are version-independent, but normalized records are not: `@SinceKotlin`
//! filtering happens while a provider builds them. Keeping this ownership boundary beside the
//! `Classpath` prevents one compilation's API view from poisoning the next compilation.

use crate::language_version::LanguageVersion;
use crate::symbol_source::SymbolNamespace;
use crate::types::TypeName;

const RESOLVED_TYPES_CAP: usize = 65536;
const SYMBOLS_CAP: usize = 65536;

pub(super) type ResolvedTypeCache =
    crate::lru::LruCache<TypeName, Option<std::sync::Arc<crate::libraries::LibraryType>>>;
pub(super) type SymbolsMemo =
    crate::lru::LruCache<String, std::rc::Rc<crate::libraries::ResolvedSymbols>>;

impl super::Classpath {
    /// Memoized `resolve_type` result for `internal` under one API version. The outer `Option`
    /// distinguishes a cold entry from a cached absence.
    pub fn cached_library_type_name(
        &self,
        api_version: LanguageVersion,
        internal: TypeName,
    ) -> Option<Option<std::sync::Arc<crate::libraries::LibraryType>>> {
        if !self.catalog_complete() {
            cache_stat!(resolved_types, false);
            return None;
        }
        // Deliberately per instance: a LibraryType embeds facts from the whole classpath
        // composition, so no entry-keyed process-global layer can serve it safely.
        let hit = self
            .resolved_types
            .borrow_mut()
            .get_mut(&api_version)
            .and_then(|types| types.get(&internal))
            .cloned();
        cache_stat!(resolved_types, hit.is_some());
        hit
    }

    pub fn cache_library_type_name(
        &self,
        api_version: LanguageVersion,
        internal: TypeName,
        ty: Option<std::sync::Arc<crate::libraries::LibraryType>>,
    ) {
        if !self.catalog_complete() {
            return;
        }
        self.resolved_types
            .borrow_mut()
            .entry(api_version)
            .or_insert_with(|| crate::lru::LruCache::new(RESOLVED_TYPES_CAP))
            .insert(internal, ty);
    }

    /// Read the already-composed namespace record for a declaration key under one API version.
    pub fn cached_symbols(
        &self,
        api_version: LanguageVersion,
        namespace: SymbolNamespace,
        name: &str,
    ) -> Option<std::rc::Rc<crate::libraries::ResolvedSymbols>> {
        if !self.catalog_complete() {
            cache_stat!(symbols_memo, false);
            return None;
        }
        let hit = self
            .symbols_memo
            .borrow_mut()
            .get_mut(&(api_version, namespace))
            .and_then(|symbols| symbols.get(name))
            .cloned();
        cache_stat!(symbols_memo, hit.is_some());
        hit
    }

    /// Store one composed namespace record and return the shared value handed to the caller.
    pub fn memoize_symbols(
        &self,
        api_version: LanguageVersion,
        namespace: SymbolNamespace,
        name: &str,
        symbols: crate::libraries::ResolvedSymbols,
    ) -> std::rc::Rc<crate::libraries::ResolvedSymbols> {
        let symbols = std::rc::Rc::new(symbols);
        if self.catalog_complete() {
            self.symbols_memo
                .borrow_mut()
                .entry((api_version, namespace))
                .or_insert_with(|| crate::lru::LruCache::new(SYMBOLS_CAP))
                .insert(name.to_string(), symbols.clone());
        }
        symbols
    }
}
