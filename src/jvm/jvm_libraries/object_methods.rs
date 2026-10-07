//! Java interface methods that only redeclare `java.lang.Object`.
//!
//! kotlinc's `JavaMember.isObjectMethodInInterface` drops `toString()`, `hashCode()`, and
//! `equals(Object)` while loading an interface (`FirJavaFacade`). They are not members of the
//! interface: a call resolves to `kotlin.Any` and the JVM names `java/lang/Object`. A class that
//! declares the same method is a real override and stays.

use super::super::classreader::MethodSig;

/// Whether `method` is an interface redeclaration of an `Object` method kotlinc does not publish.
pub(super) fn is_object_method_in_interface(is_interface: bool, method: &MethodSig) -> bool {
    is_interface && is_object_method(&method.name, &method.descriptor)
}

/// `toString`/`hashCode` with no parameters, or `equals` whose single parameter is `Object`.
/// The class file stores the erased descriptor, which is the shape `isObjectMethod` checks.
fn is_object_method(name: &str, descriptor: &str) -> bool {
    let Some((parameters, _)) = descriptor.split_once(')') else {
        return false;
    };
    match name {
        "toString" | "hashCode" => parameters == "(",
        "equals" => parameters == "(Ljava/lang/Object;",
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::is_object_method;

    #[test]
    fn an_interface_object_method_is_not_a_member() {
        assert!(is_object_method("toString", "()Ljava/lang/String;"));
        assert!(is_object_method("hashCode", "()I"));
        assert!(is_object_method("equals", "(Ljava/lang/Object;)Z"));
    }

    #[test]
    fn a_different_signature_stays_a_member() {
        assert!(!is_object_method("toString", "(I)Ljava/lang/String;"));
        assert!(!is_object_method("equals", "(Ljava/lang/String;)Z"));
        assert!(!is_object_method("name", "()Ljava/lang/String;"));
        assert!(!is_object_method("compareTo", "(Ljava/lang/Object;)I"));
    }
}
