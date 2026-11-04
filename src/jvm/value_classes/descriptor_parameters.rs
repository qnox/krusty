//! Physical parameter shapes read from already-selected JVM descriptors.

use super::{desc, erase, Under};

/// Erase the value-class types in a JVM method descriptor: each `L<fq>;` whose `<fq>` is a value class
/// becomes its underlying descriptor (`(LIv;)Ljava/lang/String;` → `(I)Ljava/lang/String;`).
pub(super) fn erase_descriptor(descriptor: &str, under: &Under) -> String {
    let bytes = descriptor.as_bytes();
    let mut out = String::with_capacity(descriptor.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'L' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j] != b';' {
                j += 1;
            }
            let fq = &descriptor[start..j];
            // A value class in a JVM reference-array component is necessarily BOXED. Erase a
            // declaration slot `LValue;` to its carrier, but never rewrite the component of
            // `[LValue;`: doing so silently changes `Array<UInt>` into `IntArray` and likewise turns
            // `Array<Data>` into the array-valued carrier of `Data`.
            let underlying = (bytes.get(i.wrapping_sub(1)) != Some(&b'['))
                .then(|| crate::types::existing_type_name(fq).and_then(|name| under.get(&name)))
                .flatten();
            if let Some(u) = underlying {
                out.push_str(&desc(&erase(u, under)));
            } else {
                out.push_str(&descriptor[i..=j]);
            }
            i = j + 1;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// Each parameter descriptor in `(…)ret`, including array depth and object terminators.
pub(super) fn types(descriptor: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = descriptor.as_bytes();
    let Some(end) = descriptor.find(')') else {
        return out;
    };
    let mut index = 1;
    while index < end {
        let start = index;
        while index < end && bytes[index] == b'[' {
            index += 1;
        }
        if index < end && bytes[index] == b'L' {
            while index < end && bytes[index] != b';' {
                index += 1;
            }
        }
        index += 1;
        out.push(descriptor[start..index].to_string());
    }
    out
}

/// Whether each descriptor parameter occupies a JVM reference slot.
pub(super) fn references(descriptor: &str) -> Vec<bool> {
    let mut out = Vec::new();
    let bytes = descriptor.as_bytes();
    let Some(end) = descriptor.find(')') else {
        return out;
    };
    let mut index = 1;
    while index < end {
        match bytes[index] {
            b'[' => {
                out.push(true);
                index += 1;
                while index < end && bytes[index] == b'[' {
                    index += 1;
                }
                if index < end && bytes[index] == b'L' {
                    while index < end && bytes[index] != b';' {
                        index += 1;
                    }
                }
                index += 1;
            }
            b'L' => {
                out.push(true);
                while index < end && bytes[index] != b';' {
                    index += 1;
                }
                index += 1;
            }
            _ => {
                out.push(false);
                index += 1;
            }
        }
    }
    out
}
