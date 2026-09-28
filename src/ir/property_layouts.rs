//! How a source property was materialized in common IR: its storage and accessor declarations.

use super::{ClassId, FunId};
use crate::types::{Ty, TypeName};

/// Common-IR declaration layout for a source property. This records which semantic storage and
/// accessor declarations were materialized, but deliberately does not choose how an ordinary
/// property access uses them. That choice belongs to the target realization pass.
#[derive(Clone, Debug)]
pub enum IrLocalPropertyLayout {
    TopLevelStorage {
        storage: u32,
        getter: Option<FunId>,
        setter: Option<FunId>,
        /// Semantic singleton qualifier for a classifier-associated constant. `None` denotes a
        /// genuinely receiverless package property.
        qualifier: Option<TypeName>,
    },
    TopLevelAccessor {
        getter: FunId,
        setter: Option<FunId>,
        receiver: Option<Ty>,
        context_parameters: Vec<Ty>,
        /// The `x$delegate` static a delegated property's accessors read, which its
        /// `JvmPropertySignature` names as the property's field. `None` for a computed property.
        delegate: Option<u32>,
    },
    Member {
        class: ClassId,
        owner: TypeName,
        backing_field: Option<u32>,
        getter: Option<FunId>,
        setter: Option<FunId>,
        interface: bool,
        name: String,
        ty: Ty,
        mutable: bool,
        private: bool,
        context_parameters: Vec<Ty>,
        property: u32,
    },
    MemberExtension {
        owner: TypeName,
        interface: bool,
        name: String,
        getter: FunId,
        setter: Option<FunId>,
        receiver: Ty,
        ty: Ty,
        context_parameters: Vec<Ty>,
    },
}
