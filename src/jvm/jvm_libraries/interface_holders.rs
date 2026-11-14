//! The receiver-first `$DefaultImpls` static a legacy (`-jvm-default=disable`) Kotlin interface
//! declaration is realized by, coupled to the metadata declaration at the provider boundary.

use super::LibraryCallable;
use crate::types::TypeName;

/// Route an interface accessor's nonvirtual calls to its legacy holder static, if it has one.
pub(super) fn attach_interface_holder(
    cp: &crate::jvm::classpath::Classpath,
    interface: TypeName,
    callable: &mut LibraryCallable,
) {
    callable.nonvirtual_realization = interface_holder_method(
        cp,
        interface,
        callable.physical_name(),
        &callable.descriptor,
    );
}

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
    if !interface_class.is_interface() {
        return None;
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::classfile::{
        ClassWriter, CodeBuilder, ACC_ABSTRACT, ACC_INTERFACE, ACC_PUBLIC, ACC_STATIC,
    };
    use crate::jvm::classpath::Classpath;
    use crate::types::type_name;

    fn owner(name: &str, is_interface: bool) -> Vec<u8> {
        let mut class = ClassWriter::new(name, "java/lang/Object");
        class.set_access(ACC_PUBLIC | ACC_ABSTRACT | if is_interface { ACC_INTERFACE } else { 0 });
        class.add_abstract_method(ACC_PUBLIC, "call", "()V");
        class.finish()
    }

    fn default_impls(owner: &str) -> Vec<u8> {
        let mut class = ClassWriter::new(&format!("{owner}$DefaultImpls"), "java/lang/Object");
        let mut body = CodeBuilder::new(1);
        body.ret_void();
        class.add_method(
            ACC_PUBLIC | ACC_STATIC,
            "call",
            &format!("(L{owner};)V"),
            &body,
        );
        class.finish()
    }

    #[test]
    fn only_interface_declarations_can_use_default_impls_holders() {
        let interface = "holder_fixture/Contract";
        let abstract_class = "holder_fixture/AbstractClass";
        let cp = Classpath::new(Vec::new());
        cp.set_stub_overlay(vec![
            (interface.to_owned(), owner(interface, true)),
            (
                format!("{interface}$DefaultImpls"),
                default_impls(interface),
            ),
            (abstract_class.to_owned(), owner(abstract_class, false)),
            (
                format!("{abstract_class}$DefaultImpls"),
                default_impls(abstract_class),
            ),
        ]);

        let realization = interface_holder_method(&cp, type_name(interface), "call", "()V")
            .expect("the interface's exact receiver-first holder method");
        assert_eq!(
            realization.owner,
            crate::types::type_name_nested_child(type_name(interface), "DefaultImpls")
        );
        assert_eq!(realization.descriptor, format!("(L{interface};)V"));
        assert!(interface_holder_method(&cp, type_name(abstract_class), "call", "()V").is_none());
    }
}
