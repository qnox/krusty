//! Emitting Kotlin BUILTIN members with NO JDK on the compile classpath (only `kotlin-stdlib.jar`).
//!
//! Without a JDK the mapped JVM owner (`java/util/List`) has no class file, so every realization fact
//! the backend normally reads off that class file — interface-ness, the physical accessor name, the
//! erased descriptor — has to come from the builtin's own `.kotlin_builtins` entry instead. When it
//! doesn't, the compile still reports `ok` and the class still verifies structurally, but it names
//! methods that do not exist and dispatches them with the wrong opcode:
//!
//! ```text
//! invokevirtual java/util/List.getSize:()I        // must be invokeinterface java/util/List.size:()I
//! invokevirtual java/util/Map.getEntries:()…      // must be invokeinterface … entrySet:()…
//! invokeinterface java/util/Map$Entry.getKey:()Ljava/lang/String;   // must be erased + checkcast
//! ```
//!
//! which fails only when the class is LOADED (`IncompatibleClassChangeError`) or CALLED
//! (`NoSuchMethodError`). A diagnostics-only assertion cannot see any of that, so these tests compile
//! against a JDK-less classpath and then actually run `box()` on a real JVM.

use super::common;

/// Compile with ONLY the stdlib jar (no JDK modules) and run `box()` on a real JVM.
///
/// Deliberately NOT written as `let Some(x) = … else { return }`: `compile_in_process` returns `None`
/// when the compiler REJECTS the source, so an early return would turn a resolution regression into a
/// silent pass. The only legitimate skip is a missing toolchain, which is checked first.
fn run_no_jdk_box(src: &str, stem: &str) -> Option<String> {
    let stdlib = common::stdlib_jar()?;
    // A runtime JVM is still required to LOAD the emitted class; only the COMPILE is JDK-less.
    common::java_home()?;
    let jars = [stdlib];
    let classes = common::compile_in_process(src, stem, &jars, None)
        .unwrap_or_else(|| panic!("{stem} must compile with no JDK on the classpath"));
    let box_class = common::find_box_class(&classes)
        .unwrap_or_else(|| panic!("{stem} emitted no static box(): String"));
    Some(
        common::run_box(&classes, &box_class, &jars)
            .unwrap_or_else(|| panic!("{stem} compiled but could not be run")),
    )
}

/// A read of a Kotlin collection PROPERTY (`size`) must realize as the mapped `java.util` stub
/// (`size()`), not an invented JavaBean getter (`getSize()`), and dispatch with `invokeinterface`.
#[test]
fn builtin_collection_property_read_runs_without_jdk() {
    let src = r#"
fun box(): String {
    val l: List<String> = listOf("a", "b", "c")
    if (l.size != 3) return "FAIL size=" + l.size
    val m: Map<String, Int> = mapOf("k" to 1)
    if (m.size != 1) return "FAIL map size=" + m.size
    if (m.keys.size != 1) return "FAIL keys=" + m.keys.size
    return "OK"
}
"#;
    let Some(out) = run_no_jdk_box(src, "nojdk_collection_property") else {
        eprintln!("skip: no kotlin-stdlib jar / JDK to run on");
        return;
    };
    assert_eq!(
        out, "OK",
        "no-JDK collection property read returned {out:?}"
    );
}

/// A builtin member CALL on a mapped interface owner must use `invokeinterface`. With no JDK the
/// interface flag has to survive the resolution round trip from the `.kotlin_builtins` entry; when it
/// is dropped the class fails to load with `IncompatibleClassChangeError`.
#[test]
fn builtin_interface_member_call_runs_without_jdk() {
    let src = r#"
fun box(): String {
    val l: List<String> = listOf("x", "y")
    if (l.get(1) != "y") return "FAIL get=" + l.get(1)
    if (l.isEmpty()) return "FAIL isEmpty"
    if (!l.contains("x")) return "FAIL contains"
    val m: Map<String, Int> = mapOf("k" to 7)
    if (m.get("k") != 7) return "FAIL map get"
    return "OK"
}
"#;
    let Some(out) = run_no_jdk_box(src, "nojdk_interface_call") else {
        eprintln!("skip: no kotlin-stdlib jar / JDK to run on");
        return;
    };
    assert_eq!(out, "OK", "no-JDK builtin interface call returned {out:?}");
}

