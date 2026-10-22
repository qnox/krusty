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
use crate::types::{ArrayFactoryKind, Ty, TyKind, TypeName};

/// Attach every compiler-owned realization fact for one normalized top-level declaration.
/// Providers call this only after constructing the complete semantic signature; consumers never
/// rediscover either fact from a package, facade, or member spelling.
pub(crate) fn attach_function_realization(
    package: TypeName,
    name: &str,
    function: &mut FunctionInfo,
) {
    if let Some(intrinsic) = normalized_function_realization(package, name, function) {
        function.callable.compiler_intrinsic = Some(intrinsic);
    }
    function.callable.semantic_role = normalized_function_semantic_role(package, name, function);
}

/// Attach package operations to a dependency declaration, admitting stdlib-only roles only when
/// the provider has authenticated the containing library as its platform stdlib. Other normalized
/// builtin operations retain their existing exact-signature rules.
pub(crate) fn attach_dependency_function_realization(
    package: TypeName,
    name: &str,
    compiler_builtin_owner: bool,
    function: &mut FunctionInfo,
    array_list_classifier: Option<TypeName>,
) {
    function.callable.compiler_intrinsic = normalized_function_realization_with_stdlib_owner(
        package,
        name,
        function,
        compiler_builtin_owner,
        array_list_classifier,
    );
    let semantic_role = normalized_function_semantic_role(package, name, function);
    function.callable.semantic_role = match semantic_role {
        Some(crate::types::SemanticCallRole::KotlinCharSequenceIterator)
            if !compiler_builtin_owner =>
        {
            None
        }
        role => role,
    };
}

fn property_reference_receiver(receiver: Ty, mutable: bool) -> Option<(u8, &'static [Ty])> {
    let receiver = receiver.non_null();
    let internal = receiver.obj_internal()?;
    let arity = [
        (
            if mutable {
                "kotlin/reflect/KMutableProperty0"
            } else {
                "kotlin/reflect/KProperty0"
            },
            0,
        ),
        (
            if mutable {
                "kotlin/reflect/KMutableProperty1"
            } else {
                "kotlin/reflect/KProperty1"
            },
            1,
        ),
    ]
    .into_iter()
    .find_map(|(candidate, arity)| {
        (internal == crate::types::type_name(candidate)).then_some(arity)
    })?;
    let arguments = receiver.type_args();
    (arguments.len() == arity as usize + 1).then_some((arity, arguments))
}

fn property_reference_metadata_parameter(ty: Ty) -> bool {
    let ty = ty.non_null();
    ty.obj_internal() == Some(crate::types::type_name("kotlin/reflect/KProperty"))
        && matches!(ty.type_args(), [argument] if argument.is_star_projection())
}

pub(crate) fn function_semantic_role(
    facts: &BuiltinFunctionDeclaration<'_>,
) -> Option<crate::types::SemanticCallRole> {
    use crate::types::SemanticCallRole;

    if facts.package.matches("kotlin/text")
        && facts.name == "iterator"
        && facts.kind == FnKind::Extension
        && facts.receiver == Some(Ty::obj_name(crate::types::wk::char_sequence()))
        && facts.params.is_empty()
        && facts.ret == Ty::obj("kotlin/collections/CharIterator")
        && facts.context_count == 0
        && facts.type_parameter_count == 0
        && facts.vararg.is_none()
        && !facts.is_suspend
        && facts.is_operator
        && !facts.is_infix
    {
        return Some(SemanticCallRole::KotlinCharSequenceIterator);
    }
    if facts.package != crate::types::wk::kotlin_package()
        || facts.kind != FnKind::Extension
        || facts.context_count != 0
        || facts.is_suspend
        || !facts.is_operator
        || facts.is_infix
        || facts.vararg.is_some()
    {
        return None;
    }
    let receiver = facts.receiver?;
    if facts.name == "getValue" {
        let (arity, arguments) = property_reference_receiver(receiver, false)?;
        let [this_ref, property] = facts.params else {
            return None;
        };
        let this_ref_matches = match arity {
            0 => *this_ref == Ty::nullable(Ty::obj_name(crate::types::wk::any())),
            1 => *this_ref == arguments[0],
            _ => false,
        };
        return (facts.type_parameter_count == arity as usize + 1
            && this_ref_matches
            && property_reference_metadata_parameter(*property)
            && facts.ret == arguments[arity as usize])
            .then_some(SemanticCallRole::KotlinPropertyReferenceDelegateGet(arity));
    }
    if facts.name == "setValue" {
        let (arity, arguments) = property_reference_receiver(receiver, true)?;
        let [this_ref, property, value] = facts.params else {
            return None;
        };
        let this_ref_matches = match arity {
            0 => *this_ref == Ty::nullable(Ty::obj_name(crate::types::wk::any())),
            1 => *this_ref == arguments[0],
            _ => false,
        };
        return (facts.type_parameter_count == arity as usize + 1
            && this_ref_matches
            && property_reference_metadata_parameter(*property)
            && *value == arguments[arity as usize]
            && facts.ret == Ty::Unit)
            .then_some(SemanticCallRole::KotlinPropertyReferenceDelegateSet(arity));
    }
    None
}

fn plain(facts: &BuiltinFunctionDeclaration<'_>, kind: FnKind) -> bool {
    facts.kind == kind
        && facts.context_count == 0
        && !facts.is_suspend
        && !facts.is_infix
        && facts.params.iter().all(|ty| *ty != Ty::Error)
        && facts.ret != Ty::Error
}

