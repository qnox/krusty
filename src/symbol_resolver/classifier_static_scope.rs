//! The static scope of a classifier named as a call qualifier (`Classifier.name(...)`): its own
//! classifier callables, without the members of its object or companion value.

use crate::libraries::FunctionInfo;
use crate::symbol_source::SymbolSource as _;
use crate::types::{Ty, TypeName};

use super::{direct_supertypes, FnKind, SymbolResolver};

impl SymbolResolver<'_> {
    /// The callables of `Classifier.name(...)` in the classifier's static scope alone: its own
    /// classifier callables, or the nearest supertype's when it inherits them. A qualifier whose
    /// object or companion value is not a receiver (one with type arguments) selects only these.
    pub(crate) fn classifier_static_call_candidates(
        &self,
        internal: TypeName,
        name: &str,
    ) -> Option<Vec<FunctionInfo>> {
        let classifier = self.src.classifier(internal)?;
        // Associated declarations are an earlier rung of their own; see
        // `classifier_associated_callables`.
        let mut candidates = classifier
            .classifier_callables(internal)
            .into_iter()
            .filter(|member| member.name == name && member.associated_classifier.is_none())
            .map(|member| FunctionInfo::classifier_member(FnKind::Member, internal, member))
            .collect::<Vec<_>>();
        if candidates.is_empty() && self.lib.inherits_classifier_callables(internal) {
            let mut seen = crate::fx_hash::HashSet::from_iter([internal]);
            let mut frontier = direct_supertypes(&self.src, Ty::obj_name(internal));
            let mut rank = 1;
            while !frontier.is_empty() {
                let mut next = Vec::new();
                for current in frontier {
                    let Some(owner) = current.kotlin_class_internal() else {
                        continue;
                    };
                    if !seen.insert(owner) {
                        continue;
                    }
                    if let Some(classifier) = self.src.classifier(owner) {
                        candidates.extend(
                            classifier
                                .classifier_callables(owner)
                                .into_iter()
                                .filter(|member| member.name == name)
                                .map(|member| {
                                    let mut candidate = FunctionInfo::classifier_member(
                                        FnKind::Member,
                                        owner,
                                        member,
                                    );
                                    candidate.receiver_rank = rank;
                                    candidate
                                }),
                        );
                        next.extend(direct_supertypes(&self.src, current));
                    }
                }
                if !candidates.is_empty() {
                    break;
                }
                frontier = next;
                rank += 1;
            }
        }
        Some(candidates)
    }
}
