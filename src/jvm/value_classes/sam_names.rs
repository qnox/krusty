//! Value-class ABI names and physical signatures of exact checker-selected SAM declarations.

use super::{erase, Under};
use crate::ir::{IrExpr, IrFile};

pub(super) fn rename(ir: &mut IrFile, value_classes: &Under) {
    if value_classes.is_empty() {
        return;
    }
    // The checker records one exact SAM declaration, including its declared signature and suspend
    // fact. Two methods of one fun interface can share a name and arity while their value-class
    // hashes differ, so the physical name must not be recovered from a `(classifier, name, arity)`
    // map.
    for expression in &mut ir.exprs {
        let IrExpr::Lambda {
            sam: Some(target), ..
        } = expression
        else {
            continue;
        };
        target.method = super::vc_mangle_once(
            &target.method,
            &target.declared_parameters,
            &target.declared_result,
            value_classes,
            false,
            target.suspend,
        );
    }
}

/// Common IR keeps the SAM declaration semantic. Once this backend has chosen value-class
/// carriers, publish the exact physical interface slot beside it. LambdaMetafactory (and the
/// class-based SAM strategy) must implement that erased slot, not a descriptor reconstructed
/// from the value-class classifier spelling.
pub(super) fn publish_jvm_signatures(ir: &mut IrFile, under: &Under) {
    ir.lambda_sam_jvm_signature = ir
        .lambda_sam_signature
        .iter()
        .map(|(&implementation, (parameters, result))| {
            (
                implementation,
                (
                    parameters
                        .iter()
                        .map(|parameter| erase(parameter, under))
                        .collect(),
                    erase(result, under),
                ),
            )
        })
        .collect();
}
