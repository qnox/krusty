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

impl super::IrExpr {
    /// A type operation of `op` on `arg` against `type_operand`. Every producer builds it here, so
    /// a fact the operation gains later is added at this one place.
    pub fn type_op(op: IrTypeOp, arg: super::ExprId, type_operand: crate::types::Ty) -> Self {
        Self::TypeOp {
            op,
            arg,
            type_operand,
            erased: None,
        }
    }

    /// This type operation recording the erasure a cast to a type parameter checks.
    pub fn with_erased(mut self, erasure: Option<crate::types::Ty>) -> Self {
        if let Self::TypeOp { erased, .. } = &mut self {
            *erased = erasure;
        }
        self
    }

    /// This expression with its operand replaced by `new_arg`, every other part of the payload
    /// kept. A pass that moves a type operation over a different operand copies it through here,
    /// so a fact the operation records survives the rewrite.
    pub fn with_arg(mut self, new_arg: super::ExprId) -> Self {
        if let Self::TypeOp { arg, .. } = &mut self {
            *arg = new_arg;
        }
        self
    }
}

impl IrTypeOp {
    /// The operator of a checked `as` cast to `target`. A cast to a type parameter follows the
    /// parameter's upper-bound nullability. Kotlin's implicit bound is `Any?`, so `value as E` in
    /// an unconstrained generic body must let `null` reach an instantiation such as `E = String?`;
    /// only `<E : Any>` or `E & Any` requires the runtime null check. The checked target already
    /// carries that distinction.
    pub fn cast_to(target: crate::types::Ty) -> IrTypeOp {
        if target.is_nullable() || (target.is_ty_param() && target.upper_bound_admits_null()) {
            IrTypeOp::Cast
        } else {
            IrTypeOp::CastNonNull
        }
    }
}

#[cfg(test)]
mod cast_operator_tests {
    use super::IrTypeOp;
    use crate::ir::IrExpr;
    use crate::types::Ty;

    #[test]
    fn a_cast_admits_null_exactly_where_the_target_does() {
        let any = Ty::obj("kotlin/Any");
        let cases = [
            (Ty::String, IrTypeOp::CastNonNull),
            (Ty::nullable(Ty::String), IrTypeOp::Cast),
            // The implicit bound `Any?` lets `null` reach `E = String?`.
            (Ty::ty_param("E", Ty::nullable(any)), IrTypeOp::Cast),
            (Ty::ty_param("E", any), IrTypeOp::CastNonNull),
        ];
        for (target, expected) in cases {
            assert_eq!(IrTypeOp::cast_to(target), expected, "{target:?}");
        }
    }

    #[test]
    fn with_arg_replaces_only_the_operand() {
        let original = IrExpr::type_op(IrTypeOp::Cast, 1, Ty::ty_param("T", Ty::obj("kotlin/Any")))
            .with_erased(Some(Ty::obj("kotlin/String")));
        let IrExpr::TypeOp {
            op,
            arg,
            type_operand,
            erased,
        } = original.with_arg(7)
        else {
            panic!("a type operation stays a type operation");
        };
        assert_eq!((op, arg), (IrTypeOp::Cast, 7));
        assert!(type_operand.is_ty_param());
        assert_eq!(erased, Some(Ty::obj("kotlin/String")));
    }
}
