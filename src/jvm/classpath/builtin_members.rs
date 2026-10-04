//! Kotlin builtin members (`String.length`, `List.get`, `Number.toInt`, …) as ordinary library
//! members: the `.kotlin_builtins` declaration joined with its JVM realization.

use super::*;
use super::{builtin_declared_return, builtin_descriptor, builtin_erased};

/// Provider-normalized projection of one package callable decoded from a builtins resource.
#[derive(Clone)]
pub(in crate::jvm) struct BuiltinPackageFunction {
    pub(in crate::jvm) generic_sig: GenericSig,
    pub(in crate::jvm) only_input_type_formals: Vec<String>,
    pub(in crate::jvm) params: Vec<Ty>,
    pub(in crate::jvm) ret: Ty,
    pub(in crate::jvm) param_names: Vec<String>,
    pub(in crate::jvm) param_defaults: Vec<bool>,
    pub(in crate::jvm) vararg: Option<usize>,
    pub(in crate::jvm) visibility: crate::types::Visibility,
    pub(in crate::jvm) is_inline: bool,
    pub(in crate::jvm) has_reified_type_params: bool,
    pub(in crate::jvm) reified_type_parameter_ordinals: Vec<u32>,
    pub(in crate::jvm) is_suspend: bool,
    pub(in crate::jvm) is_operator: bool,
    pub(in crate::jvm) is_infix: bool,
    pub(in crate::jvm) context_count: usize,
    pub(in crate::jvm) annotations: Vec<TypeName>,
}

impl Classpath {
    /// Kotlin BUILTIN members (`String.length`, `List.get`, `Number.toInt`, …) as regular
    /// `LibraryMember` facts. The source name stays in `name`; JVM realization details stay in the JVM
    /// backend/provider and descriptor data.
    pub fn builtin_members(&self, internal: &str) -> Vec<crate::libraries::LibraryMember> {
        self.builtin_members_name(type_name(internal))
    }

