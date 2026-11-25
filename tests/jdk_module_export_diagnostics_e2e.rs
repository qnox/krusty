//! A JDK package its module does not export to code outside every named module is not nameable from
//! Kotlin source: kotlinc reports `symbol is declared in module 'M' which does not export package
//! 'p'.` at each reference. Both compilers read the module descriptors of the running JDK, so every
//! case compares krusty's complete diagnostics with kotlinc's.

use super::common;
use super::diagnostics_parity_support::{errors, ObservedError};

fn assert_same_diagnostics(source: &str, expected: &[(usize, usize, &str, &str)]) {
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("ModuleExports.kt", source)],
        std::slice::from_ref(&stdlib),
    );
    let expected: Vec<ObservedError> = expected
        .iter()
        .map(|&(line, column, module, package)| ObservedError {
            file: "ModuleExports.kt".to_string(),
            line,
            column,
            message: format!(
                "symbol is declared in module '{module}' which does not export package '{package}'."
            ),
        })
        .collect();
    let mut krusty = errors(&result.krusty_stderr);
    krusty.extend(errors(&result.krusty_stdout));
    assert_eq!(errors(&result.reference_stderr), expected, "kotlinc");
    assert_eq!(krusty, expected, "krusty");
}

#[test]
fn a_type_in_an_unexported_package_is_rejected_at_its_reference() {
    assert_same_diagnostics(
        "val field: sun.jvmstat.monitor.Monitor? = null\n\
         fun parameters(x: sun.nio.ch.DirectBuffer, l: List<sun.nio.ch.DirectBuffer>): sun.nio.ch.DirectBuffer = x\n",
        &[
            (1, 12, "jdk.internal.jvmstat", "sun.jvmstat.monitor"),
            (2, 19, "java.base", "sun.nio.ch"),
            (2, 52, "java.base", "sun.nio.ch"),
            (2, 79, "java.base", "sun.nio.ch"),
        ],
    );
}

#[test]
fn a_supertype_and_type_tests_name_the_unexported_package() {
    assert_same_diagnostics(
        "abstract class Impl : sun.nio.ch.Interruptible\n\
         fun test(s: Any) {\n\
             if (s is sun.nio.ch.DirectBuffer) {}\n\
             s as sun.nio.ch.DirectBuffer\n\
         }\n",
        &[
            (1, 23, "java.base", "sun.nio.ch"),
            (3, 10, "java.base", "sun.nio.ch"),
            (4, 6, "java.base", "sun.nio.ch"),
        ],
    );
}

#[test]
fn an_import_is_checked_at_the_reference_that_uses_it() {
    assert_same_diagnostics(
        "import sun.swing.ImageIconUIResource\n\
         import sun.nio.ch.DirectBuffer as Unused\n\
         fun use(x: ImageIconUIResource?) {}\n",
        &[(3, 12, "java.desktop", "sun.swing")],
    );
}

#[test]
fn a_package_the_module_exports_only_to_named_modules_is_rejected() {
    // `jdk.internal.jvmstat` exports `sun.jvmstat.monitor` only to `jdk.jcmd` and others; the
    // exported `java.sql`, the incubator's `jdk.incubator.vector` and the unqualified
    // `sun.misc` of `jdk.unsupported` stay reachable.
    assert_same_diagnostics(
        "val hidden: sun.jvmstat.monitor.Monitor? = null\n\
         val sql: java.sql.Connection? = null\n\
         val unsupported: sun.misc.Unsafe? = null\n\
         val incubator: jdk.incubator.vector.Vector<*>? = null\n",
        &[(1, 13, "jdk.internal.jvmstat", "sun.jvmstat.monitor")],
    );
}

#[test]
fn a_class_named_in_an_expression_and_its_static_member_are_rejected() {
    assert_same_diagnostics(
        "fun expressions() {\n\
         val call = sun.swing.SwingUtilities2.isLocalDisplay()\n\
         val literal = sun.swing.SwingUtilities2::class\n\
         val vm = jdk.internal.misc.VM.isBooted()\n\
         }\n",
        &[
            (2, 12, "java.desktop", "sun.swing"),
            (2, 38, "java.desktop", "sun.swing"),
            (3, 15, "java.desktop", "sun.swing"),
            (4, 10, "java.base", "jdk.internal.misc"),
            (4, 31, "java.base", "jdk.internal.misc"),
        ],
    );
}

#[test]
fn a_constant_read_and_a_callable_reference_of_an_unexported_class_are_rejected() {
    assert_same_diagnostics(
        "fun members() {\n\
         val field = sun.nio.ch.IOStatus.EOF\n\
         val reference = jdk.internal.misc.VM::isBooted\n\
         }\n",
        &[
            (2, 13, "java.base", "sun.nio.ch"),
            (2, 33, "java.base", "sun.nio.ch"),
            (3, 17, "java.base", "jdk.internal.misc"),
            (3, 39, "java.base", "jdk.internal.misc"),
        ],
    );
}

#[test]
fn a_suppressed_declaration_names_an_unexported_package_in_its_signature_and_supertypes() {
    // The suppression covers the declaration's own signature types, its supertype reference and
    // its superclass constructor call; an unsuppressed sibling still reports both of the latter.
    assert_same_diagnostics(
        "@Suppress(\"JAVA_MODULE_DOES_NOT_EXPORT_PACKAGE\")\n\
         fun signature(x: sun.nio.ch.DirectBuffer): sun.nio.ch.DirectBuffer = x\n\
         @Suppress(\"JAVA_MODULE_DOES_NOT_EXPORT_PACKAGE\")\n\
         class Suppressed : sun.net.www.MessageHeader()\n\
         class Reported : sun.net.www.MessageHeader()\n",
        &[
            (5, 18, "java.base", "sun.net.www"),
            (5, 37, "java.base", "sun.net.www"),
        ],
    );
}
