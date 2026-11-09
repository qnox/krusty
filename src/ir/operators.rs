//! The operators of the primitive binary and type-operator expression nodes.

/// Built-in binary operators carried by `IrExpr::PrimitiveBinOp`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrBinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    /// Referential identity (`===`/`!==`): a JVM `if_acmp*` on two reference operands, never the
    /// structural `Intrinsics.areEqual` that `==`/`!=` (`Eq`/`Ne`) uses for references.
    RefEq,
    RefNe,
    And,
    Or,
    /// Bitwise/shift on `Int`/`Long` (Kotlin's `and`/`or`/`xor`/`shl`/`shr`/`ushr` infix functions).
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Ushr,
}

impl IrBinOp {
    /// The binary operation an exact builtin operator member realizes (`Int.plus`, `Long.and`,
    /// `Int.shl`): the operation its source operator form lowers to. Every backend realizes the
    /// provider-authenticated member through this one mapping.
    pub fn of_primitive_operator(intrinsic: crate::libraries::CompilerIntrinsic) -> Option<Self> {
        use crate::libraries::{CompilerIntrinsic, PrimitiveBinaryIntrinsic};
        Some(match intrinsic {
            CompilerIntrinsic::PrimitiveBinary(operation) => match operation {
                PrimitiveBinaryIntrinsic::Add => IrBinOp::Add,
                PrimitiveBinaryIntrinsic::Subtract => IrBinOp::Sub,
                PrimitiveBinaryIntrinsic::Multiply => IrBinOp::Mul,
                PrimitiveBinaryIntrinsic::Divide => IrBinOp::Div,
                PrimitiveBinaryIntrinsic::Remainder => IrBinOp::Rem,
            },
            CompilerIntrinsic::PrimitiveBitAnd => IrBinOp::BitAnd,
            CompilerIntrinsic::PrimitiveBitOr => IrBinOp::BitOr,
            CompilerIntrinsic::PrimitiveBitXor => IrBinOp::BitXor,
            CompilerIntrinsic::PrimitiveShiftLeft => IrBinOp::Shl,
            CompilerIntrinsic::PrimitiveShiftRight => IrBinOp::Shr,
            CompilerIntrinsic::PrimitiveUnsignedShiftRight => IrBinOp::Ushr,
            _ => return None,
        })
    }

    /// Whether the operator answers a `Boolean` whatever its operands: a comparison, an identity
    /// test, or a logical connective.
    pub fn yields_boolean(self) -> bool {
        matches!(
            self,
            IrBinOp::Lt
                | IrBinOp::Le
                | IrBinOp::Gt
                | IrBinOp::Ge
                | IrBinOp::Eq
                | IrBinOp::Ne
                | IrBinOp::RefEq
                | IrBinOp::RefNe
                | IrBinOp::And
                | IrBinOp::Or
        )
    }
}

/// The `IrTypeOperatorCall` operators (Kotlin IR's `IrTypeOperator`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrTypeOp {
    InstanceOf,    // `is T`
    NotInstanceOf, // `!is T`
    Cast,          // `as T?` (or `as <primitive>`): a plain `checkcast` — `null` passes
    /// `as T` to a non-null reference type: null-check (`Intrinsics.checkNotNull`) then `checkcast`,
    /// so casting `null` throws — matching kotlinc.
    CastNonNull,
    SafeCast, // `as? T`
    /// Representation coercion the backend inserts (e.g. JVM box/unbox) — explicit in the IR so it
    /// is visible and testable, not hidden in codegen.
    ImplicitCoercion,
}

impl IrTypeOp {
    /// The operation that uses a value through a smart cast to `to`, a type the frontend proved the
    /// value has: a reference target is cast, any other target is a representation coercion.
    pub fn smart_cast_to(to: crate::types::Ty) -> Self {
        if to.is_reference() {
            Self::Cast
        } else {
            Self::ImplicitCoercion
        }
    }
}
