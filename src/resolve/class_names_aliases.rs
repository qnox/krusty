//! Alias identity and expansion edges owned by one file's classifier-name inventory.

use super::{ClassNames, TypeName};

impl ClassNames {
    /// Record an alias expansion under a spelling whose ordinary scope lookup selected it.
    pub fn insert_alias_expansion(
        &mut self,
        spelling: String,
        expansion: crate::libraries::AliasExpansion,
    ) {
        self.alias_bindings.insert(spelling, expansion.identity);
        self.alias_expansions.insert(expansion.identity, expansion);
    }

    /// The expansion template for a spelling, if it names an alias.
    pub fn alias_expansion(&self, name: &str) -> Option<&crate::libraries::AliasExpansion> {
        self.alias_bindings
            .get(name)
            .and_then(|identity| self.alias_expansions.get(identity))
    }

    /// Expansion selected by its already-bound alias declaration identity. Later structural walks
    /// use this instead of repeating a spelling lookup in the file's scope map.
    pub fn alias_expansion_by_identity(
        &self,
        identity: TypeName,
    ) -> Option<&crate::libraries::AliasExpansion> {
        self.alias_expansions.get(&identity)
    }

    /// Record that `spelling` names the SOURCE `typealias` declared as `identity`.
    pub fn insert_source_alias_identity(&mut self, spelling: String, identity: TypeName) {
        self.source_alias_identities.insert(spelling, identity);
    }

    /// The fully-qualified `typealias` this spelling names — source or classpath — or `None` when
    /// it names an ordinary class.
    ///
    /// An `import pkg.Real as Fake` rename is deliberately NOT an alias: kotlinc writes no
    /// `abbreviated_type` for it (verified against 2.4.10), because a rename binds a name in one
    /// file rather than declaring a type.
    pub fn alias_identity(&self, name: &str) -> Option<TypeName> {
        self.alias_expansion(name)
            .map(|expansion| expansion.identity)
            .or_else(|| self.source_alias_identities.get(name).copied())
    }
}
