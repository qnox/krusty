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
    both_compilers_box(&main, "generic_receiver_lambda_receiver");
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
    common::expect_identical_rejection(&result, "a member the shaped receiver lacks");
}

/// Receiver and argument each supply a LOWER bound for the same formal, and neither is the other.
///
/// This is the case a separate receiver unification cannot express: whichever ran last would win,
/// binding `T` to `App` or to `Cfg`. Solved together, `T` takes the join of both. The call only
/// compiles if that join is reached, so acceptance is the assertion.
#[test]
fn a_receiver_and_an_argument_join_into_one_formal() {
    let main = format!(
        "{DECLARATIONS}\
fun <T : Any> T.pair(other: T): String = \"paired\"\n\
fun box(): String {{\n\
\x20   val joined = App().pair(Cfg())\n\
\x20   return if (joined == \"paired\") \"OK\" else \"FAIL: \" + joined\n\
}}\n"
    );
    both_compilers_box(&main, "receiver_argument_join");
}

/// The formal appears inside a VARIANT shell on the argument side. `List<out T>` admits a
/// `List<Pipe>` for `T = Pipe`, and the receiver `App` is assignable to that — so the shell must be
/// read as a lower bound too, not turned into an equation that pins `T` to `App`.
#[test]
fn a_formal_inside_a_variant_shell_still_admits_the_receiver() {
    let main = format!(
        "{DECLARATIONS}\
fun <T : Any> T.among(items: List<T>): Int = items.size\n\
fun box(): String {{\n\
\x20   val seen = App().among(listOf<Pipe>(App(), App()))\n\
\x20   return if (seen == 2) \"OK\" else \"FAIL: \" + seen\n\
}}\n"
    );
    both_compilers_box(&main, "variant_shell_receiver");
}

/// An INVARIANT argument fixes the formal to something the receiver is not. Solving receiver and
/// arguments together must still REJECT this — a lower bound is not permission to ignore the
/// argument.
///
/// Both compilers reject, and their diagnostics are recorded exactly rather than compared, because
/// they disagree about more than wording: kotlinc keeps `P` bound from the RECEIVER and reports the
/// argument against `Plug<App, …>`, while krusty binds `P` from the formal's bound and reports it
/// against `Plug<Pipe, Any>`. That divergence is not introduced here — it is what the rejecting
/// path does on both sides of this change — and converging it is its own work. What this fixture
/// pins is that the call is refused, and by exactly these diagnostics, so a future change to the
/// solve cannot start accepting it unnoticed.
#[test]
fn a_receiver_the_invariant_argument_excludes_is_still_rejected() {
    let main = format!(
        "{DECLARATIONS}\
class Other : Pipe\n\
\n\
object OtherPlug : Plug<Other, Cfg> {{\n\
\x20   override fun make(): Cfg = Cfg()\n\
}}\n\
\n\
fun <P : Pipe, B : Any> P.install(plugin: Plug<P, B>, configure: B.() -> Unit) {{\n\
\x20   plugin.make().configure()\n\
}}\n\
fun probe() {{\n\
\x20   App().install(OtherPlug) {{ json() }}\n\
}}\n"
    );
    let result = common::compiler_diagnostics(&[("Main.kt", &main)], &[]);
    assert_eq!((result.krusty_code, result.reference_code), (1, 1));
    assert_eq!(common::compiler_errors(&result.krusty_stdout), []);
    assert_eq!(
        common::compiler_errors(&result.krusty_stderr),
        [
            common::CompilerError {
                file: "Main.kt".to_string(),
                line: 31,
                column: 19,
                message: "argument type mismatch: actual type is 'OtherPlug', but \
                          'Plug<Pipe, Any>' was expected."
                    .to_string(),
            },
            common::CompilerError {
                file: "Main.kt".to_string(),
                line: 31,
                column: 32,
                message: "unresolved reference 'json'.".to_string(),
            },
        ]
    );
    assert_eq!(
        common::compiler_errors(&result.reference_stderr),
        [
            common::CompilerError {
                file: "Main.kt".to_string(),
                line: 31,
                column: 11,
                message: "cannot infer type for type parameter 'B'. Specify it explicitly."
                    .to_string(),
            },
            common::CompilerError {
                file: "Main.kt".to_string(),
                line: 31,
                column: 19,
                message: "argument type mismatch: actual type is 'OtherPlug', but \
                          'Plug<App, uninferred B (of fun <P : Pipe, B : Any> P.install)>' was \
                          expected."
                    .to_string(),
            },
            common::CompilerError {
                file: "Main.kt".to_string(),
                line: 31,
                column: 30,
                message: "cannot infer type for type parameter 'B'. Specify it explicitly."
                    .to_string(),
            },
        ]
    );
}

/// EXPLICIT type arguments fix the formals outright, so neither the receiver nor the argument may
/// move them.
#[test]
fn explicit_type_arguments_still_fix_both_formals() {
    let main = format!(
        "{DECLARATIONS}\
fun <P : Pipe, B : Any> P.install(plugin: Plug<P, B>, configure: B.() -> Unit) {{\n\
\x20   plugin.make().configure()\n\
}}\n\
fun box(): String {{\n\
\x20   val cfg = Cfg()\n\
\x20   App().install<Pipe, Cfg>(Negotiation) {{ json() }}\n\
\x20   cfg.json()\n\
\x20   return if (cfg.tag == \"json\") \"OK\" else \"FAIL: \" + cfg.tag\n\
}}\n"
    );
    both_compilers_box(&main, "explicit_type_arguments");
}

/// The formal's declared BOUND still constrains a receiver that is the only evidence for it: a
/// receiver outside the bound is rejected rather than widening the formal to admit it.
///
/// Both compilers reject at the same position; their complete diagnostics are pinned independently
/// while krusty still calls the reference unresolved.
#[test]
fn a_receiver_outside_the_formals_bound_is_still_rejected() {
    let main = format!(
        "{DECLARATIONS}\
fun <T : Pipe> T.piped(): String = \"piped\"\n\
fun probe() {{\n\
\x20   Cfg().piped()\n\
}}\n"
    );
    let result = common::compiler_diagnostics(&[("Main.kt", &main)], &[]);
    assert_eq!((result.krusty_code, result.reference_code), (1, 1));
    assert_eq!(common::compiler_errors(&result.krusty_stdout), []);
    assert_eq!(
        common::compiler_errors(&result.krusty_stderr),
        [common::CompilerError {
            file: "Main.kt".to_string(),
            line: 23,
            column: 11,
            message: krusty::diagnostic_wording::unresolved_reference_on("piped", Some("Cfg")),
        }]
    );
    let reference = common::compiler_errors(&result.reference_stderr);
    assert_eq!(reference.len(), 1, "{}", result.reference_stderr);
    assert_eq!(
        (
            reference[0].file.as_str(),
            reference[0].line,
            reference[0].column
        ),
        ("Main.kt", 23, 11)
    );
    assert_eq!(
        vec![reference[0].message.clone()],
        common::recorded_named(
            "a_receiver_outside_the_formals_bound_is_still_rejected",
            || { vec![reference[0].message.clone()] }
        )
    );
}
