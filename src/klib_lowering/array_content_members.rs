//! The array content members a data class generates. kotlinc's backends lower the `IrBuiltIns`
//! operators `dataClassArrayMemberToString` and `dataClassArrayMemberHashCode` to the content
//! members of the argument's array class; a KLIB serializes them as calls to the operator, which
//! no library declares. Body lowering realizes the call as a call of the `kotlin.collections`
//! extension (`contentToString`, `contentHashCode`) on the array, and the dependency prewalk adds
//! the same declaration as the call's edge, so both sides agree on what the body reaches.

use std::sync::OnceLock;

use super::ir_builtins::{BuiltinOperator, DataClassArrayMember, IrBuiltinOperators};
use crate::metadata::id_signature::{
    callable_signature, CallableShape, DeclarationContainer, Placement,
};
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrArguments, KlibIrExpr, KlibIrExprId, KlibIrExprKind, KlibIrMemberAccess,
};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbol, KlibIrSymbolKind};
use crate::metadata::semantic::KotlinType;
use crate::types::{prim_array_name, Ty};

/// The elements of the primitive specialized array classes, whose content extensions
/// `kotlin.collections` declares.
const ELEMENTS: [Ty; 12] = [
    Ty::Int,
    Ty::Long,
    Ty::Short,
    Ty::Byte,
    Ty::Boolean,
    Ty::Char,
    Ty::Float,
    Ty::Double,
    Ty::UByte,
    Ty::UShort,
    Ty::UInt,
    Ty::ULong,
];

struct ContentCallees {
    operators: IrBuiltinOperators,
    callees: Vec<(Ty, DataClassArrayMember, KlibIrSymbol)>,
}

fn content_callees() -> &'static ContentCallees {
    static CALLEES: OnceLock<ContentCallees> = OnceLock::new();
    CALLEES.get_or_init(|| {
        let package = ["kotlin".to_owned(), "collections".to_owned()];
        let container = DeclarationContainer::<KotlinType> {
            package: &package,
            classes: &[],
            native_interop_library: false,
        };
        let mut callees = Vec::new();
        for element in ELEMENTS {
            let array =
                prim_array_name(element).expect("a primitive array element names its class");
            let receiver = KotlinType::Class {
                internal: array.to_owned(),
                args: Vec::new(),
                nullable: true,
                shape: Default::default(),
            };
            for member in DataClassArrayMember::ALL {
                let shape = CallableShape {
                    name: member.content_function(),
                    contexts: Vec::new(),
                    receiver: Some(&receiver),
                    params: Vec::new(),
                    vararg: None,
                    type_parameters: Vec::new(),
                    expect: false,
                    placement: Placement::Ordinary,
                };
                let signature = callable_signature(container, &shape)
                    .expect("an array content extension names no type parameter");
                callees.push((
                    element,
                    member,
                    KlibIrSymbol {
                        kind: KlibIrSymbolKind::Function,
                        signature: KlibIrSignature::Public(signature),
                    },
                ));
            }
        }
        ContentCallees {
            operators: IrBuiltinOperators::new(),
            callees,
        }
    })
}

/// The `kotlin.collections` extension that computes `member` over an array of primitive `element`.
pub(super) fn content_callee(
    member: DataClassArrayMember,
    element: Ty,
) -> Option<&'static KlibIrSymbol> {
    content_callees()
        .callees
        .iter()
        .find(|(candidate, kind, _)| *candidate == element && *kind == member)
        .map(|(_, _, symbol)| symbol)
}

/// The array operand of a data class array member call: its one argument.
pub(super) fn operand(access: &KlibIrMemberAccess) -> Option<KlibIrExprId> {
    match &access.arguments {
        KlibIrArguments::Flat(arguments) => match arguments.as_slice() {
            &[Some(operand)] => Some(operand),
            _ => None,
        },
        KlibIrArguments::Split {
            dispatch_receiver: None,
            extension_receiver: None,
            values,
        } => match values.as_slice() {
            &[Some(operand)] => Some(operand),
            _ => None,
        },
        KlibIrArguments::Split { .. } => None,
    }
}

/// The element of the primitive array class `operand` has, by its classifier.
pub(super) fn array_element(arena: &KlibIrArena, operand: KlibIrExprId) -> Option<Ty> {
    let name = super::for_loops::class_name(arena, arena.expr(operand).ty?)?;
    crate::types::prim_array_element(name)
}

/// The content extension `expression`, a data class array member call, reaches. It is the
/// declaration edge the call has in place of the operator, which no library declares.
pub(crate) fn implied_callee(
    arena: &KlibIrArena,
    expression: &KlibIrExpr,
) -> Option<&'static KlibIrSymbol> {
    let KlibIrExprKind::Call {
        access,
        super_qualifier: None,
    } = &expression.kind
    else {
        return None;
    };
    let KlibIrSignature::Public(signature) = &access.symbol.signature else {
        return None;
    };
    let Some(BuiltinOperator::DataClassArrayMember(member)) =
        content_callees().operators.operator(signature)
    else {
        return None;
    };
    content_callee(member, array_element(arena, operand(access)?)?)
}
