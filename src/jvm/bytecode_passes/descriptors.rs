//! Field and method descriptors (JVMS §4.3), split the way ASM's `Type` splits them.

/// The argument descriptors of a method descriptor, in order.
pub(crate) fn argument_types(descriptor: &str) -> Option<Vec<&str>> {
    let inner = descriptor.strip_prefix('(')?;
    let close = inner.find(')')?;
    let mut rest = &inner[..close];
    let mut arguments = Vec::new();
    while !rest.is_empty() {
        let length = field_type_length(rest)?;
        arguments.push(&rest[..length]);
        rest = &rest[length..];
    }
    Some(arguments)
}

/// The return descriptor of a method descriptor (`V` for `void`).
pub(crate) fn return_type(descriptor: &str) -> Option<&str> {
    let close = descriptor.find(')')?;
    let ret = &descriptor[close + 1..];
    (!ret.is_empty()).then_some(ret)
}

/// Words a value of this field descriptor occupies (`0` for `V`).
pub(crate) fn size(descriptor: &str) -> usize {
    match descriptor.as_bytes().first() {
        Some(b'J' | b'D') => 2,
        Some(b'V') | None => 0,
        _ => 1,
    }
}

/// Whether the descriptor names a reference (a class or an array).
pub(crate) fn is_reference(descriptor: &str) -> bool {
    matches!(descriptor.as_bytes().first(), Some(b'L' | b'['))
}

/// The descriptor of an internal name as a `CONSTANT_Class` names it: an array class already is one.
pub(crate) fn of_internal_name(internal: &str) -> String {
    if internal.starts_with('[') {
        internal.to_string()
    } else {
        format!("L{internal};")
    }
}

/// The internal name a reference descriptor stands for (`[I` stays an array descriptor, as ASM's
/// `Type.getInternalName` returns it).
pub(crate) fn internal_name(descriptor: &str) -> &str {
    descriptor
        .strip_prefix('L')
        .and_then(|rest| rest.strip_suffix(';'))
        .unwrap_or(descriptor)
}

/// The element descriptor of an array descriptor, ASM's `AsmUtil.correctElementType`.
pub(crate) fn element_type(descriptor: &str) -> Option<&str> {
    descriptor.strip_prefix('[')
}

fn field_type_length(descriptor: &str) -> Option<usize> {
    let bytes = descriptor.as_bytes();
    let mut index = 0;
    while bytes.get(index) == Some(&b'[') {
        index += 1;
    }
    match bytes.get(index)? {
        b'L' => Some(descriptor[index..].find(';')? + index + 1),
        b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z' => Some(index + 1),
        _ => None,
    }
}

/// The descriptor of an int-like primitive narrower than `int` (`boolean`, `byte`, `char`,
/// `short`), which an `int` must be coerced to (ASM's `Type.isIntLike` sorts other than `INT`).
pub(crate) fn int_like(descriptor: &str) -> Option<&'static str> {
    match descriptor {
        "Z" => Some("Z"),
        "B" => Some("B"),
        "C" => Some("C"),
        "S" => Some("S"),
        _ => None,
    }
}

/// ASM's `Type.getOpcode` for a load or store: `int_opcode` (`iload` or `istore`) shifted to the
/// variant of the field descriptor's kind.
pub(crate) fn typed_opcode(descriptor: &str, int_opcode: u8) -> u8 {
    int_opcode
        + match descriptor.as_bytes().first() {
            Some(b'J') => 1,
            Some(b'F') => 2,
            Some(b'D') => 3,
            Some(b'L' | b'[') => 4,
            _ => 0,
        }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_method_descriptor_splits_into_its_arguments() {
        assert_eq!(
            argument_types(
                "(I[Ljava/lang/String;J[[DLkotlin/coroutines/Continuation;)Ljava/lang/Object;"
            ),
            Some(vec![
                "I",
                "[Ljava/lang/String;",
                "J",
                "[[D",
                "Lkotlin/coroutines/Continuation;"
            ])
        );
        assert_eq!(return_type("()V"), Some("V"));
        assert_eq!(size("J"), 2);
        assert_eq!(internal_name("Ljava/lang/Object;"), "java/lang/Object");
        assert_eq!(of_internal_name("[I"), "[I");
    }
}
