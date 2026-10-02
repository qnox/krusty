//! Projection of decoded `.kotlin_builtins` package functions into normalized provider records.

use super::{builtin_erased, BuiltinPackageFunction};
use crate::types::TypeName;

impl super::Classpath {
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
                reified_type_parameter_ordinals: function.reified_type_parameter_ordinals.clone(),
                is_suspend: function.is_suspend,
                is_operator: function.is_operator,
                is_infix: function.is_infix,
                context_count: function.context_count,
                annotations: function.annotations.clone(),
            })
            .collect()
    }
}
