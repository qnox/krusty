//! Erased JVM result slots for checked generic calls.
//!
//! Common lowering marks the exact coercion between a selected declaration result and its
//! substituted semantic result. This JVM boundary realizes only those marked coercions: other
//! coercions over the same call already consume a realized result. Value-class lowering later
//! refines marked slots after its carrier inventory exists.

use crate::ir::{Callee, ExprId, IrExpr, IrFile, IrTypeOp};
use crate::types::{Ty, TyKind};

pub(super) fn terminal_call(exprs: &[IrExpr], mut expression: ExprId) -> Option<ExprId> {
    loop {
        match exprs.get(expression as usize)? {
            IrExpr::Call { .. } | IrExpr::MethodCall { .. } => return Some(expression),
            IrExpr::Block {
                value: Some(value), ..
            } => expression = *value,
            _ => return None,
        }
    }
}

/// Find the declaration producer through operations that retain its value. A null assertion
/// checks the same slot; an expanded inline region has replaced the declaration producer.
pub(super) fn declaration_result_call(ir: &IrFile, mut expression: ExprId) -> Option<ExprId> {
    loop {
        if ir.inline_regions.contains(&expression) {
            return None;
        }
        match ir.exprs.get(expression as usize)? {
            IrExpr::Call { .. } | IrExpr::MethodCall { .. } => return Some(expression),
            IrExpr::Block {
                value: Some(value), ..
            } => expression = *value,
            IrExpr::NotNullAssert { operand, .. } => expression = *operand,
            _ => return None,
        }
    }
}

/// Whether emission evaluates `call` as that call. A statically inlined call and a compiled inline
/// module call keep their expression ID while emission replaces them with the inlined body, which
/// owns its own result value.
pub(super) fn emits_as_call(ir: &IrFile, call: ExprId) -> bool {
    !matches!(
        ir.exprs.get(call as usize),
        Some(IrExpr::Call { callee, .. })
            if matches!(&**callee, Callee::Static { inline, .. } if inline.can_inline())
    ) && ir.compiled_inline_call(call).is_none()
}

fn realized_call_result(ir: &IrFile, call: ExprId) -> Option<Ty> {
    let expression = ir.exprs.get(call as usize)?;
    let IrExpr::Call { callee, .. } = expression else {
        let IrExpr::MethodCall { class, index, .. } = expression else {
            return None;
        };
        let function = *ir
            .classes
            .get(*class as usize)?
            .methods
            .get(*index as usize)?;
        return ir
            .functions
            .get(function as usize)
            .map(|function| function.ret);
    };
    if let Some(function) = callee.source_function() {
        return ir
            .functions
            .get(function as usize)
            .map(|function| function.ret);
    }
    match &**callee {
        Callee::Intrinsic { ret, .. }
        | Callee::CrossFile { ret, .. }
        | Callee::Module { ret, .. }
        | Callee::ModuleWithDefaults { ret, .. }
        | Callee::External { ret, .. }
        | Callee::Super { ret, .. } => Some(*ret),
        Callee::Virtual {
            params: Some((_, ret)),
            ..
        } => Some(*ret),
        _ => None,
    }
}

/// The declaration's selected JVM erasure, retaining whether its erased reference slot admits
/// null. Later boundary passes consume that representation fact even though nullability does not
/// change the descriptor.
pub(super) fn erased_result_slot(ir: &IrFile, call: ExprId, declared: Ty) -> Option<Ty> {
    let erased = realized_call_result(ir, call)?;
    let nullable = declared.is_nullable()
        || matches!(declared.non_null().kind(), TyKind::TyParam(_, bound) if bound.is_nullable());
    Some(if nullable {
        Ty::nullable(erased.non_null())
    } else {
        erased
    })
}

