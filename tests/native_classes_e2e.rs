//! Kotlin classes on the native runtime, end to end.
//!
//! Every test here compiles a Kotlin program to C, links it against the emitted runtime, RUNS the
//! executable and compares its output. Nothing inspects generated C: a class layout that looks
//! right and hands the collector the wrong field offset is exactly the failure a text assertion
//! cannot see, and the whole point of these tests is that the programs run.
//!
//! Skips rather than fails when the Kotlin stdlib or a C compiler is unavailable.

use std::path::{Path, PathBuf};

use krusty::backend::Artifact;
use krusty::diag::DiagSink;
use krusty::jvm::classpath::Classpath;
use krusty::native::{NativeBackend, NativeTarget};
use krusty::source::SourceInput;

/// A scratch directory that cleans itself up.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "krusty-native-classes-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos())
        ));
        let _ = std::fs::remove_dir_all(&path);
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Compile `source` as `Main.kt` with the native backend, returning its artifacts and diagnostics.
fn compile(source: &str) -> (Vec<Artifact>, Vec<String>) {
    let jar = krusty::toolchain::stdlib_jar().expect("checked by the caller");
    let classpath = std::rc::Rc::new(Classpath::new(vec![jar]));
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(
        classpath.clone(),
    ));
    let inputs = vec![SourceInput::kotlin(source).with_file_stem("Main")];
    let stems = vec!["Main".to_string()];
    let mut features = krusty::features::LangFeatures::new();
    features.apply_source_directives(source);

    let mut diags = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &inputs, platform, &features, &mut diags,
    );
    let backend = NativeBackend::new(classpath);
    let artifacts = krusty::compiler::emit_analyzed(analysis, &stems, &backend, "main", &mut diags);
    let diagnostics = diags
        .diags
        .into_iter()
        .map(|diagnostic| diagnostic.msg)
        .collect();
    (artifacts, diagnostics)
}

/// Whether this environment can run the native tests at all.
fn available() -> bool {
    krusty::toolchain::stdlib_jar().is_some()
        && NativeTarget::host().is_some_and(krusty::native::can_build)
}

