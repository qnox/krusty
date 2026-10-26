//! The JDK methods of a mapped collection interface that only its mutable Kotlin face carries.
//!
//! kotlinc joins a mapped collection's JDK interface members into its Kotlin builtin scopes, and
//! keeps the methods of `JvmBuiltInsSignatures.MUTABLE_METHOD_SIGNATURES` out of the read-only face
//! (`Collection.removeIf`, `List.sort`, `Map.putIfAbsent`). A class that implements the read-only
//! face therefore realizes them as throwing stubs, like the mutable builtin members. The checked-in
//! table names each method by its exact physical owner, name and descriptor.

use crate::types::{type_name, TypeName};

const MUTABLE_JDK_METHODS_2_4: &str = include_str!("mutable_jdk_methods_2_4.tsv");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MutableJdkMethod {
    jvm_owner: TypeName,
    physical_name: &'static str,
    descriptor: &'static str,
}

fn mutable_jdk_methods() -> &'static [MutableJdkMethod] {
    static METHODS: std::sync::OnceLock<Box<[MutableJdkMethod]>> = std::sync::OnceLock::new();
    METHODS.get_or_init(|| {
        MUTABLE_JDK_METHODS_2_4
            .lines()
            .enumerate()
            .filter_map(|(index, line)| {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    return None;
                }
                let mut fields = line.split('\t');
                let (Some(jvm_owner), Some(physical_name), Some(descriptor), None) =
                    (fields.next(), fields.next(), fields.next(), fields.next())
                else {
                    panic!("invalid mutable JDK method policy row {}", index + 1);
                };
                Some(MutableJdkMethod {
                    jvm_owner: type_name(jvm_owner),
                    physical_name,
                    descriptor,
                })
            })
            .collect::<Vec<_>>()
            .into_boxed_slice()
    })
}

/// Whether the method `physical_name` `descriptor` that `jvm_owner` declares belongs to the mutable
/// Kotlin face of `jvm_owner` only.
pub(in crate::jvm) fn is_mutable_only_jdk_method(
    jvm_owner: TypeName,
    physical_name: &str,
    descriptor: &str,
) -> bool {
    mutable_jdk_methods().iter().any(|method| {
        method.jvm_owner == jvm_owner
            && method.physical_name == physical_name
            && method.descriptor == descriptor
    })
}

#[cfg(test)]
mod tests {
    use super::is_mutable_only_jdk_method;
    use crate::types::type_name;

    #[test]
    fn the_policy_is_owner_and_descriptor_exact() {
        let list = type_name("java/util/List");
        let collection = type_name("java/util/Collection");
        assert!(is_mutable_only_jdk_method(
            list,
            "sort",
            "(Ljava/util/Comparator;)V"
        ));
        assert!(is_mutable_only_jdk_method(
            collection,
            "removeIf",
            "(Ljava/util/function/Predicate;)Z"
        ));
        // Declared by `Collection`, not `List`.
        assert!(!is_mutable_only_jdk_method(
            list,
            "removeIf",
            "(Ljava/util/function/Predicate;)Z"
        ));
        // A read-only JDK member, and another overload of a mutable one.
        assert!(!is_mutable_only_jdk_method(
            list,
            "getFirst",
            "()Ljava/lang/Object;"
        ));
        assert!(!is_mutable_only_jdk_method(
            type_name("java/util/Map"),
            "remove",
            "(Ljava/lang/Object;)Ljava/lang/Object;"
        ));
    }
}
