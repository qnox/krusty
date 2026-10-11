//! Value-class type decisions shared by serialization declaration and body generation.

use super::element_serializer;
use crate::ir::{ExprId, IrConst, IrExpr, IrFile};
use crate::types::{type_name, Ty, TypeName};

pub(super) fn value_class_underlying(ir: &IrFile, ty: &Ty) -> Option<Ty> {
    ir.terminal_value_class_underlying(*ty)
}

/// The type a value-class element of declared type `ty` is serialized as: its terminal underlying
/// type, nullable when the element is, since a nullable element's null is the absent value.
pub(super) fn element_underlying(ir: &IrFile, ty: &Ty) -> Option<Ty> {
    let underlying = value_class_underlying(ir, ty)?;
    Some(if ty.is_nullable() {
        Ty::nullable(underlying)
    } else {
        underlying
    })
}

/// A value class's sole property and its declared type, whether this file or a checked provider
/// declares the class.
fn sole_property(ir: &IrFile, classifier: TypeName) -> Option<(String, Ty)> {
    match ir
        .classes
        .iter()
        .find(|class| class.is_value && class.fq_name == classifier)
    {
        Some(class) => class
            .fields
            .first()
            .map(|field| (field.name.clone(), field.ty)),
        None => ir
            .external_value_class_declaration(classifier)
            .map(|declaration| (declaration.property.to_string(), declaration.underlying)),
    }
}

/// The terminal underlying value an element of value-class type `ty` is serialized as: its sole
/// property read through every nested value class. A nullable element reads it only when the value
/// is present, so `null` stays the element's null. `read` produces the element value exactly once.
pub(super) fn underlying_read(
    ir: &mut IrFile,
    ty: Ty,
    temporary: u32,
    read: &mut dyn FnMut(&mut IrFile) -> Option<ExprId>,
) -> Option<ExprId> {
    let chain = |ir: &mut IrFile, mut value: ExprId| {
        let mut current = ty.non_null();
        while let Some(classifier) = current.obj_internal() {
            let Some((name, underlying)) = sole_property(ir, classifier) else {
                break;
            };
            value = ir.add_expr(IrExpr::PropertyRead {
                receiver: Some(value),
                owner: classifier,
                name,
                ty: underlying,
                interface: false,
                operation: None,
            });
            current = underlying.non_null();
        }
        value
    };
    let value = read(ir)?;
    if !ty.is_nullable() {
        return Some(chain(ir, value));
    }
    let declaration = ir.add_expr(IrExpr::Variable {
        index: temporary,
        ty,
        init: Some(value),
        named: false,
    });
    let value = ir.add_expr(IrExpr::GetValue(temporary));
    let null = ir.add_expr(IrExpr::Const(IrConst::Null));
    let present = ir.add_expr(IrExpr::PrimitiveBinOp {
        op: crate::ir::IrBinOp::Ne,
        lhs: value,
        rhs: null,
    });
    let receiver = ir.add_expr(IrExpr::GetValue(temporary));
    let underlying = chain(ir, receiver);
    let absent = ir.add_expr(IrExpr::Const(IrConst::Null));
    let selected = ir.add_expr(IrExpr::When {
        branches: vec![(Some(present), underlying), (None, absent)],
    });
    Some(ir.add_expr(IrExpr::Block {
        stmts: vec![declaration],
        value: Some(selected),
    }))
}

/// Plain `Encoder.encode*` / `Decoder.decode*` for a value class's underlying type (`encodeInt` /
/// `decodeInt`), as `(enc_name, enc_desc, dec_name, dec_desc)`. `None` for an unsupported underlying.
pub(super) fn inline_prim_methods(
    ty: &Ty,
) -> Option<(&'static str, &'static str, &'static str, &'static str)> {
    let classifier = element_serializer::builtin_element_key(ty)?;
    Some(if classifier == type_name("kotlin/Int") {
        ("encodeInt", "(I)V", "decodeInt", "()I")
    } else if classifier == type_name("kotlin/Long") {
        ("encodeLong", "(J)V", "decodeLong", "()J")
    } else if classifier == type_name("kotlin/Boolean") {
        ("encodeBoolean", "(Z)V", "decodeBoolean", "()Z")
    } else if classifier == type_name("kotlin/Double") {
        ("encodeDouble", "(D)V", "decodeDouble", "()D")
    } else if classifier == type_name("kotlin/Float") {
        ("encodeFloat", "(F)V", "decodeFloat", "()F")
    } else if classifier == type_name("kotlin/Char") {
        ("encodeChar", "(C)V", "decodeChar", "()C")
    } else if classifier == type_name("kotlin/Byte") {
        ("encodeByte", "(B)V", "decodeByte", "()B")
    } else if classifier == type_name("kotlin/Short") {
        ("encodeShort", "(S)V", "decodeShort", "()S")
    } else if classifier == type_name("kotlin/String") {
        (
            "encodeString",
            "(Ljava/lang/String;)V",
            "decodeString",
            "()Ljava/lang/String;",
        )
    } else {
        return None;
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cyclic_facts_do_not_select_an_arbitrary_underlying() {
        let first = type_name("fixture/First");
        let second = type_name("fixture/Second");
        let mut ir = IrFile::default();
        ir.insert_external_value_class_name(first, Ty::obj_name(second));
        ir.insert_external_value_class_name(second, Ty::obj_name(first));

        assert_eq!(value_class_underlying(&ir, &Ty::obj_name(first)), None);
    }

    #[test]
    fn a_nullable_underlying_read_evaluates_its_source_once() {
        let mut ir = IrFile::default();
        let mut reads = 0;
        let result = underlying_read(
            &mut ir,
            Ty::nullable(Ty::obj("fixture/Label")),
            7,
            &mut |ir| {
                reads += 1;
                Some(ir.add_expr(IrExpr::Const(IrConst::Null)))
            },
        )
        .expect("a source value produces an underlying read");

        assert_eq!(reads, 1);
        let IrExpr::Block { stmts, value } = ir.expr(result) else {
            panic!("a nullable read is guarded by a temporary block");
        };
        assert_eq!(stmts.len(), 1);
        assert!(value.is_some());
        assert!(matches!(
            ir.expr(stmts[0]),
            IrExpr::Variable {
                index: 7,
                init: Some(_),
                ..
            }
        ));
    }
}
