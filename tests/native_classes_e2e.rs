//! Classes through krusty's own code generator and linker: object layout, constructors, vtable
//! dispatch, `super`, abstract members, `is`/`as`, user `toString`, initialization order,
//! property accessors, erased generics, `object` singletons — every one a Kotlin program that is
//! compiled, linked, RUN and compared, never inspected as code.
//!
//! The collector is part of every program here: the chain test and the singleton test allocate
//! far past the collection threshold, so the descriptors the generator emits are the ones the
//! collector traces by, and a wrong field offset frees a live node or follows an `Int` as a pointer.
//!
//! Skips (never fails) when this build of krusty carries no prebuilt runtime for the host.

use std::path::{Path, PathBuf};

use krusty::backend::Artifact;
use krusty::diag::DiagSink;
use krusty::jvm::classpath::Classpath;
use krusty::native::{CraneliftBackend, NativeTarget};
use krusty::source::SourceInput;

use super::common;

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
        std::fs::create_dir_all(&path).expect("create scratch directory");
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

fn host() -> Option<NativeTarget> {
    let target = NativeTarget::host()?;
    (krusty::native::can_link(target) && krusty::toolchain::stdlib_jar().is_some())
        .then_some(target)
}

/// Whether this environment can run the native tests at all.
fn available() -> bool {
    host().is_some()
}

/// Compile a single-file program with the code generator for the host.
fn compile(source: &str) -> (Vec<Artifact>, Vec<String>) {
    let target = host().expect("checked by `available`");
    let jar = krusty::toolchain::stdlib_jar().expect("checked by the caller");
    let classpath = std::rc::Rc::new(Classpath::new(vec![jar]));
    let platform = Box::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(classpath.clone())
            .expect("JVM provider initialization"),
    );
    let inputs = vec![SourceInput::kotlin(source).with_file_stem("Main")];
    let stems = vec!["Main".to_string()];
    let mut features = krusty::features::LangFeatures::new();
    features.apply_source_directives(source);
    let mut diags = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &inputs, platform, &features, &mut diags,
    );
    let provider: std::rc::Rc<dyn krusty::libraries::SemanticPlatform> = std::rc::Rc::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(classpath)
            .expect("JVM provider initialization"),
    );
    let backend = CraneliftBackend::new(provider, target);
    let artifacts = krusty::compiler::emit_analyzed(analysis, &stems, &backend, "main", &mut diags);
    (artifacts, diags.diags.into_iter().map(|d| d.msg).collect())
}

/// Compile, link with krusty's linker, and run; the process outcome is the caller's to judge.
fn execute(source: &str) -> std::process::Output {
    let target = host().expect("checked by `available`");
    let (artifacts, diagnostics) = compile(source);
    assert!(
        diagnostics.is_empty(),
        "the code generator rejected the program: {diagnostics:?}"
    );
    let objects = artifacts
        .iter()
        .map(|(_, bytes)| bytes.as_slice())
        .collect::<Vec<_>>();
    let image = krusty::native::link_program(&objects, target)
        .unwrap_or_else(|error| panic!("krusty's linker must link the program: {error}"));
    let scratch = Scratch::new("run");
    let executable = scratch.path().join("program");
    std::fs::write(&executable, &image).expect("write executable");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
    }
    common::run_freshly_written(std::process::Command::new(&executable).env_clear())
        .expect("run the built executable")
}

