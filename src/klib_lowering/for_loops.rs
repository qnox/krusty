//! `for` loops in a serialized body that kotlinc's `ForLoopsLowering` turns into counted loops. A
//! KLIB stores every `for` loop as kotlinc's frontend desugars it, an iterator-protocol loop;
//! kotlinc then loops over an array by index, and over a range literal by a counter, on every
//! backend. Body lowering and the dependency prewalk recognize the loop here, once, so the calls
//! a counted loop does not make never become declaration edges.

use crate::metadata::id_signature::KlibAccessorIdSignature;
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrArguments, KlibIrExpr, KlibIrExprId, KlibIrExprKind, KlibIrLoop,
    KlibIrMemberAccess, KlibIrStatement, KlibIrType, KlibIrTypeId, KlibIrVariableId,
};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbolKind};

/// The iterator protocol of one serialized `for` loop.
struct ProtocolLoop<'a> {
    /// The iterable: the dispatch receiver of the loop's `iterator()` call.
    iterable: KlibIrExprId,
    inner: &'a KlibIrLoop,
    body: &'a [KlibIrStatement],
    binding: KlibIrVariableId,
    /// The protocol calls: `iterator()`, `hasNext()` and `next()`.
    calls: [KlibIrExprId; 3],
}

/// A serialized `for` loop over an array.
pub(super) struct ArrayForLoop<'a> {
    /// The array the loop iterates: the dispatch receiver of its `iterator()` call.
    pub(super) array: KlibIrExprId,
    /// The protocol's inner `while` loop, whose identity its jumps name.
    pub(super) inner: &'a KlibIrLoop,
    /// The statements of the inner loop's body; the first declares the loop's element.
    pub(super) body: &'a [KlibIrStatement],
    /// The variable the element initializes: the loop variable, or a destructured loop's
    /// element temporary.
    pub(super) binding: KlibIrVariableId,
}

/// A serialized `for` loop over the progression a call builds (`start until end`,
/// `start..end`, `start downTo end`). Whether the call is a range literal kotlinc counts through
/// is the callee's declaration, which body lowering authenticates; this is only its shape.
pub(super) struct RangeForLoop<'a> {
    /// The call building the progression.
    pub(super) range: &'a KlibIrMemberAccess,
    /// Its receiver and its one argument: the literal's bounds.
    pub(super) start: KlibIrExprId,
    pub(super) end: KlibIrExprId,
    pub(super) inner: &'a KlibIrLoop,
    pub(super) body: &'a [KlibIrStatement],
    pub(super) binding: KlibIrVariableId,
}

/// A serialized `for` loop over the progression an extension property getter builds
/// (`array.indices`). Whether the getter is kotlinc's `indices` is its declaration, which body
/// lowering authenticates; this is only its shape.
pub(super) struct IndicesForLoop<'a> {
    /// The getter whose result the loop iterates.
    pub(super) getter: &'a KlibAccessorIdSignature,
    /// The extension receiver of the getter: the array or collection whose indices are counted.
    pub(super) receiver: KlibIrExprId,
    /// The `reversed()` call applied to the indices, when the loop counts them downwards. Whether
    /// the callee is kotlinc's progression `reversed` is its declaration, which body lowering
    /// authenticates; this is only its shape.
    pub(super) reversed: Option<&'a KlibIrMemberAccess>,
    pub(super) inner: &'a KlibIrLoop,
    pub(super) body: &'a [KlibIrStatement],
    pub(super) binding: KlibIrVariableId,
}

/// The `for` loop over the indices of a receiver whose `FOR_LOOP` block holds `statements`.
pub(super) fn indices_for_loop<'a>(
    arena: &'a KlibIrArena,
    statements: &'a [KlibIrStatement],
) -> Option<IndicesForLoop<'a>> {
    let protocol = protocol_loop(arena, statements)?;
    let (reversed, indices) = indices_iterable(arena, protocol.iterable);
    let (getter, receiver) = indices_call(arena, indices)?;
    Some(IndicesForLoop {
        getter,
        receiver,
        reversed,
        inner: protocol.inner,
        body: protocol.body,
        binding: protocol.binding,
    })
}

/// The `for` loop over an array whose `FOR_LOOP` block holds `statements`, if it is one.
pub(super) fn array_for_loop<'a>(
    arena: &'a KlibIrArena,
    statements: &'a [KlibIrStatement],
) -> Option<ArrayForLoop<'a>> {
    let protocol = protocol_loop(arena, statements)?;
    is_array_type(arena, arena.expr(protocol.iterable).ty?).then_some(ArrayForLoop {
        array: protocol.iterable,
        inner: protocol.inner,
        body: protocol.body,
        binding: protocol.binding,
    })
}

/// The `for` loop over a progression a call builds, whose `FOR_LOOP` block holds `statements`.
pub(super) fn range_for_loop<'a>(
    arena: &'a KlibIrArena,
    statements: &'a [KlibIrStatement],
) -> Option<RangeForLoop<'a>> {
    let protocol = protocol_loop(arena, statements)?;
    let (range, start, end) = range_call(arena, protocol.iterable)?;
    Some(RangeForLoop {
        range,
        start,
        end,
        inner: protocol.inner,
        body: protocol.body,
        binding: protocol.binding,
    })
}

