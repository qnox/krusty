//! The access site a resolver answers visibility for: its package, file, lexically enclosing
//! classifiers, and lexical visibility-suppression policy, with the classifier, member, and
//! property access predicates every lookup reads from them.

use super::*;

impl SymbolResolver<'_> {
    pub(crate) fn with_access_context(
        mut self,
        package: TypeName,
        file: u32,
        classes: Vec<TypeName>,
    ) -> Self {
        self.access_package = Some(package);
        self.access_file = Some(file);
        self.lexical_classes = classes;
        self
    }

    /// Install the access site's lexical visibility-suppression policy (see
    /// [`Self::visibility_suppressed`]). The policy is resolved by the caller from the
    /// `kotlin.Suppress` applications in force at the site; this resolver never derives it.
    pub(crate) fn with_visibility_suppression(mut self, suppressed: bool) -> Self {
        self.visibility_suppressed = suppressed;
        self
    }

    pub(super) fn classifier_accessible(&self, internal: TypeName) -> bool {
        let Some(classifier) = self.src.classifier(internal) else {
            return false;
        };
        if self.visibility_suppressed {
            return true;
        }
        // A companion's physical nested classifier may be non-public even though source code exposes
        // it through the outer classifier's public companion value (`Double.Companion`). Admit exactly
        // that metadata edge; an arbitrary internal nested classifier remains inaccessible.
        if let Some(owner) = internal.nested_owner() {
            let exposed_companion = self
                .src
                .classifier(owner)
                .and_then(|outer| outer.companion_object.as_ref().map(|(_, name)| *name))
                .is_some_and(|companion| companion == internal);
            if exposed_companion && self.classifier_accessible(owner) {
                return true;
            }
        }
        let visibility = classifier.access.visibility();
        if classifier.access == crate::libraries::ClassifierAccess::Public
            || (classifier.access == crate::libraries::ClassifierAccess::Internal
                && (self
                    .module
                    .is_some_and(|module| module.classifier(internal).is_some())
                    || self.lib.internal_accessible(internal)))
        {
            return true;
        }
        if classifier.access == crate::libraries::ClassifierAccess::PackagePrivate
            && self.package_private_member_accessible(internal)
        {
            return true;
        }
        if classifier.access == crate::libraries::ClassifierAccess::Private
            && !classifier.is_nested
            && classifier.source_file == self.access_file
        {
            return true;
        }
        // A private/protected nested classifier is a member of its enclosing classifier. Module
        // lookup therefore needs the lexical owner stack: code in `Outer` (or one of its nested
        // classes) may name `Outer$Hidden` / `Outer$Stage` directly. Protected access from a
        // subclass is handled by the inherited-classifier walk below; this arm covers the declaring
        // class itself, which does not inherit from itself. The module source separately handles
        // top-level file-private declarations.
        if matches!(
            visibility,
            crate::types::Visibility::Private | crate::types::Visibility::Protected
        ) && self
            .module
            .is_some_and(|module| module.classifier(internal).is_some())
        {
            // A private member of a companion object is visible throughout the class that owns the
            // companion (kotlinc's private visibility treats the companion's containing class as
            // the declaring scope), so `Outer` may name `Outer$Companion$Hidden`.
            let declaring_owner = internal.nested_owner().map(|declaring_owner| {
                match declaring_owner.nested_owner() {
                    Some(outer)
                        if visibility == crate::types::Visibility::Private
                            && self.src.classifier(outer).and_then(|outer| {
                                outer.companion_object.as_ref().map(|(_, name)| *name)
                            }) == Some(declaring_owner) =>
                    {
                        outer
                    }
                    _ => declaring_owner,
                }
            });
            if declaring_owner.is_some_and(|declaring_owner| {
                self.lexical_classes
                    .iter()
                    .copied()
                    .any(|lexical| lexical.same_or_nested_within(declaring_owner))
            }) {
                return true;
            }
        }
        if !classifier.is_nested {
            return false;
        }
        let simple = internal.nested_segment_ref();
        self.lexical_classes.iter().copied().any(|owner| {
            inherited_nested_classifier_name(
                simple,
                direct_supertypes(&self.src, Ty::obj_name(owner))
                    .into_iter()
                    .filter_map(Ty::kotlin_class_internal)
                    .collect(),
                |candidate_owner| {
                    direct_supertypes(&self.src, Ty::obj_name(candidate_owner))
                        .into_iter()
                        .filter_map(Ty::kotlin_class_internal)
                        .collect()
                },
                |candidate| inherited_classifier_shape(&self.src, candidate, owner).is_some(),
            ) == InheritedNestedClassifier::Found(internal)
        })
    }

    pub(crate) fn inaccessible_classifier_access(
        &self,
        internal: TypeName,
    ) -> Option<crate::symbol_source::ClassifierAccess> {
        let access = self.src.classifier(internal)?.access;
        (!self.classifier_accessible(internal)).then_some(access)
    }

    pub(super) fn package_private_member_accessible(&self, owner: TypeName) -> bool {
        self.visibility_suppressed
            || self
                .access_package
                .is_some_and(|package| package == owner.namespace())
    }

    /// Source visibility of a package-level or extension property from this access site.
    pub(super) fn property_visible(&self, property: &PropertyInfo) -> bool {
        self.visibility_suppressed || source_property_visible(self.lib, property)
    }
}
