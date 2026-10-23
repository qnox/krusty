//! The JVM's names for declarations, as a consumer that reports them reads them.

use super::JvmLibraries;
use crate::libraries::PlatformAbi;
use crate::types::Ty;

impl PlatformAbi for JvmLibraries {
    fn type_descriptor(&self, ty: Ty) -> Option<String> {
        (!ty.mentions_pending() && !ty.mentions_error())
            .then(|| crate::jvm::names::type_descriptor(ty))
    }

    fn mangle_suffix(&self, signature: &str) -> String {
        crate::jvm::inline_class::mangle_suffix(signature)
    }

    fn getter_name(&self, property: &str) -> String {
        crate::names::property_getter_name(property)
    }

    fn setter_name(&self, property: &str) -> String {
        crate::names::property_setter_name(property)
    }
}
