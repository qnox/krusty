//! Shapes that dropped whole files from a real project's test source sets: a parse error in one
//! file removes every declaration in it, and every sibling that names them fails with
//! `unresolved reference`. Each case is accepted by kotlinc 2.4.10.

use super::common;

fn diagnostics(src: &str) -> Vec<String> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    common::front_end_diagnostics(src, std::slice::from_ref(&stdlib), Some(jdk.as_path()))
}

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

/// `enum class E(...) { A(...), B(...), ; companion object { ... } }` — the entry list closed by
/// `;` on its own line, then a companion. Parsed as `object <name>` and rejected.
#[test]
fn an_enum_companion_after_the_entry_terminator_parses() {
    let src = "enum class Family(val hostRouting: Boolean) {\n\
                   HTTP(hostRouting = true),\n\
                   TCP(hostRouting = false),\n\
                   ;\n\
               \n\
                   companion object {\n\
                       private val HTTP_FAMILY = setOf(\"http\", \"https\")\n\
                       fun classify(p: String): Family = if (p in HTTP_FAMILY) HTTP else TCP\n\
                   }\n\
               }\n\
               fun box(): String = if (Family.classify(\"https\") == Family.HTTP && !Family.TCP.hostRouting) \"OK\" else \"FAIL\"\n";
    assert_eq!(diagnostics(src), Vec::<String>::new());
    assert_eq!(run(src).as_deref(), Some("OK"));
}

/// `target op= value` where the target is not assignable (`m[k]!! += x`, `(m[k]!!) += x`) is
/// an operator call (`plusAssign`) on the value, not an assignment.
#[test]
fn a_compound_assignment_on_a_non_assignable_expression_is_an_operator_call() {
    let src = "fun box(): String {\n\
                   val m = mutableMapOf(\"k\" to mutableSetOf<String>())\n\
                   m[\"k\"]!! += \"x\"\n\
                   (m[\"k\"]!!) += \"y\"\n\
                   m.getValue(\"k\") += \"z\"\n\
                   return if (m[\"k\"] == setOf(\"x\", \"y\", \"z\")) \"OK\" else \"FAIL ${m[\"k\"]}\"\n\
               }\n";
    assert_eq!(diagnostics(src), Vec::<String>::new());
    assert_eq!(run(src).as_deref(), Some("OK"));
}

/// An `Int` literal for a `Long` parameter of a classpath companion function whose sibling
/// parameter is defaulted (`Instant.fromEpochSeconds(0)`).
#[test]
fn an_int_literal_selects_a_long_parameter_beside_a_defaulted_one() {
    // As in the real site the call sits in SIGNATURE position — the initializer of a function with
    // an inferred return type — so the Pass-1 signature solver selects it, not the body checker.
    // Two overloads exist, `(Long, Int = 0)` and `(Long, Long)` without a default; only the first
    // applies to one argument.
    let src = "import kotlin.time.Instant\n\
               data class Stamp(val name: String, val createdAt: Instant)\n\
               private fun stamp() = Stamp(name = \"x\", createdAt = Instant.fromEpochSeconds(0))\n\
               private fun later() = Instant.fromEpochSeconds(1_700_000_000)\n\
               fun box(): String =\n\
                   if (stamp().createdAt.epochSeconds == 0L && later().epochSeconds == 1_700_000_000L) \"OK\" else \"FAIL\"\n";
    assert_eq!(diagnostics(src), Vec::<String>::new());
    assert_eq!(run(src).as_deref(), Some("OK"));
}