/// Compile, link and run a program that must exit cleanly; return its standard output.
fn run(source: &str) -> String {
    let output = execute(source);
    assert!(
        output.status.success(),
        "the built executable must exit cleanly: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn a_class_with_a_field_and_a_method_constructs_calls_and_prints() {
    if !available() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
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
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
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
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
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
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
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
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
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
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
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
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // There are no exceptions yet, so a failed `as` cannot throw ClassCastException; the honest
    // realization is a diagnosable exit naming both types, the same way unboxing `null` already
    // fails. A silent pass-through would let the program read `B`'s fields off an `A`.
    let output = execute(
        "open class A\n\
         class B : A()\n\
         fun main() {\n\
         \x20   println(\"before\")\n\
         \x20   val a: A = A()\n\
         \x20   val b = a as B\n\
         \x20   println(b)\n\
         }\n",
    );
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
fn a_failed_cast_to_an_array_fails_loudly_too() {
    if !available() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // An array is not a class the program declares, so there was no class id to look a descriptor
    // up by and the cast fell through to a coercion — which changes nothing about a reference, and
    // therefore let an `Array<String>` be read as an `IntArray`: four bytes out of every
    // eight-byte slot, and a pointer answered as a number. The runtime names every array type it
    // lays out, so this is the same check a class already gets.
    let output = execute(
        "fun main() {\n\
         \x20   println(\"before\")\n\
         \x20   val value: Any = arrayOf(\"a\")\n\
         \x20   val ints = value as IntArray\n\
         \x20   println(ints[0])\n\
         }\n",
    );
    assert!(
        !output.status.success(),
        "a failed cast must not let the program continue"
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "before\n");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("kotlin.Array")
            && stderr.contains("cannot be cast to")
            && stderr.contains("kotlin.IntArray"),
        "the failure must name both array types: {stderr:?}"
    );
}

#[test]
fn a_failed_cast_to_a_string_fails_loudly_too() {
    if !available() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // `String` is not a class the program declares either, and it had the same hole an array did:
    // with no descriptor to check against, `as` fell through to a coercion and a boxed `Int` was
    // handed back typed `String`. `length` then read a field off the wrong object and answered 0.
    // Every reference the runtime names is now checked, so this is one rule and not two.
    let output = execute(
        "fun main() {\n\
         \x20   println(\"before\")\n\
         \x20   val value: Any = 1\n\
         \x20   val text = value as String\n\
         \x20   println(text.length)\n\
         }\n",
    );
    assert!(
        !output.status.success(),
        "a failed cast must not let the program continue"
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "before\n");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot be cast to") && stderr.contains("kotlin.String"),
        "the failure must name the type asked for: {stderr:?}"
    );
}

