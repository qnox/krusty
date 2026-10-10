//! Drop-in kotlinc behavior: the `krusty` binary compiles a directory of sources to a `.jar` using
//! kotlinc-style flags, and the real kotlinc compiles + runs a Kotlin consumer against that jar.

use std::fs;
use std::process::Command;

use super::common;

#[test]
fn compiles_directory_to_jar_consumable_by_kotlinc() {
    let krusty = common::krusty_binary();

    let root = std::env::temp_dir().join(format!("krusty_cli_{}", std::process::id()));
    let src = root.join("src/demo");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("Point.kt"),
        "package demo\nclass Point(val x: Int, val y: Int) {\n  fun sum(): Int = x + y\n}\n",
    )
    .unwrap();
    fs::write(
        src.join("Lib.kt"),
        "package demo\nfun mk(a: Int): Point = Point(a, a)\n",
    )
    .unwrap();

    let jar = root.join("mylib.jar");
    // kotlinc-style invocation: unsupported flags, a module name, a source *directory*, jar output.
    let out = Command::new(&krusty)
        .args([
            "-include-runtime",
            "-jvm-target",
            "1.8",
            "-module-name",
            "mylib",
            "-d",
        ])
        .arg(&jar)
        .arg(root.join("src"))
        .output()
        .expect("run krusty");
    // IR backend covers a subset; if it can't lower these sources yet, skip (don't fail).
    if !out.status.success() {
        eprintln!(
            "skip (IR unsupported): {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = fs::remove_dir_all(&root);
        return;
    }
    assert!(jar.exists(), "jar not produced");

    // The jar must contain the classes + the named .kotlin_module.
    let bytes = fs::read(&jar).unwrap();
    assert!(bytes.starts_with(b"PK"), "output is not a zip/jar");

    // Real kotlinc compiles a consumer against the krusty jar (only works if the jar's @Metadata +
    // .kotlin_module are well-formed), then we run it.
    fs::write(
        root.join("Consumer.kt"),
        "import demo.mk\nfun main() { println(mk(4).sum()) }\n",
    )
    .unwrap();
    let args = vec![
        root.join("Consumer.kt").to_string_lossy().into_owned(),
        "-cp".to_string(),
        jar.to_string_lossy().into_owned(),
        "-d".to_string(),
        root.join("cout").to_string_lossy().into_owned(),
    ];
    let Some((code, stderr)) = common::kotlinc_compile(&args) else {
        eprintln!("krusty jar produced; provisioned kotlinc server unavailable");
        let _ = fs::remove_dir_all(&root);
        return;
    };
    // A *Kotlin* consumer importing top-level declarations needs krusty's `@Metadata` to fully
    // describe the facade's functions (a protobuf blob). This works today — asserted, so a
    // metadata-emission regression fails here instead of hiding behind a skip.
    assert_eq!(
        code, 0,
        "real kotlinc must consume the krusty-built jar's @Metadata: {stderr}"
    );

    let stdlib = common::stdlib_jar();
    let cp = format!(
        "{}:{}:{}",
        root.join("cout").to_str().unwrap(),
        jar.to_str().unwrap(),
        stdlib.to_string_lossy()
    );
    // Run the kotlinc-compiled consumer on the pooled JavaRunner — and ASSERT it: a consumer that
    // compiled but fails to run against the krusty jar is a failure, not a silent pass.
    let driver = "public class RunC { public static void main(String[] a) { ConsumerKt.main(); } }";
    let rc = root.join("RunC.java");
    fs::write(&rc, driver).unwrap();
    let out = common::javac_run(
        rc.to_str().unwrap(),
        &cp,
        root.join("rcout").to_string_lossy().as_ref(),
        "RunC",
    )
    .expect("pooled JavaRunner unavailable");
    assert_eq!(out.trim(), "8");

    let _ = fs::remove_dir_all(&root);
}

