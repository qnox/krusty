//! Lowering a read of a constructor's synthetic prefix parameter.
//!
//! A lambda written in a super-constructor argument is checked inside the constructor prefix, where
//! the instance does not exist, so the value it captures is carried in as one more parameter. The
//! read of it therefore has a COORDINATE rather than a name: which declaration's capture, which
//! field, and whether the reader is the constructor itself or a body nested inside it.
//!
//! What these pin is that lowering performs exactly the one lookup that coordinate names, and fails
//! when it cannot. The contract they replaced searched every capture of the same owner and field
//! and, when that search missed, reinterpreted the node as the constructor's own parameter — which
//! answers plausibly and wrongly in a body where that slot holds another declaration.

use crate::fir::{
    BodyOwnerId, DeclarationId, FirBody, FirExpr, FirExprKind, FirStatement, FirStatementKind,
    OriginId, ResolvedModuleIndex,
};
use crate::ir::IrFile;
use crate::types::Ty;

// This module is declared inside `tests`, so `super` is that module and the lowering entry point
// is one level further out.
use super::super::lower_body;
use super::resolved;

#[test]
fn a_capture_coordinate_naming_no_slot_fails_rather_than_reading_the_parameter() {
    let origin = OriginId::from_raw(0);
    let mut body = FirBody::new(BodyOwnerId::from_raw(3));
    let owner = DeclarationId::from_raw(11);
    let read = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::ConstructorCaptureRead {
            owner,
            field: 0,
            shared_cell: false,
            site: crate::fir::FirConstructorCaptureSite::Captured { enclosing_depth: 2 },
        },
    });
    let statement = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Expression(read),
    });
    body.push_root(statement);

    let mut ir = IrFile::default();
    let failure = lower_body(body, &ResolvedModuleIndex::default(), &mut ir)
        .expect_err("a coordinate naming no slot must fail");
    // Not a silent `GetValue(field + 1)`: in a body that is not that constructor, slot 1 holds
    // another declaration entirely, and reading it would answer plausibly and wrongly.
    assert_eq!(
        failure,
        crate::fir_lower::FirLoweringFailure::MissingCapture {
            enclosing_depth: 2,
            source: crate::fir::FirCaptureSource::ConstructorPrefix { owner, field: 0 },
        }
    );
}

#[test]
fn a_parameter_coordinate_outside_its_constructor_fails() {
    let origin = OriginId::from_raw(0);
    let mut body = FirBody::new(BodyOwnerId::from_raw(3));
    let owner = DeclarationId::from_raw(11);
    let read = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::ConstructorCaptureRead {
            owner,
            field: 0,
            shared_cell: false,
            site: crate::fir::FirConstructorCaptureSite::Parameter,
        },
    });
    let statement = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Expression(read),
    });
    body.push_root(statement);

    let mut ir = IrFile::default();
    let failure = lower_body(body, &ResolvedModuleIndex::default(), &mut ir)
        .expect_err("a parameter coordinate outside its own constructor must fail");
    assert_eq!(
        failure,
        crate::fir_lower::FirLoweringFailure::InvalidConstructorCapture { owner, field: 0 }
    );
}