/// A builtin property whose declared type is a type PARAMETER (`Map.Entry.key: K`) erases to
/// `Object` in its descriptor; the read must call the erased accessor and `checkcast` the result, the
/// way the JDK-present path already does. Building the descriptor from the substituted logical type
/// emits `getKey:()Ljava/lang/String;`, which no class declares.
#[test]
fn builtin_generic_property_read_erases_without_jdk() {
    let src = r#"
fun box(): String {
    val m: Map<String, Int> = mapOf("k" to 7)
    val e: Map.Entry<String, Int> = m.entries.first()
    if (e.key != "k") return "FAIL key=" + e.key
    if (e.value != 7) return "FAIL value=" + e.value
    return "OK"
}
"#;
    let Some(out) = run_no_jdk_box(src, "nojdk_generic_property") else {
        eprintln!("skip: no kotlin-stdlib jar / JDK to run on");
        return;
    };
    assert_eq!(out, "OK", "no-JDK generic property read returned {out:?}");
}

/// The emitted bytecode must be IDENTICAL with and without a JDK on the compile classpath: the JDK
/// simply supplies, from `java/util/List.class`, the same facts the builtin entry already carries. A
/// divergence here is the general form of all three defects above, and catches ones no `box()` covers.
///
/// Deliberately uses no NESTED builtin type: a reference to `java/util/Map$Entry` also emits an
/// `InnerClasses` attribute, which is read off the owner's class file and so is still absent on a
/// JDK-less classpath. That is a metadata-only divergence (the code array and constant-pool member
/// refs match, and the class loads and runs — see the generic-property test above), listed with the
/// other JDK-less codegen gaps in `docs/IMPLEMENTATION_PLAN.md` and outside the member-realization
/// facts this asserts.
#[test]
fn no_jdk_emit_matches_jdk_emit_for_builtin_members() {
    let (Some(stdlib), Some(jdk)) = (common::stdlib_jar(), common::jdk_modules()) else {
        eprintln!("skip: no kotlin-stdlib jar / JDK modules");
        return;
    };
    let src = r#"
fun s(l: List<String>): Int = l.size
fun k(m: Map<String, Int>): Set<String> = m.keys
fun v(m: Map<String, Int>): Collection<Int> = m.values
fun g(l: List<String>): String = l.get(0)
fun e(l: List<String>): Boolean = l.isEmpty()
fun i(l: List<String>): Iterator<String> = l.iterator()
fun ms(l: MutableList<String>): Int = l.size
fun ma(l: MutableList<String>): Boolean = l.add("x")
fun mm(m: MutableMap<String, Int>): Int = m.size
fun mk(m: MutableMap<String, Int>): MutableSet<String> = m.keys
fun st(s: String): Int = s.length
fun sc(s: String): Char = s.get(0)
fun cs(c: CharSequence): Int = c.length
fun cp(a: Comparable<String>, b: String): Int = a.compareTo(b)
fun nt(n: Number): Int = n.toInt()
fun it(i: Iterator<String>): Boolean = i.hasNext()
fun ar(a: Array<String>): Int = a.size
fun ia(a: IntArray): Int = a.size
"#;
    let jars = [stdlib];
    let with_jdk = common::compile_in_process(src, "cmp", &jars, Some(&jdk))
        .expect("must compile with a JDK on the classpath");
    let no_jdk = common::compile_in_process(src, "cmp", &jars, None)
        .expect("must compile with no JDK on the classpath");
    assert_eq!(
        no_jdk.len(),
        with_jdk.len(),
        "the two classpaths must emit the same class files"
    );
    for ((name, no_jdk_bytes), (jdk_name, jdk_bytes)) in no_jdk.iter().zip(with_jdk.iter()) {
        assert_eq!(name, jdk_name, "class file order must match");
        if no_jdk_bytes != jdk_bytes {
            // Raw byte vectors are unreadable at this size; the interesting difference is always a
            // method name or a descriptor, so report the printable constant-pool strings that differ.
            let only_in = |a: &[u8], b: &[u8]| {
                let theirs = printable_tokens(b);
                let mut out: Vec<String> = printable_tokens(a)
                    .into_iter()
                    .filter(|t| !theirs.contains(t))
                    .collect();
                out.sort();
                out.dedup();
                out
            };
            panic!(
                "{name}: the JDK-less emit must match the JDK-present emit — the builtin's own \
                 .kotlin_builtins entry carries the same owner/name/descriptor/interface facts the \
                 JDK class file does.\n  only without a JDK: {:?}\n  only with a JDK:    {:?}\n  \
                 (sizes {} vs {})",
                only_in(no_jdk_bytes, jdk_bytes),
                only_in(jdk_bytes, no_jdk_bytes),
                no_jdk_bytes.len(),
                jdk_bytes.len(),
            );
        }
    }
}

/// Printable ASCII runs of 3+ characters in a class file — its constant-pool names and descriptors.
/// A crude extraction on purpose: this only has to make an assertion failure readable.
fn printable_tokens(bytes: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for &b in bytes {
        if b.is_ascii_graphic() {
            current.push(b as char);
        } else {
            if current.len() >= 3 {
                out.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
        }
    }
    if current.len() >= 3 {
        out.push(current);
    }
    out
}
