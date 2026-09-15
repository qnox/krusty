//! Federation of decoded builtin member declarations with their special JVM realizations.
//!
//! The classpath owns the decoded `.kotlin_builtins` graph. This module walks that graph while the
//! declarations are still provider-local and attaches only the target policy selected by an exact
//! declaration identity, kind, and erased descriptor.

use super::super::mapped_builtin_declarations::{
    realization_for_declaration, MappedBuiltinMember, MappedBuiltinMemberKind,
};
use super::{builtin_descriptor, Classpath};
use crate::types::TypeName;

impl Classpath {
    /// How reading a builtin property is realized on its mapped JVM owner when that owner has no
    /// class file to inspect (no JDK on the classpath). Declaration shape and inheritance come from
    /// `.kotlin_builtins`; only the exact accessor handle comes from target policy.
    pub(super) fn builtin_property_read_access(
        &self,
        owner: &str,
        property: &str,
    ) -> Option<super::super::inline::PropertyAccess> {
        let jvm_owner_id =
            super::super::jvm_class_map::to_jvm_type_name(crate::types::type_name(owner));
        let jvm_owner = jvm_owner_id.render();
        let kotlin = super::super::jvm_class_map::jvm_to_kotlin_builtin_metadata_name(jvm_owner_id)
            .unwrap_or(jvm_owner_id);
        let mut pending = std::collections::VecDeque::from([kotlin]);
        let mut seen = std::collections::HashSet::new();
        while let Some(current) = pending.pop_front() {
            if !seen.insert(current) {
                continue;
            }
            let file = self.builtins_file_for_package(Self::builtins_package_for(current));
            let Some(class) = file.get_name(current) else {
                continue;
            };
            if let Some(member) = class
                .members
                .iter()
                .find(|member| member.is_property && member.name == property)
            {
                let descriptor = builtin_descriptor(&member.generic_sig);
                let name = self
                    .mapped_builtin_realization(
                        current,
                        &member.name,
                        &descriptor,
                        MappedBuiltinMemberKind::Property,
                    )
                    .map(|(_, name)| name.to_string())
                    .unwrap_or_else(|| {
                        super::ordinary_builtin_property_jvm_name(current, &member.name)
                    });
                return Some(super::super::inline::PropertyAccess::Accessor {
                    // Dispatch stays on the mapped form of the resolved receiver owner. The decoded
                    // declaration supplies only the accessor and its own erased descriptor.
                    owner: jvm_owner.clone(),
                    name,
                    descriptor,
                    is_static: false,
                    is_interface: class.kind == crate::libraries::TypeKind::Interface,
                });
            }
            pending.extend(class.supertypes.iter_ids());
        }
        None
    }

    /// Resolve the physical special-member policy inherited by one exact builtin declaration. The
    /// member itself and every override remain metadata-owned; only the JVM realization is inherited
    /// from the nearest decoded supertype declaration carrying a policy row.
    pub(super) fn mapped_builtin_realization(
        &self,
        declaration_owner: TypeName,
        source_name: &str,
        descriptor: &str,
        kind: MappedBuiltinMemberKind,
    ) -> Option<(TypeName, &'static str)> {
        let mut pending = std::collections::VecDeque::from([declaration_owner]);
        let mut seen = std::collections::HashSet::new();
        while let Some(current) = pending.pop_front() {
            if !seen.insert(current) {
                continue;
            }
            let file = self.builtins_file_for_package(Self::builtins_package_for(current));
            let Some(class) = file.get_name(current) else {
                continue;
            };
            let declares_member = class.members.iter().any(|member| {
                member.name == source_name
                    && member.is_property == (kind == MappedBuiltinMemberKind::Property)
                    && builtin_descriptor(&member.generic_sig) == descriptor
            });
            if declares_member {
                if let Some(realization) =
                    realization_for_declaration(current, source_name, descriptor, kind)
                {
                    return Some(realization);
                }
            }
            pending.extend(class.supertypes.iter_ids());
        }
        None
    }

