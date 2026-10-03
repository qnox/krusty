//! Kotlin source-level member visibility and receiver constraints.

use super::*;

/// Whether the current compilation module declares `classifier` as an ordinary classifier.
///
/// This deliberately reads the module provider alone, not the federated resolver: dependency
/// classifiers stay visible for diagnostics but must not acquire same-module `internal` access.
/// The declaration facet is authoritative even while a streamed local classifier's full shape is
/// deferred, and its explicit `Ordinary` tag prevents a source typealias from claiming its target.
pub(super) fn module_declares_classifier(module: &dyn SymbolSource, classifier: TypeName) -> bool {
    let (namespace, name) = crate::symbol_source::SymbolNamespace::classifier_key(classifier);
    matches!(
        module.symbols(namespace, name).classifier_declaration.as_ref(),
        Some(crate::libraries::ClassifierDeclaration::Ordinary(declared))
            if *declared == classifier
    )
}

/// Facts a use site supplies to the one source-level visibility operation. Checking and signature
/// inference both call that operation; neither keeps a second copy of the rules.
pub(super) trait SourceMemberSite {
    fn visibility_suppressed(&self) -> bool;
    fn source_package(&self) -> TypeName;
    fn module_owns(&self, owner: TypeName) -> bool;
    fn internal_friend(&self, owner: TypeName) -> bool;
    fn access_classes(&self) -> Vec<TypeName>;
    fn protected_classes(&self) -> Vec<TypeName>;
    fn companion_of(&self, enclosing: TypeName) -> Option<TypeName>;
    fn is_subtype(&self, subclass: TypeName, superclass: TypeName) -> bool;
    fn receiver_assignable(&self, receiver: Ty, access_classifier: TypeName) -> bool;
}

/// Whether a member of `owner` with visibility `vis` is accessible from `site`. `internal` is
/// accessible when this module declares the owner or a friend module does. `private` reaches the
/// declaring class, classes lexically nested inside it, and a class whose companion declares the
/// member. `protected` reaches those plus any subclass of `owner`. A top-level site with no
/// enclosing class cannot see a non-public member. Java package-private declarations are
/// accessible from their declaring package.
pub(super) fn site_member_accessible(
    site: &impl SourceMemberSite,
    vis: Visibility,
    owner: TypeName,
) -> bool {
    if site.visibility_suppressed() {
        return true;
    }
    match vis {
        Visibility::Public => true,
        Visibility::Internal => site.module_owns(owner) || site.internal_friend(owner),
        Visibility::PackagePrivate => site.source_package() == owner.namespace(),
        Visibility::Private | Visibility::Protected => {
            // Access is lexical, so the enclosing chain is walked, not the receiver chain: a
            // nested class has no outer receiver, yet it sits inside its outer class's body and
            // can reach that class's private members, including its companion's. Reaching down
            // from an enclosing class is the companion's privilege alone.
            site.access_classes().into_iter().any(|enclosing| {
                let companion = site.companion_of(enclosing);
                let nested_in_owner = std::iter::successors(enclosing.nested_owner(), |current| {
                    current.nested_owner()
                })
                .any(|ancestor| ancestor == owner);
                enclosing == owner
                    || nested_in_owner
                    || companion == Some(owner)
                    || (vis == Visibility::Protected
                        && (site.is_subtype(enclosing, owner)
                            || companion
                                .is_some_and(|companion| site.is_subtype(companion, owner))))
            })
        }
    }
}

/// [`site_member_accessible`], plus the protected-receiver constraint: a `protected` member is
/// usable on a receiver assignable to the accessing subclass (or to the declaring class itself).
pub(super) fn site_receiver_member_accessible(
    site: &impl SourceMemberSite,
    vis: Visibility,
    owner: TypeName,
    receiver: Ty,
) -> bool {
    if !site_member_accessible(site, vis, owner) {
        return false;
    }
    vis != Visibility::Protected
        || site
            .protected_classes()
            .into_iter()
            .flat_map(|enclosing| std::iter::once(enclosing).chain(site.companion_of(enclosing)))
            .any(|access_classifier| {
                (access_classifier == owner || site.is_subtype(access_classifier, owner))
                    && site.receiver_assignable(receiver, access_classifier)
            })
}

impl SourceMemberSite for Checker<'_> {
    fn visibility_suppressed(&self) -> bool {
        self.visibility_access_suppressed()
    }

    fn source_package(&self) -> TypeName {
        self.source_package_name()
    }

    fn module_owns(&self, owner: TypeName) -> bool {
        module_declares_classifier(&self.module, owner)
    }

    fn internal_friend(&self, owner: TypeName) -> bool {
        self.libraries.internal_accessible(owner)
    }

    fn access_classes(&self) -> Vec<TypeName> {
        self.access_context_class_names()
    }

    fn protected_classes(&self) -> Vec<TypeName> {
        self.lexical_source_class_names()
    }

    fn companion_of(&self, enclosing: TypeName) -> Option<TypeName> {
        self.resolver().classifier(enclosing).and_then(|class| {
            class
                .companion_object
                .as_ref()
                .map(|(_, companion)| *companion)
        })
    }

    fn is_subtype(&self, subclass: TypeName, superclass: TypeName) -> bool {
        self.obj_name_is_subtype(subclass, superclass)
    }

    fn receiver_assignable(&self, receiver: Ty, access_classifier: TypeName) -> bool {
        self.receiver_is_assignable(receiver, Ty::obj_name(access_classifier))
    }
}

impl<'a> Checker<'a> {
    /// Whether a member of `owner` with visibility `vis` is accessible from the current site.
    pub(super) fn member_accessible(&self, vis: Visibility, owner: TypeName) -> bool {
        site_member_accessible(self, vis, owner)
    }

    pub(super) fn receiver_member_accessible(
        &self,
        vis: Visibility,
        owner: TypeName,
        receiver: Ty,
    ) -> bool {
        site_receiver_member_accessible(self, vis, owner, receiver)
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
