//! A CLASSPATH TOP-LEVEL property (`val plugin: Plugin` in a dependency's file facade) was reported
//! "unresolved reference" at every use site: the classpath namespace record carried a package's
//! top-level FUNCTIONS and its EXTENSION properties, but never its receiver-less top-level properties,
//! so an explicit import, a star import, and a same-package reference all found nothing. The record now
//! carries them, and a read lowers to the facade's static getter. Runtime tests consume the
//! Krusty-built dependency; kotlinc is used only as an emission oracle where explicitly stated.
use super::common;

const LIB: &str = "package lib\n\
     class Plugin(val tag: String)\n\
     val plugin: Plugin = Plugin(\"installed\")\n\
     val counter: Int = 7\n\
     val absent: String? = null\n";

/// `HAS_CONSTANT` follows kotlinc's constant-value checker, not constant folding. A Java `val`
/// field, a string template that reads one, and `1.toLong()` carry the flag. `1 + 2`,
/// `File.separator + "z"`, a `const` string concatenated with `+`, a widened `Any`, and a `var` do
/// not. The facade, the class, and the object are each compared in full.
#[test]
fn constant_initializer_metadata_matches_kotlinc() {
    const SOURCE: &str = "package parity\n\
        const val A = \"a\"\n\
        val fromJava = java.io.File.separator\n\
        val fromJavaChar = java.io.File.separatorChar\n\
        val folded = \"a\" + \"b\"\n\
        val both = \"a\" + \"b\" + \"c\"\n\
        val template = \"x${java.io.File.separator}y\"\n\
        val templateConst = \"x${A}y\"\n\
        val templatePlus = \"x${A}y\" + \"z\"\n\
        val templateJavaPlus = \"x${java.io.File.separator}y\" + \"z\"\n\
        val arith = 1 + 2\n\
        val mixed = java.io.File.separator + \"z\"\n\
        val plusConst = A + \"b\"\n\
        val widened: Any = \"a\"\n\
        val widenedJava: Any = java.io.File.separator\n\
        val toLong = 1.toLong()\n\
        val charCode = java.io.File.separatorChar.toInt()\n\
        val unary = 1.unaryMinus()\n\
        val nested = -(1 + 2)\n\
        val convOfSum = (1 + 2).toLong()\n\
        val alias = A\n\
        val max = Integer.MAX_VALUE\n\
        var mutable = java.io.File.separator\n\
        val runtime = System.getProperty(\"user.dir\")\n\
        const val c = \"c\"\n\
        class Box {\n\
            val inst: String = java.io.File.separator\n\
            val folded = \"a\" + \"b\"\n\
            val arith = 1 + 2\n\
        }\n\
        object Holder {\n\
            val fromJava: String = java.io.File.separator\n\
        }\n";
    let classpath = [common::stdlib_jar(), common::jdk_modules()];
    for class in [
        "parity/ConstantInitializersKt",
        "parity/Box",
        "parity/Holder",
    ] {
        match common::byte_diff_against_kotlinc_cp(
            "ConstantInitializers",
            SOURCE,
            class,
            &classpath,
        ) {
            None => panic!("constant initializer metadata: reference toolchain unavailable"),
            Some(Ok(())) => {}
            Some(Err(error)) => panic!("{error}"),
        }
    }
}

/// Enforce the producer contract directly. Every property is emitted by Krusty; kotlinc only supplies
/// the expected bytes. Runtime and mutable initializers must omit `HAS_CONSTANT`; literal and signed-
/// literal `val`s must carry it. Any difference in the resulting facade is a hard parity failure.
#[test]
fn facade_property_constant_emission_is_byte_identical_to_kotlinc() {
    const SOURCE: &str = "package parity\n\
        class Token\n\
        val runtime = Token()\n\
        val literal = 7\n\
        val negative = -9\n\
        var mutable = 11\n\
        val absent: String? = null\n\
        var count: Int = 0\n\
        val disabled: Boolean = false\n";
    match common::byte_diff_against_kotlinc(
        "FacadePropertyMetadata",
        SOURCE,
        "parity/FacadePropertyMetadataKt",
    ) {
        None => panic!("facade property metadata: reference toolchain unavailable"),
        Some(Ok(())) => {}
        Some(Err(error)) => panic!("{error}"),
    }
}

#[test]
fn an_imported_classpath_top_level_property_reads() {
    let main = "import lib.plugin\n\
        fun box(): String {\n\
        \x20 if (plugin.tag != \"installed\") return \"fail: \" + plugin.tag\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cptoplevelprop", LIB, main);
}

#[test]
fn a_star_imported_classpath_top_level_property_reads() {
    let main = "import lib.*\n\
        fun box(): String {\n\
        \x20 if (counter != 7) return \"fail: \" + counter\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cptoplevelpropstar", LIB, main);
}

#[test]
fn a_classpath_top_level_property_keeps_its_declared_nullability() {
    let main = "import lib.absent\n\
        fun box(): String {\n\
        \x20 val length: Int = absent?.length ?: -1\n\
        \x20 if (length != -1) return \"fail: \" + length\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cptoplevelpropnullable", LIB, main);
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
    common::expect_box_ok_against("cptoplevelpropuse", LIB, main);
}

#[test]
fn an_inferred_property_reference_keeps_its_value_class_argument_in_metadata() {
    const LIBRARY: &str = "package lib\n\
        @JvmInline value class S(val text: String)\n\
        val value = S(\"OK\")\n\
        val reference = ::value.apply { }\n";
    const MAIN: &str = "import lib.*\n\
        fun box(): String = reference.call().text\n";
    let diagnostics = common::diagnostics_against(
        "classpath_inferred_property_reference_value_class",
        LIBRARY,
        MAIN,
    )
    .expect("Kotlin toolchain must be available");
    assert!(
        diagnostics.is_empty(),
        "unexpected diagnostics: {diagnostics:?}"
    );
}

/// Kotlin `internal` is SOURCE visibility even though its file-facade getter is public JVM bytecode.
/// Namespace discovery must filter on metadata visibility before selection; otherwise an explicit import
/// can read a dependency's internal state merely because the backend accessor happens to be invocable.
#[test]
fn a_classpath_internal_top_level_property_does_not_leak_through_its_public_getter() {
    const PRIVATE_LIB: &str = "package lib\ninternal val hiddenCounter: Int = 9\n";
    let main = "import lib.hiddenCounter\nfun use(): Int = hiddenCounter\n";
    let Some(diagnostics) =
        common::diagnostics_against("cptoplevelpropinternal", PRIVATE_LIB, main)
    else {
        return;
    };
    assert_eq!(diagnostics, ["unresolved reference 'hiddenCounter'."]);
}
