//! Every emitted use of an enum this file realized, rewritten in place to its realization.

use crate::fx_hash::{HashMap, HashSet};
use crate::ir::{Callee, ExprId, FunId, IrExpr, IrFile, IrIntrinsic};
use crate::types::{Ty, TypeName};

use super::initializer::{call, entries_type, read_slot};
use super::Realized;

/// Rewrite each use in an emitted body, the realization's own bodies included (a constant's
/// arguments may read another constant), and in every default: the default-argument lowering
/// copies a function's or constructor's defaults into its entry after this pass
/// (`annotation class C(val e: E = E.A)`).
pub(super) fn rewrite(ir: &mut IrFile, realized: &HashMap<TypeName, Realized>) {
    let mut roots = crate::ir::emitted_body_roots(ir);
    roots.extend(function_defaults(ir));
    roots.extend(constructor_defaults(ir));
    let mut seen = HashSet::default();
    for root in roots {
        for expression in crate::ir::value_namespace_expressions(ir, root) {
            if seen.insert(expression) {
                rewrite_one(ir, realized, expression);
            }
        }
    }
}

/// The defaults of every function this file emits.
pub(super) fn function_defaults(ir: &IrFile) -> Vec<ExprId> {
    (0..ir.functions.len())
        .map(|function| FunId::try_from(function).expect("too many functions for a function id"))
        .filter(|function| !ir.inline_only_fns.contains(function))
        .filter_map(|function| ir.param_defaults(function))
        .flatten()
        .flatten()
        .copied()
        .collect()
}

/// The defaults of every constructor this file emits, which the default-argument lowering copies
/// into the constructor's entry.
pub(super) fn constructor_defaults(ir: &IrFile) -> Vec<ExprId> {
    ir.classes
        .iter()
        .flat_map(|class| {
            let primary = class
                .has_primary_ctor
                .then(|| ir.class_ctor_defaults_name(class.fq_name_id()))
                .flatten()
                .into_iter()
                .flatten();
            let secondaries = class
                .secondary_ctors
                .iter()
                .flat_map(|constructor| &constructor.defaults);
            primary.chain(secondaries).flatten().copied()
        })
        .collect()
}

/// The enum whose `entries` an expression reads: `E.entries`, or the `enumEntries<E>()` intrinsic
/// with its type argument known.
fn entries_of(expression: &IrExpr) -> Option<TypeName> {
    match expression {
        IrExpr::EnumEntries { classifier } => Some(*classifier),
        IrExpr::Call { callee, .. } => match &**callee {
            Callee::Intrinsic {
                operation: IrIntrinsic::EnumEntries { classifier },
                ..
            } => classifier.non_null().obj_internal(),
            _ => None,
        },
        _ => None,
    }
}

fn rewrite_one(ir: &mut IrFile, realized: &HashMap<TypeName, Realized>, expression: ExprId) {
    let replacement = match ir.expr(expression) {
        IrExpr::EnumEntry { classifier, name } => {
            let classifier = *classifier;
            let Some(enum_class) = realized.get(&classifier) else {
                return;
            };
            let ordinal = ir
                .class_id_by_name(classifier)
                .and_then(|class| {
                    ir.classes[class as usize]
                        .enum_entries
                        .iter()
                        .position(|entry| entry.name.as_str() == &**name)
                })
                .expect("a checked enum constant names one of its enum's constants");
            let slot = enum_class.slots[ordinal];
            let build = call(ir, enum_class.initializer, Vec::new(), Ty::Unit);
            let read = read_slot(ir, slot, Ty::obj_name(classifier));
            IrExpr::Block {
                stmts: vec![build],
                value: Some(read),
            }
        }
        // `E.entries`, and `enumEntries<E>()` once inline specialization has named the enum.
        node if entries_of(node).is_some() => {
            let classifier = entries_of(node).expect("matched above");
            let Some(enum_class) = realized.get(&classifier) else {
                return;
            };
            let build = call(ir, enum_class.initializer, Vec::new(), Ty::Unit);
            let read = read_slot(ir, enum_class.entries, entries_type(classifier));
            IrExpr::Block {
                stmts: vec![build],
                value: Some(read),
            }
        }
        IrExpr::EnumValues { classifier } => {
            let Some(enum_class) = realized.get(classifier) else {
                return;
            };
            IrExpr::Call {
                callee: Box::new(Callee::Local(enum_class.values)),
                dispatch_receiver: None,
                args: Vec::new(),
            }
        }
        IrExpr::EnumValueOf {
            classifier, arg, ..
        } => {
            let Some(enum_class) = realized.get(classifier) else {
                return;
            };
            IrExpr::Call {
                callee: Box::new(Callee::Local(enum_class.value_of)),
                dispatch_receiver: None,
                args: vec![*arg],
            }
        }
        _ => return,
    };
    // In place: the expression keeps its identity, and with it its checked type and every fact a
    // target froze for it.
    ir.exprs[expression as usize] = replacement;
}
