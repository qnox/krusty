//! The roles kotlinx.serialization gives the members and classifiers it generates. The IR knows a
//! role only as a plugin name, a number and a placement; this module owns what the numbers mean.

use crate::ir::{IrGeneratedClassRole, IrGeneratedFunctionRole, IrMemberPlacement};
use crate::plugins::cli::SERIALIZATION_PLUGIN_ID;

/// A function kotlinx.serialization generates on a serialized class or its `$serializer`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SerializationFunction {
    Descriptor,
    Serialize,
    Deserialize,
    ChildSerializers,
    TypeParameterSerializers,
    /// `write$Self` on a serialized class.
    WriteSelf,
    /// A generic `$serializer`'s private `getTypeSerial<k>()` for type parameter `k`.
    TypeSerializerGetter(u32),
}

/// The first role number of the per-type-parameter getters, which follow the fixed roles.
const TYPE_SERIALIZER_GETTER_BASE: u32 = 16;

impl From<SerializationFunction> for IrGeneratedFunctionRole {
    fn from(function: SerializationFunction) -> Self {
        let (role, placement) = match function {
            SerializationFunction::Descriptor => (0, IrMemberPlacement::Declared),
            SerializationFunction::Serialize => (1, IrMemberPlacement::Declared),
            SerializationFunction::Deserialize => (2, IrMemberPlacement::Declared),
            SerializationFunction::ChildSerializers => (3, IrMemberPlacement::Declared),
            SerializationFunction::TypeParameterSerializers => (4, IrMemberPlacement::Declared),
            SerializationFunction::WriteSelf => (5, IrMemberPlacement::AfterDelegatedMembers),
            SerializationFunction::TypeSerializerGetter(k) => (
                TYPE_SERIALIZER_GETTER_BASE + k,
                IrMemberPlacement::AfterConstructors,
            ),
        };
        IrGeneratedFunctionRole {
            plugin: SERIALIZATION_PLUGIN_ID,
            role,
            placement,
        }
    }
}

/// A classifier kotlinx.serialization generates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SerializationClass {
    /// The nested `$serializer` classifier.
    Serializer,
}

impl From<SerializationClass> for IrGeneratedClassRole {
    fn from(class: SerializationClass) -> Self {
        match class {
            SerializationClass::Serializer => IrGeneratedClassRole {
                plugin: SERIALIZATION_PLUGIN_ID,
                role: 0,
            },
        }
    }
}