/// The calls of the counted `for` loop `expression` is, which its counted loop does not make: an
/// array loop's protocol calls, and a range loop's protocol calls and the call building its range.
/// Every other expression has none.
pub(crate) fn folded_loop_calls(arena: &KlibIrArena, expression: &KlibIrExpr) -> Vec<KlibIrExprId> {
    let KlibIrExprKind::Block {
        statements,
        origin: Some(origin),
    } = &expression.kind
    else {
        return Vec::new();
    };
    if origin != "FOR_LOOP" {
        return Vec::new();
    }
    let Some(protocol) = protocol_loop(arena, statements) else {
        return Vec::new();
    };
    let mut calls = Vec::new();
    if arena
        .expr(protocol.iterable)
        .ty
        .is_some_and(|ty| is_array_type(arena, ty))
    {
        calls.extend(protocol.calls);
    } else if range_call(arena, protocol.iterable).is_some() {
        calls.extend(protocol.calls);
        calls.push(protocol.iterable);
    } else if indices_call(arena, indices_iterable(arena, protocol.iterable).1).is_some() {
        // The getter stays an edge: its declaration is what authenticates the counted loop.
        calls.extend(protocol.calls);
    }
    calls
}

/// The iterator protocol a `FOR_LOOP` block holding `statements` serializes: the iterator local,
/// then the inner `while` on its `hasNext()` whose body opens with the element from its `next()`.
fn protocol_loop<'a>(
    arena: &'a KlibIrArena,
    statements: &'a [KlibIrStatement],
) -> Option<ProtocolLoop<'a>> {
    let [KlibIrStatement::Variable(iterator), KlibIrStatement::Expression(inner)] = statements
    else {
        return None;
    };
    let iterator_variable = arena.variable(*iterator);
    if iterator_variable.base.origin != "FOR_LOOP_ITERATOR" {
        return None;
    }
    let iterator_call = iterator_variable.initializer?;
    let iterable = protocol_receiver(arena, iterator_call, "FOR_LOOP_ITERATOR")?;
    let KlibIrExprKind::While(loop_) = &arena.expr(*inner).kind else {
        return None;
    };
    if loop_.origin.as_deref() != Some("FOR_LOOP_INNER_WHILE") {
        return None;
    }
    let has_next = loop_.condition;
    reads_variable(
        arena,
        protocol_receiver(arena, has_next, "FOR_LOOP_HAS_NEXT")?,
        *iterator,
    )
    .then_some(())?;
    let KlibIrExprKind::Block {
        statements: body,
        origin,
    } = &arena.expr(loop_.body?).kind
    else {
        return None;
    };
    if !matches!(origin.as_deref(), None | Some("FOR_LOOP_INNER_WHILE")) {
        return None;
    }
    let Some(KlibIrStatement::Variable(binding)) = body.first() else {
        return None;
    };
    let binding_variable = arena.variable(*binding);
    if !matches!(
        binding_variable.base.origin.as_str(),
        "FOR_LOOP_VARIABLE" | "IR_TEMPORARY_VARIABLE"
    ) {
        return None;
    }
    let next = binding_variable.initializer?;
    reads_variable(
        arena,
        protocol_receiver(arena, next, "FOR_LOOP_NEXT")?,
        *iterator,
    )
    .then_some(())?;
    Some(ProtocolLoop {
        iterable,
        inner: loop_,
        body,
        binding: *binding,
        calls: [iterator_call, has_next, next],
    })
}

/// `iterable` as a call of a public function that builds a `kotlin.ranges` progression from a
/// receiver and one argument: the call, its receiver and its argument.
fn range_call(
    arena: &KlibIrArena,
    iterable: KlibIrExprId,
) -> Option<(&KlibIrMemberAccess, KlibIrExprId, KlibIrExprId)> {
    let expression = arena.expr(iterable);
    let KlibIrExprKind::Call {
        access,
        super_qualifier: None,
    } = &expression.kind
    else {
        return None;
    };
    if access.symbol.kind != KlibIrSymbolKind::Function
        || !matches!(access.symbol.signature, KlibIrSignature::Public(_))
        || !access.type_arguments.is_empty()
        || !is_progression_type(arena, expression.ty?)
    {
        return None;
    }
    let (start, end) = match &access.arguments {
        KlibIrArguments::Flat(arguments) => match arguments.as_slice() {
            &[Some(start), Some(end)] => (start, end),
            _ => return None,
        },
        KlibIrArguments::Split {
            dispatch_receiver,
            extension_receiver,
            values,
        } => match (dispatch_receiver, extension_receiver, values.as_slice()) {
            (Some(start), None, &[Some(end)]) | (None, Some(start), &[Some(end)]) => (*start, end),
            _ => return None,
        },
    };
    Some((access, start, end))
}

