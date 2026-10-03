//! The implicit rungs of the unqualified name tower: implicit value receivers interleaved with the
//! static scopes of the classifiers open at a site.
//!
//! Checked bodies and compact signature solving both walk this one priority order, so an
//! unqualified call, property read or `::name` reference binds the same declaration on either
//! path. Each side supplies its own receiver representation and says which classifier's instance
//! a receiver is.

use crate::types::TypeName;

/// One rung of the unqualified tower, nearest first.
pub(super) enum ImplicitRung<R> {
    Receiver(R),
    /// Synthetic classifier properties of this owner, immediately before its static scope.
    ///
    /// The owner is the classifier the tower already selected. Source spelling is only the lookup
    /// name; [`crate::libraries::ImplicitClassifierProperty`] decides which property that name is.
    PrioritizedClassifierProperties(TypeName),
    StaticScope(TypeName),
}

/// Interleave `receivers` (nearest first) with `static_scopes` (innermost first). A classifier's
/// static scope directly follows the receiver that `instance_of` names as that classifier's own
/// instance, ahead of every outer receiver. A static scope with no such receiver (a companion
/// block member's classifier, which has no `this`) takes the same place on the tower: ahead of the
/// receiver `classifier_of` names as the classifier its `companion_of`, and otherwise after every
/// receiver.
///
/// When `prioritized_classifier_properties` is set, each static scope is preceded by that
/// classifier's synthetic-property rung, so those properties follow the classifier's own instance
/// members and precede its companion members and every outer rung.
pub(super) fn implicit_rungs<R>(
    receivers: impl IntoIterator<Item = R>,
    mut static_scopes: Vec<TypeName>,
    instance_of: impl Fn(&R) -> Option<TypeName>,
    classifier_of: impl Fn(&R) -> Option<TypeName>,
    companion_of: impl Fn(TypeName) -> Option<TypeName>,
    prioritized_classifier_properties: bool,
) -> Vec<ImplicitRung<R>> {
    let mut rungs = Vec::new();
    let push_scope = |rungs: &mut Vec<ImplicitRung<R>>, classifier: TypeName| {
        if prioritized_classifier_properties {
            rungs.push(ImplicitRung::PrioritizedClassifierProperties(classifier));
        }
        rungs.push(ImplicitRung::StaticScope(classifier));
    };
    for receiver in receivers {
        let own_scope = instance_of(&receiver)
            .and_then(|classifier| static_scopes.iter().position(|open| *open == classifier))
            .map(|index| static_scopes.remove(index));
        let companion_owner = classifier_of(&receiver).and_then(|classifier| {
            static_scopes
                .iter()
                .position(|open| companion_of(*open) == Some(classifier))
        });
        if let Some(index) = companion_owner.filter(|_| own_scope.is_none()) {
            push_scope(&mut rungs, static_scopes.remove(index));
        }
        rungs.push(ImplicitRung::Receiver(receiver));
        if let Some(classifier) = own_scope {
            push_scope(&mut rungs, classifier);
        }
    }
    for classifier in static_scopes {
        push_scope(&mut rungs, classifier);
    }
    rungs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::type_name;

    #[test]
    fn prioritized_classifier_properties_precede_each_static_scope() {
        let enum_owner = type_name("demo/E");
        let outer = type_name("demo/Outer");
        let companion = type_name("demo/E.Companion");
        let rungs = implicit_rungs(
            [enum_owner],
            vec![enum_owner, outer],
            |receiver| Some(*receiver),
            |receiver| Some(*receiver),
            |classifier| (classifier == enum_owner).then_some(companion),
            true,
        );
        assert!(matches!(
            rungs.as_slice(),
            [
                ImplicitRung::Receiver(receiver),
                ImplicitRung::PrioritizedClassifierProperties(prioritized),
                ImplicitRung::StaticScope(scope),
                ImplicitRung::PrioritizedClassifierProperties(outer_prioritized),
                ImplicitRung::StaticScope(outer_scope),
            ] if *receiver == enum_owner
                && *prioritized == enum_owner
                && *scope == enum_owner
                && *outer_prioritized == outer
                && *outer_scope == outer
        ));
    }

    #[test]
    fn a_companion_block_puts_synthetic_properties_ahead_of_the_companion_receiver() {
        let enum_owner = type_name("demo/E");
        let companion = type_name("demo/E.Companion");
        let rungs = implicit_rungs(
            [companion],
            vec![enum_owner],
            |_| None,
            |receiver| Some(*receiver),
            |classifier| (classifier == enum_owner).then_some(companion),
            true,
        );
        assert!(matches!(
            rungs.as_slice(),
            [
                ImplicitRung::PrioritizedClassifierProperties(owner),
                ImplicitRung::StaticScope(scope),
                ImplicitRung::Receiver(receiver),
            ] if *owner == enum_owner && *scope == enum_owner && *receiver == companion
        ));
    }

    #[test]
    fn disabled_prioritized_properties_leave_the_tower_unchanged() {
        let enum_owner = type_name("demo/E");
        let rungs = implicit_rungs(
            [enum_owner],
            vec![enum_owner],
            |receiver| Some(*receiver),
            |_| None,
            |_| None,
            false,
        );
        assert!(matches!(
            rungs.as_slice(),
            [ImplicitRung::Receiver(_), ImplicitRung::StaticScope(_)]
        ));
    }
}