    /// Exact source declarations whose JVM realization has a special name/property shape. The
    /// declaration and inheritance graph come only from `.kotlin_builtins`; the target policy merely
    /// pairs an encountered declaration with its physical JVM handle.
    pub(in crate::jvm) fn mapped_builtin_members_name(
        &self,
        internal: TypeName,
    ) -> Vec<MappedBuiltinMember> {
        let mut mappings = Vec::new();
        let mut pending = std::collections::VecDeque::from([internal]);
        let mut seen = std::collections::HashSet::new();
        let mut seen_shapes = std::collections::HashSet::new();
        while let Some(current) = pending.pop_front() {
            if !seen.insert(current) {
                continue;
            }
            let file = self.builtins_file_for_package(Self::builtins_package_for(current));
            let Some(class) = file.get_name(current) else {
                continue;
            };
            for member in &class.members {
                let descriptor = builtin_descriptor(&member.generic_sig);
                let kind = if member.is_property {
                    MappedBuiltinMemberKind::Property
                } else {
                    MappedBuiltinMemberKind::Function
                };
                let Some((physical_owner, physical_name)) =
                    self.mapped_builtin_realization(current, &member.name, &descriptor, kind)
                else {
                    continue;
                };
                if !seen_shapes.insert((member.name.clone(), descriptor.clone(), kind)) {
                    continue;
                }
                mappings.push(MappedBuiltinMember {
                    declaration_owner: current,
                    source_name: member.name.clone(),
                    physical_owner,
                    physical_name: physical_name.to_string(),
                    descriptor,
                    kind,
                });
            }
            pending.extend(class.supertypes.iter_ids());
        }
        mappings
    }
}

#[cfg(test)]
mod tests {
    use super::super::Classpath;
    use crate::types::type_name;

    #[test]
    fn mapped_builtin_realizations_follow_metadata_declarations_and_supertypes() {
        let Some(jar) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let cp = Classpath::new(vec![jar]);
        let shape = |owner: &str| {
            cp.mapped_builtin_members_name(type_name(owner))
                .into_iter()
                .map(|member| {
                    let is_property = member.is_property();
                    (
                        member.declaration_owner,
                        member.source_name,
                        member.physical_owner,
                        member.physical_name,
                        member.descriptor,
                        is_property,
                    )
                })
                .collect::<Vec<_>>()
        };

        assert_eq!(
            shape("kotlin/collections/List"),
            vec![(
                type_name("kotlin/collections/List"),
                "size".to_string(),
                type_name("java/util/Collection"),
                "size".to_string(),
                "()I".to_string(),
                true,
            )],
            "List's decoded size override must inherit Collection's physical realization"
        );
        assert_eq!(
            shape("kotlin/collections/Set"),
            vec![(
                type_name("kotlin/collections/Set"),
                "size".to_string(),
                type_name("java/util/Collection"),
                "size".to_string(),
                "()I".to_string(),
                true,
            )]
        );
        assert_eq!(
            shape("kotlin/collections/Map"),
            vec![
                (
                    type_name("kotlin/collections/Map"),
                    "size".to_string(),
                    type_name("java/util/Map"),
                    "size".to_string(),
                    "()I".to_string(),
                    true,
                ),
                (
                    type_name("kotlin/collections/Map"),
                    "keys".to_string(),
                    type_name("java/util/Map"),
                    "keySet".to_string(),
                    "()Ljava/util/Set;".to_string(),
                    true,
                ),
                (
                    type_name("kotlin/collections/Map"),
                    "values".to_string(),
                    type_name("java/util/Map"),
                    "values".to_string(),
                    "()Ljava/util/Collection;".to_string(),
                    true,
                ),
                (
                    type_name("kotlin/collections/Map"),
                    "entries".to_string(),
                    type_name("java/util/Map"),
                    "entrySet".to_string(),
                    "()Ljava/util/Set;".to_string(),
                    true,
                ),
            ]
        );
        assert_eq!(
            shape("kotlin/collections/MutableList")
                .into_iter()
                .map(|member| (member.0, member.1, member.3, member.4, member.5))
                .collect::<Vec<_>>(),
            vec![
                (
                    type_name("kotlin/collections/MutableList"),
                    "removeAt".to_string(),
                    "remove".to_string(),
                    "(I)Ljava/lang/Object;".to_string(),
                    false,
                ),
                (
                    type_name("kotlin/collections/List"),
                    "size".to_string(),
                    "size".to_string(),
                    "()I".to_string(),
                    true,
                ),
            ]
        );

        let mutable_map_keys = cp
            .builtin_members_name(type_name("kotlin/collections/MutableMap"))
            .into_iter()
            .filter(|member| member.name == "keys")
            .map(|member| {
                (
                    member.name,
                    member.physical_name,
                    member.ret,
                    member.descriptor,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            mutable_map_keys,
            vec![(
                "keys".to_string(),
                Some("keySet".to_string()),
                crate::types::Ty::obj("kotlin/collections/MutableSet"),
                "()Ljava/util/Set;".to_string(),
            )],
            "MutableMap keeps its decoded MutableSet return while inheriting Map.keys' JVM handle"
        );
    }
}
