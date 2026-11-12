//! `for` loops over an array in a serialized body. A KLIB stores every `for` loop as kotlinc's
//! frontend desugars it, an iterator-protocol loop; kotlinc's `ForLoopsLowering` then loops over
//! an array by index on every backend. Body lowering and the dependency prewalk recognize the
//! loop here, once, so the protocol calls an index loop does not make never become declaration
//! edges.

use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrArguments, KlibIrExpr, KlibIrExprId, KlibIrExprKind, KlibIrLoop,
    KlibIrStatement, KlibIrType, KlibIrTypeId, KlibIrVariableId,
};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbolKind};

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
    /// The protocol calls an index loop does not make: `iterator()`, `hasNext()` and `next()`.
    calls: [KlibIrExprId; 3],
}

/// The `for` loop over an array whose `FOR_LOOP` block holds `statements`, if it is one.
pub(super) fn array_for_loop<'a>(
    arena: &'a KlibIrArena,
    statements: &'a [KlibIrStatement],
) -> Option<ArrayForLoop<'a>> {
    let [KlibIrStatement::Variable(iterator), KlibIrStatement::Expression(inner)] = statements
    else {
        return None;
    };
    let iterator_variable = arena.variable(*iterator);
    if iterator_variable.base.origin != "FOR_LOOP_ITERATOR" {
        return None;
    }
    let iterator_call = iterator_variable.initializer?;
    let array = protocol_receiver(arena, iterator_call, "FOR_LOOP_ITERATOR")?;
    if !is_array_type(arena, arena.expr(array).ty?) {
        return None;
    }
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
    Some(ArrayForLoop {
        array,
        inner: loop_,
        body,
        binding: *binding,
        calls: [iterator_call, has_next, next],
    })
}

/// The protocol calls of the array `for` loop `expression` is, which its index loop does not
/// make. Every other expression has none.
pub(crate) fn folded_array_loop_calls(
    arena: &KlibIrArena,
    expression: &KlibIrExpr,
) -> Vec<KlibIrExprId> {
    match &expression.kind {
        KlibIrExprKind::Block {
            statements,
            origin: Some(origin),
        } if origin == "FOR_LOOP" => array_for_loop(arena, statements)
            .map(|array_loop| array_loop.calls.to_vec())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
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

/// Whether `ty` is an array class type (`Array<T>`, `IntArray`, ...), by its classifier.
fn is_array_type(arena: &KlibIrArena, ty: KlibIrTypeId) -> bool {
    let KlibIrType::Simple { classifier, .. } = arena.ty(ty) else {
        return false;
    };
    if classifier.kind != KlibIrSymbolKind::Class {
        return false;
    }
    let KlibIrSignature::Public(signature) = &classifier.signature else {
        return false;
    };
    super::klib_types::classifier_name(signature)
        .is_ok_and(|name| crate::types::Ty::obj_name(name).is_array())
}
