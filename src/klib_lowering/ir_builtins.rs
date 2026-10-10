//! The compiler built-in operators a KLIB body calls.
//!
//! Kotlin's backend IR expresses a primitive relation (`a < b` on two `Int`s) or an equality
//! (`a == b`, `a === b`) as a call to a compiler built-in declaration in package
//! `kotlin.internal.ir` (Kotlin's `IrBuiltIns`), which no library declares: the compiler owns those
//! declarations. Boolean negation (`!b`) is a call to `kotlin.Boolean.not`, the language operator
//! the stdlib declares as a compiler intrinsic. This table is the closed set of these operators
//! that lowering realizes. Each entry's identity is computed by the same target-free mangler that
//! signs every library declaration, from the declaration's shape, so a body's call is joined to
//! an entry by exact signature only.

use std::collections::HashMap;

use crate::ir::IrBinOp;
use crate::metadata::id_signature::{
    callable_signature, CallableShape, ClassScope, DeclarationContainer, KlibPublicIdSignature,
    Placement,
};
use crate::metadata::semantic::{semantic_ty, KotlinFunctionTypeShape, KotlinType};
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

/// The operand types of `IrBuiltIns`' `ieee754equals`, as metadata class ids.
const FLOATING_POINT: [&str; 2] = ["kotlin/Float", "kotlin/Double"];

/// A relational operator over two operands of one primitive type, answering `Boolean`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct BuiltinRelation {
    pub(super) operator: IrBinOp,
    pub(super) operand: Ty,
}

/// One operator lowering realizes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BuiltinOperator {
    Relation(BuiltinRelation),
    /// `EQEQ(arg0: Any?, arg1: Any?): Boolean`, Kotlin's `==`.
    Equals,
    /// `EQEQEQ(arg0: Any?, arg1: Any?): Boolean`, Kotlin's `===`.
    Identical,
    /// `ieee754equals(arg0: T?, arg1: T?): Boolean` for a floating-point `T`: `==` on two
    /// operands whose static type is `T`, compared as IEEE 754 values.
    Ieee754Equals {
        operand: Ty,
    },
    /// `kotlin.Boolean.not(): Boolean`, Kotlin's `!`.
    Not,
}

/// The built-in operators lowering realizes, by exact signature.
pub(super) struct IrBuiltinOperators {
    operators: HashMap<KlibPublicIdSignature, BuiltinOperator>,
}

impl IrBuiltinOperators {
    pub(super) fn new() -> Self {
        let package = PACKAGE.map(str::to_owned);
        let builtins = DeclarationContainer::<KotlinType> {
            package: &package,
            classes: &[],
            native_interop_library: false,
        };
        let mut operators = HashMap::new();
        let mut declare = |container, name, params: Vec<&KotlinType>, operator| {
            let shape = CallableShape {
                name,
                contexts: Vec::new(),
                receiver: None,
                params,
                vararg: None,
                type_parameters: Vec::new(),
                expect: false,
                placement: Placement::Ordinary,
            };
            let signature = callable_signature(container, &shape)
                .expect("a built-in operator's shape names no type parameter");
            operators.insert(signature, operator);
        };
        for operand in OPERANDS {
            let operand = KotlinType::class(operand);
            for (name, operator) in RELATIONS {
                let relation = BuiltinRelation {
                    operator,
                    operand: semantic_ty(&operand, &HashMap::new()),
                };
                declare(
                    builtins,
                    name,
                    vec![&operand, &operand],
                    BuiltinOperator::Relation(relation),
                );
            }
        }
        let any = nullable("kotlin/Any");
        declare(builtins, "EQEQ", vec![&any, &any], BuiltinOperator::Equals);
        declare(
            builtins,
            "EQEQEQ",
            vec![&any, &any],
            BuiltinOperator::Identical,
        );
        for operand in FLOATING_POINT {
            let declared = nullable(operand);
            let operand = semantic_ty(&KotlinType::class(operand), &HashMap::new());
            declare(
                builtins,
                "ieee754equals",
                vec![&declared, &declared],
                BuiltinOperator::Ieee754Equals { operand },
            );
        }
        let kotlin = ["kotlin".to_owned()];
        let boolean = [ClassScope {
            name: "Boolean",
            type_parameters: Vec::new(),
            expect: false,
        }];
        let boolean = DeclarationContainer::<KotlinType> {
            package: &kotlin,
            classes: &boolean,
            native_interop_library: false,
        };
        declare(boolean, "not", Vec::new(), BuiltinOperator::Not);
        Self { operators }
    }

    /// The operator declared under exactly `signature`, if it is one of this table's.
    pub(super) fn operator(&self, signature: &KlibPublicIdSignature) -> Option<BuiltinOperator> {
        self.operators.get(signature).copied()
    }
}

/// The nullable type of the class `internal`, as the built-in declarations state their operands.
fn nullable(internal: &str) -> KotlinType {
    KotlinType::Class {
        internal: internal.to_owned(),
        args: Vec::new(),
        nullable: true,
        shape: KotlinFunctionTypeShape::default(),
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
            .operators
            .iter()
            .filter_map(|(signature, operator)| match operator {
                BuiltinOperator::Relation(relation) => Some((signature, relation)),
                _ => None,
            })
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

    /// The equality and negation operators, with the member ids the Kotlin/Native 2.4.20 stdlib
    /// KLIB serializes for its calls to them. Which `ieee754equals` id belongs to which operand
    /// type is checked against the stdlib's own calls (`stdlib_built_in_calls_have_the_signed_operands`).
    #[test]
    fn equalities_and_negation_are_signed_as_the_stdlib_calls_them() {
        let builtins = IrBuiltinOperators::new();
        let mut signed = builtins
            .operators
            .iter()
            .filter(|(_, operator)| !matches!(operator, BuiltinOperator::Relation(_)))
            .map(|(signature, operator)| {
                assert_eq!(signature.mask(), 0);
                (
                    signature.package().segments().join("."),
                    signature.declaration().segments().join("."),
                    signature.member_id().expect("a function has a member id"),
                    *operator,
                )
            })
            .collect::<Vec<_>>();
        signed.sort_by_key(|(_, name, member_id, _)| (name.clone(), *member_id));
        let ieee = |operand| BuiltinOperator::Ieee754Equals { operand };
        assert_eq!(
            signed,
            [
                (
                    "kotlin".to_owned(),
                    "Boolean.not".to_owned(),
                    8_206_408_119_829_444_045,
                    BuiltinOperator::Not
                ),
                (
                    "kotlin.internal.ir".to_owned(),
                    "EQEQ".to_owned(),
                    14_803_467_760_484_244_099,
                    BuiltinOperator::Equals
                ),
                (
                    "kotlin.internal.ir".to_owned(),
                    "EQEQEQ".to_owned(),
                    12_796_179_002_905_496_035,
                    BuiltinOperator::Identical
                ),
                (
                    "kotlin.internal.ir".to_owned(),
                    "ieee754equals".to_owned(),
                    4_100_363_687_899_162_055,
                    ieee(Ty::Double)
                ),
                (
                    "kotlin.internal.ir".to_owned(),
                    "ieee754equals".to_owned(),
                    13_180_658_955_541_877_466,
                    ieee(Ty::Float)
                ),
            ]
        );
    }
}
