//! Provider-boundary realization of normalized Kotlin package declarations.
//!
//! These operations are supplied by the compiler on every target. Providers may discover the
//! declarations in different artifacts, but attach a realization only after normalizing the full
//! source signature below. Package/name dispatch alone is deliberately insufficient: an overload
//! with a different receiver, parameter list, result, or declaration modifier remains ordinary.

use crate::libraries::builtin_declaration::{
    BuiltinFunctionDeclaration, BuiltinPropertyDeclaration,
};
use crate::libraries::{
    kotlin_array_factory_kind, CompilerIntrinsic, FnKind, FunctionInfo, PropKind, PropertyInfo,
};
use crate::types::{ArrayFactoryKind, Ty, TypeName};

fn plain(facts: &BuiltinFunctionDeclaration<'_>, kind: FnKind) -> bool {
    facts.kind == kind
        && facts.context_count == 0
        && !facts.is_suspend
        && !facts.is_infix
        && facts.params.iter().all(|ty| *ty != Ty::Error)
        && facts.ret != Ty::Error
}

fn function_shape(ty: Ty, arity: usize, suspend: bool) -> bool {
    matches!(ty.non_null(), Ty::Fun(signature)
        if signature.params.len() == arity
            && signature.context_count == 0
            && !signature.has_receiver
            && signature.suspend == suspend)
}

fn continuation_argument(ty: Ty) -> Option<Ty> {
    let ty = ty.non_null();
    let internal = ty.obj_internal()?;
    if !internal.matches("kotlin/coroutines/Continuation") {
        return None;
    }
    let [argument] = ty.type_args() else {
        return None;
    };
    Some(*argument)
}

fn package_receiver(receiver: Ty, packages: &[&str]) -> bool {
    receiver
        .non_null()
        .obj_internal()
        .is_some_and(|name| packages.iter().any(|package| name.matches(package)))
}

fn array_factory(facts: &BuiltinFunctionDeclaration<'_>) -> Option<CompilerIntrinsic> {
    if !facts.package.matches("kotlin")
        || !plain(facts, FnKind::TopLevel)
        || facts.receiver.is_some()
        || facts.is_operator
    {
        return None;
    }
    let kind = kotlin_array_factory_kind(facts.name)?;
    let valid = match kind {
        ArrayFactoryKind::PrimitiveVararg(element) => {
            facts.type_parameter_count == 0
                && facts.params == [Ty::array(element)]
                && facts.vararg == Some(0)
                && facts.ret == Ty::array(element)
        }
        ArrayFactoryKind::PrimitiveSize(element) => {
            let parameters_match = match facts.params {
                [Ty::Int] => true,
                [Ty::Int, initializer] => {
                    function_shape(*initializer, 1, false) && initializer.fun_ret() == Some(element)
                }
                _ => false,
            };
            facts.type_parameter_count == 0
                && facts.vararg.is_none()
                && facts.ret == Ty::array(element)
                && parameters_match
        }
        ArrayFactoryKind::ReferenceVararg => {
            let [elements] = facts.params else {
                return None;
            };
            let Some(Ty::OutProjection(element)) = elements.array_elem() else {
                return None;
            };
            facts.type_parameter_count == 1
                && facts.vararg == Some(0)
                && elements.is_reference_array()
                && facts.ret.is_reference_array()
                && facts.ret.array_elem() == Some(*element)
        }
        ArrayFactoryKind::ReferenceSize => {
            let [Ty::Int, initializer] = facts.params else {
                return None;
            };
            facts.type_parameter_count == 1
                && facts.vararg.is_none()
                && facts.ret.is_reference_array()
                && function_shape(*initializer, 1, false)
                && initializer.fun_ret() == facts.ret.array_elem()
        }
        ArrayFactoryKind::EmptyReference => {
            facts.type_parameter_count == 1
                && facts.params.is_empty()
                && facts.vararg.is_none()
                && facts.ret.is_reference_array()
        }
        ArrayFactoryKind::NullableReference => {
            facts.type_parameter_count == 1
                && facts.params == [Ty::Int]
                && facts.vararg.is_none()
                && facts.ret.is_reference_array()
                && facts.ret.array_elem().is_some_and(Ty::is_nullable)
        }
    };
    valid.then_some(CompilerIntrinsic::ArrayFactory(kind))
}

