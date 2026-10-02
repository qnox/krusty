//! Semantic roles assigned while a trusted catalog declaration enters the classpath inventory.
//! `.kotlin_builtins` records and the `kotlin.ranges.RangesKt` file facade are those catalogs.
//! A classfile or KLIB declaration with the same shape does not acquire a catalog role.

use std::collections::HashMap;

use crate::libraries::{CompilerIntrinsic, FunctionInfo, GenericSig};
use crate::types::{type_name, Ty, TypeName};

use super::{builtin_bounds, builtin_erased, builtin_ty, BuiltinPackageFunction};

fn floating_range_membership(
    operator: bool,
    suspend: bool,
    infix: bool,
    context_count: usize,
    vararg: bool,
    signature: &GenericSig,
) -> Option<CompilerIntrinsic> {
    if !operator
        || suspend
        || infix
        || context_count != 0
        || vararg
        || !signature.formals.is_empty()
    {
        return None;
    }
    let [argument] = signature.params.as_slice() else {
        return None;
    };
    let receiver = signature.receiver?;
    if receiver != *argument || !matches!(receiver, Ty::Double | Ty::Float) {
        return None;
    }
    let closed = Ty::obj_args("kotlin/ranges/ClosedFloatingPointRange", &[receiver]);
    let open = Ty::obj_args("kotlin/ranges/OpenEndRange", &[receiver]);
    (signature.ret == closed || signature.ret == open)
        .then_some(CompilerIntrinsic::FloatingRangeMembership)
}

pub(super) fn function_role(
    package: TypeName,
    function: &super::super::metadata::BuiltinFunction,
    signature: &GenericSig,
) -> Option<CompilerIntrinsic> {
    if package != crate::types::wk::kotlin_ranges_package() {
        return None;
    }
    floating_range_membership(
        function.is_operator,
        function.is_suspend,
        function.is_infix,
        function.context_count,
        function.vararg.is_some(),
        signature,
    )
}

/// Role of a published `RangesKt` metadata function. The floating `rangeTo` / `rangeUntil`
/// operators are that facade's Kotlin metadata, not `.kotlin_builtins` package functions.
pub(in crate::jvm) fn published_floating_range_membership(
    function: &FunctionInfo,
) -> Option<CompilerIntrinsic> {
    if function.callable.owner != crate::types::wk::ranges_facade() {
        return None;
    }
    let signature = function.generic_sig.as_ref()?;
    floating_range_membership(
        function.flags.operator,
        function.flags.suspend,
        function.flags.infix,
        function.context_count,
        function.call_sig.vararg,
        signature,
    )
}

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
    use std::collections::HashMap;

    use crate::libraries::{CompilerIntrinsic, FunctionInfo, GenericSig, LibraryCallable};
    use crate::symbol_source::{SymbolNamespace, SymbolSource};
    use crate::types::{type_name, Ty};

    use super::{builtin_bounds, builtin_ty, function_role, published_floating_range_membership};

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
        let mut open_end = function;
        open_end.name = "rangeUntil".to_string();
        open_end.ret = super::super::super::metadata::BuiltinTy::Class {
            internal: "kotlin/ranges/OpenEndRange".to_string(),
            args: vec![super::super::super::metadata::BuiltinTy::class(
                "kotlin/Double",
            )],
            nullable: false,
            shape: Default::default(),
        };
        let open_signature = GenericSig {
            ret: builtin_ty(&open_end.ret, &bounds),
            ..signature
        };
        assert_eq!(
            function_role(
                crate::types::wk::kotlin_ranges_package(),
                &open_end,
                &open_signature,
            ),
            Some(CompilerIntrinsic::FloatingRangeMembership)
        );
    }

    #[test]
    fn a_same_shaped_facade_does_not_publish_floating_range_membership() {
        let result = Ty::obj_args("kotlin/ranges/ClosedFloatingPointRange", &[Ty::Double]);
        let callable = LibraryCallable::library(
            "example/Ranges",
            "rangeTo",
            vec![Ty::Double, Ty::Double],
            result,
            result,
            "(DD)Lkotlin/ranges/ClosedFloatingPointRange;",
        );
        let mut function = FunctionInfo::plain(
            crate::libraries::FnKind::Extension,
            Some(Ty::Double),
            callable,
        );
        function.flags.operator = true;
        function.generic_sig = Some(GenericSig {
            formals: Vec::new(),
            formal_bounds: Vec::new(),
            receiver: Some(Ty::Double),
            params: vec![Ty::Double],
            ret: result,
            return_policy: Default::default(),
        });
        assert_eq!(published_floating_range_membership(&function), None);
    }

    #[test]
    fn the_ranges_facade_publishes_floating_range_membership() {
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let libraries = crate::jvm::jvm_libraries::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(vec![stdlib]),
        ))
        .expect("stdlib provider");
        let membership = |name: &str, descriptor: &str| {
            let symbols =
                libraries.symbols(SymbolNamespace::Package(type_name("kotlin/ranges")), name);
            let functions = match &symbols.callables {
                crate::libraries::Callables::Functions(functions)
                | crate::libraries::Callables::Both { functions, .. } => functions,
                _ => panic!("{name} is missing from the stdlib catalog"),
            };
            let function = functions
                .overloads
                .iter()
                .find(|function| function.callable.descriptor == descriptor)
                .unwrap_or_else(|| {
                    panic!(
                        "no {name} {descriptor}; overloads={:?}",
                        functions
                            .overloads
                            .iter()
                            .map(|function| (
                                function.callable.owner.render(),
                                function.callable.descriptor.as_str(),
                                function.generic_sig.as_ref().map(|signature| (
                                    signature.receiver,
                                    signature.params.clone(),
                                    signature.ret,
                                )),
                                function.flags.operator,
                                function.callable.compiler_intrinsic,
                            ))
                            .collect::<Vec<_>>()
                    )
                });
            assert_eq!(function.callable.owner, crate::types::wk::ranges_facade());
            assert_eq!(
                function.callable.compiler_intrinsic,
                Some(CompilerIntrinsic::FloatingRangeMembership)
            );
        };
        membership("rangeTo", "(DD)Lkotlin/ranges/ClosedFloatingPointRange;");
        membership("rangeTo", "(FF)Lkotlin/ranges/ClosedFloatingPointRange;");
        membership("rangeUntil", "(DD)Lkotlin/ranges/OpenEndRange;");
        membership("rangeUntil", "(FF)Lkotlin/ranges/OpenEndRange;");

        let symbols = libraries.symbols(
            SymbolNamespace::Package(type_name("kotlin/ranges")),
            "rangeTo",
        );
        let functions = match &symbols.callables {
            crate::libraries::Callables::Functions(functions)
            | crate::libraries::Callables::Both { functions, .. } => functions,
            _ => panic!("rangeTo missing"),
        };
        let comparable = functions
            .overloads
            .iter()
            .find(|function| {
                function.callable.descriptor
                    == "(Ljava/lang/Comparable;Ljava/lang/Comparable;)Lkotlin/ranges/ClosedRange;"
            })
            .expect("generic rangeTo");
        assert_eq!(comparable.callable.compiler_intrinsic, None);
    }
}
