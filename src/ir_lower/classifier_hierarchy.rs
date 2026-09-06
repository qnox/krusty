//! Legacy checked-AST lowering handoff for applied classifier hierarchies.
//!
//! Production streaming lowering copies this fact from the stable FIR index. The old whole-file
//! lowerer remains active for compatibility tests while it is removed; publish the identical
//! common-IR record here so JVM passes never need a missing-record fallback into frontend symbols.

use crate::frontend::FrontendSymbols;
use crate::ir::{IrAppliedClassifier, IrFile};

pub(super) fn publish(ir: &mut IrFile, symbols: &FrontendSymbols) {
    let classifiers = ir
        .classes
        .iter()
        .filter(|class| class.is_source_declared)
        .map(|class| class.fq_name)
        .collect::<Vec<_>>();
    for classifier in classifiers {
        if ir.classifier_hierarchies.contains_key(&classifier) {
            continue;
        }
        let hierarchy = symbols
            .applied_hierarchy(crate::types::Ty::obj_name(classifier))
            .into_iter()
            .map(|(owner, applied, depth)| IrAppliedClassifier {
                classifier: owner,
                applied,
                depth: u32::try_from(depth).expect("classifier hierarchy depth exceeds u32"),
            })
            .collect::<Vec<_>>();
        assert!(
            hierarchy
                .first()
                .is_some_and(|entry| entry.classifier == classifier && entry.depth == 0),
            "checked source classifier must head its applied hierarchy"
        );
        ir.classifier_hierarchies.insert(classifier, hierarchy);
    }
}
