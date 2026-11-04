//! kotlinc's assignment compiler plugin, as krusty's native pass.
//!
//! `-Xplugin=assignment-compiler-plugin.jar -P plugin:org.jetbrains.kotlin.assignment:annotation=<fqname>`
//! makes `target = value`, where `target` is a non-local `val` property or a final Java field whose
//! type's class or a supertype carries the annotation, the call `target.assign(value)`. The call
//! resolves like any other: a member or an extension, with no `operator` modifier. Every class
//! krusty emits must equal the reference compiler's byte for byte, and `box()` must return `OK`.

use std::path::PathBuf;

use super::common;
use super::compiler_plugin_fixture::{assert_same_classes_and_box, plugin_switches, PluginFixture};

const ASSIGNMENT_ID: &str = "org.jetbrains.kotlin.assignment";

fn assignment_switches(annotation: &str) -> Vec<String> {
    plugin_switches(
        "assignment-compiler-plugin.jar",
        ASSIGNMENT_ID,
        &[&format!("annotation={annotation}")],
    )
}

/// Java declarations, built by javac for both compilers: an annotated `Text` with a member
/// `assign`, and a `Form` exposing one as a final field and one through a getter alone.
fn java_library() -> PathBuf {
    let sources = [
        (
            "Assign.java",
            "package j;\n\
             @java.lang.annotation.Retention(java.lang.annotation.RetentionPolicy.RUNTIME)\n\
             public @interface Assign {}\n",
        ),
        (
            "Text.java",
            "package j;\n\
             @Assign\n\
             public class Text {\n\
             \x20   public String value = \"\";\n\
             \x20   public void assign(String value) { this.value = value; }\n\
             }\n",
        ),
        (
            "Form.java",
            "package j;\n\
             public final class Form {\n\
             \x20   public final Text title = new Text();\n\
             \x20   private final Text label = new Text();\n\
             \x20   public Text getLabel() { return label; }\n\
             }\n",
        ),
    ]
    .map(|(name, text)| (name.to_string(), text.to_string()));
    common::javac_compile(&sources, &[])
        .map(|(dir, _)| dir)
        .expect("javac compiles the Java declarations")
}

const KOTLIN_MAIN: &str = r#"package k

annotation class Assignable

@Assignable class Prop { var v = ""; fun assign(value: String) { v += value } }

@Assignable open class Base { var log = "" }
class Child : Base()
fun Base.assign(value: Int) { log += value }

@Assignable interface Settable
class Impl : Settable { var v = "" }
fun Settable.assign(value: Char) { (this as Impl).v += value }

val top = Prop()

object Config { val p = Prop() }

class Holder {
    val p = Prop()
    val c = Child()
    val s: Settable = Impl()
    var q = Prop()
    init { p = "init" }
    companion object { val shared = Prop() }
}

fun Holder.fill() { p = "ext"; c = 4 }

fun scoped(h: Holder) { with(h) { p = "with"; s = 'y' } }

fun box(): String {
    top = "a"
    k.top = "b"
    Config.p = "c"
    Holder.shared = "d"
    val h = Holder()
    h.p = "b"
    h.fill()
    h.c = 5
    h.s = 'x'
    scoped(h)
    h.q = Prop()
    if (top.v != "ab") return "top=${top.v}"
    if (Config.p.v + Holder.shared.v != "cd") return "qualified=${Config.p.v}${Holder.shared.v}"
    if (h.p.v != "initbextwith") return "member=${h.p.v}"
    if (h.c.log != "45") return "supertype=${h.c.log}"
    val impl = h.s as Impl
    if (impl.v != "xy") return "interface=${impl.v}"
    return "OK"
}
"#;

/// Member and extension `assign`, an annotation inherited from a superclass or an interface, a
/// top-level `val` (also package-qualified), an object's and a companion's `val`, an initializer,
/// an extension's and a scope function's implicit receiver; and a `var`, which stays an ordinary
/// assignment.
#[test]
fn val_assignments_call_assign_like_kotlinc() {
    let fixture = PluginFixture::new("assignment");
    let sources = [("Main.kt", KOTLIN_MAIN)];
    let switches = assignment_switches("k.Assignable");
    let reference = fixture.kotlinc("main", &sources, &[], &switches);
    let krusty = fixture.krusty("main", &sources, &[], &switches);
    assert_same_classes_and_box(&reference, &krusty, &[common::stdlib_jar()]);
}

const JAVA_MAIN: &str = r#"import j.*

fun box(): String {
    val form = Form()
    form.title = "title"
    form.label = "label"
    if (form.title.value != "title") return "field=${form.title.value}"
    if (form.label.value != "label") return "getter=${form.label.value}"
    return "OK"
}
"#;

/// A final Java field and a Java getter without a setter, of a Java class carrying the annotation.
#[test]
fn java_fields_and_getters_call_assign_like_kotlinc() {
    let fixture = PluginFixture::new("assignment-java");
    let library = java_library();
    let sources = [("Main.kt", JAVA_MAIN)];
    let switches = assignment_switches("j.Assign");
    let classpath = [library.clone()];
    let reference = fixture.kotlinc("main", &sources, &classpath, &switches);
    let krusty = fixture.krusty("main", &sources, &classpath, &switches);
    assert_same_classes_and_box(&reference, &krusty, &[common::stdlib_jar(), library]);
}

