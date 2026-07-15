//! A file with BOTH a top-level property AND a companion (or object) `const val`. The companion const's
//! backing field is pushed onto `ir.statics` first, so a top-level property's `GetStatic` index must be
//! OFFSET past it — otherwise the property read resolves to the const's slot and emits a wrong-field
//! `getstatic` (e.g. reading a `String` property as the `Int` const → "Bad type on operand stack" at
//! load). Mission-core hit: `MissionChangeService`'s top-level `private val logger` read as the
//! companion's `const val HEX_RADIX` in every `logger.info { … }`.
//! Needs the JVM toolchain + kotlin-stdlib; skips otherwise.
use super::common;

#[test]
fn toplevel_prop_read_not_shadowed_by_companion_const() {
    const SRC: &str = "private val greeting: String = \"hi\"\n\
        class Svc {\n\
            companion object { const val K = 16 }\n\
            fun greetLen(): Int = greeting.length\n\
            fun kPlus(): Int = K + 1\n\
        }\n\
        fun box(): String {\n\
            val s = Svc()\n\
            return if (s.greetLen() == 2 && s.kPlus() == 17) \"OK\" else \"F: \" + s.greetLen() + \" \" + s.kPlus()\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "Main");
}
