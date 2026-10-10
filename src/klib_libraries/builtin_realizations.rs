//! Compiler operations of the language's builtin declarations.
//!
//! A builtin such as `Int.plus` or `arrayOf` has its compiler operation by its exact declaration,
//! as from every provider: the shared rules check the owner, the complete semantic signature and
//! the declaration modifiers, never a spelling alone, and never a target's own intrinsic
//! annotation. A declaration the rules do not name is realized by its serialized IR body.

use crate::libraries::builtin_declaration::{semantic_call_role, BuiltinMemberDeclaration};
use crate::libraries::builtin_member_realization as member_rules;
use crate::libraries::builtin_top_level_realization as package_rules;
use crate::libraries::{FunctionInfo, MemberRealization, PropertyInfo};
use crate::types::TypeName;

/// Attach the operation the member `function` of `owner` denotes, if it is a builtin one.
pub(super) fn realize_member(owner: TypeName, function: &mut FunctionInfo) {
    let (params, ret) = match &function.generic_sig {
        Some(signature) => (signature.params.clone(), signature.ret),
        None => (function.semantic_params().to_vec(), function.callable.ret),
    };
    let annotations = function.callable.annotations.clone();
    let facts = || BuiltinMemberDeclaration {
        owner,
        name: &function.callable.name,
        params: &params,
        ret,
        is_property: false,
        is_operator: function.flags.operator,
        is_infix: function.flags.infix,
        annotations: &annotations,
    };
    let realization = match member_rules::realization(facts()) {
        MemberRealization::Dispatch => member_rules::unsigned_range_construction(&facts())
            .or_else(|| member_rules::primitive_iterator_next(&facts())),
        realization => Some(realization),
    };
    let role = semantic_call_role(facts());
    if let Some(realization) = realization {
        function.callable.member_realization = realization;
    }
    function.callable.semantic_role = role;
}

/// Attach the role the member property `property` of `owner` has, if it is a builtin one.
pub(super) fn realize_member_property(owner: TypeName, property: &mut PropertyInfo) {
    let params = property.getter.params.clone();
    property.getter.semantic_role = semantic_call_role(BuiltinMemberDeclaration {
        owner,
        name: &property.name,
        params: &params,
        ret: property.ty,
        is_property: true,
        is_operator: false,
        is_infix: false,
        annotations: &[],
    });
}

/// Attach the operation the package function `function` denotes, if it is a builtin one.
pub(super) fn realize_package_function(package: TypeName, function: &mut FunctionInfo) {
    let name = function.callable.name.clone();
    package_rules::attach_function_realization(package, &name, function);
}

/// Attach the operation the package property `property` reads, if it is a builtin one.
pub(super) fn realize_package_property(package: TypeName, property: &mut PropertyInfo) {
    let name = property.name.clone();
    property.getter.compiler_intrinsic =
        package_rules::normalized_property_realization(package, &name, property);
}
