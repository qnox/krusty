//! Kotlin source-level member visibility and receiver constraints.

use super::*;

/// Whether the current compilation module declares `classifier` as an ordinary classifier.
///
/// This deliberately reads the module provider alone, not the federated resolver: dependency
/// classifiers stay visible for diagnostics but must not acquire same-module `internal` access.
/// The declaration facet is authoritative even while a streamed local classifier's full shape is
/// deferred, and its explicit `Ordinary` tag prevents a source typealias from claiming its target.
fn module_declares_classifier(module: &dyn SymbolSource, classifier: TypeName) -> bool {
    let (namespace, name) = crate::symbol_source::SymbolNamespace::classifier_key(classifier);
    matches!(
        module.symbols(namespace, name).classifier_declaration.as_ref(),
        Some(crate::libraries::ClassifierDeclaration::Ordinary(declared))
            if *declared == classifier
    )
}

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
                let module_owned = module_declares_classifier(&self.module, owner);
                let friend = self.libraries.internal_accessible(owner);
                module_owned || friend
            }
            Visibility::PackagePrivate => self.source_package_name() == owner.namespace(),
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

#[cfg(test)]
mod tests {
    /// The pre-finalization checker carries `ModuleSymbols`, not a resolved index. Its normalized
    /// declaration record must still prove that an internal member belongs to this compilation.
    #[test]
    fn legacy_checker_uses_module_identity_for_internal_members() {
        let source = "package first\n\
                      class Owner { internal fun hidden(): Int = 1 }\n\
                      fun use(owner: Owner): Int = owner.hidden()";
        let mut diagnostics = crate::diag::DiagSink::new();
        let tokens = crate::lexer::lex(source, &mut diagnostics);
        let file = crate::parser::parse(source, &tokens, &mut diagnostics);
        let files = vec![file];
        let mut symbols = super::super::collect_signatures(&files, &mut diagnostics);

        let _ = super::super::check_file(&files[0], &mut symbols, &mut diagnostics);

        assert_eq!(
            diagnostics
                .diags
                .iter()
                .map(|diagnostic| diagnostic.msg.as_str())
                .collect::<Vec<_>>(),
            Vec::<&str>::new()
        );
    }
}
