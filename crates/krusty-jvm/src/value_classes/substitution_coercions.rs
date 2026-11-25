//! A checked coercion from a generic slot to its substituted value class (`c.a` read as `R<Int>`
//! from `R<R<Int>>`) unboxes the box that slot holds. When the coerced value is consumed by a
//! reference slot that boxes it again (`fun read(c: R<R<Int>>): Any? = c.a`), kotlinc coerces from
//! the slot's erased type straight to the consumer and emits neither call. The coercion is then an
//! identity over the box. A nullable generic slot (`shelf[0]` of a `Shelf<X?>`) is the same
//! identity: its null-safe unbox and null-safe box each keep `null` as it is.
//!
//! When nothing consumes the coerced value at all (`id(ic)` as a statement), kotlinc's codegen
//! discards the call's own result as it is: `PromisedValue.discard` pops the callee's erased
//! result type and never coerces it, so neither the `checkcast` to the box nor `unbox-impl` is
//! emitted. The coercion is then the call itself.

use super::BoxOp;
use krusty::fx_hash::{HashMap, HashSet};
use krusty::ir::{ExprId, IrExpr, IrFile, IrTypeOp};

/// Drop every value-class conversion of a coercion kotlinc never materializes: an `unbox-impl`
/// its consumer boxes straight back, and one of a value nothing consumes.
pub(super) fn drop_unmaterialized(ops: &mut Vec<(ExprId, BoxOp)>, ir: &mut IrFile) {
    drop_rebox_round_trips(ops, ir);
    drop_discarded_unboxes(ops, ir);
}

/// Drop each `unbox-impl` of a coercion's operand that the coercion's consumer immediately boxes
/// back into the same value class, together with that `box-impl`, and make the coercion the
/// operand itself: it no longer converts to the carrier it was retargeted to.
fn drop_rebox_round_trips(ops: &mut Vec<(ExprId, BoxOp)>, ir: &mut IrFile) {
    let unboxes: HashSet<(ExprId, BoxOp)> = ops
        .iter()
        .copied()
        .filter(|(_, op)| matches!(op, BoxOp::Unbox(_) | BoxOp::UnboxNull(_)))
        .collect();
    let uses = expression_use_counts(ir);
    let mut candidates = HashMap::<ExprId, Vec<(ExprId, BoxOp, BoxOp)>>::default();
    for &(coercion, op) in ops.iter() {
        let (BoxOp::Box(value_class) | BoxOp::BoxNull(value_class)) = op else {
            continue;
        };
        let IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg,
            ..
        } = ir.exprs[coercion as usize]
        else {
            continue;
        };
        let unbox = match op {
            BoxOp::Box(_) => BoxOp::Unbox(value_class),
            BoxOp::BoxNull(_) if unboxes.contains(&(arg, BoxOp::UnboxNull(value_class))) => {
                BoxOp::UnboxNull(value_class)
            }
            BoxOp::BoxNull(_) => BoxOp::Unbox(value_class),
            _ => unreachable!("guarded box operation"),
        };
        if unboxes.contains(&(arg, unbox)) {
            candidates
                .entry(arg)
                .or_default()
                .push((coercion, op, unbox));
        }
    }

    let mut identities = HashSet::default();
    for (arg, coercions) in candidates {
        // A BoxOp belongs to the ExprId, not an individual consumer. Cancel it only if every use
        // of the operand is one of these round trips; otherwise the retained use still needs the
        // unbox even though one consumer boxes the value straight back.
        if uses.get(&arg).copied().unwrap_or_default() != coercions.len() {
            continue;
        }
        for (coercion, box_op, unbox_op) in coercions {
            identities.insert((coercion, box_op));
            identities.insert((arg, unbox_op));
            ir.exprs[coercion as usize] = IrExpr::Block {
                stmts: vec![],
                value: Some(arg),
            };
            ir.take_physical_type(coercion);
        }
    }
    ops.retain(|operation| !identities.contains(operation));
}

