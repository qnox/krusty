//! A `const val` inside an `object`, read from one of the object's own methods (unqualified `MAX` or
//! qualified `Config.MAX`), and read externally (`Config.MAX`). Before, an object carrying BOTH a
//! `const val` and a method was gated out of the IR backend ("gate:object") because krusty didn't model
//! const-inlining — a method read could observe an uninitialized backing field. Now every literal-valued
//! object `const val` is inlined at each read site (like kotlinc), so the read is a constant and the
//! object lowers. Same-file only (no classpath), runnable directly.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn const_val_in_object_read_from_method_and_externally() {
    const SRC: &str = "object Config {\n\
        \x20 const val MAX = 63\n\
        \x20 private const val MIN = 1\n\
        \x20 const val NAME = \"cfg\"\n\
        \x20 fun span(): Int = MAX - MIN\n\
        \x20 fun qualified(): Int = Config.MAX\n\
        \x20 fun label(): String = NAME\n\
        }\n\
        fun box(): String {\n\
        \x20 if (Config.span() != 62) return \"fail span: ${Config.span()}\"\n\
        \x20 if (Config.qualified() != 63) return \"fail qualified\"\n\
        \x20 if (Config.label() != \"cfg\") return \"fail label\"\n\
        \x20 if (Config.MAX != 63) return \"fail external\"\n\
        \x20 if (Config.NAME != \"cfg\") return \"fail external-str\"\n\
        \x20 return \"OK\"\n\
        }\n";
    assert_eq!(run(SRC).expect("object const val"), "OK");
}