/// The reference slot a checked call leaves before the coercion to its substituted result. A call
/// whose declaration the IR carries leaves that declaration's selected erasure; a realized
/// dependency call leaves the slot its external realization recorded.
pub(super) fn checked_result_slot(ir: &IrFile, call: ExprId) -> Option<Ty> {
    match realized_call_result(ir, call) {
        Some(_) => erased_result_slot(ir, call, *ir.call_declared_ret.get(&call)?),
        None => ir.physical_types.get(&call).copied(),
    }
}

pub(super) fn realize_call_result_boundaries(ir: &mut IrFile) {
    let mut coercions = ir
        .declaration_result_coercions
        .iter()
        .copied()
        .collect::<Vec<_>>();
    coercions.sort_unstable();
    let boundaries = coercions
        .into_iter()
        .filter_map(|coercion| match ir.exprs.get(coercion as usize)? {
            IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg,
                type_operand,
            } => Some((coercion, *arg, *type_operand)),
            _ => None,
        })
        .filter_map(|(coercion, expression, target)| {
            let call = declaration_result_call(ir, expression)?;
            let declared = *ir.call_declared_ret.get(&call)?;
            let physical = erased_result_slot(ir, call, declared)?;
            let physical_jvm = crate::jvm::physical_type::ir_ty_to_jvm(&physical);
            let target_jvm = crate::jvm::physical_type::ir_ty_to_jvm(&target);
            (physical_jvm != target_jvm).then_some((coercion, expression, target, physical))
        })
        .collect::<Vec<_>>();

    for &(_, expression, _, physical) in &boundaries {
        ir.physical_types.insert(expression, physical);
    }
    fold_nullable_widenings(ir, &boundaries);
    fold_asserted_number_unbox(ir);
    test_erased_result_directly(ir);
    check_enhanced_result_before_unbox(ir);
}

/// `m["k"] as? String`, `m["k"] is String` and `m["k"] as String`, where the call leaves an erased
/// reference slot, test or cast that slot directly. kotlinc evaluates a type operator's operand,
/// and the temporary of a safe cast, as a plain reference, so the substituted result's own
/// `checkcast` (`CharSequence` for a `Map<String, CharSequence>`) is never written there. A named
/// binding (`when (val v = m["k"])`) still converts to its declared type, and so does the operand
/// of a cast whose substituted result is non-null or an array.
fn test_erased_result_directly(ir: &mut IrFile) {
    let folds = (0..ir.exprs.len())
        .filter_map(|index| {
            let (coercion, cast) = match &ir.exprs[index] {
                IrExpr::TypeOp {
                    op: IrTypeOp::InstanceOf | IrTypeOp::NotInstanceOf | IrTypeOp::SafeCast,
                    arg,
                    type_operand,
                } if type_operand.is_reference() => (*arg, false),
                IrExpr::TypeOp {
                    op: IrTypeOp::Cast | IrTypeOp::CastNonNull,
                    arg,
                    type_operand,
                } if type_operand.is_reference() => (*arg, true),
                IrExpr::Variable {
                    init: Some(init), ..
                } if ir.safe_cast_temporaries.contains(&(index as ExprId)) => (*init, false),
                _ => return None,
            };
            let (produced, slot) = erased_reference_operand(ir, coercion)?;
            // A cast still reads its operand's semantic type: whether it needs a null check, and
            // whether an array cast is only a covariant view. Only a nullable, non-array result
            // leaves both decisions unchanged when the cast reads the erased slot.
            if cast {
                let IrExpr::TypeOp {
                    type_operand: substituted,
                    ..
                } = &ir.exprs[coercion as usize]
                else {
                    return None;
                };
                if !substituted.is_nullable() || substituted.non_null().is_reference_array() {
                    return None;
                }
            }
            Some((index, produced, coercion, slot))
        })
        .collect::<Vec<_>>();
    for (index, produced, coercion, slot) in folds {
        match &mut ir.exprs[index] {
            IrExpr::TypeOp { arg, .. } => *arg = produced,
            IrExpr::Variable { init, ty, .. } => {
                *init = Some(produced);
                *ty = slot;
            }
            _ => continue,
        }
        ir.declaration_result_coercions.remove(&coercion);
        ir.jvm_external_result_coercions.remove(&coercion);
    }
}

