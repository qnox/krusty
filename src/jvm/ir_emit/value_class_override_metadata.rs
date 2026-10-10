//! How the JVM realizes a value class's generated `equals`/`hashCode`/`toString` overrides.

use crate::ir::IrValueClassAnyMember;
use crate::metadata::class_builder::JvmFunctionSignature;

/// Each override dispatches to a differently-named static `-impl` taking the erased `underlying`
/// descriptor, so each records a `JvmMethodSignature` (name + desc).
pub(super) fn signature(underlying: &str, member: IrValueClassAnyMember) -> JvmFunctionSignature {
    let (name, desc) = match member {
        IrValueClassAnyMember::Equals => {
            ("equals-impl", format!("({underlying}Ljava/lang/Object;)Z"))
        }
        IrValueClassAnyMember::HashCode => ("hashCode-impl", format!("({underlying})I")),
        IrValueClassAnyMember::ToString => {
            ("toString-impl", format!("({underlying})Ljava/lang/String;"))
        }
    };
    JvmFunctionSignature {
        name: Some(name.into()),
        desc: Some(desc),
    }
}
