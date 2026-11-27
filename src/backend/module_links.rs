//! Links from one file of a program to a declaration another file realizes, for a target that
//! lowers a program one file at a time.
//!
//! Some declarations are realized by a function only the declaring file's lowering creates: the
//! entry answering an enum constant (see `super::enums`). A file reading one calls that function,
//! but the files of a module stream in source order, so the reader may come first. The link is
//! therefore keyed by the checked declaration identity, which every file knows before any is
//! lowered: the first of the reader and the declaring file to arrive allocates the target's link
//! (a function index, a symbol), and the other one finds it. Pass 1's module facts certify that a
//! not-yet-defined declaration exists, so a reader reserves a link only for a declaration its own
//! module will define; [`ModuleLinks::undefined`] names any that were never defined.
//!
//! Identities are qualified, so one table serves every module of a program: a declaration
//! another module published is defined by the time a later module reads it.
//!
//! A module may redeclare an identity a module of its dependency chain declares: a platform
//! module's class over a common module's, the way classpath substitution lets the later
//! declaration replace the earlier one. The table remembers which module owns each link, so the
//! redeclaring module's own declaration and its reads get a link of their own, which every module
//! after it reads. The caller proves the redeclaration (the module's dependencies declare the
//! identity too); without that proof a second module's definition is [`LinkConflict::DefinedTwice`]
//! as within one module. An earlier module's files are already lowered against the link they
//! read, so a link any file has read is never replaced: that would leave the reader on the class
//! the redeclaration replaces ([`LinkConflict::ReplacedAfterRead`]).

use crate::fx_hash::HashMap;
use std::hash::Hash;

/// The target links of declarations, by declaration identity.
#[derive(Debug)]
pub struct ModuleLinks<K, V> {
    links: HashMap<K, Link<V>>,
    /// The source module whose files are being lowered.
    module: Box<str>,
}

#[derive(Debug)]
struct Link<V> {
    target: V,
    defined: bool,
    /// Whether a file has read the link, so its lowered code depends on it.
    read: bool,
    /// The source module that reserved or defined the link.
    module: Box<str>,
}

impl<K, V> Default for ModuleLinks<K, V> {
    fn default() -> Self {
        Self {
            links: HashMap::default(),
            module: Box::default(),
        }
    }
}

/// Why a declaration cannot take the link it asks for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkConflict {
    /// Two files realized one identity, of one module or of two modules neither of which
    /// redeclares the other's declaration.
    DefinedTwice,
    /// A module redeclares an identity whose earlier link a file already read: that file's
    /// lowered code would keep the declaration the redeclaration replaces.
    ReplacedAfterRead,
}

impl<K: Eq + Hash, V: Clone> ModuleLinks<K, V> {
    /// Begin (or continue) the files of source module `module`.
    pub fn enter_module(&mut self, module: &str) {
        if &*self.module != module {
            self.module = module.into();
        }
    }

    fn fresh(&self, target: V, defined: bool) -> Link<V> {
        Link {
            target,
            defined,
            read: !defined,
            module: self.module.clone(),
        }
    }

    /// Whether the current module's declaration replaces `link`, another module's: `redeclares`
    /// proves that module is of the current module's dependency chain. Without it the other
    /// module's link stands.
    fn replaces(&self, link: &Link<V>, redeclares: bool) -> Result<bool, LinkConflict> {
        if link.module == self.module || !redeclares {
            return Ok(false);
        }
        // Only a read reserves a link, so an undefined one is read and never replaced either: it
        // stays in `undefined`, which reports it.
        if link.read || !link.defined {
            return Err(LinkConflict::ReplacedAfterRead);
        }
        Ok(true)
    }