fn kotlin_function(facts: &BuiltinFunctionDeclaration<'_>) -> Option<CompilerIntrinsic> {
    if !facts.package.matches("kotlin") || !plain(facts, facts.kind) {
        return None;
    }
    match (facts.name, facts.kind, facts.receiver, facts.params) {
        ("plus", FnKind::Extension, Some(receiver), [parameter])
            if facts.type_parameter_count == 0
                && facts.vararg.is_none()
                && facts.is_operator
                && receiver == Ty::nullable(Ty::String)
                && *parameter == Ty::nullable(Ty::obj("kotlin/Any"))
                && facts.ret == Ty::String =>
        {
            Some(CompilerIntrinsic::StringPlus)
        }
        ("toString", FnKind::Extension, Some(receiver), [])
            if facts.type_parameter_count == 0
                && facts.vararg.is_none()
                && !facts.is_operator
                && receiver == Ty::nullable(Ty::obj("kotlin/Any"))
                && facts.ret == Ty::String =>
        {
            Some(CompilerIntrinsic::NullableAnyToString)
        }
        ("assert", FnKind::TopLevel, None, [Ty::Boolean])
            if facts.type_parameter_count == 0
                && facts.vararg.is_none()
                && !facts.is_operator
                && facts.ret == Ty::Unit =>
        {
            Some(CompilerIntrinsic::Assert)
        }
        ("assert", FnKind::TopLevel, None, [Ty::Boolean, message])
            if facts.type_parameter_count == 0
                && facts.vararg.is_none()
                && !facts.is_operator
                && facts.ret == Ty::Unit
                && function_shape(*message, 0, false)
                && message.fun_ret() == Some(Ty::obj("kotlin/Any")) =>
        {
            Some(CompilerIntrinsic::Assert)
        }
        ("enumValues", FnKind::TopLevel, None, [])
            if facts.type_parameter_count == 1
                && facts.vararg.is_none()
                && !facts.is_operator
                && facts.ret.is_reference_array()
                && facts
                    .ret
                    .array_elem()
                    .is_some_and(|element| matches!(element, Ty::TyParam(_, _))) =>
        {
            Some(CompilerIntrinsic::EnumValues)
        }
        ("enumValueOf", FnKind::TopLevel, None, [Ty::String])
            if facts.type_parameter_count == 1
                && facts.vararg.is_none()
                && !facts.is_operator
                && matches!(facts.ret, Ty::TyParam(_, _)) =>
        {
            Some(CompilerIntrinsic::EnumValueOf)
        }
        _ => array_factory(facts),
    }
}

fn console(facts: &BuiltinFunctionDeclaration<'_>) -> Option<CompilerIntrinsic> {
    if !facts.package.matches("kotlin/io")
        || !plain(facts, FnKind::TopLevel)
        || facts.receiver.is_some()
        || facts.type_parameter_count != 0
        || facts.vararg.is_some()
        || facts.is_operator
        || facts.ret != Ty::Unit
    {
        return None;
    }
    let parameter_is_printable = facts.params.first().is_some_and(|parameter| {
        matches!(
            *parameter,
            Ty::Boolean
                | Ty::Byte
                | Ty::Short
                | Ty::Int
                | Ty::Long
                | Ty::Float
                | Ty::Double
                | Ty::Char
                | Ty::String
        ) || *parameter == Ty::nullable(Ty::obj("kotlin/Any"))
            || *parameter == Ty::array(Ty::Char)
    });
    match (facts.name, facts.params.len()) {
        ("print", 1) if parameter_is_printable => Some(CompilerIntrinsic::Print),
        ("println", 0) => Some(CompilerIntrinsic::Println),
        ("println", 1) if parameter_is_printable => Some(CompilerIntrinsic::Println),
        _ => None,
    }
}

fn collection_or_text(facts: &BuiltinFunctionDeclaration<'_>) -> Option<CompilerIntrinsic> {
    if !plain(facts, FnKind::Extension)
        || facts.type_parameter_count > 2
        || facts.vararg.is_some()
        || facts.is_operator
    {
        return None;
    }
    let receiver = facts.receiver?;
    if facts.package.matches("kotlin/text") && receiver == Ty::String {
        return match (facts.name, facts.params) {
            ("trimIndent", []) if facts.ret == Ty::String => Some(CompilerIntrinsic::TrimIndent),
            ("trimMargin", [Ty::String]) if facts.ret == Ty::String => {
                Some(CompilerIntrinsic::TrimMargin)
            }
            _ => None,
        };
    }
    let collection_receiver = receiver == receiver.non_null()
        && (receiver.is_array()
            || package_receiver(
                receiver,
                &[
                    "kotlin/collections/Collection",
                    "kotlin/collections/Iterable",
                    "kotlin/collections/List",
                    "kotlin/collections/Map",
                ],
            ));
    if !facts.package.matches("kotlin/collections")
        || !collection_receiver
        || !facts.params.is_empty()
    {
        return None;
    }
    match (facts.name, facts.ret) {
        ("isEmpty", Ty::Boolean) => Some(CompilerIntrinsic::IsEmpty),
        ("isNotEmpty", Ty::Boolean) => Some(CompilerIntrinsic::IsNotEmpty),
        ("count", Ty::Int) => Some(CompilerIntrinsic::Count),
        _ => None,
    }
}