fn function_shape(ty: Ty, arity: usize, suspend: bool) -> bool {
    matches!(ty.non_null().kind(), TyKind::Fun(signature)
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
            let Some(TyKind::OutProjection(element)) = elements.array_elem().map(Ty::kind) else {
                return None;
            };
            facts.type_parameter_count == 1
                && facts.vararg == Some(0)
                && elements.is_reference_array()
                && facts.ret.is_reference_array()
                && facts.ret.array_elem() == Some(element)
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
        ("uintCompare", FnKind::TopLevel, None, [Ty::Int, Ty::Int])
        | ("ulongCompare", FnKind::TopLevel, None, [Ty::Long, Ty::Long])
            if facts.type_parameter_count == 0
                && facts.vararg.is_none()
                && !facts.is_operator
                && !facts.is_infix
                && facts.ret == Ty::Int =>
        {
            Some(CompilerIntrinsic::UnsignedCompare {
                carrier: facts.params[0],
            })
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
                    .is_some_and(|element| element.is_ty_param()) =>
        {
            Some(CompilerIntrinsic::EnumValues)
        }
        ("enumValueOf", FnKind::TopLevel, None, [Ty::String])
            if facts.type_parameter_count == 1
                && facts.vararg.is_none()
                && !facts.is_operator
                && facts.ret.is_ty_param() =>
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

/// kotlinc's `WithIndexHandler.matchIterable`, over the complete stdlib declarations:
/// - `kotlin.collections`: `fun <T> Array<out T>.withIndex(): Iterable<IndexedValue<T>>`, each
///   primitive array's `fun IntArray.withIndex(): Iterable<IndexedValue<Int>>`, and
///   `fun <T> Iterable<T>.withIndex(): Iterable<IndexedValue<T>>`;
/// - `kotlin.text`: `fun CharSequence.withIndex(): Iterable<IndexedValue<Char>>`;
/// - `kotlin.sequences`: `fun <T> Sequence<T>.withIndex(): Sequence<IndexedValue<T>>`.
///
/// The result must wrap exactly the receiver's element in `IndexedValue`, so a same-named
/// extension with another result or element relation stays an ordinary call.
fn with_index(facts: &BuiltinFunctionDeclaration<'_>) -> Option<CompilerIntrinsic> {
    use crate::types::wk;
    if facts.name != wk::WITH_INDEX
        || !plain(facts, FnKind::Extension)
        || !facts.params.is_empty()
        || facts.vararg.is_some()
        || facts.is_operator
    {
        return None;
    }
    let receiver = facts.receiver?;
    if receiver != receiver.non_null() {
        return None;
    }
    // The receiver's own type parameter `T`, unprojected unless the declaration says otherwise.
    let generic_element = |argument: Ty| {
        (facts.type_parameter_count == 1 && argument.is_ty_param()).then_some(argument)
    };
    let class = receiver.obj_internal();
    let (element, container) = if facts.package == wk::kotlin_collections_package() {
        if receiver.is_reference_array() {
            let [argument] = receiver.type_args() else {
                return None;
            };
            let TyKind::OutProjection(element) = argument.kind() else {
                return None;
            };
            (generic_element(element)?, wk::iterable())
        } else if receiver.is_array() {
            if facts.type_parameter_count != 0 {
                return None;
            }
            (receiver.array_elem()?, wk::iterable())
        } else if class == Some(wk::iterable()) {
            let [element] = receiver.type_args() else {
                return None;
            };
            (generic_element(*element)?, wk::iterable())
        } else {
            return None;
        }
    } else if facts.package == wk::kotlin_text_package() {
        if class != Some(wk::char_sequence())
            || !receiver.type_args().is_empty()
            || facts.type_parameter_count != 0
        {
            return None;
        }
        (Ty::Char, wk::iterable())
    } else if facts.package == wk::kotlin_sequences_package() && class == Some(wk::sequence()) {
        let [element] = receiver.type_args() else {
            return None;
        };
        (generic_element(*element)?, wk::sequence())
    } else {
        return None;
    };
    let indexed = Ty::obj_args_name(wk::indexed_value(), &[element]);
    (facts.ret == Ty::obj_args_name(container, &[indexed])).then_some(CompilerIntrinsic::WithIndex)
}

fn kotlin_test(facts: &BuiltinFunctionDeclaration<'_>) -> Option<CompilerIntrinsic> {
    if !facts.package.matches("kotlin/test")
        || !plain(facts, FnKind::TopLevel)
        || facts.name != "assertFailsWith"
        || facts.receiver.is_some()
        || facts.type_parameter_count != 1
        || facts.vararg.is_some()
        || facts.is_operator
        || !facts.ret.is_ty_param()
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

fn reflection(facts: &BuiltinFunctionDeclaration<'_>) -> Option<CompilerIntrinsic> {
    (facts.package.matches("kotlin/reflect")
        && facts.name == "typeOf"
        && plain(facts, FnKind::TopLevel)
        && facts.receiver.is_none()
        && facts.params.is_empty()
        && facts.type_parameter_count == 1
        && facts.vararg.is_none()
        && !facts.is_operator
        && facts.ret == Ty::obj("kotlin/reflect/KType"))
    .then_some(CompilerIntrinsic::TypeOf)
}

fn progression_builder(facts: &BuiltinFunctionDeclaration<'_>) -> Option<CompilerIntrinsic> {
    if facts.kind != FnKind::Extension
        || facts.context_count != 0
        || facts.is_suspend
        || facts.is_operator
        || facts.type_parameter_count != 0
        || facts.vararg.is_some()
    {
        return None;
    }
    let receiver = facts.receiver?;
    let class = |ty: Ty, class: crate::types::wk::ProgressionClass| {
        ty.type_args().is_empty()
            && ty
                .obj_internal()
                .and_then(crate::types::wk::progression_class)
                == Some(class)
    };
    use crate::types::wk::ProgressionBuilder;
    use crate::types::wk::ProgressionClass::{Progression, Range};
    let integral = |ty: Ty| {
        matches!(
            ty,
            Ty::Byte
                | Ty::Short
                | Ty::Int
                | Ty::Long
                | Ty::Char
                | Ty::UByte
                | Ty::UShort
                | Ty::UInt
                | Ty::ULong
        )
    };
    match (
        crate::types::wk::progression_builder(facts.package, facts.name)?,
        facts.params,
    ) {
        (ProgressionBuilder::DownTo, [to])
            if facts.is_infix && integral(receiver) && integral(*to) =>
        {
            class(facts.ret, Progression).then_some(CompilerIntrinsic::RangeDownTo)
        }
        (ProgressionBuilder::Until, [to])
            if facts.is_infix && integral(receiver) && integral(*to) =>
        {
            class(facts.ret, Range).then_some(CompilerIntrinsic::RangeUntil)
        }
        (ProgressionBuilder::Step, [Ty::Int | Ty::Long])
            if facts.is_infix && class(receiver, Progression) && facts.ret == receiver =>
        {
            Some(CompilerIntrinsic::ProgressionStep)
        }
        (ProgressionBuilder::Reversed, [])
            if !facts.is_infix && class(receiver, Progression) && facts.ret == receiver =>
        {
            Some(CompilerIntrinsic::ProgressionReversed)
        }
        _ => None,
    }
}

/// `ComparableRangeToHandler`: the stdlib's
/// `operator fun <T : Comparable<T>> T.rangeTo(that: T): ClosedRange<T>`, declared by the
/// authenticated compiler builtins owner. A dependency declaring the same shape keeps its body.
fn comparable_range_to(
    facts: &BuiltinFunctionDeclaration<'_>,
    compiler_builtin_owner: bool,
) -> Option<CompilerIntrinsic> {
    use crate::types::wk;
    if !compiler_builtin_owner
        || facts.name != "rangeTo"
        || !plain(facts, FnKind::Extension)
        || !facts.is_operator
        || facts.type_parameter_count != 1
        || facts.vararg.is_some()
    {
        return None;
    }
    let element = facts.receiver?;
    let bound = element.ty_param_bound()?;
    let is_element = |ty: &Ty| match (ty.kind(), element.kind()) {
        (TyKind::TyParam(name, _), TyKind::TyParam(declared, _)) => name == declared,
        _ => false,
    };
    let applied_to_element = |ty: Ty, class| {
        matches!(ty.kind(), TyKind::Obj(name, arguments)
            if name == class && matches!(arguments.as_slice(), [argument] if is_element(argument)))
    };
    (applied_to_element(bound, wk::comparable())
        && facts.params == [element]
        && applied_to_element(facts.ret, wk::closed_range()))
    .then_some(CompilerIntrinsic::ComparableRangeTo)
}

/// `matchStdlibExtensionContainsCall`: an operator `contains` extension of a closed range with one
/// parameter.
fn closed_range_contains(facts: &BuiltinFunctionDeclaration<'_>) -> Option<CompilerIntrinsic> {
    use crate::types::wk;
    if facts.kind != FnKind::Extension
        || facts.name != "contains"
        || !facts.is_operator
        || facts.is_suspend
        || facts.is_infix
        || facts.ret != Ty::Boolean
        || facts.params.len() != 1
        || facts.context_count != 0
        || facts.vararg.is_some()
    {
        return None;
    }
    let receiver = facts.receiver?.obj_internal()?;
    let closed = receiver == wk::closed_range()
        || receiver == wk::closed_floating_point_range()
        || wk::progression_class(receiver) == Some(wk::ProgressionClass::Range);
    closed.then_some(CompilerIntrinsic::ClosedRangeContains)
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
            ("startCoroutine", FnKind::Extension, Some(receiver), parameters)
                if facts.type_parameter_count >= 1
                    && !facts.is_suspend
                    && matches!(receiver.non_null().kind(), TyKind::Fun(signature)
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

/// The package and name of the top-level declaration a compiler-inserted call with `role` reaches.
/// Only this provider boundary spells it; candidates still carry `role` only after
/// [`function_realization`] has checked their full signature.
pub(crate) fn runtime_function_declaration(
    role: CompilerIntrinsic,
) -> Option<(TypeName, &'static str)> {
    match role {
        CompilerIntrinsic::UnsignedCompare { carrier: Ty::Int } => {
            Some((crate::types::type_name("kotlin"), "uintCompare"))
        }
        CompilerIntrinsic::UnsignedCompare { carrier: Ty::Long } => {
            Some((crate::types::type_name("kotlin"), "ulongCompare"))
        }
        _ => None,
    }
}

fn function_realization_with_stdlib_owner(
    facts: BuiltinFunctionDeclaration<'_>,
    compiler_builtin_owner: bool,
) -> Option<CompilerIntrinsic> {
    if let Some(intrinsic) = with_index(&facts) {
        Some(intrinsic)
    } else if facts.package.matches("kotlin") {
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
    } else if facts.package.matches("kotlin/reflect") {
        reflection(&facts)
    } else if facts.package == crate::types::wk::kotlin_ranges_package() {
        progression_builder(&facts)
            .or_else(|| comparable_range_to(&facts, compiler_builtin_owner))
            .or_else(|| {
                compiler_builtin_owner
                    .then(|| closed_range_contains(&facts))
                    .flatten()
            })
    } else {
        None
    }
}

pub(crate) fn function_realization(
    facts: BuiltinFunctionDeclaration<'_>,
) -> Option<CompilerIntrinsic> {
    function_realization_with_stdlib_owner(facts, true)
}

/// Attach the common rule to an already-normalized provider callable without reopening provider
/// metadata or inspecting a physical descriptor.
pub(crate) fn normalized_function_realization(
    package: TypeName,
    name: &str,
    function: &FunctionInfo,
) -> Option<CompilerIntrinsic> {
    normalized_function_realization_with_stdlib_owner(
        package,
        name,
        function,
        true,
        Some(crate::types::type_name("kotlin/collections/ArrayList")),
    )
}

fn normalized_function_realization_with_stdlib_owner(
    package: TypeName,
    name: &str,
    function: &FunctionInfo,
    compiler_builtin_owner: bool,
    array_list_classifier: Option<TypeName>,
) -> Option<CompilerIntrinsic> {
    let params = function.semantic_params();
    let generic = function.generic_sig.as_ref();
    let facts = BuiltinFunctionDeclaration {
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
    };
    if compiler_builtin_owner {
        if let Some(plan) = list_storage_construction(&facts, function, array_list_classifier) {
            return Some(CompilerIntrinsic::ListStorageConstruction(plan));
        }
    }
    function_realization_with_stdlib_owner(facts, compiler_builtin_owner)
}

/// Authenticate the complete generic declaration while the provider still owns it. In particular,
/// an array substituted for a singleton's T must not acquire the packed-elements representation.
fn list_storage_construction(
    facts: &BuiltinFunctionDeclaration<'_>,
    function: &FunctionInfo,
    array_list_classifier: Option<TypeName>,
) -> Option<crate::libraries::ListStorageConstruction> {
    use crate::libraries::{GenericReturnPolicy, ListStorageConstruction};

    if !facts.package.matches("kotlin/collections")
        || !plain(facts, FnKind::TopLevel)
        || facts.receiver.is_some()
        || facts.is_operator
        || function.flags.reified
        || function.flags.is_abstract
        || !function.call_sig.reified_type_parameter_ordinals.is_empty()
        || function.call_sig.vararg != facts.vararg.is_some()
        || function
            .call_sig
            .param_defaults
            .iter()
            .any(|default| *default)
    {
        return None;
    }
    let signature = function.generic_sig.as_ref()?;
    let [formal] = signature.formals.as_slice() else {
        return None;
    };
    let [bounds] = signature.formal_bounds.as_slice() else {
        return None;
    };
    let implicit_bound = Ty::nullable(Ty::obj_name(crate::types::wk::any()));
    if !(bounds.is_empty() || bounds.as_slice() == [implicit_bound])
        || signature.return_policy != GenericReturnPolicy::Exact
        || signature.receiver.is_some()
    {
        return None;
    }
    let element = Ty::ty_param(formal, implicit_bound);
    let readonly = facts.ret == Ty::obj_args("kotlin/collections/List", &[element]);
    let mutable = facts.ret == Ty::obj_args("kotlin/collections/MutableList", &[element]);
    let array_list = array_list_classifier
        .is_some_and(|classifier| facts.ret == Ty::obj_args_name(classifier, &[element]));
    let empty = facts.params.is_empty() && facts.vararg.is_none();
    let packed = facts.params == [Ty::obj_args("kotlin/Array", &[Ty::out_projection(element)])]
        && facts.vararg == Some(0);
    match facts.name {
        "emptyList" if readonly && empty => Some(ListStorageConstruction::ReadonlyEmpty),
        "listOf" if readonly && empty => Some(ListStorageConstruction::ReadonlyEmpty),
        "listOf" if readonly && facts.params == [element] && facts.vararg.is_none() => {
            Some(ListStorageConstruction::ReadonlySingleton)
        }
        "listOf" if readonly && packed => Some(ListStorageConstruction::ReadonlyPacked),
        "mutableListOf" if mutable && empty => Some(ListStorageConstruction::MutableEmpty),
        "mutableListOf" if mutable && packed => Some(ListStorageConstruction::MutablePacked),
        "arrayListOf" if array_list && empty => Some(ListStorageConstruction::MutableEmpty),
        "arrayListOf" if array_list && packed => Some(ListStorageConstruction::MutablePacked),
        _ => None,
    }
}

/// Attach a semantic language role to an already-normalized provider callable. Unlike a compiler
/// intrinsic this is consumed by every backend and remains tied to the selected declaration.
pub(crate) fn normalized_function_semantic_role(
    package: TypeName,
    name: &str,
    function: &FunctionInfo,
) -> Option<crate::types::SemanticCallRole> {
    let params = function.semantic_params();
    let generic = function.generic_sig.as_ref();
    function_semantic_role(&BuiltinFunctionDeclaration {
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

    fn normalized_function_realization_with_stdlib_owner(
        package: TypeName,
        name: &str,
        function: &FunctionInfo,
        compiler_builtin_owner: bool,
    ) -> Option<CompilerIntrinsic> {
        super::normalized_function_realization_with_stdlib_owner(
            package,
            name,
            function,
            compiler_builtin_owner,
            Some(type_name("kotlin/collections/ArrayList")),
        )
    }

    fn storage_factory(name: &str, container: &str, packed: bool, singleton: bool) -> FunctionInfo {
        use crate::libraries::{CallSig, GenericSig, LibraryCallable};
        let formal = Ty::ty_param("storage-element", Ty::nullable(Ty::obj("kotlin/Any")));
        let params = if packed {
            vec![Ty::obj_args("kotlin/Array", &[Ty::out_projection(formal)])]
        } else if singleton {
            vec![formal]
        } else {
            vec![]
        };
        let ret = Ty::obj_args(container, &[formal]);
        let generic = GenericSig {
            formals: vec!["storage-element".into()],
            formal_bounds: vec![vec![]],
            receiver: None,
            params: params.clone(),
            ret,
            return_policy: Default::default(),
        };
        let mut callable =
            LibraryCallable::library("kotlin/collections", name, params.clone(), ret, ret, "");
        callable.generic_sig = Some(Box::new(generic.clone()));
        let mut function = FunctionInfo::plain(FnKind::TopLevel, None, callable);
        function.generic_sig = Some(generic);
        function.call_sig = CallSig::metadata_member(
            params.len(),
            vec!["elements".into(); params.len()],
            vec![false; params.len()],
            packed.then_some(0),
        );
        function
    }

    #[test]
    fn list_storage_construction_is_a_declaration_owned_plan() {
        for (name, container, packed, singleton, expected) in [
            (
                "emptyList",
                "kotlin/collections/List",
                false,
                false,
                "ReadonlyEmpty",
            ),
            (
                "listOf",
                "kotlin/collections/List",
                false,
                false,
                "ReadonlyEmpty",
            ),
            (
                "listOf",
                "kotlin/collections/List",
                false,
                true,
                "ReadonlySingleton",
            ),
            (
                "listOf",
                "kotlin/collections/List",
                true,
                false,
                "ReadonlyPacked",
            ),
            (
                "mutableListOf",
                "kotlin/collections/MutableList",
                false,
                false,
                "MutableEmpty",
            ),
            (
                "mutableListOf",
                "kotlin/collections/MutableList",
                true,
                false,
                "MutablePacked",
            ),
            (
                "arrayListOf",
                "kotlin/collections/ArrayList",
                false,
                false,
                "MutableEmpty",
            ),
            (
                "arrayListOf",
                "kotlin/collections/ArrayList",
                true,
                false,
                "MutablePacked",
            ),
        ] {
            let function = storage_factory(name, container, packed, singleton);
            let package = type_name("kotlin/collections");
            let actual =
                normalized_function_realization_with_stdlib_owner(package, name, &function, true);
            assert_eq!(
                actual.map(|operation| format!("{operation:?}")),
                Some(format!("ListStorageConstruction({expected})")),
                "{name} packed={packed} singleton={singleton}",
            );
            assert_eq!(
                normalized_function_realization_with_stdlib_owner(package, name, &function, false),
                None
            );
        }
    }

    #[test]
    fn list_storage_construction_rejects_incomplete_or_changed_generic_contracts() {
        type ContractChange = fn(&mut FunctionInfo);
        let cases: &[(&str, ContractChange)] = &[
            ("missing generic declaration", |f| f.generic_sig = None),
            ("additional formal", |f| {
                f.generic_sig.as_mut().unwrap().formals.push("other".into())
            }),
            ("missing full bounds", |f| {
                f.generic_sig.as_mut().unwrap().formal_bounds.clear()
            }),
            ("non-default bound", |f| {
                f.generic_sig.as_mut().unwrap().formal_bounds[0] = vec![Ty::obj("kotlin/Number")]
            }),
            ("extra bound", |f| {
                f.generic_sig.as_mut().unwrap().formal_bounds[0] = vec![
                    Ty::nullable(Ty::obj("kotlin/Any")),
                    Ty::obj("kotlin/Number"),
                ]
            }),
            ("different result formal", |f| {
                f.generic_sig.as_mut().unwrap().ret = Ty::obj_args(
                    "kotlin/collections/List",
                    &[Ty::ty_param("other", Ty::nullable(Ty::obj("kotlin/Any")))],
                )
            }),
            ("nullable result", |f| {
                let g = f.generic_sig.as_mut().unwrap();
                g.ret = Ty::nullable(g.ret);
            }),
            ("different collection", |f| {
                f.generic_sig.as_mut().unwrap().ret = Ty::obj("kotlin/collections/Set")
            }),
            ("flexible return policy", |f| {
                f.generic_sig.as_mut().unwrap().return_policy =
                    crate::libraries::GenericReturnPolicy::FlexibleReference
            }),
            ("nullable packed array", |f| {
                let g = f.generic_sig.as_mut().unwrap();
                g.params[0] = Ty::nullable(g.params[0]);
            }),
            ("invariant packed array", |f| {
                let g = f.generic_sig.as_mut().unwrap();
                let element = g.ret.type_args()[0];
                g.params[0] = Ty::obj_args("kotlin/Array", &[element]);
            }),
            ("different packed element", |f| {
                f.generic_sig.as_mut().unwrap().params[0] =
                    Ty::obj_args("kotlin/Array", &[Ty::out_projection(Ty::String)])
            }),
            ("additional value parameter", |f| {
                f.generic_sig.as_mut().unwrap().params.push(Ty::Int)
            }),
            ("missing vararg marker", |f| {
                f.call_sig.vararg = false;
                f.call_sig.vararg_index = None;
            }),
            ("wrong vararg position", |f| {
                f.call_sig.vararg_index = Some(1)
            }),
            ("inconsistent vararg marker", |f| f.call_sig.vararg = false),
            ("defaulted parameter", |f| {
                f.call_sig.param_defaults = vec![true].into()
            }),
            ("receiver", |f| f.receiver = Some(Ty::String)),
            ("generic receiver", |f| {
                f.generic_sig.as_mut().unwrap().receiver = Some(Ty::String)
            }),
            ("member", |f| f.kind = FnKind::Member),
            ("context", |f| f.context_count = 1),
            ("suspend", |f| f.flags.suspend = true),
            ("operator", |f| f.flags.operator = true),
            ("infix", |f| f.flags.infix = true),
            ("abstract", |f| f.flags.is_abstract = true),
            ("reified flag", |f| f.flags.reified = true),
            ("reified formal", |f| {
                f.call_sig.reified_type_parameter_ordinals.push(0)
            }),
        ];
        for &(label, change) in cases {
            let mut function = storage_factory("listOf", "kotlin/collections/List", true, false);
            change(&mut function);
            assert_eq!(
                normalized_function_realization_with_stdlib_owner(
                    type_name("kotlin/collections"),
                    "listOf",
                    &function,
                    true
                ),
                None,
                "{label}"
            );
        }
        let function = storage_factory("listOf", "kotlin/collections/List", true, false);
        assert_eq!(
            normalized_function_realization_with_stdlib_owner(
                type_name("other/collections"),
                "listOf",
                &function,
                true
            ),
            None
        );
        assert_eq!(
            normalized_function_realization_with_stdlib_owner(
                type_name("kotlin/collections"),
                "otherFactory",
                &function,
                true
            ),
            None
        );
    }

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
    fn range_membership_requires_a_boolean_stdlib_operator() {
        let declaration = |ret| BuiltinFunctionDeclaration {
            package: crate::types::wk::kotlin_ranges_package(),
            name: "contains",
            kind: FnKind::Extension,
            receiver: Some(Ty::obj("kotlin/ranges/IntRange")),
            params: &[Ty::Byte],
            ret,
            context_count: 0,
            type_parameter_count: 0,
            vararg: None,
            is_suspend: false,
            is_operator: true,
            is_infix: false,
        };
        assert_eq!(
            function_realization_with_stdlib_owner(declaration(Ty::Boolean), true),
            Some(CompilerIntrinsic::ClosedRangeContains)
        );
        assert_eq!(
            function_realization_with_stdlib_owner(declaration(Ty::Boolean), false),
            None
        );
        assert_eq!(
            function_realization_with_stdlib_owner(declaration(Ty::Int), true),
            None
        );
        let mut suspend = declaration(Ty::Boolean);
        suspend.is_suspend = true;
        assert_eq!(function_realization(suspend), None);
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

    fn enum_entries<'a>(params: &'a [Ty], ret: Ty) -> BuiltinFunctionDeclaration<'a> {
        BuiltinFunctionDeclaration {
            package: type_name("kotlin/enums"),
            name: "enumEntries",
            kind: FnKind::TopLevel,
            receiver: None,
            params,
            ret,
            context_count: 0,
            type_parameter_count: 1,
            vararg: None,
            is_suspend: false,
            is_operator: false,
            is_infix: false,
        }
    }

    #[test]
    fn enum_entries_shape_has_no_realization_without_its_public_identity() {
        let parameter = Ty::ty_param("T", Ty::obj("kotlin/Enum"));
        let result = Ty::obj_args("kotlin/enums/EnumEntries", &[parameter]);
        assert_eq!(
            function_realization(enum_entries(&[], result)),
            None,
            "the zero-argument shape is not the stdlib declaration"
        );
        let array = [Ty::array(parameter)];
        assert_eq!(
            function_realization(enum_entries(&array, result)),
            None,
            "the array overload is an ordinary function"
        );
        let mut same_shape = FunctionInfo::plain(
            FnKind::TopLevel,
            None,
            crate::libraries::LibraryCallable::library(
                type_name("kotlin/enums/EnumEntriesKt"),
                "enumEntries",
                Vec::new(),
                result,
                result,
                "",
            ),
        );
        same_shape.generic_sig = Some(crate::libraries::GenericSig {
            formals: vec!["T".to_string()],
            formal_bounds: vec![vec![Ty::obj("kotlin/Enum")]],
            receiver: None,
            params: Vec::new(),
            ret: result,
            return_policy: Default::default(),
        });
        same_shape.flags.reified = true;
        assert_eq!(
            normalized_function_realization(type_name("kotlin/enums"), "enumEntries", &same_shape),
            None,
            "normalized package overloads do not acquire the intrinsic from shape"
        );
    }

    #[test]
    fn type_of_requires_the_exact_reflection_declaration_shape() {
        let declaration = |package, result| BuiltinFunctionDeclaration {
            package,
            name: "typeOf",
            kind: FnKind::TopLevel,
            receiver: None,
            params: &[],
            ret: result,
            context_count: 0,
            type_parameter_count: 1,
            vararg: None,
            is_suspend: false,
            is_operator: false,
            is_infix: false,
        };

        assert_eq!(
            function_realization(declaration(
                type_name("kotlin/reflect"),
                Ty::obj("kotlin/reflect/KType")
            )),
            Some(CompilerIntrinsic::TypeOf)
        );
        assert_eq!(
            function_realization(declaration(
                type_name("example/reflection"),
                Ty::obj("kotlin/reflect/KType")
            )),
            None
        );
        assert_eq!(
            function_realization(declaration(type_name("kotlin/reflect"), Ty::String)),
            None
        );
    }

    fn with_index_declaration<'a>(
        package: &str,
        receiver: Ty,
        type_parameter_count: usize,
        ret: Ty,
    ) -> BuiltinFunctionDeclaration<'a> {
        BuiltinFunctionDeclaration {
            package: type_name(package),
            name: "withIndex",
            kind: FnKind::Extension,
            receiver: Some(receiver),
            params: &[],
            ret,
            context_count: 0,
            type_parameter_count,
            vararg: None,
            is_suspend: false,
            is_operator: false,
            is_infix: false,
        }
    }

    fn indexed(container: &str, element: Ty) -> Ty {
        Ty::obj_args(
            container,
            &[Ty::obj_args("kotlin/collections/IndexedValue", &[element])],
        )
    }

    #[test]
    fn with_index_matches_each_stdlib_overload_completely() {
        let t = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
        let iterable = "kotlin/collections/Iterable";
        let sequence = "kotlin/sequences/Sequence";
        for declaration in [
            with_index_declaration(
                "kotlin/collections",
                Ty::obj_args("kotlin/Array", &[Ty::out_projection(t)]),
                1,
                indexed(iterable, t),
            ),
            with_index_declaration(
                "kotlin/collections",
                Ty::array(Ty::Int),
                0,
                indexed(iterable, Ty::Int),
            ),
            with_index_declaration(
                "kotlin/collections",
                Ty::obj_args(iterable, &[t]),
                1,
                indexed(iterable, t),
            ),
            with_index_declaration(
                "kotlin/text",
                Ty::obj("kotlin/CharSequence"),
                0,
                indexed(iterable, Ty::Char),
            ),
            with_index_declaration(
                "kotlin/sequences",
                Ty::obj_args(sequence, &[t]),
                1,
                indexed(sequence, t),
            ),
        ] {
            assert_eq!(
                function_realization(declaration),
                Some(CompilerIntrinsic::WithIndex)
            );
        }
    }

    #[test]
    fn a_same_spelled_with_index_with_another_signature_has_no_realization() {
        let t = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
        let iterable = "kotlin/collections/Iterable";
        let sequence = "kotlin/sequences/Sequence";
        let generic_iterable = Ty::obj_args(iterable, &[t]);
        for (declaration, why) in [
            (
                with_index_declaration("example", generic_iterable, 1, indexed(iterable, t)),
                "a repository package declaring the same extension",
            ),
            (
                with_index_declaration(
                    "kotlin/collections",
                    Ty::obj_args("example/Bag", &[t]),
                    1,
                    indexed(iterable, t),
                ),
                "a repository classifier receiver",
            ),
            (
                with_index_declaration("kotlin/collections", generic_iterable, 1, generic_iterable),
                "a result that is not IndexedValue",
            ),
            (
                with_index_declaration(
                    "kotlin/collections",
                    generic_iterable,
                    1,
                    indexed(iterable, Ty::Int),
                ),
                "an IndexedValue that does not carry the receiver's element",
            ),
            (
                with_index_declaration(
                    "kotlin/collections",
                    generic_iterable,
                    1,
                    indexed(sequence, t),
                ),
                "a Sequence result over an Iterable receiver",
            ),
            (
                with_index_declaration(
                    "kotlin/collections",
                    generic_iterable,
                    2,
                    indexed(iterable, t),
                ),
                "an extra type parameter",
            ),
            (
                with_index_declaration(
                    "kotlin/collections",
                    Ty::obj_args("kotlin/Array", &[t]),
                    1,
                    indexed(iterable, t),
                ),
                "an invariant Array<T> receiver",
            ),
            (
                with_index_declaration(
                    "kotlin/collections",
                    Ty::obj_args(iterable, &[Ty::Int]),
                    0,
                    indexed(iterable, Ty::Int),
                ),
                "a receiver argument that is not the declaration's type parameter",
            ),
            (
                with_index_declaration(
                    "kotlin/collections",
                    Ty::array(Ty::Int),
                    0,
                    indexed(iterable, Ty::Long),
                ),
                "a primitive array yielding another element",
            ),
            (
                with_index_declaration(
                    "kotlin/text",
                    Ty::obj("kotlin/CharSequence"),
                    0,
                    indexed(sequence, Ty::Char),
                ),
                "a CharSequence overload returning a Sequence",
            ),
            (
                with_index_declaration(
                    "kotlin/sequences",
                    Ty::obj_args(sequence, &[t]),
                    1,
                    indexed(iterable, t),
                ),
                "a Sequence overload returning an Iterable",
            ),
            (
                with_index_declaration(
                    "kotlin/collections",
                    Ty::nullable(generic_iterable),
                    1,
                    indexed(iterable, t),
                ),
                "a nullable receiver",
            ),
        ] {
            assert_eq!(function_realization(declaration), None, "{why}");
        }
    }

    #[test]
    fn a_shape_equivalent_floating_range_has_no_realization() {
        let package = crate::types::wk::kotlin_ranges_package();
        let result = Ty::obj_args("kotlin/ranges/ClosedFloatingPointRange", &[Ty::Double]);
        let callable = crate::libraries::LibraryCallable::library(
            package,
            "rangeTo",
            vec![Ty::Double, Ty::Double],
            result,
            result,
            "",
        );
        let mut same_shape = FunctionInfo::plain(FnKind::Extension, Some(Ty::Double), callable);
        same_shape.flags.operator = true;
        assert_eq!(
            normalized_function_realization(package, "rangeTo", &same_shape),
            None
        );
    }

    #[test]
    fn comparable_range_to_requires_the_complete_generic_declaration_and_its_owner() {
        let comparable_of = |name: &str| {
            Ty::obj_args(
                "kotlin/Comparable",
                &[Ty::ty_param(name, Ty::nullable(Ty::obj("kotlin/Any")))],
            )
        };
        let element = Ty::ty_param("T", comparable_of("T"));
        let other = Ty::ty_param("U", comparable_of("U"));
        let closed_range = |argument| Ty::obj_args("kotlin/ranges/ClosedRange", &[argument]);
        let params = [element];
        let declaration = BuiltinFunctionDeclaration {
            package: crate::types::wk::kotlin_ranges_package(),
            name: "rangeTo",
            kind: FnKind::Extension,
            receiver: Some(element),
            params: &params,
            ret: closed_range(element),
            context_count: 0,
            type_parameter_count: 1,
            vararg: None,
            is_suspend: false,
            is_operator: true,
            is_infix: false,
        };
        assert_eq!(
            function_realization_with_stdlib_owner(declaration, true),
            Some(CompilerIntrinsic::ComparableRangeTo)
        );
        assert_eq!(
            function_realization_with_stdlib_owner(declaration, false),
            None,
            "an unauthenticated provider's identical declaration"
        );

        let other_params = [other];
        let concrete_params = [comparable_of("T")];
        let foreign_bound = Ty::ty_param("T", comparable_of("U"));
        let indirect_bound = Ty::ty_param("T", Ty::ty_param("U", comparable_of("T")));
        for (near, why) in [
            (
                BuiltinFunctionDeclaration {
                    params: &other_params,
                    ..declaration
                },
                "a parameter of another type parameter",
            ),
            (
                BuiltinFunctionDeclaration {
                    params: &concrete_params,
                    ..declaration
                },
                "a Comparable parameter that is not the receiver's type parameter",
            ),
            (
                BuiltinFunctionDeclaration {
                    receiver: Some(foreign_bound),
                    params: &[foreign_bound],
                    ret: closed_range(foreign_bound),
                    ..declaration
                },
                "a bound comparing with another type parameter",
            ),
            (
                BuiltinFunctionDeclaration {
                    receiver: Some(comparable_of("T")),
                    params: &concrete_params,
                    ret: closed_range(comparable_of("T")),
                    ..declaration
                },
                "a Comparable receiver that is not a type parameter",
            ),
            (
                BuiltinFunctionDeclaration {
                    ret: closed_range(other),
                    ..declaration
                },
                "a range of another element",
            ),
            (
                BuiltinFunctionDeclaration {
                    ret: Ty::nullable(closed_range(element)),
                    ..declaration
                },
                "a nullable range",
            ),
            (
                BuiltinFunctionDeclaration {
                    ret: Ty::ty_param("U", closed_range(element)),
                    ..declaration
                },
                "another type parameter bounded by the range",
            ),
            (
                BuiltinFunctionDeclaration {
                    ret: Ty::platform_nullable(closed_range(element)),
                    ..declaration
                },
                "a platform range",
            ),
            (
                BuiltinFunctionDeclaration {
                    receiver: Some(indirect_bound),
                    params: &[indirect_bound],
                    ret: closed_range(indirect_bound),
                    ..declaration
                },
                "an indirect bound through another type parameter",
            ),
            (
                BuiltinFunctionDeclaration {
                    type_parameter_count: 2,
                    ..declaration
                },
                "a second type parameter",
            ),
            (
                BuiltinFunctionDeclaration {
                    is_infix: true,
                    ..declaration
                },
                "an infix declaration",
            ),
            (
                BuiltinFunctionDeclaration {
                    is_suspend: true,
                    ..declaration
                },
                "a suspend declaration",
            ),
            (
                BuiltinFunctionDeclaration {
                    is_operator: false,
                    ..declaration
                },
                "a declaration that is not an operator",
            ),
            (
                BuiltinFunctionDeclaration {
                    package: type_name("fixture"),
                    ..declaration
                },
                "another package",
            ),
        ] {
            assert_eq!(
                function_realization_with_stdlib_owner(near, true),
                None,
                "{why}"
            );
        }
    }

    #[test]
    fn property_reference_delegate_roles_require_the_complete_extension_signature() {
        let any = Ty::obj_name(crate::types::wk::any());
        let value = Ty::ty_param("V", any);
        let receiver = Ty::obj_args("kotlin/reflect/KProperty0", &[value]);
        let property = Ty::obj_args(
            "kotlin/reflect/KProperty",
            &[Ty::star_projection(Ty::nullable(any))],
        );
        let params = [Ty::nullable(any), property];
        let declaration = BuiltinFunctionDeclaration {
            package: crate::types::wk::kotlin_package(),
            name: "getValue",
            kind: FnKind::Extension,
            receiver: Some(receiver),
            params: &params,
            ret: value,
            context_count: 0,
            type_parameter_count: 1,
            vararg: None,
            is_suspend: false,
            is_operator: true,
            is_infix: false,
        };
        assert_eq!(
            function_semantic_role(&declaration),
            Some(crate::types::SemanticCallRole::KotlinPropertyReferenceDelegateGet(0))
        );

        let lookalike = BuiltinFunctionDeclaration {
            package: crate::types::type_name("fixture"),
            ..declaration
        };
        assert_eq!(function_semantic_role(&lookalike), None);
    }

    #[test]
    fn char_sequence_iterator_role_requires_the_complete_stdlib_declaration() {
        let declaration = BuiltinFunctionDeclaration {
            package: crate::types::type_name("kotlin/text"),
            name: "iterator",
            kind: FnKind::Extension,
            receiver: Some(Ty::obj_name(crate::types::wk::char_sequence())),
            params: &[],
            ret: Ty::obj("kotlin/collections/CharIterator"),
            context_count: 0,
            type_parameter_count: 0,
            vararg: None,
            is_suspend: false,
            is_operator: true,
            is_infix: false,
        };
        assert_eq!(
            function_semantic_role(&declaration),
            Some(crate::types::SemanticCallRole::KotlinCharSequenceIterator)
        );
        assert_eq!(
            function_semantic_role(&BuiltinFunctionDeclaration {
                package: crate::types::type_name("fixture"),
                ..declaration
            }),
            None
        );
    }
}
