//! A Kotlin builtin property that a JVM class declares through its zero-argument realizing method.
//!
//! `java.util.Collection.size()` realizes `Collection.size`, and `java.util.HashMap.keySet()`
//! overrides `Map.keys`, whose JVM realization is named `keySet`. kotlinc's Java scope shows each
//! such method as the builtin property it overrides, read through the property's getter
//! (`<get-keys>`), so the provider normalizes it into a [`PropertyInfo`] on the declaring class.

use super::*;

impl JvmLibraries {
    /// The builtin property `name` as `recv`'s class declares it through a realizing member.
    ///
    /// A builtin declaration is published under its source name and carries its JVM name
    /// (`Map.keys` realized by `keySet`), so it is among `functions`, the members named `name`. A
    /// Java method overriding it is published under its own JVM name (`HashMap.keySet()`), so for a
    /// property realized under another JVM name the members of that name are candidates too. Either
    /// way the realization is the zero-argument member with the mapping's JVM name and descriptor.
    pub(super) fn builtin_property_realization(
        &self,
        recv: Ty,
        name: &str,
        functions: &FunctionSet,
        mapped_property: Option<&MappedBuiltinMember>,
        function_renames: &[MappedBuiltinMember],
    ) -> Option<PropertyInfo> {
        let owner = recv.kotlin_class_internal()?;
        let renamed = mapped_property
            .filter(|mapping| mapping.physical_name != name)
            .map(|mapping| {
                self.member_functions_with_renames(recv, &mapping.physical_name, function_renames)
            });
        let function = functions
            .overloads
            .iter()
            .chain(renamed.iter().flat_map(|renamed| &renamed.overloads))
            .find(|function| {
                function.callable.params.is_empty()
                    && mapped_property.is_none_or(|mapping| {
                        function.callable.physical_name() == mapping.physical_name
                            && function.callable.descriptor == mapping.descriptor
                    })
            })?;
        let mut getter = function.callable.clone();
        // `FunctionInfo` keeps Kotlin declaration modality separately from its physical callable
        // handle. A builtin property synthesized from the corresponding zero-arg member must restore
        // that semantic fact: mapped `java.util.Collection.size()` is a usable JVM method handle,
        // while Kotlin `Collection.size` remains abstract and therefore cannot compete with a
        // concrete interface default in `super.size`.
        getter.is_abstract = function.flags.is_abstract;
        let ty = function
            .generic_sig
            .as_ref()
            .map(|signature| signature.ret)
            .unwrap_or_else(|| function.ret.apply(getter.ret));
        let ty = match mapped_property.and_then(|mapping| self.overridden_property_type(mapping)) {
            Some(overridden) => enhance_type_arguments(ty, overridden),
            None => ty,
        };
        getter.ret = ty;
        Some(PropertyInfo {
            return_value_status: function.flags.return_value_status,
            name: name.to_string(),
            kind: PropKind::Member,
            receiver: Some(recv),
            associated_classifier: None,
            associated_access_owner: None,
            formals: Vec::new(),
            ty,
            context_count: 0,
            context_param_names: Vec::new(),
            context_parameter_identities: Vec::new(),
            getter,
            setter: None,
            setter_visibility: function.visibility,
            setter_parameter_name: None,
            is_const: false,
            implicit_integer_coercion: false,
            compile_time_constant: None,
            metadata_constant_read: false,
            visibility: function.visibility,
            owner,
            receiver_rank: 0,
            source_key: None,
            stable_declaration: None,
            getter_declaration: None,
            setter_declaration: None,
            source_member: None,
            producer: PropertyProducer::KotlinAccessor,
            read_stability: crate::libraries::PropertyReadStability::Unstable,
        })
    }

    /// The declared type of the builtin property `mapping` realizes, as `.kotlin_builtins` states
    /// it (`MutableMap.entries: MutableSet<MutableMap.MutableEntry<K, V>>`).
    fn overridden_property_type(&self, mapping: &MappedBuiltinMember) -> Option<Ty> {
        self.cp
            .builtin_members_name(mapping.declaration_owner)
            .into_iter()
            .find(|member| {
                member.name == mapping.source_name
                    && member.params.is_empty()
                    && member.descriptor == mapping.descriptor
            })?
            .generic_sig
            .map(|signature| signature.ret)
    }
}

/// kotlinc enhances an overriding Java result's type ARGUMENTS from the overridden declaration
/// (`AbstractSignatureParts.computeIndexedQualifiers`): an argument the builtin property fixes as a
/// not-null class type (`MutableMap.MutableEntry<K, V>` in `MutableSet<…>`) makes the Java
/// argument (`Map.Entry<K, V>!`) rigid, and a type-parameter argument carries no qualifier and
/// stays flexible. The head keeps the Java result's flexibility: an enhanced property read is not
/// modeled, and the flexible head is guarded wherever kotlinc guards the enhanced one.
fn enhance_type_arguments(java: Ty, overridden: Ty) -> Ty {
    match java {
        Ty::PlatformNullable(inner) => {
            Ty::platform_nullable(enhance_type_arguments(*inner, overridden))
        }
        Ty::Obj(owner, arguments) => {
            let fixed = overridden.non_null().type_args();
            if fixed.len() != arguments.len() {
                return java;
            }
            let enhanced = arguments
                .iter()
                .zip(fixed)
                .map(|(&argument, &fixed)| match (argument, fixed) {
                    (Ty::PlatformNullable(inner), Ty::Obj(..)) => {
                        enhance_type_arguments(*inner, fixed)
                    }
                    (Ty::Obj(..) | Ty::PlatformNullable(_), _) => {
                        enhance_type_arguments(argument, fixed)
                    }
                    _ => argument,
                })
                .collect::<Vec<_>>();
            Ty::obj_args_name(owner, &enhanced)
        }
        _ => java,
    }
}

#[cfg(test)]
mod tests {
    use super::enhance_type_arguments;
    use crate::types::Ty;

    #[test]
    fn a_class_argument_the_builtin_fixes_not_null_becomes_rigid() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let entry = |key: Ty, value: Ty| {
            Ty::obj_args("kotlin/collections/MutableMap.MutableEntry", &[key, value])
        };
        let set = |element: Ty| Ty::obj_args("kotlin/collections/MutableSet", &[element]);
        let flexible = Ty::platform_nullable(Ty::String);
        let java = Ty::platform_nullable(set(Ty::platform_nullable(entry(flexible, flexible))));
        let overridden = set(entry(Ty::ty_param("K", any), Ty::ty_param("V", any)));
        assert_eq!(
            enhance_type_arguments(java, overridden),
            Ty::platform_nullable(set(entry(flexible, flexible))),
            "the entry is rigid, its type-parameter arguments and the head stay flexible"
        );
    }
}