    /// The link of `key` for a read: the defined or reserved one, or `None` when no file has
    /// realized it and `declared` (the module fact certifying it exists) is false. A read of an
    /// identity the current module redeclares (`declared` and `redeclares`, see [`Self::define`])
    /// never finds the earlier module's link: it reserves one for the module's own declaration.
    pub fn read(
        &mut self,
        key: K,
        declared: bool,
        redeclares: bool,
        allocate: impl FnOnce() -> V,
    ) -> Result<Option<V>, LinkConflict> {
        match self.links.get(&key) {
            Some(link) if !declared || !self.replaces(link, redeclares)? => {
                let link = self.links.get_mut(&key).expect("found just above");
                link.read = true;
                return Ok(Some(link.target.clone()));
            }
            Some(_) => {}
            None if !declared => return Ok(None),
            None => {}
        }
        let target = allocate();
        let link = self.fresh(target.clone(), false);
        self.links.insert(key, link);
        Ok(Some(target))
    }

    /// The link the declaring file defines `key` under: the one an earlier read of the current
    /// module reserved, or a fresh one. `redeclares` proves the identity is a redeclaration of a
    /// declaration of the current module's dependency chain; that module's link, if no file read
    /// it, is then replaced, so later modules read the redeclaration.
    pub fn define(
        &mut self,
        key: K,
        redeclares: bool,
        allocate: impl FnOnce() -> V,
    ) -> Result<V, LinkConflict> {
        if let Some(link) = self.links.get(&key) {
            if link.module != self.module && !redeclares {
                return Err(LinkConflict::DefinedTwice);
            }
            if !self.replaces(link, redeclares)? {
                let link = self.links.get_mut(&key).expect("found just above");
                if link.defined {
                    return Err(LinkConflict::DefinedTwice);
                }
                link.defined = true;
                return Ok(link.target.clone());
            }
        }
        let target = allocate();
        let link = self.fresh(target.clone(), true);
        self.links.insert(key, link);
        Ok(target)
    }

    /// The defined link of `key`.
    pub fn defined(&self, key: &K) -> Option<V> {
        self.links
            .get(key)
            .filter(|link| link.defined)
            .map(|link| link.target.clone())
    }

