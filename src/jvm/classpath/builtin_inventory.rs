//! Semantic roles assigned while decoded `.kotlin_builtins` declarations enter the classpath
//! inventory. Only this trusted catalog path calls these operations; classfile/KLIB declarations
//! with the same shape never acquire a catalog role.

use super::*;

pub(super) fn function_role(
    package: TypeName,
    function: &super::super::metadata::BuiltinFunction,
    signature: &GenericSig,
) -> Option<crate::libraries::CompilerIntrinsic> {
    let [argument] = signature.params.as_slice() else {
        return None;
    };
    let receiver = signature.receiver?;
    (package == crate::types::wk::kotlin_ranges_package()
        && function.name == "rangeTo"
        && function.is_operator
        && !function.is_suspend
        && !function.is_infix
        && function.context_count == 0
        && signature.formals.is_empty()
        && function.vararg.is_none()
        && receiver == *argument
        && matches!(receiver, Ty::Double | Ty::Float)
        && signature.ret == Ty::obj_args("kotlin/ranges/ClosedFloatingPointRange", &[receiver]))
    .then_some(crate::libraries::CompilerIntrinsic::FloatingRangeMembership)
}

impl Classpath {
    pub(in crate::jvm) fn builtin_package_functions(
        &self,
        package: TypeName,
        name: &str,
    ) -> Vec<BuiltinPackageFunction> {
        self.builtins_file_for_package(package)
            .functions
            .iter()
            .filter(|function| function.name == name)
            .map(|function| BuiltinPackageFunction {
                generic_sig: function.generic_sig.clone(),
                compiler_intrinsic: function.compiler_intrinsic,
                only_input_type_formals: function.only_input_type_formals.clone(),
                params: function
                    .generic_sig
                    .receiver
                    .iter()
                    .chain(&function.generic_sig.params)
                    .copied()
                    .map(builtin_erased)
                    .collect(),
                ret: builtin_erased(function.generic_sig.ret),
                param_names: function.param_names.clone(),
                param_defaults: function.param_defaults.clone(),
                vararg: function.vararg,
                visibility: function.visibility,
                is_inline: function.is_inline,
                has_reified_type_params: function.has_reified_type_params,
                is_suspend: function.is_suspend,
                is_operator: function.is_operator,
                is_infix: function.is_infix,
                context_count: function.context_count,
                annotations: function.annotations.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn floating_range_builtin() -> super::super::super::metadata::BuiltinFunction {
        let element = super::super::super::metadata::BuiltinTy::class("kotlin/Double");
        super::super::super::metadata::BuiltinFunction {
            name: "rangeTo".to_string(),
            receiver: Some(element.clone()),
            params: vec![element.clone()],
            ret: super::super::super::metadata::BuiltinTy::Class {
                internal: "kotlin/ranges/ClosedFloatingPointRange".to_string(),
                args: vec![element],
                nullable: false,
                shape: Default::default(),
            },
            formals: Vec::new(),
            param_names: vec!["other".to_string()],
            param_defaults: vec![false],
            vararg: None,
            visibility: crate::types::Visibility::Public,
            is_inline: false,
            has_reified_type_params: false,
            is_suspend: false,
            is_operator: true,
            is_infix: false,
            context_count: 0,
            annotations: Vec::new(),
        }
    }

    #[test]
    fn floating_range_role_is_owned_by_the_exact_builtins_catalog() {
        let function = floating_range_builtin();
        let bounds = builtin_bounds(&function.formals, &HashMap::new());
        let signature = GenericSig {
            formals: Vec::new(),
            formal_bounds: Vec::new(),
            receiver: function
                .receiver
                .as_ref()
                .map(|receiver| builtin_ty(receiver, &bounds)),
            params: function
                .params
                .iter()
                .map(|parameter| builtin_ty(parameter, &bounds))
                .collect(),
            ret: builtin_ty(&function.ret, &bounds),
            return_policy: Default::default(),
        };
        assert_eq!(
            function_role(
                crate::types::wk::kotlin_ranges_package(),
                &function,
                &signature,
            ),
            Some(crate::libraries::CompilerIntrinsic::FloatingRangeMembership)
        );
        assert_eq!(
            function_role(type_name("sample/ranges"), &function, &signature),
            None
        );
    }
}