/// Drop an `unbox-impl` only when every use of its operand is a discarded implicit coercion, and
/// make each such coercion its operand. BoxOps are attached to ExprIds, not use edges, so removing
/// one shared operand's operation when another coercion still consumes it would also remove the
/// retained conversion. The same physical operation can serve multiple discarded coercions.
fn drop_discarded_unboxes(ops: &mut Vec<(ExprId, BoxOp)>, ir: &mut IrFile) {
    let discarded = discarded_coercions(ir);
    let uses = expression_use_counts(ir);
    let removable_operands: HashSet<ExprId> = discarded
        .iter()
        .filter_map(|(&operand, coercions)| {
            (uses.get(&operand).copied().unwrap_or_default() == coercions.len()).then_some(operand)
        })
        .collect();
    ops.retain(|&(operand, op)| {
        !matches!(op, BoxOp::Unbox(_) | BoxOp::UnboxNull(_))
            || !removable_operands.contains(&operand)
    });
    for &operand in &removable_operands {
        for &coercion in &discarded[&operand] {
            ir.exprs[coercion as usize] = IrExpr::Block {
                stmts: vec![],
                value: Some(operand),
            };
            ir.take_physical_type(coercion);
        }
    }
}

/// Count references to each expression. The representation pass attaches operations to nodes, so
/// a conversion may be removed only when all direct consumers of its operand are discarded
/// coercions.
fn expression_use_counts(ir: &IrFile) -> HashMap<ExprId, usize> {
    let mut uses = HashMap::default();
    // Bodies are roots held outside the expression graph. Count all target-emitted declaration
    // roots, not just function bodies: class initializers, statics, and constructor bodies can
    // share arena nodes with discarded statement paths too.
    for root in krusty::ir::emitted_body_roots(ir) {
        *uses.entry(root).or_default() += 1;
    }
    for (index, function) in ir.functions.iter().enumerate() {
        if ir.inline_only_fns.contains(&(index as u32)) {
            if let Some(root) = function.body {
                *uses.entry(root).or_default() += 1;
            }
        }
    }
    // Default expressions are separate entry roots rather than structural bodies.
    for info in ir.fn_params.values() {
        for root in info.defaults.iter().flatten().flatten() {
            *uses.entry(*root).or_default() += 1;
        }
    }
    for root in ir.class_ctor_default_roots() {
        *uses.entry(root).or_default() += 1;
    }
    for plan in &ir.local_delegate_plans {
        *uses.entry(plan.getter.body).or_default() += 1;
        if let Some(setter) = &plan.setter {
            *uses.entry(setter.body).or_default() += 1;
        }
    }
    for class in &ir.classes {
        // The structural root inventory attaches `init_body` to a primary constructor when one
        // exists. Without one, JVM constructor emission still owns the initializer and superclass
        // argument expressions directly.
        if !class.has_primary_ctor {
            if let Some(root) = class.init_body {
                *uses.entry(root).or_default() += 1;
            }
            for root in class.super_arg_prelude.iter().chain(&class.super_args) {
                *uses.entry(*root).or_default() += 1;
            }
        }
        if let Some(root) = ir.companion_clinit_body(class.fq_name) {
            *uses.entry(root).or_default() += 1;
        }
        for entry in &class.enum_entries {
            for root in entry.argument_prelude.iter().chain(&entry.args) {
                *uses.entry(*root).or_default() += 1;
            }
        }
        for constructor in &class.secondary_ctors {
            for root in constructor.defaults.iter().flatten() {
                *uses.entry(*root).or_default() += 1;
            }
        }
    }
    for parent in 0..ir.exprs.len() as ExprId {
        krusty::ir::for_each_child(&ir.exprs, parent, &mut |child| {
            *uses.entry(child).or_default() += 1;
        });
    }
    uses
}

