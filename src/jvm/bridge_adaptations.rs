//! JVM value-class adapters selected for common bridge declarations.
//!
//! Common IR owns the declaration bridge: its name, its erased and concrete signatures, and the
//! target it delegates to. How a boxed value class crosses that boundary is a JVM representation
//! plan, so the value-class pass records it here, keyed by the bridge's stable owner and ordinal,
//! and JVM bridge emission consumes it. [`crate::ir::Bridge`] carries none of it.

use std::collections::HashMap;

use crate::types::TypeName;

/// One value class a bridge boxes or unboxes, and whether a null goes past the adapter: a nullable
/// value class whose carrier holds the null itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ValueClassAdapter {
    pub(crate) owner: TypeName,
    pub(crate) null_preserving: bool,
}

impl ValueClassAdapter {
    pub(crate) fn new(owner: TypeName, null_preserving: bool) -> Self {
        Self {
            owner,
            null_preserving,
        }
    }
}

/// What a bridge does with the value its target returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BridgeResultAdapter {
    /// The target returns the carrier and the supertype sees the box: `<owner>.box-impl`.
    Box(ValueClassAdapter),
    /// The supertype spells the value class unboxed and the target hands back a reference: the
    /// carrier comes out of `<owner>.unbox-impl`.
    Unbox(ValueClassAdapter),
}

/// The whole value-class plan of one bridge.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BridgeAdapter {
    pub(crate) result: Option<BridgeResultAdapter>,
    /// Per concrete parameter, the value class whose box arrives in an erased reference slot and is
    /// unboxed before the target call. A position past the end takes no adapter.
    pub(crate) parameters: Vec<Option<ValueClassAdapter>>,
}

impl BridgeAdapter {
    fn is_empty(&self) -> bool {
        self.result.is_none() && self.parameters.iter().all(Option::is_none)
    }

    /// The adapter for the value the bridge passes as concrete parameter `index`.
    pub(crate) fn parameter(&self, index: usize) -> Option<ValueClassAdapter> {
        self.parameters.get(index).copied().flatten()
    }
}

/// Per-file bridge plans, keyed by stable owning-class identity and bridge ordinal.
#[derive(Default)]
pub(crate) struct BridgeAdaptations {
    entries: HashMap<(TypeName, u32), BridgeAdapter>,
}

impl BridgeAdaptations {
    /// Record the plans of one class's bridges. A bridge is planned exactly once; a bridge with
    /// nothing to adapt records nothing.
    pub(crate) fn extend(
        &mut self,
        plans: impl IntoIterator<Item = ((TypeName, u32), BridgeAdapter)>,
    ) {
        for (identity, plan) in plans {
            if plan.is_empty() {
                continue;
            }
            let previous = self.entries.insert(identity, plan);
            assert!(previous.is_none(), "a bridge is adapted exactly once");
        }
    }

    pub(crate) fn get(&self, owner: TypeName, bridge: u32) -> Option<&BridgeAdapter> {
        self.entries.get(&(owner, bridge))
    }

    /// Whether `owner`'s bridge `bridge` boxes its result.
    pub(crate) fn boxes_result(&self, owner: TypeName, bridge: u32) -> bool {
        self.get(owner, bridge)
            .is_some_and(|plan| matches!(plan.result, Some(BridgeResultAdapter::Box(_))))
    }

    /// Stop boxing the result of `owner`'s bridge `bridge`, whose target now returns the box.
    pub(crate) fn remove_result_box(&mut self, owner: TypeName, bridge: u32) {
        let Some(plan) = self.entries.get_mut(&(owner, bridge)) else {
            return;
        };
        if matches!(plan.result, Some(BridgeResultAdapter::Box(_))) {
            plan.result = None;
        }
        if plan.is_empty() {
            self.entries.remove(&(owner, bridge));
        }
    }

    /// Follow `owner`'s bridge list as it drops the bridges `kept` answers `false` for: a surviving
    /// bridge's plan moves to its new ordinal, and a dropped bridge's plan goes with it.
    pub(crate) fn retain(&mut self, owner: TypeName, kept: &[bool]) {
        let mut ordinal = 0u32;
        let mut moved = Vec::new();
        for (bridge, &keep) in kept.iter().enumerate() {
            let bridge = u32::try_from(bridge).expect("bridge ordinals fit u32");
            let plan = self.entries.remove(&(owner, bridge));
            if keep {
                moved.extend(plan.map(|plan| ((owner, ordinal), plan)));
                ordinal += 1;
            }
        }
        self.entries.extend(moved);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxing(owner: TypeName) -> BridgeAdapter {
        BridgeAdapter {
            result: Some(BridgeResultAdapter::Box(ValueClassAdapter::new(
                owner, false,
            ))),
            parameters: Vec::new(),
        }
    }

    #[test]
    fn a_dropped_bridge_moves_the_plans_after_it() {
        let owner = crate::types::type_name("p/Owner");
        let value = crate::types::type_name("p/Value");
        let mut adaptations = BridgeAdaptations::default();
        adaptations.extend([((owner, 0), boxing(value)), ((owner, 2), boxing(value))]);
        adaptations.retain(owner, &[false, true, true]);
        assert_eq!(adaptations.get(owner, 0), None);
        assert_eq!(adaptations.get(owner, 1), Some(&boxing(value)));
        assert_eq!(adaptations.get(owner, 2), None);
    }

    #[test]
    fn removing_a_result_box_keeps_the_parameter_plan() {
        let owner = crate::types::type_name("p/Owner");
        let value = crate::types::type_name("p/Value");
        let parameter = ValueClassAdapter::new(value, true);
        let mut adaptations = BridgeAdaptations::default();
        adaptations.extend([(
            (owner, 0),
            BridgeAdapter {
                parameters: vec![None, Some(parameter)],
                ..boxing(value)
            },
        )]);
        adaptations.remove_result_box(owner, 0);
        assert!(!adaptations.boxes_result(owner, 0));
        let plan = adaptations
            .get(owner, 0)
            .expect("the parameter plan survives");
        assert_eq!(plan.parameter(1), Some(parameter));
        assert_eq!(plan.parameter(2), None);
    }
}
