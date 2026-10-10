//! The compiler built-in operators a KLIB body calls.
//!
//! Kotlin's backend IR expresses a primitive relation (`a < b` on two `Int`s) as a call to a
//! compiler built-in declaration in package `kotlin.internal.ir` (Kotlin's `IrBuiltIns`), which no
//! library declares: the compiler owns those declarations. This table is the closed set of them
//! that lowering realizes. Each entry's identity is computed by the same target-free mangler that
//! signs every library declaration, from the declaration's shape, so a body's call is joined to
//! an entry by exact signature only.

use std::collections::HashMap;

use crate::ir::IrBinOp;
use crate::metadata::id_signature::{
    callable_signature, CallableShape, DeclarationContainer, KlibPublicIdSignature, Placement,
};
use crate::metadata::semantic::{semantic_ty, KotlinType};
use crate::types::Ty;

/// The package of the compiler built-in declarations.
const PACKAGE: [&str; 3] = ["kotlin", "internal", "ir"];

/// `IrBuiltIns`' relational operators, each declared as `name(arg0: T, arg1: T): Boolean` for every
/// primitive operand type below.
const RELATIONS: [(&str, IrBinOp); 4] = [
    ("less", IrBinOp::Lt),
    ("lessOrEqual", IrBinOp::Le),
    ("greater", IrBinOp::Gt),
    ("greaterOrEqual", IrBinOp::Ge),
];

/// The operand types whose relations lowering realizes, as metadata class ids.
const OPERANDS: [&str; 4] = ["kotlin/Int", "kotlin/Long", "kotlin/Float", "kotlin/Double"];

/// A relational operator over two operands of one primitive type, answering `Boolean`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct BuiltinRelation {
    pub(super) operator: IrBinOp,
    pub(super) operand: Ty,
}

/// The built-in operators lowering realizes, by exact signature.
pub(super) struct IrBuiltinOperators {
    relations: HashMap<KlibPublicIdSignature, BuiltinRelation>,
}

impl IrBuiltinOperators {
    pub(super) fn new() -> Self {
        let package = PACKAGE.map(str::to_owned);
        let container = DeclarationContainer::<KotlinType> {
            package: &package,
            classes: &[],
            native_interop_library: false,
        };
        let mut relations = HashMap::new();
        for operand in OPERANDS {
            let operand = KotlinType::class(operand);
            for (name, operator) in RELATIONS {
                let shape = CallableShape {
                    name,
                    contexts: Vec::new(),
                    receiver: None,
                    params: vec![&operand, &operand],
                    vararg: None,
                    type_parameters: Vec::new(),
                    expect: false,
                    placement: Placement::Ordinary,
                };
                let signature = callable_signature(container, &shape)
                    .expect("a built-in relation's shape names no type parameter");
                let relation = BuiltinRelation {
                    operator,
                    operand: semantic_ty(&operand, &HashMap::new()),
                };
                relations.insert(signature, relation);
            }
        }
        Self { relations }
    }

    /// The relation declared under exactly `signature`, if it is one of this table's.
    pub(super) fn relation(&self, signature: &KlibPublicIdSignature) -> Option<BuiltinRelation> {
        self.relations.get(signature).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The member ids the Kotlin/Native 2.4.20 stdlib KLIB serializes for its calls to these
    /// operators.
    #[test]
    fn relations_are_signed_as_the_stdlib_calls_them() {
        let builtins = IrBuiltinOperators::new();
        let signed = builtins
            .relations
            .iter()
            .map(|(signature, relation)| {
                assert_eq!(signature.package().segments(), PACKAGE);
                assert_eq!(signature.mask(), 0);
                let [name] = signature.declaration().segments() else {
                    panic!("a built-in operator is a top-level declaration");
                };
                (
                    name.clone(),
                    relation.operand,
                    relation.operator,
                    signature.member_id().expect("a function has a member id"),
                )
            })
            .collect::<Vec<_>>();
        let stdlib = [
            ("less", Ty::Int, IrBinOp::Lt, 15_243_539_548_336_993_179),
            ("less", Ty::Long, IrBinOp::Lt, 11_229_592_551_388_186_192),
            ("less", Ty::Float, IrBinOp::Lt, 3_108_846_675_387_124_829),
            ("less", Ty::Double, IrBinOp::Lt, 6_515_268_044_613_540_054),
            ("greater", Ty::Int, IrBinOp::Gt, 10_306_676_650_683_172_728),
            ("greater", Ty::Long, IrBinOp::Gt, 14_989_184_280_288_315_174),
            ("greater", Ty::Float, IrBinOp::Gt, 9_724_039_108_225_158_906),
        ];
        for (name, operand, operator, member_id) in stdlib {
            assert!(
                signed.contains(&(name.to_owned(), operand, operator, member_id)),
                "{name}({operand:?}) is not signed {member_id}: {signed:?}"
            );
        }
        assert_eq!(signed.len(), RELATIONS.len() * OPERANDS.len());
    }
}