/// Compile, link and run a single-file program; return its standard output.
fn run(source: &str) -> String {
    let (artifacts, diagnostics) = compile(source);
    assert!(
        diagnostics.is_empty(),
        "the native backend rejected the program: {diagnostics:?}"
    );

    let scratch = Scratch::new("run");
    let executable = scratch.path().join("program");
    krusty::native::link_executable(
        &artifacts,
        scratch.path(),
        &executable,
        NativeTarget::host().expect("checked by `available`"),
    )
    .unwrap_or_else(|error| panic!("the generated C must compile: {error}"));

    let output = std::process::Command::new(&executable)
        .output()
        .expect("run the built executable");
    assert!(
        output.status.success(),
        "the built executable must exit cleanly: {}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn a_class_with_a_field_and_a_method_constructs_calls_and_prints() {
    if !available() {
        eprintln!("skipping: needs the Kotlin stdlib and a C compiler");
        return;
    }
    // The minimum end-to-end path through a class: allocation through the collector, the emitted
    // constructor storing a primary-constructor parameter into its field, an instance method
    // called on the result, and a property read inside that method.
    assert_eq!(
        run("class Greeter(val name: String) {\n\
             \x20   fun greet(): String = \"Hello, $name!\"\n\
             }\n\
             fun main() {\n\
             \x20   println(Greeter(\"world\").greet())\n\
             }\n"),
        "Hello, world!\n"
    );
    // A class with no `toString` of its own renders in Kotlin's default shape. The hex part is an
    // identity the runtime derives from the address, so only the prefix is asserted.
    let rendered = run("class Greeter(val name: String)\n\
         fun main() { println(Greeter(\"world\")) }\n");
    assert!(
        rendered.starts_with("Greeter@"),
        "expected Kotlin's `Name@hash` default, got {rendered:?}"
    );
}

#[test]
fn a_call_through_a_base_typed_value_reaches_the_override() {
    if !available() {
        eprintln!("skipping: needs the Kotlin stdlib and a C compiler");
        return;
    }
    // The vtable proof: the static type says `A`, the object is a `B`, and the call must land in
    // `B.name`. A direct call by static type would compile, link, run and print `A`.
    assert_eq!(
        run("open class A { open fun name(): String = \"A\" }\n\
             class B : A() { override fun name(): String = \"B\" }\n\
             fun main() {\n\
             \x20   val x: A = B()\n\
             \x20   val y: A = A()\n\
             \x20   println(x.name())\n\
             \x20   println(y.name())\n\
             }\n"),
        "B\nA\n"
    );
}

#[test]
fn a_super_call_runs_the_base_implementation_then_the_override() {
    if !available() {
        eprintln!("skipping: needs the Kotlin stdlib and a C compiler");
        return;
    }
    // `super.name()` is a non-virtual call to `A`'s own body; the override then adds to it. A
    // `super` call that dispatched would recurse forever.
    assert_eq!(
        run("open class A { open fun name(): String = \"A\" }\n\
             class B : A() { override fun name(): String = super.name() + \"B\" }\n\
             fun main() { println(B().name()) }\n"),
        "AB\n"
    );
}

#[test]
fn an_abstract_method_dispatches_to_each_implementation() {
    if !available() {
        eprintln!("skipping: needs the Kotlin stdlib and a C compiler");
        return;
    }
    // The abstract class's own concrete method (`describe`) calls the abstract `area` on `this`,
    // so the dispatch has to happen from inside the base class's code as well as from `main`.
    assert_eq!(
        run("abstract class Shape(val name: String) {\n\
             \x20   abstract fun area(): Int\n\
             \x20   fun describe(): String = \"$name:${area()}\"\n\
             }\n\
             class Square(val side: Int) : Shape(\"square\") {\n\
             \x20   override fun area(): Int = side * side\n\
             }\n\
             class Rect(val w: Int, val h: Int) : Shape(\"rect\") {\n\
             \x20   override fun area(): Int = w * h\n\
             }\n\
             fun main() {\n\
             \x20   val shapes = Square(3)\n\
             \x20   val other: Shape = Rect(2, 5)\n\
             \x20   println(shapes.describe())\n\
             \x20   println(other.describe())\n\
             \x20   println(other.area() + shapes.area())\n\
             }\n"),
        "square:9\nrect:10\n19\n"
    );
}

#[test]
fn a_three_level_hierarchy_inherits_fields_and_overrides_at_each_level() {
    if !available() {
        eprintln!("skipping: needs the Kotlin stdlib and a C compiler");
        return;
    }
    // Fields declared at three levels are read through the most derived object, and each level's
    // override — or inherited implementation — is the one that runs through the root type.
    assert_eq!(
        run(
            "open class A(val a: Int) { open fun f(): Int = a\n open fun g(): Int = 1 }\n\
             open class B(a: Int, val b: Int) : A(a) { override fun f(): Int = a + b }\n\
             class C(val c: Int) : B(10, 20) { override fun g(): Int = c + f() }\n\
             fun main() {\n\
             \x20   val root: A = C(5)\n\
             \x20   println(root.f())\n\
             \x20   println(root.g())\n\
             \x20   val mid: B = B(1, 2)\n\
             \x20   println(mid.f() + mid.g())\n\
             }\n"
        ),
        "30\n35\n4\n"
    );
}

#[test]
fn is_and_safe_casts_follow_the_superclass_chain() {
    if !available() {
        eprintln!("skipping: needs the Kotlin stdlib and a C compiler");
        return;
    }
    // Three levels: a `C` is a `B` and an `A`; an `A` is not a `B`. `as?` yields the object or
    // `null`, and a successful `as` keeps the identity (the cast object is still a `C`).
    assert_eq!(
        run("open class A\n\
             open class B : A()\n\
             class C : B()\n\
             fun main() {\n\
             \x20   val x: A = C()\n\
             \x20   val y: A = A()\n\
             \x20   println(x is B)\n\
             \x20   println(x is C)\n\
             \x20   println(y !is B)\n\
             \x20   println(y is A)\n\
             \x20   val failed = y as? B\n\
             \x20   println(failed == null)\n\
             \x20   val passed = x as? B\n\
             \x20   println(passed != null)\n\
             \x20   val w = x as B\n\
             \x20   println(w is C)\n\
             \x20   val anything: Any = C()\n\
             \x20   println(anything is A)\n\
             \x20   println(anything is String)\n\
             \x20   println(\"text\" is String)\n\
             \x20   if (x is C) println(\"smart cast\")\n\
             }\n"),
        "true\ntrue\ntrue\ntrue\ntrue\ntrue\ntrue\ntrue\nfalse\ntrue\nsmart cast\n"
    );
}

#[test]
fn a_failed_cast_fails_loudly_naming_both_types() {
    if !available() {
        eprintln!("skipping: needs the Kotlin stdlib and a C compiler");
        return;
    }
    // There are no exceptions yet, so a failed `as` cannot throw ClassCastException; the honest
    // realization is a diagnosable exit naming both types, the same way unboxing `null` already
    // fails. A silent pass-through would let the program read `B`'s fields off an `A`.
    let (artifacts, diagnostics) = compile(
        "open class A\n\
         class B : A()\n\
         fun main() {\n\
         \x20   println(\"before\")\n\
         \x20   val a: A = A()\n\
         \x20   val b = a as B\n\
         \x20   println(b)\n\
         }\n",
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let scratch = Scratch::new("cast");
    let executable = scratch.path().join("program");
    krusty::native::link_executable(
        &artifacts,
        scratch.path(),
        &executable,
        NativeTarget::host().expect("checked by `available`"),
    )
    .unwrap_or_else(|error| panic!("the generated C must compile: {error}"));
    let output = std::process::Command::new(&executable)
        .output()
        .expect("run the built executable");
    assert!(
        !output.status.success(),
        "a failed cast must not let the program continue"
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "before\n");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("A") && stderr.contains("cannot be cast to") && stderr.contains("B"),
        "the failure must name both types: {stderr:?}"
    );
}

#[test]
fn a_user_to_string_is_reached_through_println_templates_and_explicit_calls() {
    if !available() {
        eprintln!("skipping: needs the Kotlin stdlib and a C compiler");
        return;
    }
    // Three routes to the same override: `println(obj)` renders through the runtime, `"$obj"`
    // renders inside string concatenation, and `obj.toString()` is a direct member call. All must
    // reach `P.toString`, including through a base-typed value.
    assert_eq!(
        run("open class P(val x: Int) { override fun toString(): String = \"P($x)\" }\n\
             class Q(x: Int) : P(x) { override fun toString(): String = \"Q<${super.toString()}>\" }\n\
             fun main() {\n\
             \x20   val p = P(1)\n\
             \x20   println(p)\n\
             \x20   println(\"[$p]\")\n\
             \x20   println(p.toString())\n\
             \x20   val q: P = Q(2)\n\
             \x20   println(q)\n\
             \x20   println(\"$q!\")\n\
             }\n"),
        "P(1)\n[P(1)]\nP(1)\nQ<P(2)>\nQ<P(2)>!\n"
    );
    // A class without one prints Kotlin's `Name@hash` default; the hash is an identity derived
    // from the address, so only the prefix is asserted — and it is the same object twice.
    let rendered = run("class Plain\n\
         fun main() {\n\
         \x20   val a = Plain()\n\
         \x20   println(a)\n\
         \x20   println(a.toString().equals(a.toString()))\n\
         \x20   println(a.hashCode() == a.hashCode())\n\
         \x20   println(a.equals(a))\n\
         \x20   println(a.equals(Plain()))\n\
         }\n");
    let mut lines = rendered.lines();
    let first = lines.next().unwrap_or_default();
    assert!(
        first.starts_with("Plain@"),
        "expected Kotlin's `Name@hash` default, got {first:?}"
    );
    assert_eq!(
        lines.collect::<Vec<_>>(),
        vec!["true", "true", "true", "false"],
        "two renderings of one object are equal strings, the default hash is stable, and \
         default equality is identity"
    );
}
