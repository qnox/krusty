//! Recognize scalar box/unbox adapters around an inlined lambda boundary.
//!
//! The splice may cancel only an adjacent wrapper pair. Recognition reads complete JVM owner,
//! member, and descriptor identities from the source or target constant pool.

use super::{class_name, name_and_type, ClassWriter, Insn, C};

/// The primitive descriptor consumed by a host `Wrapper.valueOf` call.
pub(super) fn boxing_call_primitive(src_cp: &[C], insn: &Insn) -> Option<char> {
    let Insn::Plain { op: 0xb8, operands } = insn else {
        return None;
    };
    let index = (u16::from(*operands.first()?) << 8) | u16::from(*operands.get(1)?);
    let (class, name, descriptor) = methodref_signature(src_cp, index)?;
    exact_box_accessor(class, name, descriptor)
}

fn wrapper_primitive(class: &str) -> Option<char> {
    Some(match class {
        "java/lang/Integer" => 'I',
        "java/lang/Long" => 'J',
        "java/lang/Short" => 'S',
        "java/lang/Byte" => 'B',
        "java/lang/Character" => 'C',
        "java/lang/Boolean" => 'Z',
        "java/lang/Float" => 'F',
        "java/lang/Double" => 'D',
        _ => return None,
    })
}

fn unboxes_through(class: &str, primitive: char) -> bool {
    wrapper_primitive(class) == Some(primitive)
        || (class == "java/lang/Number" && "IJSBFD".contains(primitive))
}

/// Number of leading target instructions that unbox `primitive`.
pub(super) fn leading_unboxing(cw: &ClassWriter, insns: &[Insn], primitive: char) -> usize {
    let mut at = 0;
    if let Some(Insn::Plain { op: 0xc0, operands }) = insns.first() {
        if let Some((high, low)) = operands.first().zip(operands.get(1)) {
            let index = (u16::from(*high) << 8) | u16::from(*low);
            if cw
                .class_name_at(index)
                .is_some_and(|name| unboxes_through(name, primitive))
            {
                at = 1;
            }
        }
    }
    let Some(Insn::Plain { op: 0xb6, operands }) = insns.get(at) else {
        return 0;
    };
    let Some((high, low)) = operands.first().zip(operands.get(1)) else {
        return 0;
    };
    let index = (u16::from(*high) << 8) | u16::from(*low);
    let Some((class, method, descriptor)) = cw.methodref_parts(index) else {
        return 0;
    };
    if exact_unbox_accessor(class, method, descriptor) == Some(primitive)
        && unboxes_through(class, primitive)
    {
        at + 1
    } else {
        0
    }
}

/// Whether a target instruction boxes `primitive`.
pub(super) fn is_target_boxing(cw: &ClassWriter, insn: &Insn, primitive: char) -> bool {
    let Insn::Plain { op: 0xb8, operands } = insn else {
        return false;
    };
    let Some((high, low)) = operands.first().zip(operands.get(1)) else {
        return false;
    };
    let index = (u16::from(*high) << 8) | u16::from(*low);
    cw.methodref_parts(index)
        .and_then(|(class, name, descriptor)| exact_box_accessor(class, name, descriptor))
        == Some(primitive)
}

/// An optional host checkcast plus its exact scalar accessor.
pub(super) fn host_unboxing(src_cp: &[C], insns: &[Insn]) -> Option<(char, usize)> {
    let mut at = 0;
    if let Some(Insn::Plain { op: 0xc0, operands }) = insns.first() {
        let index = (u16::from(*operands.first()?) << 8) | u16::from(*operands.get(1)?);
        if class_name(src_cp, index)
            .is_some_and(|name| name == "java/lang/Number" || wrapper_primitive(name).is_some())
        {
            at = 1;
        }
    }
    let Insn::Plain { op: 0xb6, operands } = insns.get(at)? else {
        return None;
    };
    let index = (u16::from(*operands.first()?) << 8) | u16::from(*operands.get(1)?);
    let (class, method, descriptor) = methodref_signature(src_cp, index)?;
    exact_unbox_accessor(class, method, descriptor).map(|primitive| (primitive, at + 1))
}

fn exact_unbox_accessor(class: &str, method: &str, descriptor: &str) -> Option<char> {
    Some(match (class, method, descriptor) {
        ("java/lang/Integer" | "java/lang/Number", "intValue", "()I") => 'I',
        ("java/lang/Long" | "java/lang/Number", "longValue", "()J") => 'J',
        ("java/lang/Short" | "java/lang/Number", "shortValue", "()S") => 'S',
        ("java/lang/Byte" | "java/lang/Number", "byteValue", "()B") => 'B',
        ("java/lang/Character", "charValue", "()C") => 'C',
        ("java/lang/Boolean", "booleanValue", "()Z") => 'Z',
        ("java/lang/Float" | "java/lang/Number", "floatValue", "()F") => 'F',
        ("java/lang/Double" | "java/lang/Number", "doubleValue", "()D") => 'D',
        _ => return None,
    })
}

fn exact_box_accessor(class: &str, method: &str, descriptor: &str) -> Option<char> {
    Some(match (class, method, descriptor) {
        ("java/lang/Integer", "valueOf", "(I)Ljava/lang/Integer;") => 'I',
        ("java/lang/Long", "valueOf", "(J)Ljava/lang/Long;") => 'J',
        ("java/lang/Short", "valueOf", "(S)Ljava/lang/Short;") => 'S',
        ("java/lang/Byte", "valueOf", "(B)Ljava/lang/Byte;") => 'B',
        ("java/lang/Character", "valueOf", "(C)Ljava/lang/Character;") => 'C',
        ("java/lang/Boolean", "valueOf", "(Z)Ljava/lang/Boolean;") => 'Z',
        ("java/lang/Float", "valueOf", "(F)Ljava/lang/Float;") => 'F',
        ("java/lang/Double", "valueOf", "(D)Ljava/lang/Double;") => 'D',
        _ => return None,
    })
}

fn methodref_signature(src_cp: &[C], index: u16) -> Option<(&str, &str, &str)> {
    let (class, name_and_type_index) = match src_cp.get(index as usize)? {
        C::Methodref(class, name_and_type) | C::InterfaceMethodref(class, name_and_type) => {
            (*class, *name_and_type)
        }
        _ => return None,
    };
    let (name, descriptor) = name_and_type(src_cp, name_and_type_index)?;
    Some((class_name(src_cp, class)?, name, descriptor))
}

#[cfg(test)]
mod tests {
    use super::{exact_box_accessor, exact_unbox_accessor};

    #[test]
    fn scalar_adapter_recognition_requires_complete_member_identity() {
        assert_eq!(
            exact_box_accessor("java/lang/Integer", "valueOf", "(I)Ljava/lang/Integer;"),
            Some('I')
        );
        assert_eq!(
            exact_unbox_accessor("java/lang/Number", "intValue", "()I"),
            Some('I')
        );
        assert_eq!(
            exact_unbox_accessor("java/lang/Number", "hashCode", "()I"),
            None
        );
        assert_eq!(
            exact_box_accessor(
                "java/lang/Integer",
                "valueOf",
                "(Ljava/lang/String;)Ljava/lang/Integer;"
            ),
            None
        );
    }
}
