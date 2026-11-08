//! Every emitted use of an enum this file realized, rewritten in place to its realization.

use crate::fx_hash::{HashMap, HashSet};
use crate::ir::{Callee, ExprId, FunId, IrExpr, IrFile, IrIntrinsic};
use crate::types::{Ty, TypeName};

use super::initializer::{call, entries_type, read_slot, values_array};
use super::{Contract, Realized};

/// Rewrite each use in an emitted body, the realization's own bodies included (a constant's
/// arguments may read another constant), and in every default: the default-argument lowering
/// copies a function's or constructor's defaults into its entry after this pass
/// (`annotation class C(val e: E = E.A)`).
pub(super) fn rewrite(ir: &mut IrFile, realized: &HashMap<TypeName, Realized>, contract: Contract) {
    let mut seen = HashSet::default();
    for root in roots(ir) {
        for expression in crate::ir::value_namespace_expressions(ir, root) {
            if seen.insert(expression) {
                rewrite_one(ir, realized, contract, expression);
            }
        }
    }
}

/// Every root a use can sit under: each emitted body and each default.
fn roots(ir: &IrFile) -> Vec<ExprId> {
    let mut roots = crate::ir::emitted_body_roots(ir);
    roots.extend(function_defaults(ir));
    roots.extend(constructor_defaults(ir));
    roots
}

/// The enums whose `entries` some use of this file reads. Before realization a constant's
/// arguments are no emitted body yet, so they are scanned too (`A(F.entries.size)`).
pub(super) fn entries_read(ir: &IrFile) -> HashSet<TypeName> {
    roots(ir)
        .into_iter()
        .chain(entry_arguments(ir))
        .flat_map(|root| crate::ir::value_namespace_expressions(ir, root))
        .filter_map(|expression| entries_of(ir.expr(expression)))
        .collect()
}

/// Every enum constant's argument prelude and arguments.
fn entry_arguments(ir: &IrFile) -> Vec<ExprId> {
    ir.classes
        .iter()
        .flat_map(|class| &class.enum_entries)
        .flat_map(|entry| entry.argument_prelude.iter().chain(&entry.args))
        .copied()
        .collect()
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

fn rewrite_one(
    ir: &mut IrFile,
    realized: &HashMap<TypeName, Realized>,
    contract: Contract,
    expression: ExprId,
) {
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
            let entries = enum_class
                .entries
                .expect("an enum whose `entries` this file reads keeps them");
            let mut stmts = vec![build];
            if contract == Contract::KotlinWasm {
                stmts.push(build_entries_on_first_read(
                    ir, classifier, enum_class, entries,
                ));
            }
            let read = read_slot(ir, entries, entries_type(classifier));
            IrExpr::Block {
                stmts,
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

/// `if (entries == null) entries = EnumEntriesList(values)`: kotlinc-js/wasm build the list from
/// `values()` on its first read, so a read from inside a constructor already finds one.
fn build_entries_on_first_read(
    ir: &mut IrFile,
    classifier: TypeName,
    enum_class: &Realized,
    entries: u32,
) -> ExprId {
    let current = read_slot(ir, entries, Ty::nullable(entries_type(classifier)));
    let null = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Null));
    ir.set_logical_type(null, Ty::Null);
    let missing = ir.add_expr(IrExpr::Equality {
        op: crate::ir::IrBinOp::Eq,
        mode: crate::types::EqualityMode::Structural,
        lhs: current,
        rhs: null,
    });
    ir.set_logical_type(missing, Ty::Boolean);
    let values = values_array(ir, classifier, &enum_class.slots);
    let list = ir.add_expr(IrExpr::EnumEntriesList { classifier, values });
    ir.set_logical_type(list, entries_type(classifier));
    let store = ir.add_expr(IrExpr::SetStatic {
        index: entries,
        value: list,
    });
    ir.set_logical_type(store, Ty::Unit);
    let when = ir.add_expr(IrExpr::When {
        branches: vec![(Some(missing), store)],
    });
    ir.set_logical_type(when, Ty::Unit);
    when
}