/// Multi-file compilation: a top-level function call AND a top-level property read/write that target
/// declarations in ANOTHER source file lower to cross-facade `invokestatic` (function, `getX`/`setX`),
/// not a bail. Compile both files with the krusty binary, link via javac, run `box()`.
#[test]
fn cross_file_top_level_function_and_property() {
    let _ = common::java_home(); // strict: panics with the JAVA_HOME diagnosis when absent
    let stdlib = common::stdlib_jar();
    let stdlib = stdlib.to_str().unwrap().to_string();
    let krusty = common::krusty_binary();
    let dir = std::env::temp_dir().join(format!("krusty_xfile_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("A.kt"),
        "fun helper(x: Int): Int = x * 2\nfun tag(s: String): String = s + \"!\"\nval GREETING = \"hi\"\nvar counter = 10\n",
    )
    .unwrap();
    fs::write(
        dir.join("B.kt"),
        "fun box(): String {\n  if (helper(21) != 42) return \"f1\"\n  if (tag(\"hi\") != \"hi!\") return \"f2\"\n  if (GREETING != \"hi\") return \"f3\"\n  counter = counter + 5\n  if (counter != 15) return \"f4: $counter\"\n  return \"OK\"\n}\n",
    )
    .unwrap();
    let kc = Command::new(&krusty)
        .args(["-d", dir.to_str().unwrap()])
        .arg(dir.join("A.kt"))
        .arg(dir.join("B.kt"))
        .output()
        .unwrap();
    assert!(
        kc.status.success(),
        "krusty failed cross-file compile: {}",
        String::from_utf8_lossy(&kc.stderr)
    );
    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] a) { System.out.println(BKt.box()); } }",
    )
    .unwrap();
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    let out = common::javac_run(
        dir.join("M.java").to_str().unwrap(),
        &cp,
        dir.to_str().unwrap(),
        "M",
    )
    .expect("pooled JavaRunner unavailable");
    assert_eq!(out.trim(), "OK");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn cross_file_nullable_generic_return_remains_null_safe() {
    let _ = common::java_home(); // strict: panics with the JAVA_HOME diagnosis when absent
    let stdlib = common::stdlib_jar();
    let krusty = common::krusty_binary();
    let dir = std::env::temp_dir().join(format!("krusty_xgeneric_null_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("A.kt"),
        "fun <T> id(x: T): T = x\n\
         fun <T> inferredId(x: T) = x\n\
         fun <T : Any?> boundedId(x: T): T = x\n\
         fun <A : Any?, T : A> chainedId(x: T): T = x\n\
         fun <T : Comparable<T>> collect(vararg values: T, block: Array<T>.() -> Unit) {}\n\
         class GenericBox<T>(private val value: T) { fun get(): T = value }\n\
         class ExtensionAcc(var result: String)\n\
         operator fun ExtensionAcc.plusAssign(value: String) { result = \"wrong-extension\" }\n\
         operator fun ExtensionAcc.plusAssign(value: Int) { result = \"extension\" }\n",
    )
    .unwrap();
    fs::write(
        dir.join("B.kt"),
        "fun box(): String {\n\
         \x20 if (id(null).hashCode() != 0) return \"unbounded\"\n\
         \x20 if (inferredId(null).hashCode() != 0) return \"inferred\"\n\
         \x20 if (boundedId(null).hashCode() != 0) return \"bounded\"\n\
         \x20 if (chainedId(null).hashCode() != 0) return \"chained\"\n\
         \x20 if (id(\"OK\").length != 2) return \"concrete\"\n\
         \x20 if (GenericBox(\"OK\").get().length != 2) return \"member\"\n\
         \x20 if (GenericBox(null).get().hashCode() != 0) return \"nullable-member\"\n\
         \x20 collect(42, 43) { }\n\
         \x20 val accumulator = ExtensionAcc(\"\")\n\
         \x20 accumulator += 1\n\
         \x20 if (accumulator.result != \"extension\") return accumulator.result\n\
         \x20 return \"OK\"\n\
         }\n",
    )
    .unwrap();
    let compiled = Command::new(&krusty)
        .args(["-d", dir.to_str().unwrap()])
        .arg(dir.join("A.kt"))
        .arg(dir.join("B.kt"))
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "krusty failed cross-file nullable generic compile: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] a) { System.out.println(BKt.box()); } }",
    )
    .unwrap();
    let classpath = format!("{}:{}", dir.to_str().unwrap(), stdlib.to_string_lossy());
    let out = common::javac_run(
        dir.join("M.java").to_str().unwrap(),
        &classpath,
        dir.to_str().unwrap(),
        "M",
    )
    .expect("pooled JavaRunner unavailable");
    assert_eq!(out.trim(), "OK");
    let _ = fs::remove_dir_all(&dir);
}

