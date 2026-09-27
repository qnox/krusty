//! An `enum class` that declares no entries is still an enum, as kotlinc compiles it: a private
//! `(String, int)` constructor, `$VALUES`/`$ENTRIES`, `values()`, `valueOf`, `getEntries` and a
//! static initializer. krusty took an empty entry list to mean an ordinary class and emitted none
//! of them, so `Empty.values()` failed with a NoSuchMethodError.

use super::common;

const SOURCE: &str = "\
    enum class Bare\n\
    enum class Braced {}\n\
    enum class WithMembers { ; fun answer(): Int = 42 }\n\
    ";

#[test]
fn an_entryless_enum_compiles_like_kotlinc() {
    for class in ["Bare", "Braced", "WithMembers"] {
        let pair = common::ModuleClassPair::compile(&[("Enums.kt", SOURCE)], class);
        assert!(
            pair.krusty == pair.kotlinc,
            "{class} differs from kotlinc's"
        );
    }
}

#[test]
fn an_entryless_enum_has_no_values_and_rejects_every_name() {
    let source = format!(
        "{SOURCE}\
        fun box(): String {{\n\
        \x20   for (value in Bare.values()) return \"bare\"\n\
        \x20   for (value in Braced.values()) return \"braced\"\n\
        \x20   for (value in WithMembers.values()) return \"members\"\n\
        \x20   try {{\n\
        \x20       Bare.valueOf(\"MISSING\")\n\
        \x20       return \"valueOf\"\n\
        \x20   }} catch (e: IllegalArgumentException) {{\n\
        \x20       return \"OK\"\n\
        \x20   }}\n\
        }}\n"
    );
    common::expect_box_same_as_kotlinc(&source, "EmptyEnum");
}
