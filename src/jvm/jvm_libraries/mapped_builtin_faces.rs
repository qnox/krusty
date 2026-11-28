//! The mapped builtin members a JVM class's hierarchy realizes: the Kotlin declarations that its
//! mapped JVM supertypes (`java.util.List`, `java.lang.CharSequence`, `java.lang.Number`) stand
//! for, with the JVM names they keep.

use super::JvmLibraries;
use crate::jvm::mapped_builtin_declarations::MappedBuiltinMember;
use crate::types::TypeName;

impl JvmLibraries {
    /// Metadata declarations realized by the mapped collection interfaces in a concrete JVM class's
    /// hierarchy. The provider walks the physical hierarchy only to discover applied interfaces; the
    /// member set and Kotlin inheritance come from `.kotlin_builtins`.
    pub(super) fn mapped_collection_members(&self, internal: TypeName) -> Vec<MappedBuiltinMember> {
        let mut mappings = Vec::new();
        let mut seen = crate::fx_hash::HashSet::default();
        let root = crate::jvm::jvm_class_map::to_jvm_type_name(internal);
        let mut pending = std::collections::VecDeque::new();
        pending.push_back(root);
        while let Some(owner) = pending.pop_front() {
            if !seen.insert(owner) {
                continue;
            }
            // A collection face renames through its mutable declaration (`MutableList.removeAt`);
            // every other mapped JVM owner through the builtin it realizes (`CharSequence.get`
            // as `charAt`, `Number.toInt` as `intValue`) for the Java classes below it. The
            // mapped class's own members are not renamed: `java.lang.Number` named as a Java
            // class keeps `intValue`.
            if let Some(kotlin) =
                crate::jvm::jvm_class_map::jvm_collection_to_kotlin_mutable_type_name(owner)
                    .or_else(|| {
                        (owner != root)
                            .then(|| {
                                crate::jvm::jvm_class_map::jvm_to_kotlin_builtin_metadata_name(
                                    owner,
                                )
                            })
                            .flatten()
                    })
            {
                for mapping in self.cp.mapped_builtin_members_name(kotlin) {
                    if mapping.physical_owner != owner {
                        continue;
                    }
                    if !mappings.contains(&mapping) {
                        mappings.push(mapping);
                    }
                }
            }
            if let Some(class) = self.cp.find_name(owner) {
                pending.extend(class.interfaces.iter_ids().chain(class.super_class));
            }
        }
        mappings
    }

    /// Kotlin source renames for function members only. Property mappings are consumed separately
    /// by `declared_callables_for`, where they become `PropertyInfo` declarations backed by the
    /// concrete class's physical zero-argument method.
    pub(super) fn mapped_collection_function_renames(
        &self,
        internal: TypeName,
    ) -> Vec<MappedBuiltinMember> {
        self.mapped_collection_members(internal)
            .into_iter()
            .filter(|mapping| {
                !mapping.is_property() && mapping.source_name != mapping.physical_name
            })
            .collect()
    }
}
