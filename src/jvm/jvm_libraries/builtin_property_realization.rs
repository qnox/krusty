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
}