    /// The declarations a read reserved but no file defined.
    pub fn undefined(&self) -> impl Iterator<Item = &K> {
        self.links
            .iter()
            .filter(|(_, link)| !link.defined)
            .map(|(key, _)| key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counter() -> impl FnMut() -> u32 {
        let mut next = 0;
        move || {
            next += 1;
            next
        }
    }

    #[test]
    fn a_read_before_the_declaring_file_reserves_the_link_it_defines() {
        let mut links = ModuleLinks::<&str, u32>::default();
        let mut fresh = counter();
        assert_eq!(links.read("A", true, false, &mut fresh), Ok(Some(1)));
        assert_eq!(links.read("A", true, false, &mut fresh), Ok(Some(1)));
        assert_eq!(links.undefined().collect::<Vec<_>>(), [&"A"]);
        assert_eq!(links.defined(&"A"), None);
        assert_eq!(links.define("A", false, &mut fresh), Ok(1));
        assert_eq!(links.defined(&"A"), Some(1));
        assert_eq!(links.undefined().count(), 0);
        assert_eq!(
            links.define("A", false, &mut fresh),
            Err(LinkConflict::DefinedTwice)
        );
    }

    #[test]
    fn a_later_module_redeclaring_an_identity_replaces_the_link_for_every_module_after_it() {
        let mut links = ModuleLinks::<&str, u32>::default();
        let mut fresh = counter();
        links.enter_module("common");
        assert_eq!(links.define("A", false, &mut fresh), Ok(1));
        links.enter_module("platform");
        // The platform module's own read, before its declaring file, reserves its own link.
        assert_eq!(links.read("A", true, true, &mut fresh), Ok(Some(2)));
        assert_eq!(links.define("A", true, &mut fresh), Ok(2));
        assert_eq!(links.defined(&"A"), Some(2));
        links.enter_module("app");
        assert_eq!(links.read("A", false, false, &mut fresh), Ok(Some(2)));
        assert_eq!(links.undefined().count(), 0);
    }

    #[test]
    fn a_redeclaring_definition_replaces_the_unread_link_of_its_dependency() {
        let mut links = ModuleLinks::<&str, u32>::default();
        let mut fresh = counter();
        links.enter_module("common");
        assert_eq!(links.define("A", false, &mut fresh), Ok(1));
        links.enter_module("platform");
        assert_eq!(links.define("A", true, &mut fresh), Ok(2));
        assert_eq!(links.defined(&"A"), Some(2));
    }

    #[test]
    fn a_module_outside_the_dependency_chain_cannot_define_another_modules_identity() {
        let mut links = ModuleLinks::<&str, u32>::default();
        let mut fresh = counter();
        links.enter_module("left");
        assert_eq!(links.define("A", false, &mut fresh), Ok(1));
        links.enter_module("right");
        // Without a proven redeclaration the sibling's read finds the first link, and its own
        // definition conflicts with it.
        assert_eq!(links.read("A", true, false, &mut fresh), Ok(Some(1)));
        assert_eq!(
            links.define("A", false, &mut fresh),
            Err(LinkConflict::DefinedTwice)
        );
        assert_eq!(links.defined(&"A"), Some(1));
    }

    #[test]
    fn a_sibling_cannot_define_another_modules_reserved_link() {
        let mut links = ModuleLinks::<&str, u32>::default();
        let mut fresh = counter();
        links.enter_module("left");
        assert_eq!(links.read("A", true, false, &mut fresh), Ok(Some(1)));
        links.enter_module("right");
        assert_eq!(
            links.define("A", false, &mut fresh),
            Err(LinkConflict::DefinedTwice)
        );
        assert_eq!(links.undefined().collect::<Vec<_>>(), [&"A"]);
    }

    #[test]
    fn a_read_link_is_never_replaced() {
        let mut links = ModuleLinks::<&str, u32>::default();
        let mut fresh = counter();
        links.enter_module("common");
        assert_eq!(links.define("A", false, &mut fresh), Ok(1));
        links.enter_module("reader");
        assert_eq!(links.read("A", false, false, &mut fresh), Ok(Some(1)));
        links.enter_module("platform");
        assert_eq!(
            links.read("A", true, true, &mut fresh),
            Err(LinkConflict::ReplacedAfterRead)
        );
        assert_eq!(
            links.define("A", true, &mut fresh),
            Err(LinkConflict::ReplacedAfterRead)
        );
        assert_eq!(links.defined(&"A"), Some(1));
    }

    #[test]
    fn a_reserved_undefined_link_is_never_replaced_and_stays_reported() {
        let mut links = ModuleLinks::<&str, u32>::default();
        let mut fresh = counter();
        links.enter_module("common");
        assert_eq!(links.read("A", true, false, &mut fresh), Ok(Some(1)));
        links.enter_module("platform");
        assert_eq!(
            links.define("A", true, &mut fresh),
            Err(LinkConflict::ReplacedAfterRead)
        );
        assert_eq!(links.undefined().collect::<Vec<_>>(), [&"A"]);
    }

    #[test]
    fn a_module_that_does_not_redeclare_an_identity_reads_the_earlier_modules_link() {
        let mut links = ModuleLinks::<&str, u32>::default();
        links.enter_module("common");
        assert_eq!(links.define("A", false, || 1), Ok(1));
        links.enter_module("app");
        assert_eq!(links.read("A", false, false, || 2), Ok(Some(1)));
    }

    #[test]
    fn a_read_after_the_declaring_file_finds_its_link() {
        let mut links = ModuleLinks::<&str, u32>::default();
        assert_eq!(links.define("A", false, || 7), Ok(7));
        assert_eq!(links.read("A", false, false, || 8), Ok(Some(7)));
    }

    #[test]
    fn an_undeclared_read_reserves_nothing() {
        let mut links = ModuleLinks::<&str, u32>::default();
        assert_eq!(links.read("A", false, false, || 1), Ok(None));
        assert_eq!(links.undefined().count(), 0);
    }
}