/// The implicit coercions whose result is discarded, indexed by operand. A vector is required:
/// distinct coercion nodes can share one operand and all of them must be rewritten.
fn discarded_coercions(ir: &IrFile) -> HashMap<ExprId, Vec<ExprId>> {
    let uses = expression_use_counts(ir);
    let mut discarded_uses = HashMap::<ExprId, usize>::default();

    // A block's statements are discarded expressions. If one of those expressions is itself a
    // block, its value is discarded too. Resolve that relation to a fixed point so a shared
    // coercion is considered discarded only when every reference to that coercion is discarded.
    for expression in &ir.exprs {
        if let IrExpr::Block { stmts, .. } = expression {
            for &statement in stmts {
                *discarded_uses.entry(statement).or_default() += 1;
            }
        }
    }
    let mut fully_discarded = HashSet::default();
    loop {
        let newly_discarded: Vec<_> = (0..ir.exprs.len() as ExprId)
            .filter(|id| !fully_discarded.contains(id))
            .filter(|id| {
                let total = uses.get(id).copied().unwrap_or_default();
                total != 0 && discarded_uses.get(id).copied().unwrap_or_default() == total
            })
            .collect();
        if newly_discarded.is_empty() {
            break;
        }
        for id in newly_discarded {
            fully_discarded.insert(id);
            if let IrExpr::Block {
                value: Some(value), ..
            } = ir.exprs[id as usize]
            {
                *discarded_uses.entry(value).or_default() += 1;
            }
        }
    }

    let mut coercions = HashMap::<ExprId, Vec<ExprId>>::default();
    for (index, expression) in ir.exprs.iter().enumerate() {
        let id = index as ExprId;
        if !fully_discarded.contains(&id) {
            continue;
        }
        if let IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg,
            ..
        } = expression
        {
            coercions.entry(*arg).or_default().push(id);
        }
    }
    coercions
}

#[cfg(test)]
mod tests {
    use super::*;
    use krusty::types::{type_name, Ty};