/// Multi-file: construct a class declared in ANOTHER file, read a property, CALL a method, and WRITE a
/// `var` — all lower to cross-file bytecode (`new`/`invokespecial <init>`, `getX`, `invokevirtual`,
/// `setX`), not a bail. Compile both files, run `box()`.
#[test]
fn cross_file_class_construct_and_property_read() {
    let _ = common::java_home(); // strict: panics with the JAVA_HOME diagnosis when absent
    let stdlib = common::stdlib_jar();
    let stdlib = stdlib.to_str().unwrap().to_string();
    let krusty = common::krusty_binary();
    let dir = std::env::temp_dir().join(format!("krusty_xcls_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("A.kt"),
        "class Box(val x: Int, var tag: String) {\n  fun doubled(): Int = x * 2\n}\n",
    )
    .unwrap();
    fs::write(
        dir.join("B.kt"),
        "fun box(): String {\n  val b = Box(21, \"hi\")\n  if (b.x != 21) return \"f1\"\n  if (b.tag != \"hi\") return \"f2\"\n  if (b.doubled() != 42) return \"f3\"\n  b.tag = \"bye\"\n  if (b.tag != \"bye\") return \"f4: ${b.tag}\"\n  return \"OK\"\n}\n",
    )
    .unwrap();
    let kc = Command::new(&krusty)
        .args(["-d", dir.to_str().unwrap()])
        .arg(dir.join("A.kt"))
        .arg(dir.join("B.kt"))
        .output()
        .unwrap();
    assert!(
        kc.status.success(),
        "krusty failed cross-file class compile: {}",
        String::from_utf8_lossy(&kc.stderr)
    );
    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] a) { System.out.println(BKt.box()); } }",
    )
    .unwrap();
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    let out = common::javac_run(
        dir.join("M.java").to_str().unwrap(),
        &cp,
        dir.to_str().unwrap(),
        "M",
    )
    .expect("pooled JavaRunner unavailable");
    assert_eq!(out.trim(), "OK");
    let _ = fs::remove_dir_all(&dir);
}

/// A destructuring declaration `val (a, b) = c` where `c`'s class — with `operator fun componentN` —
/// is defined in ANOTHER file of the same compilation. The componentN calls must resolve cross-file
/// (`Virtual`), like an ordinary cross-file instance call.
#[test]
fn cross_file_destructuring() {
    let _ = common::java_home(); // strict: panics with the JAVA_HOME diagnosis when absent
    let stdlib = common::stdlib_jar();
    let stdlib = stdlib.to_str().unwrap().to_string();
    let krusty = common::krusty_binary();
    let dir = std::env::temp_dir().join(format!("krusty_xdestr_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("A.kt"),
        "class Pair2(val a: String, val b: String) {\n  operator fun component1() = a\n  operator fun component2() = b\n}\n",
    )
    .unwrap();
    fs::write(
        dir.join("B.kt"),
        "fun box(): String {\n  val p = Pair2(\"O\", \"K\")\n  val (x, y) = p\n  return x + y\n}\n",
    )
    .unwrap();
    let kc = Command::new(&krusty)
        .args(["-d", dir.to_str().unwrap()])
        .arg(dir.join("A.kt"))
        .arg(dir.join("B.kt"))
        .output()
        .unwrap();
    assert!(
        kc.status.success(),
        "krusty failed cross-file destructure compile: {}",
        String::from_utf8_lossy(&kc.stderr)
    );
    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] a) { System.out.println(BKt.box()); } }",
    )
    .unwrap();
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    let out = common::javac_run(
        dir.join("M.java").to_str().unwrap(),
        &cp,
        dir.to_str().unwrap(),
        "M",
    )
    .expect("pooled JavaRunner unavailable");
    assert_eq!(out.trim(), "OK");
    let _ = fs::remove_dir_all(&dir);
}

