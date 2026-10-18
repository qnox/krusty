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
    /// A `@JvmStatic` member of a named object, written as a `static` method of the object. The
    /// common IR still numbers its receiver as value 0; that value reads the singleton's instance
    /// field instead of a parameter slot, and the descriptor carries no receiver.
    Singleton {
        static_owner: StaticOwner,
        singleton: crate::types::TypeName,
    },
}
