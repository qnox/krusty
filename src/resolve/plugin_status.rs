//! Declaration status contributed by native compiler plugins: kotlinc's
//! `FirStatusTransformerExtension`, as all-open uses it.
//!
//! A plugin names annotations ([`crate::plugins::IrPlugin::open_by_default_annotations`]); a class
//! they match takes `open` as its default modality, and so does every member it declares. Matching
//! follows kotlinc's `AbstractSimpleClassPredicateMatchingService`: the class carries a named
//! annotation, carries an annotation that is meta-annotated with one at any depth, or has a supertype
//! that matches. Annotations and supertypes are the resolved identities of source declarations and
//! the normalized dependency classifiers; nothing is looked up by spelling.
//!
//! The status is applied once, where the frontend publishes each declaration's header, so the
//! checker, lowering and every backend read the transformed modality.

use std::collections::{HashMap, HashSet};

use crate::fir::{DeclarationFlags, DeclarationId, DeclarationKind, StreamedHeaderModule};
use crate::types::TypeName;

use super::SymbolTable;

/// The source declarations whose default modality a plugin's status transform makes `open`.
#[derive(Default)]
pub(super) struct OpenByDefault {
    /// Matched classifiers kotlinc transforms themselves: plain classes, not `@JvmRecord`.
    classes: HashSet<DeclarationId>,
    /// Matched classifiers whose declared members kotlinc transforms: non-local plain classes,
    /// and abstract or sealed value classes.
    member_owners: HashSet<DeclarationId>,
}

impl OpenByDefault {
    /// Match every source classifier against the annotations the enabled plugins name.
    pub(super) fn collect(
        headers: &StreamedHeaderModule,
        table: &SymbolTable,
        classifier_types: &HashMap<DeclarationId, TypeName>,
    ) -> OpenByDefault {
        let annotations = table
            .native_plugins
            .host("main")
            .open_by_default_annotations();
        if annotations.is_empty() {
            return OpenByDefault::default();
        }
        let mut matcher = Matcher {
            table,
            annotations: annotations.into_iter().collect(),
            classes: HashMap::new(),
            meta: HashMap::new(),
        };
        let jvm_record = crate::types::type_name("kotlin/jvm/JvmRecord");
        let mut open = OpenByDefault::default();
        for stub in &headers.stubs {
            if stub.kind != DeclarationKind::Classifier {
                continue;
            }
            let Some(&classifier) = classifier_types.get(&stub.id) else {
                continue;
            };
            let flags = stub.flags;
            if !is_class_kind(flags) || !matcher.class_matches(classifier) {
                continue;
            }
            let record = table
                .classes
                .get(&classifier)
                .is_some_and(|class| class.annotations.contains(&jvm_record));
            if !is_value(flags) && !record {
                open.classes.insert(stub.id);
            }
            let concrete_value = is_value(flags)
                && !flags.has(DeclarationFlags::ABSTRACT)
                && !flags.has(DeclarationFlags::SEALED);
            if !flags.has(DeclarationFlags::LOCAL_CLASS) && !concrete_value {
                open.member_owners.insert(stub.id);
            }
        }
        open
    }

    /// The header flags `declaration` publishes: its own flags, with `open` as the default modality
    /// where a plugin's status transform applies to it.
    pub(super) fn status(
        &self,
        headers: &StreamedHeaderModule,
        declaration: DeclarationId,
        kind: DeclarationKind,
        flags: DeclarationFlags,
    ) -> DeclarationFlags {
        if self.member_owners.is_empty() && self.classes.is_empty()
            || has_explicit_modality(kind, flags)
        {
            return flags;
        }
        let transformed = match kind {
            DeclarationKind::Classifier => self.classes.contains(&declaration),
            DeclarationKind::Function | DeclarationKind::Property => {
                !flags.has(DeclarationFlags::COMPILER_GENERATED)
                    && !flags.has(DeclarationFlags::GENERATED_STRUCTURAL_MEMBER)
                    && !flags.has(DeclarationFlags::COMPANION_BLOCK_MEMBER)
                    && headers
                        .declarations
                        .anchor(declaration)
                        .and_then(|anchor| anchor.owner)
                        .is_some_and(|owner| self.member_owners.contains(&owner))
            }
            _ => false,
        };
        if transformed {
            flags
                .with(DeclarationFlags::OPEN, true)
                .with(DeclarationFlags::FINAL, false)
        } else {
            flags
        }
    }
}