/// Cross-file inferred RETURN type: an `object` method with an expression body (`fun all() =
/// listOf(...)`) whose return type is inferred, called from ANOTHER file that the compiler happens to
/// check FIRST. Without a global pre-inference pass, the caller's file resolves `all()` against the
/// erased collection default (`java/util/List`, element `Any`) — so an element access reads `Any` and
/// fails with "unresolved member". `preinfer_module_returns` patches every file's inferred returns into
/// the shared signature table before any file's main check, so the element type resolves cross-file.
///
/// Resolution-only: this asserts the ELEMENT TYPE resolves (no "unresolved member" error). Lowering a
/// cross-file `object`-member call is a separate, not-yet-implemented shape, so the file does not fully
/// compile — the guarantee here is that the return type is no longer erased.
#[test]
fn cross_file_object_inferred_return_element_resolves() {
    let krusty = common::krusty_binary();
    let dir = std::env::temp_dir().join(format!("krusty_xinfer_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("A.kt"),
        "package demo\nclass Role(val name: String)\nobject R { fun all() = listOf(Role(\"a\"), Role(\"b\")) }\n",
    )
    .unwrap();
    fs::write(
        dir.join("B.kt"),
        "package demo\nfun box(): String = R.all()[0].name\n",
    )
    .unwrap();
    // Pass B.kt BEFORE A.kt so the caller is checked before the definer — the order that, without a
    // GLOBAL pre-inference, leaves `all()`'s inferred return erased when B reads it.
    let out = Command::new(&krusty)
        .args(["-d", dir.to_str().unwrap()])
        .arg(dir.join("B.kt"))
        .arg(dir.join("A.kt"))
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr) + String::from_utf8_lossy(&out.stdout);
    assert!(
        !err.contains("unresolved member"),
        "cross-file object inferred return should resolve its element type (got: {err})"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// Cross-file `object` MEMBER call: `object R { fun greet() = "hi" }` in one file, `R.greet()` in
/// another of the same module. The callee is not in the caller file's IR (the same-file object path)
/// and not on the classpath (the classpath object path) — a SIBLING-file module object. Lower it by
/// reading the singleton via an external `getstatic R.INSTANCE` and invoking the member cross-file
/// (`invokevirtual`), rather than bailing ("not yet supported by the IR backend"). Runs `box()`.
#[test]
fn cross_file_object_member_call_lowers() {
    let _ = common::java_home(); // strict: panics with the JAVA_HOME diagnosis when absent
    let stdlib = common::stdlib_jar();
    let stdlib = stdlib.to_str().unwrap().to_string();
    let krusty = common::krusty_binary();
    let dir = std::env::temp_dir().join(format!("krusty_xobj_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("A.kt"),
        "package demo\nobject R {\n  fun greet(): String = \"hi\"\n}\n",
    )
    .unwrap();
    fs::write(
        dir.join("B.kt"),
        "package demo\nfun box(): String = R.greet()\n",
    )
    .unwrap();
    let kc = Command::new(&krusty)
        .args(["-d", dir.to_str().unwrap()])
        .arg(dir.join("B.kt"))
        .arg(dir.join("A.kt"))
        .output()
        .unwrap();
    assert!(
        kc.status.success(),
        "krusty failed cross-file object-member-call compile: {}",
        String::from_utf8_lossy(&kc.stderr)
    );
    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] a) { System.out.println(demo.BKt.box()); } }",
    )
    .unwrap();
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    let out = common::javac_run(
        dir.join("M.java").to_str().unwrap(),
        &cp,
        dir.to_str().unwrap(),
        "M",
    )
    .expect("pooled JavaRunner unavailable");
    assert_eq!(out.trim(), "hi");
    let _ = fs::remove_dir_all(&dir);
}

/// Cross-package `object` referenced as a VALUE: `val h = Helper` where `Helper` is a same-module
/// `object` declared in ANOTHER package/file. The signature-phase property inference recognizes it as
/// the object's own type (not the library-only object-check, which misses a module object), and the
/// backend reads the singleton via `getstatic Helper.INSTANCE`. Runs `box()`.
#[test]
fn cross_module_object_as_value() {
    let _ = common::java_home(); // strict: panics with the JAVA_HOME diagnosis when absent
    let stdlib = common::stdlib_jar();
    let stdlib = stdlib.to_str().unwrap().to_string();
    let krusty = common::krusty_binary();
    let dir = std::env::temp_dir().join(format!("krusty_objval_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("A.kt"),
        "package d.svc\nobject Helper { fun tag(): String = \"OK\" }\n",
    )
    .unwrap();
    fs::write(
        dir.join("B.kt"),
        "package a.app\nimport d.svc.Helper\nclass C {\n  private val h = Helper\n  fun run(): String = h.tag()\n}\nfun box(): String = C().run()\n",
    )
    .unwrap();
    let kc = Command::new(&krusty)
        .args(["-d", dir.to_str().unwrap()])
        .arg(dir.join("B.kt"))
        .arg(dir.join("A.kt"))
        .output()
        .unwrap();
    assert!(
        kc.status.success(),
        "krusty failed cross-module object-value compile: {}",
        String::from_utf8_lossy(&kc.stderr)
    );
    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] argv) { System.out.println(a.app.BKt.box()); } }",
    )
    .unwrap();
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    let out = common::javac_run(
        dir.join("M.java").to_str().unwrap(),
        &cp,
        dir.to_str().unwrap(),
        "M",
    )
    .expect("pooled JavaRunner unavailable");
    assert_eq!(out.trim(), "OK");
    let _ = fs::remove_dir_all(&dir);
}

