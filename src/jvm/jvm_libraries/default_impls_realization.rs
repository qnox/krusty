//! The legacy `$DefaultImpls` realization of a concrete interface declaration.

use crate::types::TypeName;

/// Exact nonvirtual realization of a legacy concrete interface declaration, if the classpath
/// publishes one. Semantic selection stays on the metadata declaration; this only couples it to the
/// matching receiver-first static method at the provider boundary.
pub(super) fn interface_holder_method(
    cp: &crate::jvm::classpath::Classpath,
    interface: TypeName,
    name: &str,
    descriptor: &str,
) -> Option<Box<crate::libraries::NonvirtualCallRealization>> {
    // `$DefaultImpls` also exists in compatibility mode, where the interface method itself is
    // concrete and remains the dispatch target. Only the legacy shape has an ABSTRACT interface
    // method whose implementation must be replaced by the receiver-first holder static. Keeping
    // this representation test here makes functions and property accessors share one ABI rule.
    let interface_class = cp.find_name(interface)?;
    if !interface_class.methods.iter().any(|method| {
        method.is_abstract() && method.name == name && method.descriptor == descriptor
    }) {
        return None;
    }
    let holder = crate::types::type_name_nested_child(interface, "DefaultImpls");
    let class = cp.find_name(holder)?;
    let method = class.methods.iter().find(|method| {
        method.is_static()
            && method.name == name
            && crate::jvm::names::descriptor_prepends_classifier(
                descriptor,
                interface,
                &method.descriptor,
            )
    })?;
    Some(Box::new(crate::libraries::NonvirtualCallRealization {
        owner: holder,
        descriptor: method.descriptor.clone(),
    }))
}