const NON_UNIT_ASSIGN: &str = "package k\n\
annotation class Assignable\n\
@Assignable class R { fun assign(value: String): Int = 1 }\n\
@Assignable open class S { fun assign(value: String) = 1 }\n\
class T : S()\n\
fun T.assign(value: Int): String = \"\"\n\
@Assignable interface U { fun assign(value: Char): Int }\n\
class Holder { val r = R() }\n\
fun use(h: Holder) { h.r = \"s\" }\n";

/// An `assign` the plugin could call, on an annotated receiver, must return `Unit`: kotlinc rejects
/// the declaration at its result type (its name when inferred), and the assignment at its `=`.
#[test]
fn non_unit_assign_is_rejected_like_kotlinc() {
    let fixture = PluginFixture::new("assignment-non-unit");
    let sources = [("Main.kt", NON_UNIT_ASSIGN)];
    let switches = assignment_switches("k.Assignable");
    assert_eq!(
        fixture.kotlinc_rejects("main", &sources, &[], &switches),
        "<fixture>/main/src/Main.kt:3:50: error: function 'assign' used for '=' overload must return 'Unit'.\n\
         @Assignable class R { fun assign(value: String): Int = 1 }\n\
         \x20                                                ^^^\n\
         <fixture>/main/src/Main.kt:4:32: error: function 'assign' used for '=' overload must return 'Unit'.\n\
         @Assignable open class S { fun assign(value: String) = 1 }\n\
         \x20                              ^^^^^^\n\
         <fixture>/main/src/Main.kt:6:27: error: function 'assign' used for '=' overload must return 'Unit'.\n\
         fun T.assign(value: Int): String = \"\"\n\
         \x20                         ^^^^^^\n\
         <fixture>/main/src/Main.kt:7:52: error: function 'assign' used for '=' overload must return 'Unit'.\n\
         @Assignable interface U { fun assign(value: Char): Int }\n\
         \x20                                                  ^^^\n\
         <fixture>/main/src/Main.kt:9:26: error: function 'assign' used for '=' overload must return 'Unit'.\n\
         fun use(h: Holder) { h.r = \"s\" }\n\
         \x20                        ^\n"
    );
    assert_eq!(
        fixture.krusty_rejects("main-krusty", &sources, &[], &switches),
        "<fixture>/main-krusty/src/Main.kt:3:50: error: function 'assign' used for '=' overload must return 'Unit'.\n\
         <fixture>/main-krusty/src/Main.kt:4:32: error: function 'assign' used for '=' overload must return 'Unit'.\n\
         <fixture>/main-krusty/src/Main.kt:6:27: error: function 'assign' used for '=' overload must return 'Unit'.\n\
         <fixture>/main-krusty/src/Main.kt:7:52: error: function 'assign' used for '=' overload must return 'Unit'.\n\
         <fixture>/main-krusty/src/Main.kt:9:26: error: function 'assign' used for '=' overload must return 'Unit'.\n\
         krusty: 5 error(s)\n"
    );
}

const NO_APPLICABLE_MAIN: &str = r#"package k
annotation class Assignable
@Assignable class Bare
@Assignable class Member { fun assign(value: String) {} }
@Assignable class Ext
fun Ext.assign(value: String) {}
class Other
fun Other.assign(value: Int) {}
class Zed
fun <T : Zed> T.assign(value: T) {}
class Holder { val bare = Bare(); val member = Member(); val ext = Ext() }
fun use(h: Holder) {
    h.bare = 1
    h.member = 2
    h.ext = 3
}
"#;

const NO_APPLICABLE_SINGLE: &str = r#"package s
import k.Assignable
@Assignable class Lone
class Other
fun Other.assign(value: Int) {}
class Zed
fun <T : Zed> T.assign(value: T) {}
class Holder { val lone = Lone() }
fun use(h: Holder) { h.lone = "s" }
"#;

const NO_APPLICABLE_UNRESOLVED: &str = r#"package u
import k.Assignable
@Assignable class Lone
class Holder { val lone = Lone() }
fun use(h: Holder) { h.lone = 1 }
"#;