    pub fn builtin_members_name(
        &self,
        internal_id: TypeName,
    ) -> Vec<crate::libraries::LibraryMember> {
        let catalog_complete = self.catalog_complete();
        if catalog_complete {
            if let Some(members) = self.builtin_members.borrow_mut().get(&internal_id) {
                cache_stat!(builtin_members, true);
                return members.as_ref().clone();
            }
        }
        cache_stat!(builtin_members, false);
        let f = self.builtins_file_for_package(Self::builtins_package_for(internal_id));
        let members: Vec<_> = f
            .get_name(internal_id)
            .map(|class| {
                class.members.iter().map(|m| {
                    // A `LibraryMember` states the member in its ERASED, JVM-descriptor shape (the form
                    // a classpath member arrives in, and the form overload alignment compares against);
                    // the declared shape rides along in `generic_sig`. Both are the one decoded builtin
                    // signature, erased here.
                    // Kotlin array classes are semantic builtin classifiers with no loadable JVM
                    // class or callable methods. Preserve their selected declaration without an
                    // opaque method descriptor; the JVM emitter realizes the exact `actual`
                    // signature with array bytecodes (or a metadata-verified helper).
                    let descriptor = if Ty::obj_name(internal_id).is_array() {
                        String::new()
                    } else {
                        builtin_descriptor(&m.generic_sig)
                    };
                    let params: Vec<Ty> = m
                        .generic_sig
                        .params
                        .iter()
                        .map(|p| builtin_erased(*p))
                        .collect();
                    let ret = builtin_erased(m.generic_sig.ret);
                    crate::trace_compiler!(
                        "resolve",
                        "builtin member {}.{} declared_ret={:?} erased_ret={ret:?}",
                        internal_id,
                        m.name,
                        m.generic_sig.ret,
                    );
                    let physical_ret = ret;
                    // The owner's JVM class: the kotlin↔JVM map (`kotlin/String` → `java/lang/String`), and for the
                    // non-collection mapped builtins (`kotlin/CharSequence` → `java/lang/CharSequence`, …) the
                    // emit-only simple-name mapping — the member virtual-dispatches on that JVM type.
                    let owner = crate::jvm::jvm_class_map::to_jvm_type_name(internal_id);
                    // Interface dispatch: prefer the real class flag, else the builtin's OWN
                    // `.kotlin_builtins` `CLASS_KIND` — a Kotlin builtin and the JVM class it maps to
                    // always agree on interface-ness (`List`/`java.util.List`, `Number`/`java.lang
                    // .Number`), and every member here comes from a builtins entry that carries the flag
                    // — so no curated per-name table is needed (the old fallback covered a handful of
                    // names and answered `false` for every `java/util/*`, emitting `invokevirtual` on an
                    // interface).
                    let is_iface = self
                        .find_name(owner)
                        .map(|ci| ci.is_interface())
                        .unwrap_or(class.kind == crate::libraries::TypeKind::Interface);
                    let kind = if m.is_property {
                        crate::jvm::mapped_builtin_declarations::MappedBuiltinMemberKind::Property
                    } else {
                        crate::jvm::mapped_builtin_declarations::MappedBuiltinMemberKind::Function
                    };
                    let physical_name = self
                        .mapped_builtin_realization(internal_id, &m.name, &descriptor, kind)
                        .map(|(_, name)| name.to_string())
                        .unwrap_or_else(|| {
                            if m.is_property {
                                ordinary_builtin_property_jvm_name(&m.name)
                            } else {
                                m.name.clone()
                            }
                        });
                    let physical_name = (physical_name != m.name).then_some(physical_name);
                    let realization = crate::libraries::builtin_member_realization::realization(
                        crate::libraries::builtin_declaration::BuiltinMemberDeclaration {
                            owner: internal_id,
                            name: &m.name,
                            params: &m.generic_sig.params,
                            ret: m.generic_sig.ret,
                            is_property: m.is_property,
                            is_operator: m.is_operator,
                            is_infix: m.is_infix,
                            annotations: &m.annotations,
                        },
                    );
                    let semantic_role = crate::libraries::builtin_declaration::semantic_call_role(
                        crate::libraries::builtin_declaration::BuiltinMemberDeclaration {
                            owner: internal_id,
                            name: &m.name,
                            params: &m.generic_sig.params,
                            ret: m.generic_sig.ret,
                            is_property: m.is_property,
                            is_operator: m.is_operator,
                            is_infix: m.is_infix,
                            annotations: &m.annotations,
                        },
                    );
                    crate::libraries::LibraryMember {
                        external_identity: None,
                        external_default_provider: None,
                        external_property_identity: None,
                        semantic_role,
                        singleton_dispatch: None,
                        name: m.name.clone(),
                        owner: Some(owner),
                        physical_name,
                        physical_params: params.clone(),
                        physical_parameter_plan: Some(
                            crate::libraries::physical_parameter_plan::source_parameter_plan(
                                params.len(),
                            ),
                        ),
                        params,
                        ret,
                        physical_ret,
                        descriptor,
                        realization,
                        signature: None,
                        // A builtin member has no JVM `Signature`: its DECODED signature alone keeps a
                        // generic member from resolving with an `Any`-erased return.
                        generic_sig: Some(m.generic_sig.clone()),
                        projected_return_hazard: false,
                        // `ret_nullable` — the declared return nullability from the `.kotlin_builtins`
                        // `Type.nullable` flag (`Map.get(K): V?`); the JVM descriptor erases it.
                        flags: crate::libraries::LmFlags::default()
                            .with_ret_nullable(m.ret_nullable)
                            .with_is_interface(is_iface)
                            .with_is_operator(m.is_operator)
                            .with_is_infix(m.is_infix)
                            .with_is_abstract(m.is_abstract)
                            .with_inherited_by_delegation(
                                crate::jvm::jvm_libraries::inherited_by_delegation(
                                    m.is_abstract,
                                    true,
                                    &m.annotations,
                                ),
                            ),
                        inline: crate::libraries::InlineKind::None,
                        reified: false,
                        inline_body_plan: None,
                        // Builtin (`.kotlin_builtins`) members are all public API.
                        visibility: crate::libraries::Visibility::Public,
                        // A builtin member's parameters are named in `.kotlin_builtins`
                        // (`MutableList.add(element)`); a bridge over one takes those names.
                        call_sig: crate::libraries::CallSig::metadata_member(
                            m.generic_sig.params.len(),
                            m.param_names.clone(),
                            Vec::new(),
                            None,
                        ),
                        context_count: 0,
                        annotations: m.annotations.clone(),
                        contract: None,
                        equality_bound: None,
                        return_value_status: Some(m.return_value_status),
                        default_values: Vec::new(),
                        default_realization: None,
                        nonvirtual_realization: None,
                        declared_ret: builtin_declared_return(m.ret_nullable, m.generic_sig.ret),
                        overridden_results: Box::new([]),
                        implicit_classifier_callable: None,
                        associated_classifier: None,
                        associated_access_owner: None,
                        plugin_expression: None,
                        stable_declaration: None,
                        source_member: None,
                    }
                })
            })
            .into_iter()
            .flatten()
            .collect();
        if catalog_complete {
            self.builtin_members
                .borrow_mut()
                .insert(internal_id, std::rc::Rc::new(members.clone()));
        }
        members
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_properties_consume_their_exact_mapped_realizations() {
        let Some(jar) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let members = Classpath::new(vec![jar]).builtin_members("kotlin/Enum");
        let property = |name: &str| {
            let selected = members
                .iter()
                .filter(|member| member.name == name)
                .collect::<Vec<_>>();
            assert_eq!(selected.len(), 1, "exactly one kotlin.Enum.{name}");
            let member = selected[0];
            (
                member.owner,
                member.physical_name.as_deref(),
                member.descriptor.as_str(),
                member.params.as_slice(),
                member.ret,
            )
        };

        assert_eq!(
            property("name"),
            (
                Some(type_name("java/lang/Enum")),
                None,
                "()Ljava/lang/String;",
                &[] as &[Ty],
                Ty::String,
            )
        );
        assert_eq!(
            property("ordinal"),
            (
                Some(type_name("java/lang/Enum")),
                None,
                "()I",
                &[] as &[Ty],
                Ty::Int,
            )
        );
    }

    #[test]
    fn enum_name_builtin_keeps_its_qualified_intrinsic_annotation() {
        let Some(jar) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let members = Classpath::new(vec![jar]).builtin_members("kotlin/Enum");
        let member = members
            .iter()
            .find(|member| {
                member.name == "name"
                    && member.realization == crate::libraries::MemberRealization::Dispatch
            })
            .expect("kotlin.Enum.name declaration");
        assert_eq!(
            member.annotations,
            vec![crate::types::wk::intrinsic_const_evaluation()]
        );
    }
}
