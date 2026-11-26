//! Value-class erasure of already-selected JVM descriptors.

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
                .then(|| krusty::types::existing_type_name(fq).and_then(|name| under.get(&name)))
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