#[test]
fn a_user_to_string_is_reached_through_println_templates_and_explicit_calls() {
    if !available() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
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

#[test]
fn initialization_runs_the_superclass_first_then_fields_then_init_blocks() {
    if !available() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // Kotlin's order: the superclass constructor (with its argument evaluated from the derived
    // parameter), then the derived class's property initializers and `init` blocks in source
    // order. Each step prints, so a reordering shows up in the output.
    assert_eq!(
        run("fun tag(s: String): String { println(s); return s }\n\
             open class Base(val a: Int) { init { println(\"base $a\") } }\n\
             class Derived(val n: Int) : Base(n + 1) {\n\
             \x20   val label: String = tag(\"field\")\n\
             \x20   init { println(\"derived $label $n $a\") }\n\
             \x20   var count: Int = n * 2\n\
             \x20   init { println(\"count $count\") }\n\
             }\n\
             fun main() {\n\
             \x20   val d = Derived(1)\n\
             \x20   println(d.a + d.n + d.count)\n\
             }\n"),
        "base 2\nfield\nderived field 1 2\ncount 2\n5\n"
    );
}

#[test]
fn properties_with_custom_accessors_run_their_bodies() {
    if !available() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // `area` has no storage at all — a read is a call. `t` has storage and both accessors written
    // in source; the read and the write must go through them, and `field` inside them must be the
    // backing field, not a recursive accessor call.
    assert_eq!(
        run("class R(val w: Int, val h: Int) {\n\
             \x20   val area: Int get() = w * h\n\
             \x20   var t: Int = 0\n\
             \x20       get() = field + 1\n\
             \x20       set(v) { field = v * 2 }\n\
             \x20   var plain: Int = 7\n\
             }\n\
             fun main() {\n\
             \x20   val r = R(2, 3)\n\
             \x20   println(r.area)\n\
             \x20   r.t = 5\n\
             \x20   println(r.t)\n\
             \x20   r.plain = r.plain + 1\n\
             \x20   println(r.plain)\n\
             }\n"),
        "6\n11\n8\n"
    );
}

#[test]
fn an_open_property_read_through_the_base_type_reaches_the_override() {
    if !available() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // `v` is overridden with a second backing field, `w` with a second getter body, and `m` is a
    // `var` whose override adds its own setter. Through an `A`-typed value each must reach `B`'s.
    assert_eq!(
        run("open class A {\n\
             \x20   open val v: Int = 1\n\
             \x20   open val w: Int get() = 10\n\
             \x20   open var m: Int = 100\n\
             \x20   fun sum(): Int = v + w + m\n\
             }\n\
             class B : A() {\n\
             \x20   override val v: Int = 2\n\
             \x20   override val w: Int get() = 20\n\
             \x20   override var m: Int = 200\n\
             \x20       set(value) { field = value + 1 }\n\
             }\n\
             fun main() {\n\
             \x20   val x: A = B()\n\
             \x20   println(x.v)\n\
             \x20   println(x.w)\n\
             \x20   x.m = 300\n\
             \x20   println(x.m)\n\
             \x20   println(x.sum())\n\
             \x20   val a = A()\n\
             \x20   println(a.sum())\n\
             }\n"),
        "2\n20\n301\n323\n111\n"
    );
}

#[test]
fn a_generic_class_erases_its_parameter_to_a_reference() {
    if !available() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // `T` is carried as a reference, exactly as on the JVM: `Box(1)` boxes the `Int` on the way
    // into the field and the read unboxes it back; `Box("s")` stores the string as is.
    assert_eq!(
        run("class Box<T>(val value: T) {\n\
             \x20   fun get(): T = value\n\
             }\n\
             fun main() {\n\
             \x20   println(Box(1).value)\n\
             \x20   println(Box(\"s\").value)\n\
             \x20   val b = Box(41)\n\
             \x20   println(b.get() + 1)\n\
             \x20   val nested = Box(Box(\"inner\"))\n\
             \x20   println(nested.value.value)\n\
             }\n"),
        "1\ns\n42\ninner\n"
    );
}

#[test]
fn an_object_declaration_is_one_instance_rooted_across_collections() {
    if !available() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // Part one: one instance, mutated through its methods and properties. Part two is the
    // collector proof, and it is arranged so that NOTHING but the registered global root keeps
    // the singleton alive: `setup` creates it and stores a HEAP string in its field (a template,
    // not a literal — a literal lives in static storage and would survive anything), then
    // `setup`'s frame is gone and `deep` overwrites the stack region it used, then `churn`
    // allocates far past the collection threshold. `main` itself never holds the singleton until
    // it reads it back at the end. Without `kt_gc_add_global_root` in the emitted getter this
    // printed a reused slot's bytes; with it, the string is intact.
    assert_eq!(
        run("object Counter {\n\
             \x20   var n: Int = 0\n\
             \x20   var last: String? = null\n\
             \x20   val label: String = \"counter\"\n\
             \x20   fun bump() { n = n + 1 }\n\
             \x20   fun describe(): String = \"$label=$n\"\n\
             }\n\
             fun setup() {\n\
             \x20   var i = 0\n\
             \x20   while (i < 1000) { Counter.bump(); i = i + 1 }\n\
             \x20   println(Counter.describe())\n\
             \x20   Counter.last = \"kept-${Counter.n}\"\n\
             }\n\
             fun deep(depth: Int): Int = if (depth == 0) 0 else deep(depth - 1) + 1\n\
             fun churn(): String {\n\
             \x20   var j = 0\n\
             \x20   var garbage = \"\"\n\
             \x20   while (j < 100000) { garbage = \"garbage-$j-$j\"; j = j + 1 }\n\
             \x20   return garbage\n\
             }\n\
             fun main() {\n\
             \x20   setup()\n\
             \x20   println(deep(500))\n\
             \x20   println(churn())\n\
             \x20   println(Counter.last)\n\
             \x20   println(Counter.n)\n\
             \x20   println(Counter === Counter)\n\
             }\n"),
        "counter=1000\n500\ngarbage-99999-99999\nkept-1000\n1000\ntrue\n"
    );
}

#[test]
fn a_linked_chain_built_under_collection_pressure_is_traced_through_emitted_layouts() {
    if !available() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // Every iteration allocates a node AND a heap string that is garbage by the next iteration,
    // so the collector runs many times while the chain is being built. Only the head is rooted
    // (a local in `main`); every other node is reachable solely through `next`, which the emitted
    // `reference_offsets` must list at the right offset. A wrong offset frees nodes or follows an
    // `Int` as a pointer, and the walk afterwards would not sum to exactly this.
    assert_eq!(
        run("class Node(val value: Int, val next: Node?)\n\
             fun main() {\n\
             \x20   var head: Node? = null\n\
             \x20   var i = 0\n\
             \x20   var scratch = \"\"\n\
             \x20   while (i < 40000) {\n\
             \x20       head = Node(i, head)\n\
             \x20       scratch = \"garbage-$i-${i * 3}-${i + 7}\"\n\
             \x20       i = i + 1\n\
             \x20   }\n\
             \x20   var sum = 0\n\
             \x20   var count = 0\n\
             \x20   var n = head\n\
             \x20   while (n != null) {\n\
             \x20       sum = sum + n.value\n\
             \x20       count = count + 1\n\
             \x20       n = n.next\n\
             \x20   }\n\
             \x20   println(count)\n\
             \x20   println(sum)\n\
             \x20   println(scratch)\n\
             }\n"),
        "40000\n799980000\ngarbage-39999-119997-40006\n"
    );
}

#[test]
fn a_companion_object_is_initialized_when_its_class_is_first_constructed() {
    if !available() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // Constructing a class is where the JVM would have run its `<clinit>`, and for a class with a
    // companion that means creating the companion instance — so its initializers run BEFORE the
    // class's own, and not at all until something constructs the class. Each step appends to a
    // top-level property, so both the order and the laziness are visible in one string.
    assert_eq!(
        run("var global = \"A\"\n\
             class C {\n\
             \x20   init { global += \"D\" }\n\
             \x20   companion object {\n\
             \x20       init { global += \"B\" }\n\
             \x20       init { global += \"C\" }\n\
             \x20   }\n\
             }\n\
             fun main() {\n\
             \x20   println(global)\n\
             \x20   C()\n\
             \x20   println(global)\n\
             \x20   C()\n\
             \x20   println(global)\n\
             }\n"),
        "A\nABCD\nABCDD\n",
        "the companion initializes once, before the first instance, and never again"
    );
}

#[test]
fn a_super_property_read_reaches_the_base_classs_own_storage() {
    if !available() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // A `super` access on a PROPERTY names the property, not an accessor, and a class with default
    // accessors declares no method for it — so there is nothing to call and the field is the
    // answer. It must be the BASE's field: an overriding `var` declares storage of its own, and
    // `super.b` is how a program can tell the two apart.
    assert_eq!(
        run("open class Base { open var b: Int = 1 }\n\
             class Derived : Base() {\n\
             \x20   override var b: Int = 9\n\
             \x20   fun ofBase(): Int = super.b\n\
             }\n\
             fun main() {\n\
             \x20   val d = Derived()\n\
             \x20   println(d.b)\n\
             \x20   println(d.ofBase())\n\
             }\n"),
        "9\n1\n"
    );
}

#[test]
fn a_super_property_write_reaches_the_base_classs_own_storage() {
    if !available() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // The write half, and the read that proves it went where it was aimed: the override's own
    // field is untouched by it.
    assert_eq!(
        run("open class Base { open var b: Int = 1 }\n\
             class Derived : Base() {\n\
             \x20   override var b: Int = 9\n\
             \x20   fun bumpBase(): Int {\n\
             \x20       super.b = super.b + 5\n\
             \x20       return super.b\n\
             \x20   }\n\
             }\n\
             fun main() {\n\
             \x20   val d = Derived()\n\
             \x20   println(d.bumpBase())\n\
             \x20   println(d.b)\n\
             }\n"),
        "6\n9\n"
    );
}

#[test]
fn a_super_property_reaches_a_written_accessor_without_dispatching() {
    if !available() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // When the base wrote its own accessor, that body is what `super.p` runs — called directly.
    // Dispatching would reach the override, which is the accessor doing the asking, and the
    // program would not finish.
    assert_eq!(
        run("open class Base {\n\
             \x20   open val label: String get() = \"base\"\n\
             }\n\
             class Derived : Base() {\n\
             \x20   override val label: String get() = super.label + \"+derived\"\n\
             }\n\
             fun main() { println(Derived().label) }\n"),
        "base+derived\n"
    );
}

#[test]
fn a_super_property_of_an_interface_reaches_its_default_accessor() {
    if !available() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    assert_eq!(
        run("interface Named {\n\
             \x20   val label: String get() = \"named\"\n\
             }\n\
             class Thing : Named {\n\
             \x20   override val label: String get() = super.label + \"!\"\n\
             }\n\
             fun main() { println(Thing().label) }\n"),
        "named!\n"
    );
}

#[test]
fn an_override_that_changes_representation_is_reached_through_a_bridge() {
    if !available() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // `A<T : Number>.foo(): T` erases its result to a reference; `Z : A<Int>` returns an unboxed
    // machine integer. The base's slot cannot hold `Z`'s body — a caller reading it through `A`
    // would read an integer as a pointer — so it holds a bridge with the BASE's signature that
    // converts and forwards. The two call sites below are the whole point: one reads the slot
    // through `Z`, the other through `A`, and they must agree on the answer while disagreeing
    // about its representation.
    assert_eq!(
        run("open class A<T : Number>(val t: T) {\n\
             \x20   open fun foo(): T = t\n\
             }\n\
             class Z : A<Int>(17) {\n\
             \x20   override fun foo() = 239\n\
             }\n\
             fun main() {\n\
             \x20   val z = Z()\n\
             \x20   val a: A<Int> = z\n\
             \x20   println(z.foo())\n\
             \x20   println(a.foo())\n\
             }\n"),
        "239\n239\n"
    );
}

#[test]
fn a_bridge_reaches_a_further_override_rather_than_the_one_that_needed_it() {
    if !available() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // The reason a bridge forwards by DISPATCH and not by calling the override it was made for.
    // `Y : Z` replaces the target slot with its own body; a bridge that had named `Z.foo` would
    // keep running `Z`'s. Read through all three static types, on one `Y`.
    assert_eq!(
        run("open class A<T : Number>(val t: T) {\n\
             \x20   open fun foo(): T = t\n\
             }\n\
             open class Z : A<Int>(17) {\n\
             \x20   override fun foo() = 239\n\
             }\n\
             class Y : Z() {\n\
             \x20   override fun foo() = 7\n\
             }\n\
             fun main() {\n\
             \x20   val y = Y()\n\
             \x20   val z: Z = y\n\
             \x20   val a: A<Int> = y\n\
             \x20   println(y.foo())\n\
             \x20   println(z.foo())\n\
             \x20   println(a.foo())\n\
             }\n"),
        "7\n7\n7\n"
    );
}

#[test]
fn a_bridge_converts_its_arguments_as_well_as_its_answer() {
    if !available() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // The other direction: the base's parameter is a reference and the override's is an unboxed
    // integer, so the bridge unboxes on the way in rather than boxing on the way out.
    assert_eq!(
        run("open class Box<T> {\n\
             \x20   open fun take(value: T): String = \"base\"\n\
             }\n\
             class Ints : Box<Int>() {\n\
             \x20   override fun take(value: Int): String = \"got $value\"\n\
             }\n\
             fun main() {\n\
             \x20   val b: Box<Int> = Ints()\n\
             \x20   println(b.take(5))\n\
             \x20   println(Ints().take(6))\n\
             }\n"),
        "got 5\ngot 6\n"
    );
}

#[test]
fn an_abstract_member_carries_the_default_its_override_is_called_with() {
    if !available() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // Kotlin writes a default on the DECLARATION, and an interface member's declaration has no
    // body. So `i.f()` has two halves that belong in different places: the argument is computed
    // from the interface's own default, and which implementation then runs is decided by the
    // receiver. Filling the argument must not settle the dispatch — the generator refused these
    // outright rather than risk answering with the wrong implementation.
    assert_eq!(
        run("interface I {\n\
             \x20   fun f(x: Int = 23): String\n\
             }\n\
             abstract class Base : I\n\
             class C : Base(), I {\n\
             \x20   override fun f(x: Int) = \"C:\" + x\n\
             }\n\
             class D : Base(), I {\n\
             \x20   override fun f(x: Int) = \"D:\" + x\n\
             }\n\
             fun main() {\n\
             \x20   val c: I = C()\n\
             \x20   val d: I = D()\n\
             \x20   println(c.f())\n\
             \x20   println(d.f())\n\
             \x20   println(c.f(42))\n\
             }\n"),
        "C:23\nD:23\nC:42\n"
    );
}

#[test]
fn a_default_on_an_abstract_member_reads_the_receiver_it_was_declared_on() {
    if !available() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // The default is an expression in the CALLEE's frame, so it may read the receiver — which is
    // the whole reason it cannot be computed at the call site. Here it reads an abstract property
    // each implementation answers differently, so a wrapper evaluating it against the wrong
    // receiver, or once and for all, prints the wrong line.
    assert_eq!(
        run("interface Named {\n\
             \x20   val tag: String\n\
             \x20   fun greet(who: String = tag): String\n\
             }\n\
             class A : Named {\n\
             \x20   override val tag = \"a\"\n\
             \x20   override fun greet(who: String) = \"A->\" + who\n\
             }\n\
             class B : Named {\n\
             \x20   override val tag = \"b\"\n\
             \x20   override fun greet(who: String) = \"B->\" + who\n\
             }\n\
             fun main() {\n\
             \x20   for (n in listOf<Named>(A(), B())) println(n.greet())\n\
             }\n"),
        "A->a\nB->b\n"
    );
}

#[test]
fn an_override_called_by_its_own_type_takes_the_defaults_of_the_member_it_overrides() {
    if !available() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // An override cannot declare defaults, so `Mid().f()` fills its arguments from `Base.f`'s
    // declaration and still runs `Mid.f`. The call names the override, and the frontend hands it
    // over with the declaration the defaults come from; the arguments are computed in THAT frame
    // (`b = a + 1` reads the `a` passed to it) and the call then dispatches, so a `Mid`-typed
    // `Leaf` runs `Leaf.f`.
    assert_eq!(
        run("open class Base {\n\
             \x20   open fun f(x: Int = 23, y: String = \"b\"): String = \"Base:\" + x + y\n\
             \x20   open fun g(a: Int, b: Int = a + 1): String = \"Base:\" + a + b\n\
             }\n\
             open class Mid : Base() {\n\
             \x20   override fun f(x: Int, y: String) = \"Mid:\" + x + y\n\
             \x20   override fun g(a: Int, b: Int) = \"Mid:\" + a + b\n\
             }\n\
             class Leaf : Mid() {\n\
             \x20   override fun f(x: Int, y: String) = \"Leaf:\" + x + y\n\
             }\n\
             interface I {\n\
             \x20   fun h(x: Int = 7): String\n\
             }\n\
             class K : I {\n\
             \x20   override fun h(x: Int) = \"K:\" + x\n\
             }\n\
             fun main() {\n\
             \x20   println(Mid().f())\n\
             \x20   println(Leaf().f(1))\n\
             \x20   val m: Mid = Leaf()\n\
             \x20   println(m.f(y = \"z\"))\n\
             \x20   println(Mid().g(4))\n\
             \x20   println(K().h())\n\
             }\n"),
        "Mid:23b\nLeaf:1b\nLeaf:23z\nMid:45\nK:7\n"
    );
}

/// A `value class` declines until the backend reads what the frontend checked about it.
///
/// It is not a one-field class: Kotlin answers its `equals`, `hashCode` and `toString` by the
/// value it wraps, and where it is boxed is the representation policy's to decide. Emitting it as
/// an ordinary class would answer `W(1) == W(1)` by identity, so it is refused rather than guessed.
#[test]
fn a_value_class_declines_until_its_declarations_are_read() {
    common::expect_native_decline(
        "@JvmInline value class W(val v: Int)\n\
         fun box(): String = if (W(1) == W(1)) \"OK\" else \"fail\"\n",
        "ValueClassDeclines",
        "a value class",
    );
}
