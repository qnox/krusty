//! Inventory of source binding reassignment facts published on common-IR reads.
//!
//! Declaration forms differ structurally in checked FIR, but they all feed one backend-independent
//! contract. Keeping that union here prevents consumers such as `when` lowering and JVM emission
//! from growing subtly different definitions of an immutable binding.

use std::collections::HashMap;

use crate::fir::{FirBody, FirDestructureEntry, FirExprKind, FirLoopHeader, FirStatementKind};
use crate::ir::IrBindingStability;

use super::BodyLowering;

pub(super) fn inventory(body: &FirBody) -> HashMap<crate::fir::LocalValueId, IrBindingStability> {
    let mut bindings = HashMap::new();
    let mut record = |value, stability| {
        let previous = bindings.insert(value, stability);
        debug_assert!(
            previous.is_none_or(|previous| previous == stability),
            "one checked local value has conflicting binding stability"
        );
    };

    for parameter in body.parameters() {
        record(parameter.value, IrBindingStability::Stable);
    }

    for raw in 0..body.statement_count() {
        let statement = body
            .statement(crate::fir::FirStatementId::from_raw(raw as u32))
            .expect("FIR statement index");
        match &statement.kind {
            FirStatementKind::Local {
                target,
                mutable,
                lateinit,
                ..
            } => record(
                *target,
                if *mutable || *lateinit {
                    IrBindingStability::Mutable
                } else {
                    IrBindingStability::Stable
                },
            ),
            FirStatementKind::Destructure { entries, .. } => {
                for entry in entries {
                    if let FirDestructureEntry::Binding {
                        target, mutable, ..
                    } = entry
                    {
                        record(
                            *target,
                            if *mutable {
                                IrBindingStability::Mutable
                            } else {
                                IrBindingStability::Stable
                            },
                        );
                    }
                }
            }
            FirStatementKind::Loop { header, .. } => match header {
                FirLoopHeader::Range { variable, .. }
                | FirLoopHeader::Iterable { variable, .. }
                | FirLoopHeader::Iterator { variable, .. } => {
                    record(*variable, IrBindingStability::Stable);
                }
                FirLoopHeader::While { .. } | FirLoopHeader::DoWhile { .. } => {}
            },
            _ => {}
        }
    }

    for raw in 0..body.expression_count() {
        let expression = body
            .expr(crate::fir::FirExprId::from_raw(raw as u32))
            .expect("FIR expression index");
        if let FirExprKind::Try { catches, .. } = &expression.kind {
            for catch in catches {
                record(catch.parameter, IrBindingStability::Stable);
            }
        }
    }

    bindings
}

impl BodyLowering<'_> {
    pub(super) fn local_value_is_mutable(&self, value: crate::fir::LocalValueId) -> bool {
        self.binding_stability.get(&value) == Some(&IrBindingStability::Mutable)
    }
}
