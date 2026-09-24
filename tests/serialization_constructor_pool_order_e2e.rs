//! The deserialization constructor's local names are interned before its frames.
//!
//! ASM visits `visitLocalVariable` before `visitMaxs`, so kotlinc's constant pool carries a local's
//! name and descriptor ahead of every class constant the frame computation introduces. krusty builds
//! the `StackMapTable` inside `add_method`, which interns each parameter's verification type — so
//! `kotlinx/serialization/internal/SerializationConstructorMarker` landed AHEAD of `seen0` and
//! `serializationConstructorMarker` instead of behind them.
//!
//! The marker's `Class` entry is referenced by nothing in either compiler's output: it exists only
//! because the frame names that parameter's type. Its position was the last non-debug difference on
//! a `@Serializable` class with defaulted properties.

use super::common::compare_with_kotlinc_plugin;
use super::serialization_companion_byte_parity_e2e::plugin_and_runtime;

/// The `Utf8` entries of a `javap -v` pool, in order, filtered to those asked for.
fn pool_order(disassembly: &str, wanted: &[&str]) -> Vec<String> {
    disassembly
        .lines()
        .map(str::trim)
        .filter_map(|line| line.split_once("= Utf8"))
        .map(|(_, value)| value.trim().to_string())
        .filter(|value| wanted.contains(&value.as_str()))
        .collect()
}

#[test]
fn the_serialization_constructors_locals_intern_before_its_frames() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    // Defaulted properties give the constructor branches, so it HAS frames — without them the
    // stackmap is never built and the ordering under test does not arise.
    let src = "import kotlinx.serialization.Serializable\n\
               @Serializable\n\
               data class Opt2(\n\
               \x20   val a: Int = 1,\n\
               \x20   val b: String? = null,\n\
               )\n";
    let Some(built) =
        compare_with_kotlinc_plugin("SerializationCtorPoolOrder", src, "Opt2", &cp, "25", &extra)
    else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let wanted = [
        "seen0",
        "serializationConstructorMarker",
        "Lkotlinx/serialization/internal/SerializationConstructorMarker;",
        "kotlinx/serialization/internal/SerializationConstructorMarker",
    ];
    let want = pool_order(&built.reference, &wanted);
    assert_eq!(
        want,
        vec![
            "seen0",
            "serializationConstructorMarker",
            "Lkotlinx/serialization/internal/SerializationConstructorMarker;",
            "kotlinx/serialization/internal/SerializationConstructorMarker",
        ],
        "kotlinc's interning order, stated so a change in the reference is visible here"
    );
    assert_eq!(
        pool_order(&built.krusty, &wanted),
        want,
        "krusty interns the constructor's locals where kotlinc does"
    );
}

/// The generated constructor's `LineNumberTable` is kotlinc's.
///
/// A constructor's line table is CURATED — `add_method` drops the marks a body emitted, because an
/// ordinary `<init>` builds its table from the class declaration and its property initializers.
/// A GENERATED constructor has no such curation to fall back on, so it shipped a single entry at
/// pc 0 while kotlinc attributes each defaulted property's DEFAULT VALUE to that property's own
/// line and the store after it back to the class's declaration line.
///
/// Both halves matter for stepping: without the first a debugger never stops on the property whose
/// default is being applied, and without the second that property's line stays in effect over
/// everything after the store.
#[test]
fn the_serialization_constructors_line_table_is_kotlincs() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    // One property per line, so a per-property line is distinguishable from the class's.
    let src = "import kotlinx.serialization.Serializable\n\
               \n\
               @Serializable\n\
               data class Opt2(\n\
               \x20   val a: Int = 1,\n\
               \x20   val b: String? = null,\n\
               )\n";
    let Some(built) =
        compare_with_kotlinc_plugin("SerializationCtorLines", src, "Opt2", &cp, "25", &extra)
    else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let table = |disassembly: &str| -> Vec<String> {
        let mut rows = Vec::new();
        let mut in_ctor = false;
        let mut in_table = false;
        for raw in disassembly.lines() {
            let line = raw.trim();
            if line.starts_with("public Opt2(int, int, java.lang.String,") {
                in_ctor = true;
                continue;
            }
            if !in_ctor {
                continue;
            }
            if line == "LineNumberTable:" {
                in_table = true;
                continue;
            }
            if in_table {
                match line.strip_prefix("line ") {
                    Some(row) => rows.push(row.to_string()),
                    None => break,
                }
            }
        }
        rows
    };
    let want = table(&built.reference);
    // The class is on line 4, `a` on line 5, `b` on line 6; `@Serializable` opens the declaration
    // on line 3, which is where kotlinc puts the constructor's own entry.
    assert_eq!(
        want,
        vec!["3: 0", "5: 28", "3: 29", "6: 47", "3: 48"],
        "kotlinc's table, stated so a change in the reference is visible here"
    );
    assert_eq!(
        table(&built.krusty),
        want,
        "Opt2's deserialization constructor"
    );
}

// The whole class IS byte-identical to kotlinc's when built by the CLI — verified by hand on this
// exact fixture. It is deliberately NOT asserted here: `compare_with_kotlinc_plugin` compiles
// in-process, and that path records different `@Metadata` d1 function flags than the shipped
// compiler for plugin-generated members, so a whole-class assertion through this harness would fail
// for a reason that has nothing to do with the class. The two tests above pin the halves that were
// actually wrong; the CLI keeps the whole.
