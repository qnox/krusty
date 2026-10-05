//! A value-class construction that omits defaulted arguments.
//!
//! It calls the `$default` stub of the selected constructor's static `constructor-impl`. When
//! this file synthesized that function, the construction becomes an ordinary default call of it,
//! so step 5 adapts the supplied operands to the stub's parameters and the shared default-call
//! realization materializes the placeholders, masks and marker. A class another source file
//! declares has no such function here; its call is realized directly over the constructor's
//! declared parameters with the same materializer.

use super::{desc, erase, Under};
use crate::ir::{Callee, ExprId, IrExpr, IrFile};
use crate::libraries::InlineKind;
use crate::types::{Ty, TypeName};

pub(super) struct DefaultConstruction {
    pub(super) owner: TypeName,
    /// The erased underlying the construction produces.
    pub(super) underlying: Ty,
    /// The `constructor-impl` this file synthesized for the selected constructor.
    pub(super) function: Option<u32>,
    pub(super) args: Vec<ExprId>,
    pub(super) omitted: Box<[u32]>,
}

impl DefaultConstruction {
    pub(super) fn realize(self, ir: &mut IrFile, construction: ExprId, under: &Under) -> IrExpr {
        let Self {
            owner,
            underlying,
            function,
            args,
            omitted,
        } = self;
        ir.record_erased_value_construction(construction, owner, underlying);
        match function {
            Some(function) => IrExpr::Call {
                callee: Callee::ClassStaticWithDefaults {
                    owner,
                    function,
                    defaults: omitted,
                },
                dispatch_receiver: None,
                args,
            },
            None => sibling_call(ir, construction, owner, underlying, args, &omitted, under),
        }
    }
}

/// `owner.constructor-impl$default(<params>, <masks>, DefaultConstructorMarker)` for a class
/// another source file declares.
///
/// Like a sibling function's `$default` call, every parameter takes its erased carrier: the
/// declared defaults of another file's constructor are not known here, so a defaulted
/// nullable-carrier value-class parameter that kotlinc keeps boxed is not boxed.
fn sibling_call(
    ir: &mut IrFile,
    construction: ExprId,
    owner: TypeName,
    underlying: Ty,
    args: Vec<ExprId>,
    omitted: &[u32],
    under: &Under,
) -> IrExpr {
    let declared = ir
        .construction_declared_params
        .remove(&construction)
        .expect("a checked construction records its declared parameters");
    let physical = declared.iter().map(|p| erase(p, under)).collect();
    let (mut parameters, args, _) =
        crate::jvm::module_calls::realize_default_arguments(ir, physical, args, omitted, None, &[])
            .expect("a checked construction's omissions fit its constructor's parameters");
    *parameters
        .last_mut()
        .expect("a default call ends with its marker") =
        Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker");
    // Supplied operands adapt to their declared types; the synthesized ones already have their
    // physical type.
    let boundary = parameters
        .iter()
        .enumerate()
        .map(|(index, physical)| match declared.get(index) {
            Some(declared) if !omitted.contains(&(index as u32)) => *declared,
            _ => *physical,
        })
        .collect();
    ir.call_declared_params.insert(construction, boundary);
    let parameters: String = parameters.iter().map(desc).collect();
    IrExpr::Call {
        callee: Callee::Static {
            owner,
            name: "constructor-impl$default".to_string(),
            descriptor: format!("({parameters}){}", desc(&underlying)),
            inline: InlineKind::None,
        },
        dispatch_receiver: None,
        args,
    }
}
