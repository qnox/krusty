//! The names a platform gives declarations in its binary interface, for a consumer that reports
//! them, such as a KSP processor asking for a declaration's JVM signature.

use crate::types::Ty;

pub trait PlatformAbi {
    /// The descriptor of `ty`, with a type parameter erased to its bound and a value class spelled
    /// as the class, or `None` when the platform cannot spell it.
    fn type_descriptor(&self, ty: Ty) -> Option<String>;

    /// The suffix the platform appends to a function's name for its mangling signature.
    fn mangle_suffix(&self, signature: &str) -> String;

    /// The name of the getter of the property `property`.
    fn getter_name(&self, property: &str) -> String;

    /// The name of the setter of the property `property`.
    fn setter_name(&self, property: &str) -> String;
}
