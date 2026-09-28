//! The bridges that take `FunctionN.invoke`'s packed argument array.
//!
//! Past the numbered function interfaces, a function-type supertype is `FunctionN`, whose
//! `invoke(Object[])` receives every erased argument in one array: its bridge checks the array's
//! length and casts each element to the override's parameter. That calling convention is the JVM's
//! realization of the common-IR bridge, so the bridge pass records it here and bridge emission
//! reads it, rather than common IR carrying it.

use std::collections::HashSet;

use crate::ir::Bridge;
use crate::types::{Ty, TypeName};

/// A bridge by its class and its own erased signature, which no later reordering or pruning of the
/// class's bridges changes.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct BridgeSignature {
    owner: TypeName,
    name: Box<str>,
    params: Box<[Ty]>,
    ret: Ty,
}

impl BridgeSignature {
    fn of(owner: TypeName, bridge: &Bridge) -> Self {
        Self {
            owner,
            name: bridge.name.as_str().into(),
            params: bridge.erased_params.as_slice().into(),
            ret: bridge.erased_ret,
        }
    }
}

/// The file's argument-array bridges.
#[derive(Default)]
pub(crate) struct FunctionArgumentArrays {
    bridges: HashSet<BridgeSignature>,
}

impl FunctionArgumentArrays {
    /// Record that `owner`'s `bridge` takes its arguments packed in one array.
    pub(crate) fn record(&mut self, owner: TypeName, bridge: &Bridge) {
        self.bridges.insert(BridgeSignature::of(owner, bridge));
    }

    /// Whether `owner`'s `bridge` takes its arguments packed in one array.
    pub(crate) fn packs(&self, owner: TypeName, bridge: &Bridge) -> bool {
        self.bridges.contains(&BridgeSignature::of(owner, bridge))
    }
}