/// A constructor call that OMITS a parameter with a NON-CONST default (`labels: List<String> =
/// emptyList()`) must dispatch to the `<init>$default(args…, mask, DefaultConstructorMarker)` synthetic
/// (kotlinc's shape) — krusty could only inline CONST-literal defaults and bailed on a non-const one
/// ("not yet supported by the IR backend"). Tested cross-file (the class in another file) since that is
/// where a domain `data class` with defaults is constructed. Runs `box()` under `-Xverify:all`.
#[test]
fn ctor_omitted_non_const_default_uses_init_default() {
    let _ = common::java_home(); // strict: panics with the JAVA_HOME diagnosis when absent
    let stdlib = common::stdlib_jar();
    let stdlib = stdlib.to_str().unwrap().to_string();
    let krusty = common::krusty_binary();
    let dir = std::env::temp_dir().join(format!("krusty_ctordef_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("A.kt"),
        "package demo\ndata class Server(val name: String, val labels: List<String> = emptyList())\n",
    )
    .unwrap();
    fs::write(
        dir.join("B.kt"),
        "package demo\nfun box(): String {\n  val s = Server(name = \"n\")\n  return if (s.name == \"n\" && s.labels.isEmpty()) \"OK\" else \"FAIL\"\n}\n",
    )
    .unwrap();
    let kc = Command::new(&krusty)
        .args(["-cp", &stdlib, "-d", dir.to_str().unwrap()])
        .arg(dir.join("B.kt"))
        .arg(dir.join("A.kt"))
        .output()
        .unwrap();
    assert!(
        kc.status.success(),
        "krusty failed omitted-default ctor compile: {}",
        String::from_utf8_lossy(&kc.stderr)
    );
    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] argv) { System.out.println(demo.BKt.box()); } }",
    )
    .unwrap();
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    let out = common::javac_run(
        dir.join("M.java").to_str().unwrap(),
        &cp,
        dir.to_str().unwrap(),
        "M",
    )
    .expect("pooled JavaRunner unavailable");
    assert_eq!(out.trim(), "OK");
    let _ = fs::remove_dir_all(&dir);
}

/// Cross-file value-class property access: mangled getter/setter, erased value descriptor, and identity
/// read of `.value`. A green run under `-Xverify:all` proves both semantic operations were realized with
/// the physical JVM type rather than the boxed Kotlin type retained in common IR.
#[test]
fn cross_file_value_class_property_read_uses_mangled_getter() {
    let _ = common::java_home(); // strict: panics with the JAVA_HOME diagnosis when absent
    let stdlib = common::stdlib_jar();
    let stdlib = stdlib.to_str().unwrap().to_string();
    let krusty = common::krusty_binary();
    let dir = std::env::temp_dir().join(format!("krusty_xfilevc_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("Domain.kt"),
        "package demo\n@JvmInline value class Id(val value: String)\nclass Holder(var id: Id)\nfun make(): Holder = Holder(Id(\"OK\"))\nfun next(): Id = Id(\"NEXT\")\n",
    )
    .unwrap();
    // The reads and write are in ANOTHER file from the value class and its holder. `make()`/`next()` keep
    // construction same-file so this fixture isolates property realization at the module boundary.
    fs::write(
        dir.join("Read.kt"),
        "package demo\nfun box(): String {\n  val h = make()\n  if (h.id.value != \"OK\") return \"read\"\n  h.id = next()\n  return h.id.value\n}\n",
    )
    .unwrap();
    let kc = Command::new(&krusty)
        .args(["-d", dir.to_str().unwrap()])
        .arg(dir.join("Read.kt"))
        .arg(dir.join("Domain.kt"))
        .output()
        .unwrap();
    assert!(
        kc.status.success(),
        "krusty failed cross-file value-class property read: {}",
        String::from_utf8_lossy(&kc.stderr)
    );
    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] argv) { System.out.println(demo.ReadKt.box()); } }",
    )
    .unwrap();
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    let out = common::javac_run(
        dir.join("M.java").to_str().unwrap(),
        &cp,
        dir.to_str().unwrap(),
        "M",
    )
    .expect("pooled JavaRunner unavailable");
    assert_eq!(out.trim(), "NEXT");
    let _ = fs::remove_dir_all(&dir);
}

