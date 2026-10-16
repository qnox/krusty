//! Value-class type decisions shared by serialization declaration and body generation.

use super::element_serializer;
use crate::ir::IrFile;
use crate::types::{type_name, Ty};

pub(super) fn value_class_underlying(ir: &IrFile, ty: &Ty) -> Option<Ty> {
    ir.terminal_value_class_underlying(*ty)
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
}
