//! Stable source distinctions retained beside otherwise-generic expression nodes.

/// Which declaration a checked enum `valueOf` operation selected. Both name the same lookup by
/// entry name; they are different declarations, and only one of them is `inline`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnumValueOfDeclaration {
    /// The classifier's own implicit member — `E.valueOf(name)`.
    Member,
    /// The standard library's top-level `enumValueOf<E>(name)`, whose body expands at the call.
    StandardLibraryTopLevel,
}

/// Source Boolean operator represented by a generic [`super::IrExpr::When`] after common lowering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrShortCircuitKind {
    And,
    Or,
}

/// A source operator, conversion, indexing or member call the frontend selected as a builtin
/// operation, retained on the node that realizes it. A backend decides how its codegen realizes
/// the call, and so where the call's line goes.
#[derive(Clone, Debug, PartialEq)]
pub struct IrBuiltinCall {
    pub kind: IrBuiltinCallKind,
    /// The call's checked result type.
    pub result: crate::types::Ty,
    /// Each operand's checked type, receiver first.
    pub operands: Vec<crate::types::Ty>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrBuiltinCallKind {
    Unary(crate::fir::FirUnaryOperation),
    Binary(crate::fir::FirBinaryOperation),
    /// An implicit numeric conversion of the one operand to the result type.
    NumericConversion,
    /// `receiver[index]` on an array or a string.
    IndexedRead,
    /// A member call the frontend selected as a compiler intrinsic; the node's callee names it.
    Intrinsic,
}

/// A read of a safe call's receiver temporary, by the role fir2ir gives it, with the source line
/// it carries there.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrSafeCallRead {
    /// The `null` test, at the safe call's own line.
    NullCheck { line: u32 },
    /// The selector's receiver, at the line the receiver starts on.
    Receiver { start_line: u32 },
}