fn kotlin_test(facts: &BuiltinFunctionDeclaration<'_>) -> Option<CompilerIntrinsic> {
    if !facts.package.matches("kotlin/test")
        || !plain(facts, FnKind::TopLevel)
        || facts.name != "assertFailsWith"
        || facts.receiver.is_some()
        || facts.type_parameter_count != 1
        || facts.vararg.is_some()
        || facts.is_operator
        || !matches!(facts.ret, Ty::TyParam(_, _))
    {
        return None;
    }
    let block = match facts.params {
        [block] => *block,
        [message, block] if *message == Ty::String || *message == Ty::nullable(Ty::String) => {
            *block
        }
        _ => return None,
    };
    (function_shape(block, 0, false) && block.fun_ret() == Some(Ty::Unit))
        .then_some(CompilerIntrinsic::AssertFailsWith)
}

fn coroutine(facts: &BuiltinFunctionDeclaration<'_>) -> Option<CompilerIntrinsic> {
    if facts.context_count != 0
        || facts.vararg.is_some()
        || facts.is_operator
        || facts.is_infix
        || facts.ret == Ty::Error
    {
        return None;
    }
    if facts.package.matches("kotlin/coroutines") {
        return match (facts.name, facts.kind, facts.receiver, facts.params) {
            ("suspendCoroutine", FnKind::TopLevel, None, [block])
                if facts.type_parameter_count == 1
                    && facts.is_suspend
                    && function_shape(*block, 1, false)
                    && block.fun_ret() == Some(Ty::Unit)
                    && block
                        .fun_params()
                        .and_then(|params| params.first().copied())
                        .and_then(continuation_argument)
                        == Some(facts.ret) =>
            {
                Some(CompilerIntrinsic::SuspendCoroutine)
            }
            ("startCoroutine", FnKind::Extension, Some(receiver), parameters)
                if facts.type_parameter_count >= 1
                    && !facts.is_suspend
                    && matches!(receiver.non_null(), Ty::Fun(signature)
                        if signature.context_count == 0
                            && signature.suspend
                            && parameters.len() == signature.params.len() + 1
                            && parameters[..signature.params.len()] == signature.params[..]
                            && parameters
                                .last()
                                .copied()
                                .and_then(continuation_argument)
                                == Some(signature.ret))
                    && facts.ret == Ty::Unit =>
            {
                Some(CompilerIntrinsic::StartCoroutine)
            }
            _ => None,
        };
    }
    if facts.package.matches("kotlin/coroutines/intrinsics") {
        return match (facts.name, facts.kind, facts.receiver, facts.params) {
            ("suspendCoroutineUninterceptedOrReturn", FnKind::TopLevel, None, [block])
                if facts.type_parameter_count == 1
                    && facts.is_suspend
                    && function_shape(*block, 1, false)
                    && block.fun_ret().is_some_and(|ret| {
                        ret.is_nullable() && ret.non_null() == Ty::obj("kotlin/Any")
                    })
                    && block
                        .fun_params()
                        .and_then(|params| params.first().copied())
                        .and_then(continuation_argument)
                        == Some(facts.ret) =>
            {
                Some(CompilerIntrinsic::SuspendCoroutineUninterceptedOrReturn)
            }
            _ => None,
        };
    }
    None
}

pub(crate) fn function_realization(
    facts: BuiltinFunctionDeclaration<'_>,
) -> Option<CompilerIntrinsic> {
    if facts.package.matches("kotlin") {
        kotlin_function(&facts)
    } else if facts.package.matches("kotlin/io") {
        console(&facts)
    } else if facts.package.matches("kotlin/collections") || facts.package.matches("kotlin/text") {
        collection_or_text(&facts)
    } else if facts.package.matches("kotlin/coroutines")
        || facts.package.matches("kotlin/coroutines/intrinsics")
    {
        coroutine(&facts)
    } else if facts.package.matches("kotlin/test") {
        kotlin_test(&facts)
    } else {
        None
    }
}

