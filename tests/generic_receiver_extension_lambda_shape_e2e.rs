//! A lambda argument is shaped even when the extension's RECEIVER type is a type parameter.
//!
//! `fun <P : Pipe, B : Any> P.install(plugin: Plug<P, B>, configure: B.() -> Unit)` is the shape every
//! plugin-installing DSL uses. krusty shaped the `configure` lambda only when the extension's receiver
//! type was concrete; with a generic receiver, `B` was never substituted into the lambda's own type, so
//! the lambda kept the ENCLOSING receiver and every member reached through it was reported missing:
//!
//! ```text
//! error: unresolved reference 'json'.
//! ```
//!
//! The same call with the receiver passed as an ordinary parameter always worked, which is what made
//! this look like a lookup problem rather than a shaping one. Declaring `val c: Cfg = this` inside the
//! lambda is what named it: `initializer type mismatch: expected 'Cfg', actual 'App'`.

use super::common;

/// Compare the COMPLETE diagnostic set of both compilers — count, file, line, column, message and
/// order. A nonzero-exit or substring assertion passes on an unrelated rejection, which is how a
/// "both compilers agree" claim goes stale.
fn expect_identical_rejection(result: &common::CompilerDiagnosticResult, tag: &str) {
    let rendered = format!("{}{}", result.krusty_stdout, result.krusty_stderr);
    let krusty = common::compiler_errors(&rendered);
    let reference = common::compiler_errors(&result.reference_stderr);
    assert_ne!(
        result.reference_code, 0,
        "{tag}: kotlinc accepted the fixture: {}",
        result.reference_stderr
    );
    assert_ne!(result.krusty_code, 0, "{tag}: krusty accepted: {rendered}");
    assert!(
        !reference.is_empty(),
        "{tag}: kotlinc rejected with no parseable diagnostic: {}",
        result.reference_stderr
    );
    assert_eq!(
        krusty, reference,
        "{tag}: diagnostics differ.\nkrusty:  {krusty:#?}\nkotlinc: {reference:#?}"
    );
}

/// Run one fixture under BOTH compilers and require the same `box()` value.
///
/// `expect_box_ok_files_with_stdlib` alone only proves krusty agrees with itself. These shapes are
/// about matching the reference compiler, so it must run the identical source.
fn both_compilers_box(main: &str, stem: &str) {
    let reference = common::kotlinc_box_result(main);
    assert_eq!(
        reference, "OK",
        "{stem}: the reference compiler disagrees: {reference}"
    );
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", main)], stem);
}

const DECLARATIONS: &str = "interface Pipe\n\
\n\
interface Plug<P : Pipe, B : Any> {\n\
\x20   fun make(): B\n\
}\n\
\n\
class Cfg {\n\
\x20   var tag: String = \"none\"\n\
}\n\
\n\
class App : Pipe\n\
\n\
object Negotiation : Plug<Pipe, Cfg> {\n\
\x20   override fun make(): Cfg = Cfg()\n\
}\n\
\n\
fun Cfg.json() {\n\
\x20   tag = \"json\"\n\
}\n\
\n";

/// The failing shape: the extension's receiver is a type parameter and the lambda takes `B` as its
/// own receiver.
#[test]
fn a_generic_receiver_extension_still_shapes_its_lambda_receiver() {
    let main = format!(
        "{DECLARATIONS}\
fun <P : Pipe, B : Any> P.install(plugin: Plug<P, B>, configure: B.() -> Unit): B {{\n\
\x20   val built = plugin.make()\n\
\x20   built.configure()\n\
\x20   return built\n\
}}\n\
fun box(): String {{\n\
\x20   val cfg = App().install(Negotiation) {{ json() }}\n\
\x20   return if (cfg.tag == \"json\") \"OK\" else \"FAIL: \" + cfg.tag\n\
}}\n"
    );
    common::expect_box_ok_files_with_stdlib(
        &[("Main.kt", &main)],
        "generic_receiver_lambda_receiver",
    );
}

/// The same loss with an ordinary value parameter instead of a lambda receiver — so the defect is the
/// substitution of `B`, not the receiver convention.
#[test]
fn a_generic_receiver_extension_still_shapes_a_plain_lambda_parameter() {
    let main = format!(
        "{DECLARATIONS}\
fun <P : Pipe, B : Any> P.install(plugin: Plug<P, B>, configure: (B) -> Unit): B {{\n\
\x20   val built = plugin.make()\n\
\x20   configure(built)\n\
\x20   return built\n\
}}\n\
fun box(): String {{\n\
\x20   val cfg = App().install(Negotiation) {{ c -> c.json() }}\n\
\x20   return if (cfg.tag == \"json\") \"OK\" else \"FAIL: \" + cfg.tag\n\
}}\n"
    );
    both_compilers_box(&main, "generic_receiver_lambda_param");
}

/// The spelling that already worked: the same type parameters with the receiver passed as an ordinary
/// argument.
#[test]
fn an_ordinary_parameter_of_the_same_shape_still_shapes_its_lambda() {
    let main = format!(
        "{DECLARATIONS}\
fun <P : Pipe, B : Any> install(self: P, plugin: Plug<P, B>, configure: B.() -> Unit): B {{\n\
\x20   println(self)\n\
\x20   val built = plugin.make()\n\
\x20   built.configure()\n\
\x20   return built\n\
}}\n\
fun box(): String {{\n\
\x20   val cfg = install(App(), Negotiation) {{ json() }}\n\
\x20   return if (cfg.tag == \"json\") \"OK\" else \"FAIL: \" + cfg.tag\n\
}}\n"
    );
    both_compilers_box(&main, "ordinary_parameter_lambda");
}

/// The other spelling that already worked: a CONCRETE extension receiver, `B` still generic.
#[test]
fn a_concrete_receiver_extension_still_shapes_its_lambda() {
    let main = format!(
        "{DECLARATIONS}\
fun <B : Any> App.install(plugin: Plug<Pipe, B>, configure: B.() -> Unit): B {{\n\
\x20   val built = plugin.make()\n\
\x20   built.configure()\n\
\x20   return built\n\
}}\n\
fun box(): String {{\n\
\x20   val cfg = App().install(Negotiation) {{ json() }}\n\
\x20   return if (cfg.tag == \"json\") \"OK\" else \"FAIL: \" + cfg.tag\n\
}}\n"
    );
    both_compilers_box(&main, "concrete_receiver_lambda");
}

/// A member that the shaped receiver genuinely does not have is still rejected, so shaping cannot
/// start accepting anything. Both compilers' output is asserted.
#[test]
fn a_member_the_shaped_receiver_lacks_is_still_rejected() {
    let main = format!(
        "{DECLARATIONS}\
fun <P : Pipe, B : Any> P.install(plugin: Plug<P, B>, configure: B.() -> Unit): B {{\n\
\x20   val built = plugin.make()\n\
\x20   built.configure()\n\
\x20   return built\n\
}}\n\
fun bad() {{\n\
\x20   App().install(Negotiation) {{ noSuchMember() }}\n\
}}\n"
    );
    let result = common::compiler_diagnostics(&[("Main.kt", &main)], &[]);
    expect_identical_rejection(&result, "a member the shaped receiver lacks");
}
