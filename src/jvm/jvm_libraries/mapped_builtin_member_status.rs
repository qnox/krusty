//! Provider-boundary policy for physical JVM members on mapped Kotlin builtin scopes.
//!
//! Kotlin builtins metadata owns the source declaration of mapped collections, while the mapped
//! JDK interface remains their physical realization. Kotlin's JVM builtins policy admits a small,
//! versioned set of methods from that realization. This module applies that policy while classfile
//! members are still provider-owned; core resolution receives only the normalized result.
//! The checked-in policy mirrors the collection-facing subset of
//! `JvmBuiltInsSignatures.VISIBLE_METHOD_SIGNATURES` and is verified against the supported Kotlin
//! 2.4.0, 2.4.10 and 2.4.20 toolchains.

use crate::libraries::LibraryMember;
use crate::types::{type_name, TypeName};

const VISIBLE_METHODS_2_4: &str =
    include_str!("mapped_builtin_member_status/visible_methods_2_4.tsv");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MappedBuiltinMemberStatus {
    Visible,
    Hidden,
}

impl MappedBuiltinMemberStatus {
    pub(super) fn is_visible(self) -> bool {
        self == Self::Visible
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct VisibleMethod {
    kotlin_face: TypeName,
    jvm_owner: TypeName,
    physical_name: &'static str,
    descriptor: &'static str,
}

fn visible_methods() -> &'static [VisibleMethod] {
    static METHODS: std::sync::OnceLock<Box<[VisibleMethod]>> = std::sync::OnceLock::new();
    METHODS.get_or_init(|| {
        VISIBLE_METHODS_2_4
            .lines()
            .enumerate()
            .filter_map(|(index, line)| {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    return None;
                }
                let mut fields = line.split('\t');
                let (
                    Some(kotlin_face),
                    Some(jvm_owner),
                    Some(physical_name),
                    Some(descriptor),
                    None,
                ) = (
                    fields.next(),
                    fields.next(),
                    fields.next(),
                    fields.next(),
                    fields.next(),
                )
                else {
                    panic!("invalid mapped-builtin member policy row {}", index + 1);
                };
                Some(VisibleMethod {
                    kotlin_face: type_name(kotlin_face),
                    jvm_owner: type_name(jvm_owner),
                    physical_name,
                    descriptor,
                })
            })
            .collect::<Vec<_>>()
            .into_boxed_slice()
    })
}

/// Classify a classfile member for one mapped Kotlin declaration.
///
/// The caller supplies a member actually decoded from `jvm_owner`; the policy only decides whether
/// that exact physical signature attaches to `kotlin_face`. A visible row cannot create a member
/// absent from the active JDK, and a same-named overload cannot inherit another overload's status.
pub(super) fn mapped_builtin_member_status(
    kotlin_face: TypeName,
    jvm_owner: TypeName,
    member: &LibraryMember,
) -> MappedBuiltinMemberStatus {
    let physical_name = member
        .physical_name
        .as_deref()
        .unwrap_or(member.name.as_str());
    if visible_methods().iter().any(|visible| {
        visible.kotlin_face == kotlin_face
            && visible.jvm_owner == jvm_owner
            && visible.physical_name == physical_name
            && visible.descriptor == member.descriptor
    }) {
        MappedBuiltinMemberStatus::Visible
    } else {
        MappedBuiltinMemberStatus::Hidden
    }
}

#[cfg(test)]
mod tests {
    use super::{mapped_builtin_member_status, visible_methods, MappedBuiltinMemberStatus};
    use crate::libraries::LibraryMember;
    use crate::types::{type_name, Ty};

    fn member(name: &str, descriptor: &str) -> LibraryMember {
        LibraryMember::new(name.to_owned(), Vec::new(), Ty::Unit, descriptor.to_owned())
    }

    fn status(face: &str, owner: &str, name: &str, descriptor: &str) -> MappedBuiltinMemberStatus {
        mapped_builtin_member_status(type_name(face), type_name(owner), &member(name, descriptor))
    }

    #[test]
    fn versioned_visible_method_policy_is_exact_and_owner_qualified() {
        let keys = visible_methods()
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(visible_methods().len(), 22);
        assert_eq!(keys.len(), visible_methods().len());

        assert_eq!(
            status(
                "kotlin/collections/Collection",
                "java/util/Collection",
                "stream",
                "()Ljava/util/stream/Stream;",
            ),
            MappedBuiltinMemberStatus::Visible
        );
        assert_eq!(
            status(
                "kotlin/collections/MutableCollection",
                "java/util/Collection",
                "stream",
                "()Ljava/util/stream/Stream;",
            ),
            MappedBuiltinMemberStatus::Hidden
        );
        assert_eq!(
            status(
                "kotlin/collections/MutableList",
                "java/util/List",
                "addFirst",
                "(Ljava/lang/Object;)V",
            ),
            MappedBuiltinMemberStatus::Visible
        );
        assert_eq!(
            status(
                "kotlin/collections/List",
                "java/util/List",
                "addFirst",
                "(Ljava/lang/Object;)V",
            ),
            MappedBuiltinMemberStatus::Hidden
        );
        assert_eq!(
            status(
                "kotlin/collections/MutableList",
                "java/util/Collection",
                "addFirst",
                "(Ljava/lang/Object;)V",
            ),
            MappedBuiltinMemberStatus::Hidden
        );
    }

    #[test]
    fn map_for_each_attaches_to_the_read_only_declaration_by_exact_physical_signature() {
        let descriptor = "(Ljava/util/function/BiConsumer;)V";
        assert_eq!(
            status(
                "kotlin/collections/Map",
                "java/util/Map",
                "forEach",
                descriptor,
            ),
            MappedBuiltinMemberStatus::Visible
        );
        assert_eq!(
            status(
                "kotlin/collections/MutableMap",
                "java/util/Map",
                "forEach",
                descriptor,
            ),
            MappedBuiltinMemberStatus::Hidden,
            "MutableMap inherits the admitted declaration from Map instead of duplicating it"
        );
        assert_eq!(
            status(
                "kotlin/collections/Map",
                "java/util/Map",
                "forEach",
                "(Ljava/util/function/Consumer;)V",
            ),
            MappedBuiltinMemberStatus::Hidden
        );
    }

    #[test]
    fn mutable_map_defaults_attach_only_to_the_mutable_declaration() {
        let descriptor = "(Ljava/lang/Object;Ljava/util/function/Function;)Ljava/lang/Object;";
        assert_eq!(
            status(
                "kotlin/collections/MutableMap",
                "java/util/Map",
                "computeIfAbsent",
                descriptor,
            ),
            MappedBuiltinMemberStatus::Visible
        );
        assert_eq!(
            status(
                "kotlin/collections/Map",
                "java/util/Map",
                "computeIfAbsent",
                descriptor,
            ),
            MappedBuiltinMemberStatus::Hidden
        );
    }

    #[test]
    fn physical_name_controls_status_for_a_provider_alias() {
        let mut alias = member("sourceAlias", "(Ljava/util/function/BiConsumer;)V");
        alias.physical_name = Some("forEach".to_owned());
        assert_eq!(
            mapped_builtin_member_status(
                type_name("kotlin/collections/Map"),
                type_name("java/util/Map"),
                &alias,
            ),
            MappedBuiltinMemberStatus::Visible
        );
    }
}
