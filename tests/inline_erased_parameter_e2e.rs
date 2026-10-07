//! An inlined type parameter is stored as its erased bound.
//!
//! `T : AutoCloseable?` occupies an `AutoCloseable` slot. `this?.close()` invokes that bound
//! directly: a non-null call-site type has no null check and no `checkcast`, and a nullable one
//! keeps `dup; ifnull` on the same slot. `T : Comparable<T>` is stored as `Comparable` and
//! `checkcast` back to `String` only at `length`. An unconstrained `T` stays the specialized
//! reference, so `String.length` has no cast.

use super::common;
use super::common::{compare_with_kotlinc_plugin_jdk, method_instructions};
use super::temporary_elimination_e2e::stack_map;

const SOURCE: &str = "inline fun <T : java.lang.AutoCloseable?, R> T.use(block: (T) -> R): R {\n\
    \x20   var closed = false\n\
    \x20   try {\n\
    \x20       return block(this)\n\
    \x20   } catch (e: Exception) {\n\
    \x20       closed = true\n\
    \x20       try {\n\
    \x20           this?.close()\n\
    \x20       } catch (closeException: Exception) {\n\
    \x20       }\n\
    \x20       throw e\n\
    \x20   } finally {\n\
    \x20       if (!closed) {\n\
    \x20           this?.close()\n\
    \x20       }\n\
    \x20   }\n\
    }\n\
    \n\
    inline fun <T : Comparable<T>> id(x: T): T = x\n\
    inline fun <T> plainId(x: T): T = x\n\
    \n\
    fun load(reader: java.io.BufferedReader) {\n\
    \x20   reader.use { r -> r.read() }\n\
    }\n\
    \n\
    fun loadNull(reader: java.io.BufferedReader?) {\n\
    \x20   reader.use { }\n\
    }\n\
    \n\
    fun go(s: String) = id(s).length\n\
    fun plain(s: String) = plainId(s).length\n";

#[test]
fn an_inlined_type_parameter_is_stored_as_its_erased_bound() {
    let Some(built) = compare_with_kotlinc_plugin_jdk(
        "InlineErasedParameter",
        SOURCE,
        "InlineErasedParameterKt",
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    for member in ["int go(java.lang.String)", "int plain(java.lang.String)"] {
        let reference = method_instructions(&built.reference, member);
        assert!(!reference.is_empty(), "{member} not found");
        assert_eq!(
            method_instructions(&built.krusty, member),
            reference,
            "{member}"
        );
        assert_eq!(
            stack_map(&built.krusty, member),
            stack_map(&built.reference, member),
            "{member} frames"
        );
    }
    for member in [
        "void load(java.io.BufferedReader)",
        "void loadNull(java.io.BufferedReader)",
    ] {
        let reference = close_sites(&method_instructions(&built.reference, member));
        assert!(!reference.is_empty(), "{member} has no close");
        assert_eq!(
            close_sites(&method_instructions(&built.krusty, member)),
            reference,
            "{member} close"
        );
    }
}

/// Each `AutoCloseable.close` together with the instructions after the preceding `aload`.
/// Jump offsets are not part of the cast shape.
fn close_sites(instructions: &[String]) -> Vec<String> {
    let mut sites = Vec::new();
    for (index, instruction) in instructions.iter().enumerate() {
        let Some((_, op)) = instruction.split_once(": ") else {
            continue;
        };
        if !op.contains("invokeinterface") || !op.contains("close:") {
            continue;
        }
        let start = instructions[..index]
            .iter()
            .rposition(|earlier| {
                earlier
                    .split_once(": ")
                    .is_some_and(|(_, op)| op.starts_with("aload"))
            })
            .unwrap_or(index);
        let site = instructions[start..=index]
            .iter()
            .map(|instruction| {
                normalize_jump(
                    instruction
                        .split_once(": ")
                        .map(|(_, op)| op)
                        .unwrap_or(instruction),
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        sites.push(site);
    }
    sites
}

fn normalize_jump(op: &str) -> String {
    let mut tokens = op.split_whitespace().collect::<Vec<_>>();
    if tokens
        .first()
        .is_some_and(|op| op.starts_with("if") || *op == "goto")
        && tokens
            .last()
            .is_some_and(|token| token.parse::<u32>().is_ok())
    {
        let last = tokens.len() - 1;
        tokens[last] = "L";
    }
    tokens.join(" ")
}

#[test]
fn an_inlined_bound_still_runs() {
    common::expect_box_ok_with_stdlib(
        &format!(
            "{SOURCE}\
             fun box(): String {{\n\
             \x20   val reader = java.io.BufferedReader(java.io.StringReader(\"ab\"))\n\
             \x20   var n = 0\n\
             \x20   reader.use {{ r -> n = r.read() }}\n\
             \x20   val nullable: java.io.BufferedReader? = java.io.BufferedReader(java.io.StringReader(\"z\"))\n\
             \x20   nullable.use {{ }}\n\
             \x20   val empty: java.io.BufferedReader? = null\n\
             \x20   empty.use {{ }}\n\
             \x20   if (plain(\"ok\") != 2) return \"plain\"\n\
             \x20   return if (n == 'a'.code && go(\"abcd\") == 4) \"OK\" else \"FAIL\"\n\
             }}\n"
        ),
        "inlined erased bound",
    );
}