/// Attach the common rule to an already-normalized provider callable without reopening provider
/// metadata or inspecting a physical descriptor.
pub(crate) fn normalized_function_realization(
    package: TypeName,
    name: &str,
    function: &FunctionInfo,
) -> Option<CompilerIntrinsic> {
    let params = function.semantic_params();
    let generic = function.generic_sig.as_ref();
    function_realization(BuiltinFunctionDeclaration {
        package,
        name,
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
    })
}

pub(crate) fn property_realization(
    facts: BuiltinPropertyDeclaration<'_>,
) -> Option<CompilerIntrinsic> {
    if facts.context_count != 0 || facts.type_parameter_count != 0 || facts.mutable {
        return None;
    }
    match (
        facts.package,
        facts.name,
        facts.kind,
        facts.receiver,
        facts.ty,
    ) {
        (package, "code", PropKind::Extension, Some(Ty::Char), Ty::Int)
            if package.matches("kotlin") =>
        {
            Some(CompilerIntrinsic::CharCode)
        }
        (package, "COROUTINE_SUSPENDED", PropKind::TopLevel, None, ty)
            if package.matches("kotlin/coroutines/intrinsics") && ty == Ty::obj("kotlin/Any") =>
        {
            Some(CompilerIntrinsic::CoroutineSuspended)
        }
        (package, "coroutineContext", PropKind::TopLevel, None, ty)
            if package.matches("kotlin/coroutines")
                && ty == Ty::obj("kotlin/coroutines/CoroutineContext") =>
        {
            Some(CompilerIntrinsic::CoroutineContext)
        }
        _ => None,
    }
}

pub(crate) fn normalized_property_realization(
    package: TypeName,
    name: &str,
    property: &PropertyInfo,
) -> Option<CompilerIntrinsic> {
    property_realization(BuiltinPropertyDeclaration {
        package,
        name,
        kind: property.kind,
        receiver: property.receiver,
        ty: property.ty,
        context_count: property.context_count,
        type_parameter_count: property.formals.len(),
        mutable: property.setter.is_some(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::builtin_declaration::BuiltinFunctionDeclaration;
    use crate::types::type_name;

    fn string_plus<'a>(params: &'a [Ty], ret: Ty) -> BuiltinFunctionDeclaration<'a> {
        BuiltinFunctionDeclaration {
            package: type_name("kotlin"),
            name: "plus",
            kind: FnKind::Extension,
            receiver: Some(Ty::nullable(Ty::String)),
            params,
            ret,
            context_count: 0,
            type_parameter_count: 0,
            vararg: None,
            is_suspend: false,
            is_operator: true,
            is_infix: false,
        }
    }

    #[test]
    fn same_spelling_needs_the_complete_declaration() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        assert_eq!(
            function_realization(string_plus(&[any], Ty::String)),
            Some(CompilerIntrinsic::StringPlus)
        );
        assert_eq!(
            function_realization(string_plus(&[Ty::Int], Ty::String)),
            None
        );
        assert_eq!(function_realization(string_plus(&[any], Ty::Int)), None);
        let non_operator_params = [any];
        let mut non_operator = string_plus(&non_operator_params, Ty::String);
        non_operator.is_operator = false;
        assert_eq!(function_realization(non_operator), None);
    }

    #[test]
    fn reference_array_factory_matches_the_metadata_vararg_projection() {
        let type_parameter = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
        let parameter = Ty::obj_args("kotlin/Array", &[Ty::out_projection(type_parameter)]);
        let result = Ty::obj_args("kotlin/Array", &[type_parameter]);
        let declaration = |params| BuiltinFunctionDeclaration {
            package: type_name("kotlin"),
            name: "arrayOf",
            kind: FnKind::TopLevel,
            receiver: None,
            params,
            ret: result,
            context_count: 0,
            type_parameter_count: 1,
            vararg: Some(0),
            is_suspend: false,
            is_operator: false,
            is_infix: false,
        };

        assert_eq!(
            function_realization(declaration(std::slice::from_ref(&parameter))),
            Some(CompilerIntrinsic::ArrayFactory(
                ArrayFactoryKind::ReferenceVararg
            ))
        );

        let invariant_parameter = Ty::obj_args("kotlin/Array", &[type_parameter]);
        assert_eq!(
            function_realization(declaration(std::slice::from_ref(&invariant_parameter))),
            None,
            "an ordinary same-named Array<T> parameter is not the builtin vararg declaration"
        );
    }
}
