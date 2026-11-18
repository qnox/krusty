//! The `toString`, `hashCode` and `equals` a data class or a final value class generates.
//!
//! Kotlin's generated object overrides are ordinary final declarations with a written member's
//! stable identity and callable shape; common IR owns their bodies. A member the class writes
//! itself is not generated. Under `StrictEquals`, from the release with strict equality for
//! structural classes, kotlinc binds the generated `equals` to its own class and records that
//! bound in the class metadata.

use super::*;

/// The class whose `Any` overrides are generated.
pub(super) struct GeneratedAnyOverrides<'a> {
    pub(super) owner: TypeName,
    /// Whether the generated `equals` is bound to [`Self::owner`].
    pub(super) equality_bound: bool,
    pub(super) compact_headers: Option<&'a crate::fir::StreamedHeaderModule>,
    pub(super) compact_owner: Option<crate::fir::DeclarationId>,
}

/// Add each generated override the class does not declare to its methods, in declaration order.
pub(super) fn publish_generated_any_overrides(
    methods: &mut MethodMap,
    declared_callable_order: &mut Vec<String>,
    class: &GeneratedAnyOverrides<'_>,
) {
    for (name, params, ret) in [
        ("toString", Vec::new(), Ty::String),
        ("hashCode", Vec::new(), Ty::Int),
        (
            "equals",
            vec![Ty::nullable(Ty::obj("kotlin/Any"))],
            Ty::Boolean,
        ),
    ] {
        let already_declared = methods.get(name).is_some_and(|overloads| {
            overloads
                .iter()
                .any(|signature| signature.is_override() && signature.params.len() == params.len())
        });
        if already_declared {
            continue;
        }
        if !declared_callable_order
            .iter()
            .any(|declared| declared == name)
        {
            declared_callable_order.push(name.to_string());
        }
        let parameter_count = params.len();
        methods.entry(name.into()).or_default().push(Signature {
            params,
            ret,
            generic_sig: None,
            projected_return_hazard: false,
            flags: SigFlags::default()
                .with_is_operator(name == "equals")
                .with_is_override(true)
                .with_is_final(true),
            annotations: Vec::new(),
            equality_bound: (name == "equals" && class.equality_bound)
                .then(|| Ty::obj_name(class.owner)),
            vararg_index: None,
            required: parameter_count,
            param_defaults: vec![false; parameter_count].into(),
            exact_params: vec![false; parameter_count].into(),
            no_infer_params: vec![false; parameter_count].into(),
            implicit_integer_coercion: vec![false; parameter_count].into(),
            param_default_values: vec![None; parameter_count],
            param_names: if name == "equals" {
                vec!["other".into()]
            } else {
                Vec::new()
            },
            parameter_identities: Vec::new(),
            lambda_param_types: vec![Vec::new(); parameter_count],
            lambda_recv: vec![false; parameter_count].into(),
            inline_modifiers: Vec::new(),
            visibility: Visibility::Public,
            context_count: 0,
            source_decl: None,
            stable_declaration: compact_generated_member_declaration(
                class.compact_headers,
                class.compact_owner,
                name,
            ),
            source_file: None,
            source_member: None,
            source_receiver: None,
            package: TypeName::ROOT,
            contract: None,
            plugin_expression: None,
        });
    }
}
