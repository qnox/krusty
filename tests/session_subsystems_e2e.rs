//! Round-trip coverage for subsystems reworked in the Ty/IR consolidation: generic erasure (a generic
//! `var` field forces the erased-field + checkcast-on-read path — `Ty::TyParam`/erasure), value classes
//! (the unboxed underlying representation), and `IrClass` field flags via a multi-field data class
//! (per-field final/var, `copy`, componentN, structural equality) and an enum (`values()`/`valueOf`/
//! `ordinal`/`name`). Each compiles through the full pipeline and runs `box()` on the JVM, returning
//! "OK". (Stdlib instance-member resolution — `String.length`, `is String`, `String?` smart-cast — and
//! value-class-through-erased-generic are exercised by the conformance corpus, not the in-process e2e
//! harness, so they are not duplicated here.)

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "P", &[sl], Some(&jdk))
}

fn toolchain_ready() -> bool {
    common::java_home().is_some() && common::stdlib_jar().is_some()
}

/// A generic class with a `var T` field storing two different concrete types — forces the
/// erased-field + checkcast-on-read path (not just a value round-trip), pinning erasure-to-Object.
#[test]
fn generic_var_field_erasure() {
    if !toolchain_ready() {
        return;
    }
    const SRC: &str = "// WITH_STDLIB\n\
class Cell<T>(var v: T)\n\
fun <T> id(x: T): T = x\n\
fun box(): String {\n\
    val ci = Cell<Int>(7)\n\
    val cs = Cell<String>(\"k\")\n\
    if (ci.v != 7) return \"fail int\"\n\
    if (cs.v != \"k\") return \"fail str\"\n\
    ci.v = 8\n\
    if (ci.v != 8) return \"fail mut\"\n\
    if (id(\"a\") != \"a\") return \"fail id\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("generic var field should compile + run"),
        "OK"
    );
}

/// `@JvmInline value class` arithmetic — the unboxed underlying representation.
#[test]
fn value_class_unboxed_arith() {
    if !toolchain_ready() {
        return;
    }
    const SRC: &str = "// WITH_STDLIB\n\
@JvmInline value class Meters(val v: Int)\n\
fun add(a: Meters, b: Meters): Meters = Meters(a.v + b.v)\n\
fun box(): String {\n\
    val r = add(Meters(2), Meters(3))\n\
    return if (r.v == 5) \"OK\" else \"fail\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("value class arith should compile + run"),
        "OK"
    );
}

/// Multi-field data class mixing `val`/`var` with `copy` — pins per-field finality/visibility/order
/// against a desync after merging `IrClass`'s five field-parallel `Vec`s into one `Vec<IrField>`.
#[test]
fn data_class_multifield_copy() {
    if !toolchain_ready() {
        return;
    }
    const SRC: &str = "// WITH_STDLIB\n\
data class R(val a: Int, var b: Int, val c: String)\n\
fun box(): String {\n\
    val r = R(1, 2, \"z\")\n\
    val s = r.copy(b = 9)\n\
    if (s.a != 1 || s.b != 9 || s.c != \"z\") return \"fail copy\"\n\
    if (r == s) return \"fail neq\"\n\
    if (r != R(1, 2, \"z\")) return \"fail eq\"\n\
    if (r.component1() != 1 || r.component3() != \"z\") return \"fail componentN\"\n\
    r.b = 5\n\
    if (r.b != 5) return \"fail var\"\n\
    if (s.b != 9) return \"fail shared\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("multi-field data class copy should compile + run"),
        "OK"
    );
}

/// `lateinit var` — the backing field carries `IrField::is_lateinit`; a read after init returns the
/// value (the per-access null-check passes). Pins the field-flag fold.
#[test]
fn lateinit_var_set_then_get() {
    if !toolchain_ready() {
        return;
    }
    const SRC: &str = "// WITH_STDLIB\n\
class Holder {\n\
    lateinit var name: String\n\
}\n\
fun box(): String {\n\
    val h = Holder()\n\
    h.name = \"hi\"\n\
    return if (h.name == \"hi\") \"OK\" else \"fail\"\n\
}\n";
    assert_eq!(run(SRC).expect("lateinit var should compile + run"), "OK");
}

/// Enum — `IrClass` enum-entry fields + `values()`/`valueOf`/`ordinal`/`name`.
#[test]
fn enum_entries_and_values() {
    if !toolchain_ready() {
        return;
    }
    const SRC: &str = "// WITH_STDLIB\n\
enum class Color { RED, GREEN, BLUE }\n\
fun box(): String {\n\
    if (Color.RED.ordinal != 0) return \"fail ord\"\n\
    if (Color.BLUE.name != \"BLUE\") return \"fail name\"\n\
    if (Color.values().size != 3) return \"fail values\"\n\
    if (Color.valueOf(\"GREEN\") != Color.GREEN) return \"fail valueOf\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("enum should compile + run"), "OK");
}