/// When `target.assign(value)` does not resolve, kotlinc reports the call's own diagnostics and
/// adds its error at the `=`: an argument mismatch of a member or extension `assign`; each
/// same-named extension a receiver type mismatch rejects, in declaration order, or only the one more
/// specific than the others (a non-generic one over a generic one); and an unresolved reference
/// when no `assign` is in scope.
#[test]
fn inapplicable_assign_is_rejected_like_kotlinc() {
    let fixture = PluginFixture::new("assignment-no-applicable");
    let sources = [
        ("Main.kt", NO_APPLICABLE_MAIN),
        ("Single.kt", NO_APPLICABLE_SINGLE),
        ("Unresolved.kt", NO_APPLICABLE_UNRESOLVED),
    ];
    let switches = assignment_switches("k.Assignable");
    assert_eq!(
        fixture.kotlinc_rejects("main", &sources, &[], &switches),
        "<fixture>/main/src/Main.kt:13:5: error: none of the following candidates is applicable:\n\
         \n\
         fun Ext.assign(value: String): Unit:\n\
         \x20 Candidate 'fun Ext.assign(value: String): Unit' is inapplicable because of a receiver type mismatch.\n\
         \n\
         fun Other.assign(value: Int): Unit:\n\
         \x20 Candidate 'fun Other.assign(value: Int): Unit' is inapplicable because of a receiver type mismatch.\n\
         \n\
         fun <T : Zed> T.assign(value: T): Unit:\n\
         \x20 Candidate 'fun <T : Zed> T.assign(value: T): Unit' is inapplicable because of a receiver type mismatch.\n\
         \x20   h.bare = 1\n\
         \x20   ^^^^^^\n\
         <fixture>/main/src/Main.kt:13:12: error: no applicable 'assign' function found for '=' overload.\n\
         \x20   h.bare = 1\n\
         \x20          ^\n\
         <fixture>/main/src/Main.kt:14:14: error: no applicable 'assign' function found for '=' overload.\n\
         \x20   h.member = 2\n\
         \x20            ^\n\
         <fixture>/main/src/Main.kt:14:16: error: argument type mismatch: actual type is 'Int', but 'String' was expected.\n\
         \x20   h.member = 2\n\
         \x20              ^\n\
         <fixture>/main/src/Main.kt:15:11: error: no applicable 'assign' function found for '=' overload.\n\
         \x20   h.ext = 3\n\
         \x20         ^\n\
         <fixture>/main/src/Main.kt:15:13: error: argument type mismatch: actual type is 'Int', but 'String' was expected.\n\
         \x20   h.ext = 3\n\
         \x20           ^\n\
         <fixture>/main/src/Single.kt:9:22: error: candidate 'fun Other.assign(value: Int): Unit' is inapplicable because of a receiver type mismatch.\n\
         fun use(h: Holder) { h.lone = \"s\" }\n\
         \x20                    ^^^^^^\n\
         <fixture>/main/src/Single.kt:9:29: error: no applicable 'assign' function found for '=' overload.\n\
         fun use(h: Holder) { h.lone = \"s\" }\n\
         \x20                           ^\n\
         <fixture>/main/src/Unresolved.kt:5:22: error: unresolved reference 'assign' on receiver of type 'Lone'.\n\
         fun use(h: Holder) { h.lone = 1 }\n\
         \x20                    ^^^^^^\n\
         <fixture>/main/src/Unresolved.kt:5:29: error: no applicable 'assign' function found for '=' overload.\n\
         fun use(h: Holder) { h.lone = 1 }\n\
         \x20                           ^\n"
    );
    assert_eq!(
        fixture.krusty_rejects("main-krusty", &sources, &[], &switches),
        "<fixture>/main-krusty/src/Main.kt:13:5: error: none of the following candidates is applicable:\n\
         \n\
         fun Ext.assign(value: String): Unit:\n\
         \x20 Candidate 'fun Ext.assign(value: String): Unit' is inapplicable because of a receiver type mismatch.\n\
         \n\
         fun Other.assign(value: Int): Unit:\n\
         \x20 Candidate 'fun Other.assign(value: Int): Unit' is inapplicable because of a receiver type mismatch.\n\
         \n\
         fun <T : Zed> T.assign(value: T): Unit:\n\
         \x20 Candidate 'fun <T : Zed> T.assign(value: T): Unit' is inapplicable because of a receiver type mismatch.\n\
         <fixture>/main-krusty/src/Main.kt:13:12: error: no applicable 'assign' function found for '=' overload.\n\
         <fixture>/main-krusty/src/Main.kt:14:14: error: no applicable 'assign' function found for '=' overload.\n\
         <fixture>/main-krusty/src/Main.kt:14:16: error: argument type mismatch: actual type is 'Int', but 'String' was expected.\n\
         <fixture>/main-krusty/src/Main.kt:15:11: error: no applicable 'assign' function found for '=' overload.\n\
         <fixture>/main-krusty/src/Main.kt:15:13: error: argument type mismatch: actual type is 'Int', but 'String' was expected.\n\
         <fixture>/main-krusty/src/Single.kt:9:22: error: candidate 'fun Other.assign(value: Int): Unit' is inapplicable because of a receiver type mismatch.\n\
         <fixture>/main-krusty/src/Single.kt:9:29: error: no applicable 'assign' function found for '=' overload.\n\
         <fixture>/main-krusty/src/Unresolved.kt:5:22: error: unresolved reference 'assign' on receiver of type 'Lone'.\n\
         <fixture>/main-krusty/src/Unresolved.kt:5:29: error: no applicable 'assign' function found for '=' overload.\n\
         krusty: 10 error(s)\n"
    );
}
