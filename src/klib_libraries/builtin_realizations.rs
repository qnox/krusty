//! Compiler operations of the language's builtin declarations.
//!
//! A builtin such as `Int.plus` or `arrayOf` has its compiler operation by its exact declaration,
//! as from every provider: the shared rules check the owner, the complete semantic signature and
//! the declaration modifiers, never a spelling alone, and never a target's own intrinsic
//! annotation. A declaration the rules do not name is realized by its serialized IR body.

mod arrays;
mod collection_size;
mod library_helpers;

use crate::libraries::builtin_declaration::{semantic_call_role, BuiltinMemberDeclaration};
use crate::libraries::builtin_member_realization as member_rules;
use crate::libraries::builtin_top_level_realization as package_rules;
use crate::libraries::{CompilerIntrinsic, FunctionInfo, MemberRealization, PropertyInfo};
use crate::types::TypeName;

mod throwables;

pub(super) use arrays::realize_array_constructor;
pub(super) use throwables::{realize_constructor, realize_throwable_property};

/// Attach the operation the member `function` of `owner` denotes, if it is a builtin one.
pub(super) fn realize_member(
    owner: TypeName,
    compiler_builtin_owner: bool,
    function: &mut FunctionInfo,
) {
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
        MemberRealization::Intrinsic(CompilerIntrinsic::StringPlus) if !compiler_builtin_owner => {
            None
        }
        MemberRealization::Dispatch => member_rules::unsigned_range_construction(&facts())
            .or_else(|| {
                member_rules::unsigned_bitwise_intrinsic(&facts()).map(MemberRealization::Intrinsic)
            })
            .or_else(|| member_rules::primitive_iterator_next(&facts())),
        realization => Some(realization),
    };
    let role = semantic_call_role(facts());
    if let Some(realization) = realization {
        function.callable.member_realization = realization;
    }
    function.callable.semantic_role = role;
}

/// Attach the operation and role the member property `property` of `owner` has, if it is a
/// builtin one.
pub(super) fn realize_member_property(owner: TypeName, property: &mut PropertyInfo) {
    let params = property.getter.params.clone();
    let facts = || BuiltinMemberDeclaration {
        owner,
        name: &property.name,
        params: &params,
        ret: property.ty,
        is_property: true,
        is_operator: false,
        is_infix: false,
        annotations: &[],
    };
    // An array's `size` is the array operation, as the JVM provider realizes the same declaration,
    // and `String.length` the string's, as the core builtins publish it.
    if let realization @ MemberRealization::Intrinsic(_) = member_rules::realization(facts()) {
        property.getter.member_realization = realization;
    } else if owner == crate::types::wk::string()
        && property.name == "length"
        && params.is_empty()
        && property.ty == crate::types::Ty::Int
    {
        property.getter.member_realization =
            MemberRealization::Intrinsic(crate::libraries::CompilerIntrinsic::StringLength);
    }
    property.getter.semantic_role = semantic_call_role(facts());
}

/// Attach the operation the package function `function` denotes, if it is a builtin one.
pub(super) fn realize_package_function(
    package: TypeName,
    compiler_builtin_owner: bool,
    signature: &crate::metadata::id_signature::KlibPublicIdSignature,
    function: &mut FunctionInfo,
) {
    let name = function.callable.name.clone();
    package_rules::attach_dependency_function_realization(
        package,
        &name,
        compiler_builtin_owner,
        function,
        Some(crate::types::type_name("kotlin/collections/ArrayList")),
    );
    let params = function.semantic_params();
    let generic = function.generic_sig.as_ref();
    let helper = library_helpers::realization(
        crate::libraries::builtin_declaration::BuiltinFunctionDeclaration {
            package,
            name: &name,
            kind: function.kind,
            receiver: function.semantic_receiver(),
            params: params.as_ref(),
            ret: generic.map_or(function.callable.ret, |signature| signature.ret),
            context_count: function.context_count,
            type_parameter_count: generic.map_or(0, |signature| signature.formals.len()),
            vararg: function.call_sig.vararg_index,
            is_suspend: function.flags.suspend,
            is_operator: function.flags.operator,
            is_infix: function.flags.infix,
        },
        compiler_builtin_owner,
    );
    if let Some(operation) = helper {
        function.callable.compiler_intrinsic = Some(operation);
    }
    if let Some(operation) = collection_size::realization(package, compiler_builtin_owner, function)
    {
        function.callable.compiler_intrinsic = Some(operation);
    }
    if compiler_builtin_owner {
        if let Some(intrinsic) = crate::libraries::stdlib_klib_intrinsic(signature) {
            if intrinsic.accepts_normalized_klib(function) {
                function.callable.compiler_intrinsic = Some(intrinsic.compiler_intrinsic());
            }
        }
    }
}

/// Attach the operation the package property `property` reads, if it is a builtin one.
pub(super) fn realize_package_property(package: TypeName, property: &mut PropertyInfo) {
    let name = property.name.clone();
    property.getter.compiler_intrinsic =
        package_rules::normalized_property_realization(package, &name, property);
}
