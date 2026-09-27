//! A `String` constant on the JVM operand stack.
//!
//! A class-file `CONSTANT_Utf8` holds at most 65535 bytes of modified UTF-8. kotlinc pushes a
//! longer constant, such as one folded from a concatenation of `const val`s, as a `StringBuilder`
//! sized to the whole string that appends the constant in parts, each filled greedily up to that
//! limit. A split may fall between the two halves of a surrogate pair; appending rejoins them.

use crate::jvm::classfile::{ClassWriter, CodeBuilder};
use crate::kt_string::{KtString, KtStringBuf};

/// The most modified-UTF-8 bytes one `CONSTANT_Utf8` entry holds.
const UTF8_ENTRY_LIMIT: usize = 65535;

pub(crate) fn push_string(value: &KtString, code: &mut CodeBuilder, cw: &mut ClassWriter) {
    let parts = split(value);
    let [single] = parts.as_slice() else {
        // Each constant is interned where the instruction using it is emitted, as kotlinc's
        // pool order follows its instruction stream.
        let builder = cw.class_ref("java/lang/StringBuilder");
        code.new_obj(builder);
        code.dup();
        let length = i32::try_from(value.len_utf16())
            .expect("a class-file string constant is shorter than i32::MAX units");
        code.push_int(length, cw);
        let sized = cw.methodref("java/lang/StringBuilder", "<init>", "(I)V");
        code.invokespecial(sized, 1, 0);
        for part in &parts {
            code.push_string_kt(part, cw);
            let append = cw.methodref(
                "java/lang/StringBuilder",
                "append",
                "(Ljava/lang/String;)Ljava/lang/StringBuilder;",
            );
            code.invokevirtual(append, 1, 1);
        }
        let to_string = cw.methodref(
            "java/lang/StringBuilder",
            "toString",
            "()Ljava/lang/String;",
        );
        code.invokevirtual(to_string, 0, 1);
        return;
    };
    code.push_string_kt(single, cw);
}

/// `value` cut into the fewest leading-greedy parts that each fit one `CONSTANT_Utf8` entry.
fn split(value: &KtString) -> Vec<KtString> {
    let mut parts = Vec::new();
    let mut part = KtStringBuf::new();
    let mut size = 0;
    for unit in value.units() {
        let unit_size = match unit {
            0x0001..=0x007f => 1,
            0x0000 | 0x0080..=0x07ff => 2,
            _ => 3,
        };
        if size + unit_size > UTF8_ENTRY_LIMIT {
            parts.push(std::mem::take(&mut part).finish());
            size = 0;
        }
        part.push_unit(unit);
        size += unit_size;
    }
    parts.push(part.finish());
    parts
}

#[cfg(test)]
mod tests {
    use super::split;
    use crate::kt_string::KtString;

    #[test]
    fn a_constant_within_one_entry_stays_whole() {
        let value = KtString::from_units(vec![u16::from(b'a'); 65535]);
        assert_eq!(split(&value), vec![value]);
    }

    #[test]
    fn parts_fill_each_entry_by_modified_utf8_size_even_inside_a_surrogate_pair() {
        // 21845 three-byte units fill 65535 bytes; the pair's low half starts the next part.
        let mut units = vec![0x0800; 21844];
        units.extend([0xd83c, 0xdf09, 0x0000]);
        let parts = split(&KtString::from_units(units));
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].len_utf16(), 21845);
        assert_eq!(parts[0].units().last(), Some(0xd83c));
        assert_eq!(parts[1].units().collect::<Vec<_>>(), vec![0xdf09, 0x0000]);
    }
}
