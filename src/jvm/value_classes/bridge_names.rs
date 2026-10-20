//! The name a bridge answers to.
//!
//! A bridge satisfies a supertype member, so it carries that member's JVM name: the source name,
//! mangled over that member's own declared signature when it mentions a value class. The override's
//! substituted parameters name the override, never the bridge: `I<T>.foo(x: T): IC` is
//! `foo-<hash of "_:LIC;">` on `I` and on every bridge to it, whatever `T` an implementation picks.

use super::{bridge_returns::mentions_value_class, erase, member_names::vc_mangle, Under};
use crate::fx_hash::HashSet;
use crate::ir::Bridge;
use crate::types::{Ty, TypeName};

pub(super) struct BridgeNaming<'a> {
    pub(super) owner: TypeName,
    pub(super) under: &'a Under,
    pub(super) suspend: &'a HashSet<(Option<TypeName>, String, usize)>,
}

impl BridgeNaming<'_> {
    /// Mangle the bridge's name over the supertype member's declared parameters and result.
    pub(super) fn mangle_by_supertype(&self, bridge: &mut Bridge) {
        bridge.name = self.mangled(bridge, &bridge.erased_params);
    }

    /// A supertype member with a value-class parameter of its own (`foo(i: T?)` with
    /// `T : Inlined` erases to `foo-<hash>(Inlined)`) is mangled whatever the override takes. The
    /// bridge takes that name and parameter, and calls the override by the override's own name.
    pub(super) fn answer_to_value_class_parameters(&self, bridge: &mut Bridge, target: &str) {
        if !mentions_value_class(&bridge.erased_params, Ty::Unit, self.under) {
            return;
        }
        bridge.name = self.mangled(bridge, &bridge.erased_params);
        bridge.target_name.get_or_insert_with(|| target.to_string());
        for parameter in &mut bridge.erased_params {
            *parameter = erase(parameter, self.under);
        }
    }

    fn mangled(&self, bridge: &Bridge, params: &[Ty]) -> String {
        let key = (Some(self.owner), bridge.name.clone(), params.len());
        let is_suspend = self.suspend.contains(&key);
        vc_mangle(
            &bridge.name,
            params,
            &bridge.erased_ret,
            self.under,
            false,
            is_suspend,
        )
    }
}
