//! A CLASSPATH TOP-LEVEL property (`val plugin: Plugin` in a dependency's file facade) was reported
//! "unresolved reference" at every use site: the classpath namespace record carried a package's
//! top-level FUNCTIONS and its EXTENSION properties, but never its receiver-less top-level properties,
//! so an explicit import, a star import, and a same-package reference all found nothing. The record now
//! carries them, and a read lowers to the facade's static getter. Verified end-to-end on a real JVM
//! against a kotlinc-compiled dependency.
use super::common;

const LIB: &str = "package lib\n\
     class Plugin(val tag: String)\n\
     val plugin: Plugin = Plugin(\"installed\")\n\
     val counter: Int = 7\n\
     val absent: String? = null\n";

#[test]
fn an_imported_classpath_top_level_property_reads() {
    let main = "import lib.plugin\n\
        fun box(): String {\n\
        \x20 if (plugin.tag != \"installed\") return \"fail: \" + plugin.tag\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against_ref("cptoplevelprop", LIB, main);
}

#[test]
fn a_star_imported_classpath_top_level_property_reads() {
    let main = "import lib.*\n\
        fun box(): String {\n\
        \x20 if (counter != 7) return \"fail: \" + counter\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against_ref("cptoplevelpropstar", LIB, main);
}

#[test]
fn a_classpath_top_level_property_keeps_its_declared_nullability() {
    let main = "import lib.absent\n\
        fun box(): String {\n\
        \x20 val length: Int = absent?.length ?: -1\n\
        \x20 if (length != -1) return \"fail: \" + length\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against_ref("cptoplevelpropnullable", LIB, main);
}

#[test]
fn a_classpath_top_level_property_is_an_argument_and_a_receiver() {
    let main = "import lib.Plugin\n\
        import lib.plugin\n\
        fun name(p: Plugin): String = p.tag\n\
        fun box(): String {\n\
        \x20 if (name(plugin) != \"installed\") return \"fail arg\"\n\
        \x20 if (plugin.tag.uppercase() != \"INSTALLED\") return \"fail receiver\"\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against_ref("cptoplevelpropuse", LIB, main);
}

/// Kotlin `internal` is SOURCE visibility even though its file-facade getter is public JVM bytecode.
/// Namespace discovery must filter on metadata visibility before selection; otherwise an explicit import
/// can read a dependency's internal state merely because the backend accessor happens to be invocable.
#[test]
fn a_classpath_internal_top_level_property_does_not_leak_through_its_public_getter() {
    const PRIVATE_LIB: &str = "package lib\ninternal val hiddenCounter: Int = 9\n";
    let main = "import lib.hiddenCounter\nfun use(): Int = hiddenCounter\n";
    let Some(diagnostics) =
        common::diagnostics_against_ref("cptoplevelpropinternal", PRIVATE_LIB, main)
    else {
        return;
    };
    assert_eq!(diagnostics, ["unresolved reference 'hiddenCounter'."]);
}