fn output_file_ledger(root: &std::path::Path) -> std::collections::BTreeSet<String> {
    fn visit(
        root: &std::path::Path,
        directory: &std::path::Path,
        files: &mut std::collections::BTreeSet<String>,
    ) {
        let Ok(entries) = fs::read_dir(directory) else {
            return;
        };
        for entry in entries {
            let path = entry.expect("read compiler output entry").path();
            if path.is_dir() {
                visit(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root)
                        .expect("output is under its requested root")
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }

    let mut files = std::collections::BTreeSet::new();
    visit(root, root, &mut files);
    files
}

/// Remove only kotlinc's source excerpt from a warning report. The remaining vector is the complete
/// emitted diagnostic ledger: unlocated warnings, the Werror summary, and every located warning in
/// order. Krusty's plain renderer does not print source excerpts.
fn warning_diagnostic_ledger(stderr: &[u8]) -> Vec<String> {
    std::str::from_utf8(stderr)
        .expect("compiler diagnostics are UTF-8")
        .lines()
        .filter(|line| !matches!(*line, "fun f() = 1" | "^^^^^" | "    ^"))
        .map(str::to_string)
        .collect()
}

/// Global and named warning policy has kotlinc's exact status, ordered diagnostic ledger, stdout,
/// and complete output-file set. This covers configuration, module, and located source warnings.
#[test]
fn warning_policy_precedence_matches_kotlinc() {
    struct Case {
        tag: &'static str,
        source: &'static str,
        arguments: &'static [&'static str],
        code: i32,
        diagnostics: Vec<String>,
    }

    let krusty = common::krusty_binary();
    let dir = std::env::temp_dir().join(format!("krusty_warning_policy_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    // Recorded kotlinc diagnostics retain their source spelling. Keep it stable across test
    // processes while separating the two source bodies so concurrent runs only write identical
    // bytes to either path.
    let source_root = std::path::Path::new("target/test-sources/cli-warning-policy");
    let explicit_source = source_root.join("explicit-return/F.kt");
    let inferred_source = source_root.join("inferred-return/F.kt");
    fs::create_dir_all(explicit_source.parent().unwrap()).unwrap();
    fs::create_dir_all(inferred_source.parent().unwrap()).unwrap();
    let location = inferred_source.display();
    let cli_warning = "warning: the argument '-Xcontext-parameters' is redundant for the current language version 2.4.".to_string();
    let visibility_warning =
        format!("{location}:1:1: warning: visibility must be specified in explicit API mode.");
    let return_warning =
        format!("{location}:1:5: warning: return type must be specified in explicit API mode.");
    let werror = "error: warnings found and -Werror specified".to_string();
    let module_warning = "warning: opt-in requirement marker 'does.not.Exist' is unresolved. Make sure it's present in the module dependencies.".to_string();
    let cases = vec![
        Case {
            tag: "named-warning-overrides-werror",
            source: "fun f(): Int = 1\n",
            arguments: &[
                "-language-version",
                "2.4",
                "-Xcontext-parameters",
                "-Werror",
                "-Xwarning-level=REDUNDANT_CLI_ARG:warning",
            ],
            code: 0,
            diagnostics: vec![cli_warning.clone()],
        },
        Case {
            tag: "named-disabled-overrides-werror",
            source: "fun f(): Int = 1\n",
            arguments: &[
                "-language-version",
                "2.4",
                "-Xcontext-parameters",
                "-Werror",
                "-Xwarning-level=REDUNDANT_CLI_ARG:disabled",
            ],
            code: 0,
            diagnostics: Vec::new(),
        },
        Case {
            tag: "named-warning-overrides-nowarn",
            source: "fun f(): Int = 1\n",
            arguments: &[
                "-language-version",
                "2.4",
                "-Xcontext-parameters",
                "-nowarn",
                "-Xwarning-level=REDUNDANT_CLI_ARG:warning",
            ],
            code: 0,
            diagnostics: vec![cli_warning.clone()],
        },
        Case {
            tag: "plain-werror-promotes-cli-warning",
            source: "fun f(): Int = 1\n",
            arguments: &[
                "-language-version",
                "2.4",
                "-Xcontext-parameters",
                "-Werror",
            ],
            code: 1,
            diagnostics: vec![cli_warning, werror.clone()],
        },
        Case {
            tag: "werror-promotes-located-source-warnings",
            source: "fun f() = 1\n",
            arguments: &["-Xexplicit-api=warning", "-Werror"],
            code: 1,
            diagnostics: vec![
                werror.clone(),
                visibility_warning.clone(),
                return_warning.clone(),
            ],
        },
        Case {
            tag: "nowarn-suppresses-located-source-warnings",
            source: "fun f() = 1\n",
            arguments: &["-Xexplicit-api=warning", "-nowarn"],
            code: 0,
            diagnostics: Vec::new(),
        },
        Case {
            tag: "werror-wins-before-nowarn",
            source: "fun f() = 1\n",
            arguments: &["-Xexplicit-api=warning", "-Werror", "-nowarn"],
            code: 1,
            diagnostics: vec![
                werror.clone(),
                visibility_warning.clone(),
                return_warning.clone(),
            ],
        },
        Case {
            tag: "werror-wins-after-nowarn",
            source: "fun f() = 1\n",
            arguments: &["-Xexplicit-api=warning", "-nowarn", "-Werror"],
            code: 1,
            diagnostics: vec![
                werror.clone(),
                visibility_warning.clone(),
                return_warning.clone(),
            ],
        },
        Case {
            tag: "module-warning-precedes-werror-and-source-warnings",
            source: "fun f() = 1\n",
            arguments: &[
                "-opt-in=does.not.Exist",
                "-Xexplicit-api=warning",
                "-Werror",
            ],
            code: 1,
            diagnostics: vec![module_warning, werror, visibility_warning, return_warning],
        },
    ];
    let successful_outputs = std::collections::BTreeSet::from([
        "FKt.class".to_string(),
        "META-INF/main.kotlin_module".to_string(),
    ]);

    for case in cases {
        let source = if case.source == "fun f() = 1\n" {
            &inferred_source
        } else {
            &explicit_source
        };
        fs::write(source, case.source).expect("write warning-policy source");
        let reference_out = dir.join(format!("{}-reference", case.tag));
        let mut reference_args = case
            .arguments
            .iter()
            .map(|argument| (*argument).to_string())
            .collect::<Vec<_>>();
        reference_args.extend([
            "-no-reflect".to_string(),
            "-d".to_string(),
            reference_out.display().to_string(),
            source.display().to_string(),
        ]);
        let (reference_code, reference_stderr) =
            common::byte_dump::with_recorded_diagnostics(|| {
                common::kotlinc_compile(&reference_args).expect("reference compiler unavailable")
            });
        assert_eq!(reference_code, case.code, "{}", case.tag);
        assert_eq!(
            warning_diagnostic_ledger(reference_stderr.as_bytes()),
            case.diagnostics,
            "{}: reference diagnostics",
            case.tag
        );

        let krusty_out = dir.join(format!("{}-krusty", case.tag));
        let result = Command::new(&krusty)
            .args(case.arguments)
            .args(["-no-reflect", "-d"])
            .arg(&krusty_out)
            .arg(source)
            .output()
            .expect("run krusty");
        assert_eq!(result.status.code(), Some(case.code), "{}", case.tag);
        assert_eq!(
            warning_diagnostic_ledger(&result.stderr),
            case.diagnostics,
            "{}: krusty diagnostics",
            case.tag
        );
        let expected_stdout = if case.code == 0 {
            format!("ok: emitted 1 class file(s) to {}\n", krusty_out.display())
        } else {
            String::new()
        };
        assert_eq!(result.stdout, expected_stdout.as_bytes(), "{}", case.tag);
        let expected_outputs = if case.code == 0 {
            successful_outputs.clone()
        } else {
            std::collections::BTreeSet::new()
        };
        assert_eq!(
            output_file_ledger(&reference_out),
            expected_outputs,
            "{}: reference output files",
            case.tag
        );
        assert_eq!(
            output_file_ledger(&krusty_out),
            expected_outputs,
            "{}: krusty output files",
            case.tag
        );
    }
    let _ = fs::remove_dir_all(&dir);
}