/// The call result a reference coercion converts and the erased reference slot that call leaves.
/// Only a coercion that owns a call boundary qualifies: one common lowering marked between a
/// declaration's result and its substitution, or the one dependency realization placed over a
/// provider's physical result. Its operand must still be the retained call itself, not an expanded
/// inline region or another transformed value; any other coercion converts an already realized
/// semantic result and keeps its conversion.
fn erased_reference_operand(ir: &IrFile, coercion: ExprId) -> Option<(ExprId, Ty)> {
    if !ir.declaration_result_coercions.contains(&coercion)
        && !ir.jvm_external_result_coercions.contains(&coercion)
    {
        return None;
    }
    let IrExpr::TypeOp {
        op: IrTypeOp::ImplicitCoercion,
        arg: produced,
        type_operand: substituted,
    } = ir.exprs.get(coercion as usize)?
    else {
        return None;
    };
    if declaration_result_call(ir, *produced) != Some(*produced) || !emits_as_call(ir, *produced) {
        return None;
    }
    let slot = *ir.physical_types.get(produced)?;
    let physical = crate::jvm::physical_type::ir_ty_to_jvm(&slot);
    (substituted.is_reference() && physical.is_reference()).then_some((*produced, slot))
}

/// An erased reference slot read as a primitive and then widened to that primitive's nullable type
/// is one reference conversion. Retarget the widening to the slot so a null is not unboxed and
/// immediately reboxed.
fn fold_nullable_widenings(ir: &mut IrFile, boundaries: &[(ExprId, ExprId, Ty, Ty)]) {
    let carriers = boundaries
        .iter()
        .filter(|(_, _, target, physical)| !target.is_reference() && physical.is_reference())
        .map(|&(coercion, expression, target, _)| (coercion, (expression, target)))
        .collect::<crate::fx_hash::HashMap<_, _>>();
    if carriers.is_empty() {
        return;
    }
    for expression in &mut ir.exprs {
        if let IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg,
            type_operand,
        } = expression
        {
            if let Some(&(slot, primitive)) = carriers.get(arg) {
                if type_operand.nullable_primitive() == Some(primitive) {
                    *arg = slot;
                }
            }
        }
    }
}

/// `val x: Int = f()!!`, where `f` returns an erased type parameter, null-checks the reference the
/// call produced and unboxes a number through `java/lang/Number`. The nullable primitive
/// substitution (`Int?`) is not a value that was stored or explicitly cast, so its wrapper
/// `checkcast` would reject a `Long` that `Number.intValue` accepts. `Boolean` and `Char` use the
/// same erased reference, then their own wrappers. A physical result that is already the wrapper
/// keeps that cast.
fn fold_asserted_number_unbox(ir: &mut IrFile) {
    let folds = (0..ir.exprs.len())
        .filter_map(|index| {
            let IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: assertion,
                type_operand: primitive,
            } = &ir.exprs[index]
            else {
                return None;
            };
            let (assertion, primitive) = (*assertion, *primitive);
            if !primitive.is_numeric() && primitive != Ty::Boolean && primitive != Ty::Char {
                return None;
            }
            let IrExpr::NotNullAssert {
                operand: inner,
                check: crate::ir::NullCheck::Source,
            } = &ir.exprs[assertion as usize]
            else {
                return None;
            };
            let inner = *inner;
            if !ir.declaration_result_coercions.contains(&inner) {
                return None;
            }
            let IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: produced,
                type_operand: nullable,
            } = &ir.exprs[inner as usize]
            else {
                return None;
            };
            if nullable.nullable_primitive() != Some(primitive) {
                return None;
            }
            let physical =
                crate::jvm::physical_type::ir_ty_to_jvm(ir.physical_types.get(produced)?);
            let wrapper = crate::jvm::physical_type::ir_ty_to_jvm(&primitive.boxed_ref()?);
            if !physical.is_reference() || physical == wrapper {
                return None;
            }
            Some((assertion, *produced, inner))
        })
        .collect::<Vec<_>>();
    for (assertion, produced, inner) in folds {
        let IrExpr::NotNullAssert { operand, .. } = &mut ir.exprs[assertion as usize] else {
            continue;
        };
        *operand = produced;
        ir.declaration_result_coercions.remove(&inner);
    }
}

