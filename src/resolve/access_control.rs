//! Kotlin source-level member visibility and receiver constraints.

use super::*;

impl<'a> Checker<'a> {
    /// Whether a member of `owner` with visibility `vis` is accessible from the CURRENT site (the class
    /// being checked, `scope.this_ty()`), by Kotlin's rules. `internal` is accessible only when its
    /// declaring classifier belongs to this compilation module; dependency providers retain those
    /// declarations so this check can produce an accessibility diagnostic instead of unresolved.
    /// `private` reaches the declaring class and classes lexically nested inside it (an inner/nested
    /// class or the companion, whose JVM internal name is `<owner>$…`), plus — in the other direction
    /// — a class whose own COMPANION declares the member, since a companion's members are in the
    /// containing class's scope. `protected` reaches those plus any subclass of `owner`. At a
    /// top-level site (no enclosing class) a non-public member is inaccessible.
    /// Java package-private declarations are accessible from their declaring package.
    pub(super) fn member_accessible(&self, vis: Visibility, owner: TypeName) -> bool {
        if self.visibility_access_suppressed() {
            return true;
        }
        match vis {
            Visibility::Public => true,
            Visibility::Internal => {
                let module_owned = match self.resolved_index {
                    // The finalized module index contains only this compilation module. A
                    // dependency-source fallback is deliberately exposed through the semantic
                    // provider instead, so mere resolver visibility cannot grant `internal`
                    // access across that module boundary.
                    Some(index) => index.classifier_declaration(owner).is_some(),
                    // The legacy whole-source checker has no stable index; its module symbol
                    // table remains the only way to identify a same-compilation owner.
                    None => self
                        .module
                        .legacy_symbols()
                        .is_some_and(|symbols| symbols.class_by_type_name(owner).is_some()),
                };
                let friend = self.libraries.internal_accessible(owner);
                module_owned || friend
            }
            Visibility::PackagePrivate => {
                let declared = owner.package();
                self.source_package_name().matches(&declared)
            }
            Visibility::Private | Visibility::Protected => {
                // Access is LEXICAL, so the ENCLOSING chain is walked, not the receiver chain: a
                // NESTED (non-`inner`) class has no outer receiver at all, yet it sits inside its
                // outer class's body and Kotlin lets it reach that class's private members — including
                // its companion's. Reading the receiver labels alone reported `C.create()` from
                // `class C { companion object { private fun create() … }; class ZZZ { … } }` as
                // inaccessible, which kotlinc compiles.
                self.access_context_class_names()
                    .into_iter()
                    .any(|enclosing| {
                        // Reaching DOWN from an enclosing class is the COMPANION's privilege alone:
                        // its members belong to the containing class's scope. A sibling nested
                        // class's private member is not in that scope — kotlinc rejects `C.ZZZ`
                        // reading `C.Inner`'s private member, and the companion reading it too — so
                        // this arm names the companion instead of admitting every nested owner.
                        let companion_of_enclosing =
                            self.resolver().classifier(enclosing).and_then(|class| {
                                class.companion_object.as_ref().map(|(_, owner)| *owner)
                            }) == Some(owner);
                        let nested_in_owner =
                            std::iter::successors(enclosing.nested_owner(), |current| {
                                current.nested_owner()
                            })
                            .any(|ancestor| ancestor == owner);
                        enclosing == owner
                            || nested_in_owner
                            || companion_of_enclosing
                            // Protected access uses the federated subtype relation so dependency
                            // superclasses participate in the same check as source classes.
                            || (vis == Visibility::Protected
                                && (self.obj_name_is_subtype(enclosing, owner)
                                    || self
                                        .resolver()
                                        .classifier(enclosing)
                                        .and_then(|class| {
                                            class
                                                .companion_object
                                                .as_ref()
                                                .map(|(_, owner)| *owner)
                                        })
                                        .is_some_and(|companion| {
                                            self.obj_name_is_subtype(companion, owner)
                                        })))
                    })
            }
        }
    }

    pub(super) fn receiver_member_accessible(
        &self,
        vis: Visibility,
        owner: TypeName,
        receiver: Ty,
    ) -> bool {
        if !self.member_accessible(vis, owner) {
            return false;
        }
        vis != Visibility::Protected
            || self
                .lexical_source_class_names()
                .into_iter()
                .flat_map(|enclosing| {
                    std::iter::once(enclosing).chain(
                        self.resolver().classifier(enclosing).and_then(|class| {
                            class.companion_object.as_ref().map(|(_, owner)| *owner)
                        }),
                    )
                })
                .any(|access_classifier| {
                    (access_classifier == owner
                        || self.obj_name_is_subtype(access_classifier, owner))
                        && self.receiver_is_assignable(receiver, Ty::obj_name(access_classifier))
                })
    }
}
