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

/// The generic JVM→builtin metadata mapping covers mapped classes outside the collection-specific
/// table too. `CharSequence.length` must therefore recover both its plain physical name (`length`, not
/// `getLength`) and its interface dispatch bit from `kotlin/CharSequence` with no JDK class available.
#[test]
fn builtin_noncollection_property_read_runs_without_jdk() {
    let src = r#"
fun box(): String {
    val text: CharSequence = "shape"
    return if (text.length == 5) "OK" else "FAIL length=" + text.length
}
"#;
    let Some(out) = run_no_jdk_box(src, "nojdk_noncollection_property") else {
        eprintln!("skip: no kotlin-stdlib jar / JDK to run on");
        return;
    };
    assert_eq!(
        out, "OK",
        "no-JDK noncollection property read returned {out:?}"
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
/// Includes a NESTED builtin reference (`m.entries.first().key` → `java/util/Map$Entry`), which also
/// makes the class carry an `InnerClasses` attribute. That nesting fact used to be read only off the
/// owner's class file, so it silently vanished on a JDK-less classpath; it comes from the builtin's own
/// `.kotlin_builtins` entry (`kotlin/collections/Map.Entry`) instead.
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
fun n(m: Map<String, Int>): String = m.entries.first().key
fun q(s: CharSequence): Int = s.length
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

/// What the recovered `InnerClasses` entry must SAY, asserted directly rather than only against the
/// JDK-present emit. The comparison above is anchored to the JDK today because its other side reads
/// `java/util/Map.class` — but both sides run the same resolver, so a restructure that routed the
/// JDK-present path through the builtins fallback too would leave it passing on whatever flags the
/// decode happened to produce. `0x0609` (`public static interface abstract`) is what javac put in
/// `java/util/Map`, and it is not negotiable.
#[test]
fn recovered_inner_class_entry_carries_the_jdk_flags() {
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skip: no kotlin-stdlib jar");
        return;
    };
    let src = "fun n(m: Map<String, Int>): String = m.entries.first().key\n";
    let classes = common::compile_in_process(src, "innercls", &[stdlib], None)
        .expect("must compile with no JDK on the classpath");
    let (name, bytes) = classes.first().expect("must emit a class file");
    let info = krusty::jvm::classreader::parse_class(bytes)
        .unwrap_or_else(|e| panic!("{name} must be a readable class file: {e:?}"));
    let entry = info
        .inner_classes
        .iter()
        .find(|e| e.inner == "java/util/Map$Entry")
        .unwrap_or_else(|| {
            panic!(
                "{name} must carry an InnerClasses entry for the nested builtin it references; got {:?}",
                info.inner_classes
            )
        });
    assert_eq!(entry.outer.as_deref(), Some("java/util/Map"));
    assert_eq!(entry.name.as_deref(), Some("Entry"));
    assert_eq!(
        entry.access, 0x0609,
        "the entry must carry the flags java/util/Map records for Map$Entry \
         (public static interface abstract), not whatever the builtins decode defaults to"
    );
}

/// The guardrails that keep the `$`-decomposition from INVENTING a nesting relation. Requiring the
/// `.kotlin_builtins` fragment to actually declare the nested class is the whole reason a `$` that is
/// merely part of a mangled name, a lambda class, or a non-builtin owner cannot be reported as nesting.
#[test]
fn builtin_nested_class_only_answers_for_declared_nestings() {
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skip: no kotlin-stdlib jar");
        return;
    };
    let cp = krusty::jvm::classpath::Classpath::new(vec![stdlib]);
    assert_eq!(
        cp.builtin_nested_class("java/util/Map$Entry"),
        Some(("java/util/Map".to_string(), "Entry".to_string(), 0x0609)),
        "the one nesting a JDK-less compile actually has to recover"
    );
    // Not nested at all — no `$` to decompose.
    assert_eq!(cp.builtin_nested_class("java/util/Map"), None);
    // A `$` that is part of a mangled name, not a nesting: the enclosing half maps to no builtin.
    assert_eq!(cp.builtin_nested_class("com/example/Foo$bar$1"), None);
    assert_eq!(cp.builtin_nested_class("MainKt$main$1"), None);
    // The enclosing half maps to a builtin, but the fragment declares no such nested class — the
    // decomposition alone must not be enough.
    assert_eq!(cp.builtin_nested_class("java/util/Map$Absent"), None);
    // Multi-level: the enclosing half is itself the mapped nested name, which declares nothing under it.
    assert_eq!(cp.builtin_nested_class("java/util/Map$Entry$Deeper"), None);
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