/// A Java result enhanced to a not-null primitive (`ArrayList<Int>.get`, overriding
/// `List<E>.get(): E`) is checked before it is unboxed. kotlinc's JVM type mapper keeps a type
/// carrying `EnhancedNullability` at its wrapper, so the check sees the reference the call produced
/// and the unbox through `java/lang/Number` follows it. Common lowering asserts the substituted
/// primitive; this boundary moves the assertion under the coercion that unboxes the erased slot
/// the call left (a scalar itself can never be null, so an assertion over it checks nothing).
fn check_enhanced_result_before_unbox(ir: &mut IrFile) {
    let moves = (0..ir.exprs.len())
        .filter_map(|index| {
            let IrExpr::NotNullAssert {
                operand: coercion,
                check: crate::ir::NullCheck::Named(_),
            } = &ir.exprs[index]
            else {
                return None;
            };
            let coercion = *coercion;
            let IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: produced,
                type_operand: primitive,
            } = &ir.exprs[coercion as usize]
            else {
                return None;
            };
            let physical =
                crate::jvm::physical_type::ir_ty_to_jvm(ir.physical_types.get(produced)?);
            (primitive.is_jvm_scalar() && physical.is_reference()).then_some((
                index as ExprId,
                coercion,
                *produced,
            ))
        })
        .collect::<Vec<_>>();
    for (assertion, coercion, produced) in moves {
        // The assertion's slot becomes the unboxing coercion, which now reads the checked value.
        ir.exprs.swap(assertion as usize, coercion as usize);
        if let IrExpr::TypeOp { arg, .. } = &mut ir.exprs[assertion as usize] {
            *arg = coercion;
        }
        if let IrExpr::NotNullAssert { operand, .. } = &mut ir.exprs[coercion as usize] {
            *operand = produced;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{IrFunction, NullCheck};

    fn producer() -> (IrFile, ExprId) {
        let mut ir = IrFile::default();
        let function = ir.add_fun(IrFunction {
            name: "produced".to_string(),
            params: Vec::new(),
            ret: Ty::obj("java/lang/Object"),
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        let call = ir.add_expr(IrExpr::Call {
            callee: Box::new(Callee::Local(function)),
            dispatch_receiver: None,
            args: Vec::new(),
        });
        ir.call_declared_ret
            .insert(call, Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any"))));
        (ir, call)
    }

    #[test]
    fn only_the_marked_checked_result_retains_its_declaration_slot() {
        let (mut ir, call) = producer();
        let assertions = [0, 1].map(|_| {
            ir.add_expr(IrExpr::NotNullAssert {
                operand: call,
                check: NullCheck::Source,
            })
        });
        for (index, assertion) in assertions.into_iter().enumerate() {
            let coercion = ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: assertion,
                type_operand: Ty::String,
            });
            if index == 0 {
                ir.declaration_result_coercions.insert(coercion);
            }
        }
        realize_call_result_boundaries(&mut ir);
        assert_eq!(
            ir.physical_types.get(&assertions[0]),
            Some(&Ty::nullable(Ty::obj("java/lang/Object")))
        );
        assert!(!ir.physical_types.contains_key(&assertions[1]));
        for assertion in assertions {
            assert!(
                matches!(ir.expr(assertion), IrExpr::NotNullAssert { operand, .. } if *operand == call)
            );
        }
    }

    #[test]
    fn a_checked_inline_region_keeps_its_replacement_producer_boundary() {
        let (mut ir, call) = producer();
        let region = ir.add_expr(IrExpr::Block {
            stmts: Vec::new(),
            value: Some(call),
        });
        ir.inline_regions.insert(region);
        let assertion = ir.add_expr(IrExpr::NotNullAssert {
            operand: region,
            check: NullCheck::Source,
        });
        let coercion = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: assertion,
            type_operand: Ty::String,
        });
        ir.declaration_result_coercions.insert(coercion);
        realize_call_result_boundaries(&mut ir);
        assert!(!ir.physical_types.contains_key(&assertion));
        assert!(
            matches!(ir.expr(assertion), IrExpr::NotNullAssert { operand, .. } if *operand == region)
        );
    }

    /// `produced() is String` over a coercion to `CharSequence`, marked or not.
    fn instance_test(
        marked: bool,
        operand: impl FnOnce(&mut IrFile, ExprId) -> ExprId,
    ) -> (IrFile, ExprId, ExprId, ExprId) {
        let (mut ir, call) = producer();
        let produced = operand(&mut ir, call);
        let coercion = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: produced,
            type_operand: Ty::obj("kotlin/CharSequence"),
        });
        if marked {
            ir.declaration_result_coercions.insert(coercion);
        }
        let test = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::InstanceOf,
            arg: coercion,
            type_operand: Ty::String,
        });
        realize_call_result_boundaries(&mut ir);
        (ir, call, coercion, test)
    }

    fn tested_operand(ir: &IrFile, test: ExprId) -> ExprId {
        let IrExpr::TypeOp { arg, .. } = ir.expr(test) else {
            panic!("the instance test survives");
        };
        *arg
    }

    #[test]
    fn a_marked_checked_result_is_tested_in_its_erased_slot() {
        let (ir, call, _, test) = instance_test(true, |_, call| call);
        assert_eq!(tested_operand(&ir, test), call);
    }

    #[test]
    fn an_unmarked_coercion_keeps_its_semantic_conversion() {
        let (mut ir, call) = producer();
        ir.physical_types.insert(call, Ty::obj("kotlin/Any"));
        let coercion = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: call,
            type_operand: Ty::obj("kotlin/CharSequence"),
        });
        let test = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::InstanceOf,
            arg: coercion,
            type_operand: Ty::String,
        });
        realize_call_result_boundaries(&mut ir);
        assert_eq!(tested_operand(&ir, test), coercion);
    }

    #[test]
    fn a_replaced_inline_result_keeps_its_semantic_conversion() {
        let (ir, _, coercion, test) = instance_test(true, |ir, call| {
            let region = ir.add_expr(IrExpr::Block {
                stmts: Vec::new(),
                value: Some(call),
            });
            ir.inline_regions.insert(region);
            ir.physical_types.insert(region, Ty::obj("kotlin/Any"));
            region
        });
        assert_eq!(tested_operand(&ir, test), coercion);
    }

    #[test]
    fn a_transformed_result_keeps_its_semantic_conversion() {
        let (ir, _, coercion, test) = instance_test(true, |ir, call| {
            let asserted = ir.add_expr(IrExpr::NotNullAssert {
                operand: call,
                check: NullCheck::Source,
            });
            ir.physical_types.insert(asserted, Ty::obj("kotlin/Any"));
            asserted
        });
        assert_eq!(tested_operand(&ir, test), coercion);
    }

    #[test]
    fn a_compiled_inline_call_keeps_its_semantic_conversion() {
        let (ir, _, coercion, test) = instance_test(true, |ir, call| {
            let IrExpr::Call { callee, .. } = ir.expr(call) else {
                panic!("the producer is a call");
            };
            let Callee::Local(function) = **callee else {
                panic!("the producer calls a local declaration");
            };
            ir.set_same_module_inlining(crate::ir::SameModuleInlining::CompiledBodies);
            ir.record_compiled_inline_call(call, function);
            call
        });
        assert_eq!(tested_operand(&ir, test), coercion);
    }
}
