//! Where the JVM backend writes a function's method.

use super::StaticOwner;

/// Where a method body is written: on its declaring class, or onto an interface's
/// `$DefaultImpls` holder, where the code and slots are the instance method's but the method is
/// `static` and its descriptor carries the receiver as parameter 0.
#[derive(Clone, Copy)]
pub(super) enum MethodPlacement {
    Declared {
        static_owner: StaticOwner,
        instance: bool,
    },
    Holder(crate::types::TypeName),
}