/// `iterable` as the indices read a loop counts, and the `reversed()` call applied to it, if any.
fn indices_iterable(
    arena: &KlibIrArena,
    iterable: KlibIrExprId,
) -> (Option<&KlibIrMemberAccess>, KlibIrExprId) {
    match reversed_call(arena, iterable) {
        Some((reversed, receiver)) => (Some(reversed), receiver),
        None => (None, iterable),
    }
}

/// `iterable` as a call of a public function on a `kotlin.ranges` progression, with no other
/// argument, that yields a progression: the call and its receiver.
fn reversed_call(
    arena: &KlibIrArena,
    iterable: KlibIrExprId,
) -> Option<(&KlibIrMemberAccess, KlibIrExprId)> {
    let expression = arena.expr(iterable);
    let KlibIrExprKind::Call {
        access,
        super_qualifier: None,
    } = &expression.kind
    else {
        return None;
    };
    if access.symbol.kind != KlibIrSymbolKind::Function
        || !matches!(access.symbol.signature, KlibIrSignature::Public(_))
        || !access.type_arguments.is_empty()
        || !is_progression_type(arena, expression.ty?)
    {
        return None;
    }
    let receiver = match &access.arguments {
        KlibIrArguments::Flat(arguments) => match arguments.as_slice() {
            &[Some(receiver)] => receiver,
            _ => return None,
        },
        KlibIrArguments::Split {
            dispatch_receiver,
            extension_receiver,
            values,
        } if values.is_empty() => match (dispatch_receiver, extension_receiver) {
            (Some(receiver), None) | (None, Some(receiver)) => *receiver,
            _ => return None,
        },
        KlibIrArguments::Split { .. } => return None,
    };
    Some((access, receiver))
}

/// `iterable` as a read of a public extension property whose type is a `kotlin.ranges`
/// progression: the getter and its extension receiver.
fn indices_call(
    arena: &KlibIrArena,
    iterable: KlibIrExprId,
) -> Option<(&KlibAccessorIdSignature, KlibIrExprId)> {
    let expression = arena.expr(iterable);
    let KlibIrExprKind::Call {
        access,
        super_qualifier: None,
    } = &expression.kind
    else {
        return None;
    };
    let KlibIrSignature::Accessor(getter) = &access.symbol.signature else {
        return None;
    };
    if !is_progression_type(arena, expression.ty?) {
        return None;
    }
    let receiver = match &access.arguments {
        KlibIrArguments::Split {
            dispatch_receiver: None,
            extension_receiver: Some(receiver),
            values,
        } if values.is_empty() => *receiver,
        KlibIrArguments::Flat(arguments) => match arguments.as_slice() {
            &[Some(receiver)] => receiver,
            _ => return None,
        },
        _ => return None,
    };
    Some((getter, receiver))
}

/// The dispatch receiver of `call`, a member call without other arguments that the loop
/// protocol makes with `origin`.
fn protocol_receiver(
    arena: &KlibIrArena,
    call: KlibIrExprId,
    origin: &str,
) -> Option<KlibIrExprId> {
    let KlibIrExprKind::Call {
        access,
        super_qualifier: None,
    } = &arena.expr(call).kind
    else {
        return None;
    };
    if access.origin.as_deref() != Some(origin) {
        return None;
    }
    match &access.arguments {
        KlibIrArguments::Split {
            dispatch_receiver: Some(receiver),
            extension_receiver: None,
            values,
        } if values.is_empty() => Some(*receiver),
        // Kotlin 2.4 lists the dispatch receiver as the only argument of a member without
        // parameters.
        KlibIrArguments::Flat(arguments) => match arguments.as_slice() {
            [Some(receiver)] => Some(*receiver),
            _ => None,
        },
        _ => None,
    }
}

fn reads_variable(
    arena: &KlibIrArena,
    expression: KlibIrExprId,
    variable: KlibIrVariableId,
) -> bool {
    matches!(
        &arena.expr(expression).kind,
        KlibIrExprKind::GetValue { symbol, .. } if symbol == &arena.variable(variable).base.symbol
    )
}

/// The class `ty` applies, by its classifier, when that classifier is a public class.
fn class_name(arena: &KlibIrArena, ty: KlibIrTypeId) -> Option<crate::types::TypeName> {
    let KlibIrType::Simple { classifier, .. } = arena.ty(ty) else {
        return None;
    };
    if classifier.kind != KlibIrSymbolKind::Class {
        return None;
    }
    let KlibIrSignature::Public(signature) = &classifier.signature else {
        return None;
    };
    super::klib_types::classifier_name(signature).ok()
}

/// Whether `ty` is an array class type (`Array<T>`, `IntArray`, ...), by its classifier.
fn is_array_type(arena: &KlibIrArena, ty: KlibIrTypeId) -> bool {
    class_name(arena, ty).is_some_and(|name| crate::types::Ty::obj_name(name).is_array())
}

/// Whether `ty` is a `kotlin.ranges` progression class type (`IntRange`, `LongProgression`, ...),
/// by its classifier.
fn is_progression_type(arena: &KlibIrArena, ty: KlibIrTypeId) -> bool {
    class_name(arena, ty).is_some_and(|name| crate::types::wk::progression_class(name).is_some())
}
