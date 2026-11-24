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
//! A module may redeclare an identity an earlier module of its dependency chain declares: a
//! platform module's class over a common module's, the way classpath substitution lets the later
//! declaration replace the earlier one. The table remembers which module owns each link, so the
//! later module's own declaration and its reads get a link of their own while every module after
//! it reads the replacement; the earlier module's files, already lowered, keep their link.

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

/// A declaration defined twice: two files of one module realized one identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DefinedTwice;

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
            module: self.module.clone(),
        }
    }

    /// The link of `key` for a read: the defined or reserved one, or `None` when no file has
    /// realized it and `declared` (the module fact certifying it exists) is false. A read of an
    /// identity the current module declares never finds an earlier module's link: the module
    /// redeclares it, so the read reserves a link for the module's own declaration.
    pub fn read(&mut self, key: K, declared: bool, allocate: impl FnOnce() -> V) -> Option<V> {
        if let Some(link) = self.links.get(&key) {
            if !declared || link.module == self.module {
                return Some(link.target.clone());
            }
        } else if !declared {
            return None;
        }
        let target = allocate();
        let link = self.fresh(target.clone(), false);
        self.links.insert(key, link);
        Some(target)
    }

    /// The link the declaring file defines `key` under: the one an earlier read of the current
    /// module reserved, or a fresh one. A link another module owns is replaced, so later modules
    /// read the redeclaration.
    pub fn define(&mut self, key: K, allocate: impl FnOnce() -> V) -> Result<V, DefinedTwice> {
        match self.links.get_mut(&key) {
            Some(link) if link.module == self.module => {
                if link.defined {
                    return Err(DefinedTwice);
                }
                link.defined = true;
                return Ok(link.target.clone());
            }
            _ => {}
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

    #[test]
    fn a_read_before_the_declaring_file_reserves_the_link_it_defines() {
        let mut links = ModuleLinks::<&str, u32>::default();
        let mut next = 0;
        let mut fresh = || {
            next += 1;
            next
        };
        assert_eq!(links.read("A", true, &mut fresh), Some(1));
        assert_eq!(links.read("A", true, &mut fresh), Some(1));
        assert_eq!(links.undefined().collect::<Vec<_>>(), [&"A"]);
        assert_eq!(links.defined(&"A"), None);
        assert_eq!(links.define("A", &mut fresh), Ok(1));
        assert_eq!(links.defined(&"A"), Some(1));
        assert_eq!(links.undefined().count(), 0);
        assert_eq!(links.define("A", &mut fresh), Err(DefinedTwice));
    }

    #[test]
    fn a_later_module_redeclaring_an_identity_replaces_the_link_for_every_module_after_it() {
        let mut links = ModuleLinks::<&str, u32>::default();
        let mut next = 0;
        let mut fresh = || {
            next += 1;
            next
        };
        links.enter_module("common");
        assert_eq!(links.define("A", &mut fresh), Ok(1));
        links.enter_module("platform");
        // The platform module's own read, before its declaring file, reserves its own link.
        assert_eq!(links.read("A", true, &mut fresh), Some(2));
        assert_eq!(links.define("A", &mut fresh), Ok(2));
        assert_eq!(links.defined(&"A"), Some(2));
        links.enter_module("app");
        assert_eq!(links.read("A", false, &mut fresh), Some(2));
        assert_eq!(links.undefined().count(), 0);
    }

    #[test]
    fn a_module_that_does_not_redeclare_an_identity_reads_the_earlier_modules_link() {
        let mut links = ModuleLinks::<&str, u32>::default();
        links.enter_module("common");
        assert_eq!(links.define("A", || 1), Ok(1));
        links.enter_module("app");
        assert_eq!(links.read("A", false, || 2), Some(1));
    }

    #[test]
    fn a_read_after_the_declaring_file_finds_its_link() {
        let mut links = ModuleLinks::<&str, u32>::default();
        assert_eq!(links.define("A", || 7), Ok(7));
        assert_eq!(links.read("A", false, || 8), Some(7));
    }

    #[test]
    fn an_undeclared_read_reserves_nothing() {
        let mut links = ModuleLinks::<&str, u32>::default();
        assert_eq!(links.read("A", false, || 1), None);
        assert_eq!(links.undefined().count(), 0);
    }
}
