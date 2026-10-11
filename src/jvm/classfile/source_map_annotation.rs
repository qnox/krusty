//! The `@kotlin.jvm.internal.SourceDebugExtension` copy of a class's source map.
//!
//! kotlinc publishes the source map a second time as a BINARY-retained annotation, which is how a
//! Kotlin consumer reads it back without parsing the class file's own attribute. Its `value` is a
//! string array: one element, or, for a map longer than one `CONSTANT_Utf8` entry holds, the parts
//! kotlinc's `splitStringConstant` cuts it into.

use super::{u2, ConstPool};
use crate::jvm::string_constant::text_entry_parts;

const SOURCE_DEBUG_EXTENSION_DESC: &str = "Lkotlin/jvm/internal/SourceDebugExtension;";

impl ConstPool {
    /// Intern the annotation's constants in kotlinc's order and return its `annotation` structure.
    pub(super) fn source_map_annotation(&mut self, map: &str) -> Vec<u8> {
        let mut body = Vec::new();
        let annotation = self.utf8(SOURCE_DEBUG_EXTENSION_DESC);
        u2(&mut body, annotation);
        u2(&mut body, 1); // one element pair
        let name = self.utf8("value");
        u2(&mut body, name);
        body.push(b'[');
        let parts = text_entry_parts(map);
        u2(
            &mut body,
            u16::try_from(parts.len()).expect("a source map fits one class file"),
        );
        for part in &parts {
            body.push(b's');
            let value = self.utf8_kt(part);
            u2(&mut body, value);
        }
        body
    }
}