    fn coercion(ir: &mut IrFile) -> (ExprId, ExprId) {
        let argument = ir.add_expr(IrExpr::GetValue(0));
        let coercion = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: argument,
            type_operand: Ty::nullable(Ty::obj("kotlin/Any")),
        });
        (argument, coercion)
    }

    #[test]
    fn nullable_unbox_is_not_cancelled_by_a_non_null_box() {
        let mut ir = IrFile::default();
        let (argument, coercion) = coercion(&mut ir);
        let value_class = type_name("fixture/Value");
        let mut operations = vec![
            (argument, BoxOp::UnboxNull(value_class)),
            (coercion, BoxOp::Box(value_class)),
        ];

        drop_rebox_round_trips(&mut operations, &mut ir);

        assert_eq!(operations.len(), 2);
        assert!(matches!(
            ir.expr(coercion),
            IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg,
                ..
            } if *arg == argument
        ));
    }

    #[test]
    fn nullable_unbox_and_nullable_box_cancel_as_one_identity() {
        let mut ir = IrFile::default();
        let (argument, coercion) = coercion(&mut ir);
        let value_class = type_name("fixture/Value");
        let mut operations = vec![
            (argument, BoxOp::UnboxNull(value_class)),
            (coercion, BoxOp::BoxNull(value_class)),
        ];

        drop_rebox_round_trips(&mut operations, &mut ir);

        assert!(operations.is_empty());
        assert!(matches!(
            ir.expr(coercion),
            IrExpr::Block {
                stmts,
                value: Some(value)
            } if stmts.is_empty() && *value == argument
        ));
    }

    #[test]
    fn shared_unbox_is_kept_when_another_use_does_not_rebox() {
        let mut ir = IrFile::default();
        let argument = ir.add_expr(IrExpr::GetValue(0));
        let round_trip = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: argument,
            type_operand: Ty::obj("fixture/Value"),
        });
        let retained = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: argument,
            type_operand: Ty::obj("fixture/OtherValue"),
        });
        let value_class = type_name("fixture/Value");
        let box_op = BoxOp::Box(value_class);
        let unbox_op = BoxOp::Unbox(value_class);
        let mut operations = vec![(argument, unbox_op), (round_trip, box_op)];

        drop_rebox_round_trips(&mut operations, &mut ir);

        assert_eq!(operations, vec![(argument, unbox_op), (round_trip, box_op)]);
        for coercion in [round_trip, retained] {
            assert!(matches!(
                ir.expr(coercion),
                IrExpr::TypeOp {
                    op: IrTypeOp::ImplicitCoercion,
                    arg,
                    ..
                } if *arg == argument
            ));
        }
    }

    #[test]
    fn every_discarded_coercion_sharing_an_operand_loses_its_unbox() {
        let mut ir = IrFile::default();
        let argument = ir.add_expr(IrExpr::GetValue(0));
        let first = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: argument,
            type_operand: Ty::obj("fixture/Value"),
        });
        let second = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: argument,
            type_operand: Ty::obj("fixture/Value"),
        });
        let nested_block = ir.add_expr(IrExpr::Block {
            stmts: vec![],
            value: Some(first),
        });
        ir.add_expr(IrExpr::Block {
            stmts: vec![nested_block, second],
            value: None,
        });
        let value_class = type_name("fixture/Value");
        let mut operations = vec![(argument, BoxOp::Unbox(value_class))];

        drop_discarded_unboxes(&mut operations, &mut ir);

        assert!(operations.is_empty());
        for coercion in [first, second] {
            assert!(matches!(
                ir.expr(coercion),
                IrExpr::Block {
                    stmts,
                    value: Some(value)
                } if stmts.is_empty() && *value == argument
            ));
        }
    }

    #[test]
    fn retained_coercion_use_keeps_a_shared_operands_unbox() {
        let mut ir = IrFile::default();
        let argument = ir.add_expr(IrExpr::GetValue(0));
        let discarded = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: argument,
            type_operand: Ty::obj("fixture/Value"),
        });
        let retained = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: argument,
            type_operand: Ty::obj("fixture/Value"),
        });
        ir.add_expr(IrExpr::Block {
            stmts: vec![discarded],
            value: Some(retained),
        });
        let value_class = type_name("fixture/Value");
        let mut operations = vec![(argument, BoxOp::Unbox(value_class))];

        drop_discarded_unboxes(&mut operations, &mut ir);

        assert_eq!(operations, vec![(argument, BoxOp::Unbox(value_class))]);
        for coercion in [discarded, retained] {
            assert!(matches!(
                ir.expr(coercion),
                IrExpr::TypeOp {
                    op: IrTypeOp::ImplicitCoercion,
                    arg,
                    ..
                } if *arg == argument
            ));
        }
    }

    #[test]
    fn coercion_shared_by_discarded_and_retained_blocks_is_not_discarded() {
        let mut ir = IrFile::default();
        let argument = ir.add_expr(IrExpr::GetValue(0));
        let coercion = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: argument,
            type_operand: Ty::obj("fixture/Value"),
        });
        ir.add_expr(IrExpr::Block {
            stmts: vec![coercion],
            value: None,
        });
        ir.add_expr(IrExpr::Block {
            stmts: vec![],
            value: Some(coercion),
        });
        let value_class = type_name("fixture/Value");
        let mut operations = vec![(argument, BoxOp::Unbox(value_class))];

        drop_discarded_unboxes(&mut operations, &mut ir);

        assert_eq!(operations, vec![(argument, BoxOp::Unbox(value_class))]);
        assert!(matches!(
            ir.expr(coercion),
            IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg,
                ..
            } if *arg == argument
        ));
    }

    #[test]
    fn class_initializer_root_keeps_a_block_shared_with_discarded_code() {
        let mut ir = IrFile::default();
        let value = ir.add_expr(IrExpr::GetValue(0));
        let nested = ir.add_expr(IrExpr::Block {
            stmts: vec![],
            value: Some(value),
        });
        let coercion = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: nested,
            type_operand: Ty::obj("fixture/Value"),
        });
        ir.add_expr(IrExpr::Block {
            stmts: vec![nested, coercion],
            value: None,
        });
        let mut class = krusty::ir::test_support::blank_class("fixture/Owner");
        class.has_primary_ctor = true;
        class.init_body = Some(nested);
        ir.add_class(class);
        let value_class = type_name("fixture/Value");
        let mut operations = vec![(nested, BoxOp::Unbox(value_class))];

        drop_discarded_unboxes(&mut operations, &mut ir);

        assert_eq!(operations, vec![(nested, BoxOp::Unbox(value_class))]);
        assert!(matches!(
            ir.expr(coercion),
            IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg,
                ..
            } if *arg == nested
        ));
    }

    #[test]
    fn no_primary_class_initializer_keeps_a_node_shared_with_discarded_code() {
        let mut ir = IrFile::default();
        let value = ir.add_expr(IrExpr::GetValue(0));
        let nested = ir.add_expr(IrExpr::Block {
            stmts: vec![],
            value: Some(value),
        });
        let coercion = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: nested,
            type_operand: Ty::obj("fixture/Value"),
        });
        ir.add_expr(IrExpr::Block {
            stmts: vec![nested, coercion],
            value: None,
        });
        let mut class = krusty::ir::test_support::blank_class("fixture/Owner");
        class.init_body = Some(nested);
        ir.add_class(class);
        let value_class = type_name("fixture/Value");
        let mut operations = vec![(nested, BoxOp::Unbox(value_class))];

        drop_discarded_unboxes(&mut operations, &mut ir);

        assert_eq!(operations, vec![(nested, BoxOp::Unbox(value_class))]);
        assert!(matches!(
            ir.expr(coercion),
            IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg,
                ..
            } if *arg == nested
        ));
    }

    #[test]
    fn no_primary_class_roots_include_super_argument_frames() {
        let mut ir = IrFile::default();
        let root = ir.add_expr(IrExpr::GetValue(0));
        let mut class = krusty::ir::test_support::blank_class("fixture/Owner");
        class.super_arg_prelude = vec![root];
        class.super_args = vec![root];
        ir.add_class(class);

        assert_eq!(expression_use_counts(&ir).get(&root), Some(&2));
    }

    #[test]
    fn enum_entry_arguments_keep_a_node_shared_with_discarded_code() {
        let mut ir = IrFile::default();
        let argument = ir.add_expr(IrExpr::GetValue(0));
        let coercion = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: argument,
            type_operand: Ty::obj("fixture/Value"),
        });
        ir.add_expr(IrExpr::Block {
            stmts: vec![coercion],
            value: None,
        });
        let mut class = krusty::ir::test_support::blank_class("fixture/Enum");
        class.is_enum = true;
        class.enum_entries.push(krusty::ir::IrEnumEntry {
            name: "VALUE".to_owned(),
            diagnostic_span: None,
            argument_prelude: vec![argument],
            args: vec![argument],
            constructor_parameter_types: vec![Ty::obj("fixture/Value")],
            constructor: krusty::ir::IrConstructorTarget::UNRESTRICTED_PRIMARY,
            default_parameters: Vec::new(),
            decl_line: 0,
            source_order: 0,
            subclass: None,
        });
        ir.add_class(class);
        let value_class = type_name("fixture/Value");
        let mut operations = vec![(argument, BoxOp::Unbox(value_class))];

        drop_discarded_unboxes(&mut operations, &mut ir);

        assert_eq!(operations, vec![(argument, BoxOp::Unbox(value_class))]);
        assert!(matches!(
            ir.expr(coercion),
            IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg,
                ..
            } if *arg == argument
        ));
    }
}
