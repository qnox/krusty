//! The lexical owner of each classifier the parser hoists out of a classifier body.
//!
//! A nested classifier is published in [`File::decls`] beside its owner rather than inside it, and
//! its qualified name is only a spelling. The owner edge is recorded here, by identity, when the
//! owner itself is published: everything hoisted while its body was parsed and not yet claimed by a
//! nearer owner is its direct member.

use super::{Decl, DeclId, File};

impl File {
    /// Record `owner` as the owner of the classifiers hoisted from its body, which occupy
    /// `self.decls[from..]`. Classifiers nested in a local class or an enum entry body, and
    /// anonymous-object classes, belong to those scopes instead and are never claimed.
    pub(crate) fn adopt_hoisted_classifiers(&mut self, owner: DeclId, from: usize) {
        let members = self.decls[from..]
            .iter()
            .copied()
            .filter(|&declaration| {
                declaration != owner
                    && matches!(self.decl(declaration), Decl::Class(_))
                    && !self.hoisted_classifier_owners.contains_key(&declaration)
                    && !self
                        .enum_entry_nested_classifier_owners
                        .contains_key(&declaration)
                    && !self.is_anonymous_object_class(declaration)
                    && !self
                        .local_class_nested
                        .values()
                        .any(|nested| nested.contains(&declaration))
            })
            .collect::<Vec<_>>();
        for member in members {
            self.hoisted_classifier_owners.insert(member, owner);
        }
    }

    /// The classifier whose body declares `classifier`, or `None` for a top-level or local one.
    pub fn hoisted_classifier_owner(&self, classifier: DeclId) -> Option<DeclId> {
        self.hoisted_classifier_owners.get(&classifier).copied()
    }
}