/// A written `final`, `open`, `abstract` or `sealed` (an `override` is open already). kotlinc's
/// transformer changes only the default modality, so an explicit one stays. A member's `FINAL` is
/// always explicit (written, or given by the compiler to a generated member); a classifier's is its
/// default, so a classifier's written `final` is `FINAL_MODIFIER`.
fn has_explicit_modality(kind: DeclarationKind, flags: DeclarationFlags) -> bool {
    flags.has(DeclarationFlags::FINAL_MODIFIER)
        || (kind != DeclarationKind::Classifier && flags.has(DeclarationFlags::FINAL))
        || flags.has(DeclarationFlags::OPEN)
        || flags.has(DeclarationFlags::ABSTRACT)
        || flags.has(DeclarationFlags::SEALED)
}

/// kotlinc's status transformer applies to a `class`: not an interface, object, enum or
/// annotation class.
fn is_class_kind(flags: DeclarationFlags) -> bool {
    !flags.has(DeclarationFlags::INTERFACE)
        && !flags.has(DeclarationFlags::SINGLETON)
        && !flags.has(DeclarationFlags::ENUM)
        && !flags.has(DeclarationFlags::ANNOTATION_CLASS)
}

fn is_value(flags: DeclarationFlags) -> bool {
    flags.has(DeclarationFlags::VALUE) || flags.has(DeclarationFlags::VALUE_KEYWORD)
}

/// kotlinc's predicate `annotated(names) or metaAnnotated(names, includeItself = true)`, extended to
/// supertypes. Answers are memoized per classifier; a classifier is marked unmatched while it is
/// being visited, so a cycle (an annotation annotated with itself) terminates.
struct Matcher<'a> {
    table: &'a SymbolTable,
    annotations: HashSet<TypeName>,
    classes: HashMap<TypeName, bool>,
    meta: HashMap<TypeName, bool>,
}

impl Matcher<'_> {
    fn class_matches(&mut self, classifier: TypeName) -> bool {
        if let Some(&known) = self.classes.get(&classifier) {
            return known;
        }
        self.classes.insert(classifier, false);
        let (annotations, supertypes) = self.annotations_and_supertypes(classifier);
        let matches = annotations
            .into_iter()
            .any(|annotation| self.annotation_matches(annotation))
            || supertypes
                .into_iter()
                .any(|supertype| self.class_matches(supertype));
        self.classes.insert(classifier, matches);
        matches
    }

    /// Whether `annotation` is one of the plugin's, or is annotated with one at any depth.
    fn annotation_matches(&mut self, annotation: TypeName) -> bool {
        if self.annotations.contains(&annotation) {
            return true;
        }
        if let Some(&known) = self.meta.get(&annotation) {
            return known;
        }
        self.meta.insert(annotation, false);
        let (meta_annotations, _) = self.annotations_and_supertypes(annotation);
        let matches = meta_annotations
            .into_iter()
            .any(|meta| self.annotation_matches(meta));
        self.meta.insert(annotation, matches);
        matches
    }

    /// A classifier's resolved annotations and direct supertypes, from its source declaration or
    /// its dependency provider.
    fn annotations_and_supertypes(&self, classifier: TypeName) -> (Vec<TypeName>, Vec<TypeName>) {
        if let Some(class) = self.table.classes.get(&classifier) {
            let supertypes = class
                .super_internal
                .into_iter()
                .chain(class.interfaces.iter())
                .collect();
            return (class.annotations.clone(), supertypes);
        }
        crate::symbol_source::SymbolSource::classifier(self.table.libraries.as_ref(), classifier)
            .map(|shape| {
                (
                    shape
                        .annotations
                        .iter()
                        .map(|annotation| annotation.annotation)
                        .collect(),
                    shape.supertypes.iter().collect(),
                )
            })
            .unwrap_or_default()
    }
}
