//! The class predicate kotlinc's annotation-driven plugins share
//! (`AbstractSimpleClassPredicateMatchingService`): all-open's status transform and no-arg's
//! constructor generation both apply to a class that carries one of the plugin's annotations,
//! carries an annotation meta-annotated with one at any depth, or has a supertype that matches.
//! Annotations and supertypes are the resolved identities of source declarations and the
//! normalized dependency classifiers; nothing is looked up by spelling.

use std::collections::{HashMap, HashSet};

use crate::types::TypeName;

use super::SymbolTable;

/// kotlinc's predicate `annotated(names) or metaAnnotated(names, includeItself = true)`, extended to
/// supertypes. Answers are memoized per classifier; a classifier is marked unmatched while it is
/// being visited, so a cycle (an annotation annotated with itself) terminates.
pub(super) struct ClassPredicate<'a> {
    table: &'a SymbolTable,
    annotations: HashSet<TypeName>,
    classes: HashMap<TypeName, bool>,
    /// Positive meta-annotation reachability. A negative result cannot be cached while walking a
    /// cycle: another node in that cycle may still reach a configured annotation after the edge
    /// back to the active node. Each top-level query therefore owns its visiting set, and only a
    /// proven match becomes shared state.
    matching_meta: HashSet<TypeName>,
}

impl<'a> ClassPredicate<'a> {
    pub(super) fn new(table: &'a SymbolTable, annotations: Vec<TypeName>) -> Self {
        ClassPredicate {
            table,
            annotations: annotations.into_iter().collect(),
            classes: HashMap::new(),
            matching_meta: HashSet::new(),
        }
    }

    pub(super) fn class_matches(&mut self, classifier: TypeName) -> bool {
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
        self.annotation_matches_from(annotation, &mut HashSet::new())
    }

    fn annotation_matches_from(
        &mut self,
        annotation: TypeName,
        visiting: &mut HashSet<TypeName>,
    ) -> bool {
        if self.annotations.contains(&annotation) {
            return true;
        }
        if self.matching_meta.contains(&annotation) {
            return true;
        }
        if !visiting.insert(annotation) {
            return false;
        }
        let (meta_annotations, _) = self.annotations_and_supertypes(annotation);
        let matches = meta_annotations
            .into_iter()
            .any(|meta| self.annotation_matches_from(meta, visiting));
        visiting.remove(&annotation);
        if matches {
            self.matching_meta.insert(annotation);
        }
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
