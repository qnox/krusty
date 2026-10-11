//! A class whose source map outgrows one `CONSTANT_Utf8` entry (65535 bytes). Every inlined call
//! adds mapped lines, so a large generated client easily gets there. The map's
//! `@SourceDebugExtension` copy is then a string array of several parts, as kotlinc writes it; a
//! single oversized entry wrapped its 16-bit length and left a constant pool no JVM can read.
use super::common;

fn source() -> String {
    let mut source = String::from(
        "inline fun step(x: Int): Int {\n    val y = x + 1\n    return y\n}\nclass Client {\n",
    );
    for function in 0..100 {
        source.push_str(&format!(
            "    fun f{function}(x: Int): Int {{\n        var r = x\n"
        ));
        for _ in 0..60 {
            source.push_str("        r = step(r)\n");
        }
        source.push_str("        return r\n    }\n");
    }
    source.push_str("}\nfun box(): String = if (Client().f99(0) == 60) \"OK\" else \"fail\"\n");
    source
}

#[test]
fn a_source_map_longer_than_one_utf8_entry_is_written_in_parts() {
    common::expect_box_same_as_kotlinc(&source(), "SourceMapAnnotationParts");
}
