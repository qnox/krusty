//! `-jvm-default`: the JVM shape an interface's members with bodies are compiled into.
//!
//! Every assertion here is measured against the reference kotlinc, not asserted from the spec: the
//! three modes differ in which classes exist, which methods are abstract, and what `@Metadata`
//! records, and only a differential catches a shape that is plausible but not what kotlinc emits.

use super::common;

use krusty::jvm::ir_emit::JvmDefaultMode;
use std::path::Path;

/// An interface exercising every member kind whose realization `-jvm-default` changes: a property
/// with a default getter, a method with a body, a method with a default parameter value (which adds
/// a `$default` stub), and an abstract method that must stay abstract in all three modes.
const INTERFACE_SOURCE: &str = r#"
interface I {
    val x: Int get() = 1
    fun f(): String = "f" + x
    fun g(a: Int = 5): String = "g$a"
    fun abs(): Int
}

class C : I {
    override fun abs(): Int = 7
}

fun box(): String {
    val c: I = C()
    return if (c.f() == "f1" && c.g() == "g5" && c.x == 1 && c.abs() == 7) "OK" else "fail"
}
"#;

const HOLDER_BYTE_SOURCE: &str = r#"
interface AuditI<T> {
    fun echo(value: T): T = value
    fun text(value: String): String = value
    val answer: Int get() = 42
}

class AuditC : AuditI<String>
"#;

fn class_names(classes: &[(String, Vec<u8>)]) -> Vec<String> {
    let mut names: Vec<String> = classes.iter().map(|(name, _)| name.clone()).collect();
    names.sort();
    names
}

fn collect_classes(root: &Path, dir: &Path, classes: &mut Vec<(String, Vec<u8>)>) {
    for entry in std::fs::read_dir(dir).expect("read compiler output") {
        let path = entry.expect("read compiler output entry").path();
        if path.is_dir() {
            collect_classes(root, &path, classes);
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("class") {
            let name = path
                .strip_prefix(root)
                .expect("class below output root")
                .with_extension("")
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            classes.push((name, std::fs::read(path).expect("read emitted class")));
        }
    }
}

fn compile(mode: JvmDefaultMode) -> Vec<(String, Vec<u8>)> {
    compile_source(mode, INTERFACE_SOURCE, "I")
}

fn compile_source(mode: JvmDefaultMode, source_text: &str, stem: &str) -> Vec<(String, Vec<u8>)> {
    compile_sources(mode, &[(stem, source_text)])
}

fn compile_sources(mode: JvmDefaultMode, sources: &[(&str, &str)]) -> Vec<(String, Vec<u8>)> {
    let flag = match mode {
        JvmDefaultMode::Enable => "enable",
        JvmDefaultMode::NoCompatibility => "no-compatibility",
        JvmDefaultMode::Disable => "disable",
    };
    let work = common::scratch_dir().expect("allocate krusty fixture");
    let output = work.join("out");
    let source_paths = sources
        .iter()
        .map(|(stem, source_text)| {
            let source = work.join(format!("{stem}.kt"));
            std::fs::write(&source, source_text).expect("write krusty fixture");
            source
        })
        .collect::<Vec<_>>();
    let mut command = std::process::Command::new(common::krusty_binary());
    command
        .args(["-d", output.to_str().expect("UTF-8 output")])
        .arg(format!("-jvm-default={flag}"));
    let result = command.args(&source_paths).output().expect("run krusty");
    assert!(
        result.status.success(),
        "krusty failed under {mode:?}: stdout={} stderr={}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let mut classes = Vec::new();
    collect_classes(&output, &output, &mut classes);
    classes.sort_by(|left, right| left.0.cmp(&right.0));
    let _ = std::fs::remove_dir_all(work);
    classes
}

fn compile_module_to(
    mode: JvmDefaultMode,
    source_text: &str,
    stem: &str,
    output: &Path,
    classpath: Option<&Path>,
) {
    let flag = match mode {
        JvmDefaultMode::Enable => "enable",
        JvmDefaultMode::NoCompatibility => "no-compatibility",
        JvmDefaultMode::Disable => "disable",
    };
    let source = output
        .parent()
        .expect("module output parent")
        .join(format!("{stem}.kt"));
    std::fs::write(&source, source_text).expect("write module source");
    let mut command = std::process::Command::new(common::krusty_binary());
    command
        .args(["-d", output.to_str().expect("UTF-8 output")])
        .arg(format!("-jvm-default={flag}"));
    if let Some(classpath) = classpath {
        command.args(["-classpath", classpath.to_str().expect("UTF-8 classpath")]);
    }
    let result = command.arg(&source).output().expect("run krusty module");
    assert!(
        result.status.success(),
        "krusty module {stem} failed under {mode:?}: stdout={} stderr={}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

fn compile_reference(flag: &str) -> Vec<(String, Vec<u8>)> {
    compile_reference_source(flag, INTERFACE_SOURCE, "I")
}

fn compile_reference_source(flag: &str, source_text: &str, stem: &str) -> Vec<(String, Vec<u8>)> {
    let work = common::scratch_dir().expect("allocate kotlinc fixture");
    let source = work.join(format!("{stem}.kt"));
    let output = work.join("out");
    std::fs::create_dir_all(&output).expect("create kotlinc output");
    std::fs::write(&source, source_text).expect("write kotlinc fixture");
    let args = vec![
        "-d".to_string(),
        output.to_string_lossy().into_owned(),
        "-nowarn".to_string(),
        format!("-jvm-default={flag}"),
        source.to_string_lossy().into_owned(),
    ];
    let (code, stderr) = common::kotlinc_compile(&args).expect("reference compiler unavailable");
    assert_eq!(code, 0, "kotlinc failed under {flag}: {stderr}");
    let mut classes = Vec::new();
    collect_classes(&output, &output, &mut classes);
    classes.sort_by(|left, right| left.0.cmp(&right.0));
    let _ = std::fs::remove_dir_all(work);
    classes
}

fn public_method_shape(
    classes: &[(String, Vec<u8>)],
    class_name: &str,
) -> Vec<(String, String, u16)> {
    let bytes = classes
        .iter()
        .find_map(|(name, bytes)| (name == class_name).then_some(bytes))
        .unwrap_or_else(|| panic!("missing {class_name}.class"));
    let class = krusty::jvm::classreader::parse_class(bytes)
        .unwrap_or_else(|_| panic!("parse {class_name}.class"));
    let mut methods = class
        .methods
        .iter()
        .filter(|method| method.is_public())
        .map(|method| {
            (
                method.name.clone(),
                method.descriptor.clone(),
                method.access,
            )
        })
        .collect::<Vec<_>>();
    methods.sort();
    methods
}

/// kotlinc's own default emits both the interface default methods AND the `$DefaultImpls`
/// compatibility copy, so krusty's default must keep producing that class.
#[test]
fn enable_keeps_the_default_impls_compatibility_class() {
    let names = class_names(&compile(JvmDefaultMode::Enable));
    assert!(
        names.iter().any(|name| name == "I$DefaultImpls"),
        "`enable` keeps the compatibility holder: {names:?}"
    );
}

/// `enable` is a three-part compatibility surface, all measured against kotlinc: the interface keeps
/// its default methods and gains a `public static synthetic access$<name>$jd` bridge per
/// non-private body; the holder's statics FORWARD to those bridges; and every implementing class
/// gets an `ACC_BRIDGE` forwarder override per inherited default. A shape-level diff of all three
/// classes catches a member emitted with the right name but the wrong owner, flags, or realization.
#[test]
fn enable_public_method_realization_matches_kotlinc() {
    let ours = compile(JvmDefaultMode::Enable);
    let reference = compile_reference("enable");
    for class_name in ["I", "I$DefaultImpls", "C"] {
        assert_eq!(
            public_method_shape(&ours, class_name),
            public_method_shape(&reference, class_name),
            "{class_name} diverges under -jvm-default=enable"
        );
    }
}

/// The `enable` holder is a published ABI class exactly like the `disable` one: generic receiver
/// signatures, the `@java.lang.Deprecated` marker kotlinc puts on each forward, parameter
/// annotations, debug tables, `InnerClasses`, and the synthetic Kotlin metadata record all have to
/// match, or a consumer compiled against kotlinc's holder reads a different ABI from krusty's.
#[test]
fn the_enable_holder_is_byte_identical_to_kotlinc() {
    let ours = compile_source(JvmDefaultMode::Enable, HOLDER_BYTE_SOURCE, "Audit");
    let reference = compile_reference_source("enable", HOLDER_BYTE_SOURCE, "Audit");
    let ours = ours
        .iter()
        .find_map(|(name, bytes)| (name == "AuditI$DefaultImpls").then_some(bytes))
        .expect("krusty AuditI$DefaultImpls.class");
    let reference = reference
        .iter()
        .find_map(|(name, bytes)| (name == "AuditI$DefaultImpls").then_some(bytes))
        .expect("kotlinc AuditI$DefaultImpls.class");
    assert_eq!(ours, reference);
}

/// A sub-interface that declares NOTHING still republishes the compatibility surface for every
/// default it inherits: its own `access$…$jd` bridge (an `invokespecial` through itself, resolving
/// to the inherited default method) and its own `$DefaultImpls` holder forwarding to that bridge.
/// Without these, a legacy consumer naming `B$DefaultImpls.f` links against a class that exists in
/// kotlinc's output but not krusty's.
#[test]
fn an_empty_subinterface_republishes_the_inherited_compat_surface() {
    const SOURCE: &str =
        "interface A { fun f(): String = \"A.f\" }\ninterface B : A\nclass C : B\n";
    let ours = compile_source(JvmDefaultMode::Enable, SOURCE, "Sub");
    let reference = compile_reference_source("enable", SOURCE, "Sub");
    assert_eq!(
        class_names(&ours),
        class_names(&reference),
        "class set diverges for an empty sub-interface under -jvm-default=enable"
    );
    for class_name in ["B", "B$DefaultImpls", "C"] {
        assert_eq!(
            public_method_shape(&ours, class_name),
            public_method_shape(&reference, class_name),
            "{class_name} diverges for an empty sub-interface under -jvm-default=enable"
        );
    }
}

/// A `suspend` default member's forwarder must keep the CPS shape: kotlinc's implementing-class
/// forwarder is `s(Continuation)Object` (`ACC_BRIDGE`), never the semantic `s()I` — a forwarder
/// built from the declared signature names a method the interface does not have, and the class
/// then calls a `NoSuchMethodError` into existence. Compared against kotlinc on the class only,
/// in BOTH modes that forward (the `disable` forwarder shape was broken the same way): the
/// interface's own `s$suspendImpl` indirection is a separate, pre-existing suspend gap.
#[test]
fn a_suspend_default_member_forwarder_keeps_its_cps_shape() {
    const SOURCE: &str = "interface S1 { suspend fun s(): Int = 1 }\nclass SC : S1\n";
    for (mode, flag) in [
        (JvmDefaultMode::Enable, "enable"),
        (JvmDefaultMode::Disable, "disable"),
    ] {
        let ours = compile_source(mode, SOURCE, "Susp");
        let reference = compile_reference_source(flag, SOURCE, "Susp");
        assert_eq!(
            public_method_shape(&ours, "SC"),
            public_method_shape(&reference, "SC"),
            "the suspend forwarder shape diverges under -jvm-default={flag}"
        );
    }
}

/// An inherited default whose JVM name collides with a property ACCESSOR of the implementing
/// class must still get its forwarder unless the accessor IS the override (same full descriptor).
/// Suppressing by name alone broke both directions, measured on kotlinc: a `val` emits no setter,
/// so `setX(I)V` still needs its forwarder (dropping it left the class abstract —
/// `AbstractMethodError` under `disable`); and `getX()Ljava/lang/String;` (the accessor) legally
/// COEXISTS with the `getX()I` forwarder, since the JVM keys methods on the full descriptor.
#[test]
fn a_property_accessor_name_does_not_swallow_an_inherited_default() {
    for (name, source) in [
        (
            "val does not stand in for an inherited setter",
            "interface I { fun setX(v: Int) { } }\n\
             class C : I { val x: Int = 1 }\n\
             fun box(): String { val c: I = C(); c.setX(5); return if (C().x == 1) \"OK\" else \"fail\" }\n",
        ),
        (
            "distinct-return accessor coexists with the forwarder",
            "interface I { fun getX(): Int = 1 }\n\
             class C : I { val x: String = \"s\" }\n\
             fun box(): String { val c: I = C(); return if (c.getX() == 1 && C().x == \"s\") \"OK\" else \"fail\" }\n",
        ),
    ] {
        for mode in [JvmDefaultMode::Disable, JvmDefaultMode::Enable] {
            let classes = compile_source(mode, source, "T");
            let box_class = common::find_box_class(&classes)
                .unwrap_or_else(|| panic!("{name} ({mode:?}): no box class emitted"));
            let result = common::run_box(&classes, &box_class, &[common::stdlib_jar()])
                .unwrap_or_else(|| panic!("{name} ({mode:?}): JVM unavailable"));
            assert_eq!(result, "OK", "{name} under {mode:?}");
        }
    }
}

/// The same inheritance shapes the `disable` table runs, under `enable`: forwarder overrides,
/// diamond selection, transitive inheritance, and interface `super` calls all change realization
/// with the mode, and each of these failed at RUN time in some emitter state, not at compile time.
#[test]
fn every_inheritance_shape_runs_under_enable() {
    for (name, source, expected) in [
        (
            "direct",
            "interface I { fun f(): String = \"I.f\" }\n             class C : I\n             fun box(): String = if (C().f() == \"I.f\") \"OK\" else \"fail\"\n",
            "OK",
        ),
        (
            "transitive",
            "interface A { fun f(): String = \"A.f\" }\n             interface B : A\n             class C : B\n             fun box(): String { val c: A = C(); return if (c.f() == \"A.f\") \"OK\" else \"fail\" }\n",
            "OK",
        ),
        (
            "generic override",
            "interface I<T> { fun f(t: T): String = \"I.f\" }\n             class C : I<String> { override fun f(t: String): String = \"C.f\" }\n             fun box(): String = if (C().f(\"x\") == \"C.f\") \"OK\" else \"fail\"\n",
            "OK",
        ),
        (
            "enum",
            "interface I { fun f(): String = \"I.f\" }\n             enum class E : I { A, B }\n             fun box(): String = if (E.A.f() == \"I.f\") \"OK\" else \"fail\"\n",
            "OK",
        ),
        (
            "super call",
            "interface I { fun f(): String = \"I.f\" }\n             class C : I { override fun f(): String = \"C+\" + super.f() }\n             fun box(): String = if (C().f() == \"C+I.f\") \"OK\" else \"fail\"\n",
            "OK",
        ),
        (
            "overload, one overridden",
            "interface I { fun f(x: String): String = \"I:$x\"\n                           fun f(x: Int): String = \"I:$x\" }\n             class C : I { override fun f(x: Int): String = \"C:$x\" }\n             fun box(): String { val c: I = C()\n               return if (c.f(\"a\") == \"I:a\" && c.f(1) == \"C:1\") \"OK\" else \"fail\" }\n",
            "OK",
        ),
        (
            "diamond, most derived wins",
            "interface A { fun f(): String = \"A.f\" }\n             interface B : A { override fun f(): String = \"B.f\" }\n             interface D : A\n             class C : D, B\n             fun box(): String = if (C().f() == \"B.f\") \"OK\" else \"fail: \" + C().f()\n",
            "OK",
        ),
        (
            "private member",
            "interface I { private fun h(): String = \"h\"; fun f(): String = h() + \"!\" }\n             class C : I\n             fun box(): String = if (C().f() == \"h!\") \"OK\" else \"fail\"\n",
            "OK",
        ),
        (
            // A sub-interface overriding with an interface `super` call: the body compiles to a
            // direct `invokespecial A.f` from B's default method, and the class forwards to B.
            "sub-interface super chain",
            "interface A { fun f(): String = \"A.f\" }\n             interface B : A { override fun f(): String = \"B+\" + super.f() }\n             class C : B\n             fun box(): String = if (C().f() == \"B+A.f\") \"OK\" else \"fail\"\n",
            "OK",
        ),
    ] {
        let classes = compile_source(JvmDefaultMode::Enable, source, "T");
        let box_class = common::find_box_class(&classes)
            .unwrap_or_else(|| panic!("{name}: no box class emitted"));
        let result = common::run_box(&classes, &box_class, &[common::stdlib_jar()])
            .unwrap_or_else(|| panic!("{name}: JVM unavailable for the enable behavior test"));
        assert_eq!(result, expected, "{name}");
    }
}

/// THE risk this mode models: krusty's `enable` metadata advertises the full compatibility
/// realization (`jvmClassFlags` = 3), and a kotlinc-compiled downstream module in `disable` mode
/// trusts it — measured, its class forwarders are `invokespecial` on the dependency's default
/// methods and its omitted-default call sites `invokestatic` the dependency's interface-side
/// `$default` stub, while the `$DefaultImpls` holder remains the linking surface for
/// already-compiled legacy consumers. Any advertised piece missing from the jar is a
/// `NoSuchMethodError` at RUN time in the consumer's build — this test compiles a real kotlinc
/// consumer against the krusty jar and runs the mixed-compiler program.
#[test]
fn a_kotlinc_disable_consumer_links_against_the_krusty_enable_holder() {
    let work = common::scratch_dir().expect("allocate mixed-compiler jvm-default fixture");
    let library = work.join("library");
    let application = work.join("application");
    compile_module_to(
        JvmDefaultMode::Enable,
        r#"package dep
            interface I {
                val x: Int get() = 3
                fun f(): String = "I.f"
                fun g(value: Int = 7): String = "g$value"
            }
        "#,
        "Library",
        &library,
        None,
    );
    let app_source = work.join("Application.kt");
    std::fs::write(
        &app_source,
        r#"package app
            import dep.I
            class Inherited : I
            fun box(): String {
                val inherited: I = Inherited()
                return if (inherited.x == 3 && inherited.f() == "I.f" && inherited.g() == "g7")
                    "OK" else "fail"
            }
        "#,
    )
    .expect("write kotlinc consumer source");
    std::fs::create_dir_all(&application).expect("create kotlinc consumer output");
    let args = vec![
        "-d".to_string(),
        application.to_string_lossy().into_owned(),
        "-nowarn".to_string(),
        "-jvm-default=disable".to_string(),
        "-classpath".to_string(),
        library.to_string_lossy().into_owned(),
        app_source.to_string_lossy().into_owned(),
    ];
    let (code, stderr) = common::kotlinc_compile(&args).expect("reference compiler unavailable");
    assert_eq!(code, 0, "kotlinc consumer failed: {stderr}");
    let mut classes = Vec::new();
    collect_classes(&library, &library, &mut classes);
    collect_classes(&application, &application, &mut classes);
    let box_class = common::find_box_class(&classes).expect("mixed-compiler box class");
    let result = common::run_box(&classes, &box_class, &[common::stdlib_jar()])
        .expect("JVM unavailable for the mixed-compiler jvm-default test");
    assert_eq!(result, "OK");
    let _ = std::fs::remove_dir_all(work);
}

/// The exact class shape a `disable` consumer emits over an `enable` dependency, measured against
/// kotlinc on the SAME kotlinc-built dependency (so only the consumer differs): `invokespecial`
/// forwarders on the implementing class — never `invokestatic` into the dependency's holder,
/// which serves already-compiled legacy consumers only. The runtime test above cannot see the
/// difference (both realizations run); this differential pins the shape.
#[test]
fn a_disable_consumer_class_shape_over_an_enable_dependency_matches_kotlinc() {
    let work = common::scratch_dir().expect("allocate consumer-shape fixture");
    let library = work.join("library");
    std::fs::create_dir_all(&library).expect("create kotlinc dependency output");
    let dependency_source = work.join("Library.kt");
    std::fs::write(
        &dependency_source,
        r#"package dep
            interface I {
                val x: Int get() = 3
                fun f(): String = "I.f"
                fun g(value: Int = 7): String = "g$value"
            }
        "#,
    )
    .expect("write kotlinc dependency source");
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        library.to_string_lossy().into_owned(),
        "-nowarn".to_string(),
        "-jvm-default=enable".to_string(),
        dependency_source.to_string_lossy().into_owned(),
    ])
    .expect("reference compiler unavailable");
    assert_eq!(code, 0, "kotlinc dependency failed: {stderr}");

    const APP: &str = "package app\nimport dep.I\nclass Inherited : I\n";
    let ours_dir = work.join("ours");
    compile_module_to(
        JvmDefaultMode::Disable,
        APP,
        "Application",
        &ours_dir,
        Some(&library),
    );
    let reference_dir = work.join("reference");
    std::fs::create_dir_all(&reference_dir).expect("create kotlinc consumer output");
    let reference_source = work.join("Reference.kt");
    std::fs::write(&reference_source, APP).expect("write kotlinc consumer source");
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        "-nowarn".to_string(),
        "-jvm-default=disable".to_string(),
        "-classpath".to_string(),
        library.to_string_lossy().into_owned(),
        reference_source.to_string_lossy().into_owned(),
    ])
    .expect("reference compiler unavailable");
    assert_eq!(code, 0, "kotlinc consumer failed: {stderr}");

    let mut ours = Vec::new();
    collect_classes(&ours_dir, &ours_dir, &mut ours);
    let mut reference = Vec::new();
    collect_classes(&reference_dir, &reference_dir, &mut reference);
    assert_eq!(
        public_method_shape(&ours, "app/Inherited"),
        public_method_shape(&reference, "app/Inherited"),
        "the disable-consumer class shape over an enable dependency diverges from kotlinc"
    );
    let _ = std::fs::remove_dir_all(work);
}

/// The same consumption story inside krusty: a `disable`-mode module implementing an
/// `enable`-compiled dependency interface forwards through the dependency's PUBLISHED realization
/// (`invokespecial` to its default methods), not through a holder shape the consumer's own mode
/// would have produced.
#[test]
fn a_krusty_disable_consumer_uses_the_enable_dependency_defaults() {
    let work = common::scratch_dir().expect("allocate cross-module jvm-default fixture");
    let library = work.join("library");
    let application = work.join("application");
    compile_module_to(
        JvmDefaultMode::Enable,
        r#"package dep
            interface I {
                val x: Int get() = 3
                fun f(): String = "I.f"
            }
        "#,
        "Library",
        &library,
        None,
    );
    compile_module_to(
        JvmDefaultMode::Disable,
        r#"package app
            import dep.I
            class Inherited : I
            class Explicit : I {
                override fun f(): String = "E+" + super.f()
            }
            fun box(): String {
                val inherited: I = Inherited()
                return if (inherited.x == 3 && inherited.f() == "I.f" &&
                    Explicit().f() == "E+I.f") "OK" else "fail"
            }
        "#,
        "Application",
        &application,
        Some(&library),
    );
    let mut classes = Vec::new();
    collect_classes(&library, &library, &mut classes);
    collect_classes(&application, &application, &mut classes);
    let box_class = common::find_box_class(&classes).expect("cross-module box class");
    let result = common::run_box(&classes, &box_class, &[common::stdlib_jar()])
        .expect("JVM unavailable for cross-module jvm-default test");
    assert_eq!(result, "OK");
    let _ = std::fs::remove_dir_all(work);
}

/// The mode intellij-community builds with (`-Xjvm-default=all`). kotlinc emits NO `$DefaultImpls`
/// at all — a build that links against these classes would resolve a holder that should not exist.
#[test]
fn no_compatibility_emits_no_default_impls_class() {
    let names = class_names(&compile(JvmDefaultMode::NoCompatibility));
    assert!(
        !names.iter().any(|name| name.contains("DefaultImpls")),
        "`no-compatibility` emits no compatibility holder: {names:?}"
    );
    assert!(
        names.iter().any(|name| name == "I"),
        "the interface itself is still emitted: {names:?}"
    );
}

/// `disable`: no default methods at all. Every interface member is abstract, every body moves to the
/// `$DefaultImpls` holder as a static taking the receiver as parameter 0, and each implementing class
/// forwards to it with `invokestatic`. Measured against kotlinc 2.4.10 — the class SET and the member
/// set of all three classes must match.
#[test]
fn disable_moves_every_body_to_the_holder() {
    let ours = compile(JvmDefaultMode::Disable);
    let reference = compile_reference("disable");
    assert_eq!(
        class_names(&ours),
        class_names(&reference),
        "class set diverges under -jvm-default=disable"
    );
    for class_name in ["I", "I$DefaultImpls", "C"] {
        let mut mine = public_method_shape(&ours, class_name);
        let mut theirs = public_method_shape(&reference, class_name);
        mine.sort();
        theirs.sort();
        assert_eq!(
            mine, theirs,
            "{class_name} diverges under -jvm-default=disable"
        );
    }
}

/// The holder is a published ABI class, not an implementation detail. This fixture covers every
/// attribute that adding its receiver can shift: generic signatures, parameter annotations, local
/// slots, nested-class metadata, and the synthetic Kotlin metadata record. Exact bytes ensure none
/// of those silently falls back to a merely executable shape.
#[test]
fn the_disable_holder_is_byte_identical_to_kotlinc() {
    let ours = compile_source(JvmDefaultMode::Disable, HOLDER_BYTE_SOURCE, "Audit");
    let reference = compile_reference_source("disable", HOLDER_BYTE_SOURCE, "Audit");
    let ours = ours
        .iter()
        .find_map(|(name, bytes)| (name == "AuditI$DefaultImpls").then_some(bytes))
        .expect("krusty AuditI$DefaultImpls.class");
    let reference = reference
        .iter()
        .find_map(|(name, bytes)| (name == "AuditI$DefaultImpls").then_some(bytes))
        .expect("kotlinc AuditI$DefaultImpls.class");
    assert_eq!(ours, reference);
}

/// A bodied interface whose PROPERTY comes FIRST. `AuditI` above declares its property last, which
/// the old functions-then-accessors grouping happened to reproduce — this fixture is the one that
/// tells the source-order rule apart from the grouping under `disable`. The holder (published ABI)
/// must be byte-identical; the interface class still carries a known, unrelated `@Metadata` flags
/// gap on bodied members (kotlinc marks them OPEN), so its member ORDER is compared through the
/// classreader instead of whole-file bytes: `getAnswer` before `echo` before `getTail`.
const PROPERTY_FIRST_SOURCE: &str = r#"
interface PropFirstI {
    val answer: Int get() = 42
    fun echo(value: String): String = value
    val tail: Int get() = 7
}

class PropFirstC : PropFirstI
"#;

#[test]
fn disable_keeps_property_first_member_order() {
    let ours = compile_source(JvmDefaultMode::Disable, PROPERTY_FIRST_SOURCE, "PropFirst");
    let reference = compile_reference_source("disable", PROPERTY_FIRST_SOURCE, "PropFirst");
    let mine = ours
        .iter()
        .find_map(|(name, bytes)| (name == "PropFirstI$DefaultImpls").then_some(bytes))
        .expect("krusty PropFirstI$DefaultImpls.class");
    let theirs = reference
        .iter()
        .find_map(|(name, bytes)| (name == "PropFirstI$DefaultImpls").then_some(bytes))
        .expect("kotlinc PropFirstI$DefaultImpls.class");
    assert_eq!(
        mine, theirs,
        "PropFirstI$DefaultImpls bytes diverge under -jvm-default=disable"
    );
    // ORDERED method tables (no sort — the order is the assertion).
    let ordered = |classes: &[(String, Vec<u8>)]| -> Vec<(String, String)> {
        let bytes = classes
            .iter()
            .find_map(|(name, bytes)| (name == "PropFirstI").then_some(bytes))
            .expect("missing PropFirstI.class");
        let class = krusty::jvm::classreader::parse_class(bytes).expect("parse PropFirstI.class");
        class
            .methods
            .iter()
            .map(|method| (method.name.clone(), method.descriptor.clone()))
            .collect()
    };
    assert_eq!(
        ordered(&ours),
        ordered(&reference),
        "PropFirstI member order diverges under -jvm-default=disable"
    );
}

/// The forwarders are what make the artifact correct: without them an implementing class does not
/// implement its own interface, and every inherited call is an `AbstractMethodError` at run time.
#[test]
fn a_disable_compiled_program_still_runs() {
    let classes = compile(JvmDefaultMode::Disable);
    let box_class = common::find_box_class(&classes).expect("no box class emitted");
    let Some(result) = common::run_box(&classes, &box_class, &[common::stdlib_jar()]) else {
        return; // no JVM available — the shape assertions above still ran
    };
    assert_eq!(result, "OK");
}

/// The set of class files krusty produces must match kotlinc's for the same sources and the same
/// `-jvm-default` value. This is the check that would have caught krusty emitting its one interface
/// strategy regardless of the flag.
#[test]
fn the_emitted_class_set_matches_kotlinc_per_mode() {
    for (mode, flag) in [
        (JvmDefaultMode::Enable, "enable"),
        (JvmDefaultMode::NoCompatibility, "no-compatibility"),
    ] {
        let reference = class_names(&compile_reference(flag));
        let ours = class_names(&compile(mode));
        assert_eq!(
            ours, reference,
            "class set diverges under -jvm-default={flag}"
        );
    }
}

/// `no-compatibility` is claimed as a fully modelled class shape, so compare the public methods and
/// their concrete/abstract/static realization as well as the class set. A class-name-only check
/// cannot distinguish a correctly emitted interface from one whose methods live in the wrong owner.
#[test]
fn no_compatibility_public_method_shape_matches_kotlinc() {
    let ours = compile(JvmDefaultMode::NoCompatibility);
    let reference = compile_reference("no-compatibility");
    for class_name in ["I", "C"] {
        assert_eq!(
            public_method_shape(&ours, class_name),
            public_method_shape(&reference, class_name),
            "{class_name} public method shape"
        );
    }
}

/// `jvmClassFlags` (`Class` JvmProtoBuf extension field 104) records the shape a consumer will find:
/// bit 0 "bodies live on the interface", bit 1 "a compatibility copy exists". kotlinc writes 3 for
/// `enable` and 1 for `no-compatibility`.
///
/// Asserted on the EMITTED metadata, not on the enum: a mode that changed the class set but left the
/// metadata saying "a compatibility copy exists" would publish a class file whose bytes and whose
/// metadata disagree, which is exactly what a consumer compiling against it would act on.
#[test]
fn the_emitted_metadata_records_the_mode_that_produced_the_class() {
    for (mode, flag, trailer) in [
        (JvmDefaultMode::Enable, "enable", "\\u0006\\u0003"),
        (
            JvmDefaultMode::NoCompatibility,
            "no-compatibility",
            "\\u0006\\u0001",
        ),
    ] {
        let classes = compile(mode);
        let metadata = interface_metadata_line(&classes);
        let reference_metadata = interface_metadata_line(&compile_reference(flag));
        assert!(
            reference_metadata.contains(trailer),
            "kotlinc {flag}: expected jvmClassFlags trailer {trailer} in {reference_metadata}"
        );
        assert!(
            metadata.contains(trailer),
            "{mode:?}: expected the jvmClassFlags trailer {trailer} in {metadata}"
        );
    }
}

/// The `d1` line of `I`'s `@Metadata`, via the required pooled `javap` tool.
fn interface_metadata_line(classes: &[(String, Vec<u8>)]) -> String {
    let dir = common::scratch_dir().expect("allocate metadata fixture");
    for (name, bytes) in classes {
        let path = dir.join(format!("{name}.class"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create metadata class directory");
        }
        std::fs::write(path, bytes).expect("write metadata class");
    }
    let class_file = dir.join("I.class");
    let text = common::javap(&["-v", "-p", &class_file.to_string_lossy()])
        .expect("JVM unavailable for metadata inspection");
    let line = text
        .lines()
        .find(|line| line.trim_start().starts_with("d1="))
        .expect("interface Metadata.d1")
        .to_string();
    let _ = std::fs::remove_dir_all(&dir);
    line
}

/// The shape change must not change behavior: the same program produces the same result whichever
/// compatibility strategy its interfaces were compiled with.
#[test]
fn a_program_behaves_the_same_under_every_modelled_mode() {
    for mode in [JvmDefaultMode::Enable, JvmDefaultMode::NoCompatibility] {
        let classes = compile(mode);
        let Some(box_class) = common::find_box_class(&classes) else {
            panic!("{mode:?}: no box class emitted");
        };
        let result = common::run_box(&classes, &box_class, &[common::stdlib_jar()])
            .expect("JVM unavailable for jvm-default behavior test");
        assert_eq!(result, "OK", "{mode:?}");
    }
}

/// The shapes a one-interface fixture cannot see. Each of these compiled clean and then failed at RUN
/// time before the emitter handled it — `AbstractMethodError`, `ClassFormatError`, `VerifyError` —
/// so each asserts the program's OUTPUT, not its class shape.
#[test]
fn every_inheritance_shape_runs_under_disable() {
    for (name, source, expected) in [
        (
            "direct",
            "interface I { fun f(): String = \"I.f\" }\n             class C : I\n             fun box(): String = if (C().f() == \"I.f\") \"OK\" else \"fail\"\n",
            "OK",
        ),
        (
            // The body is inherited through an intermediate interface: forwarding only for DIRECT
            // superinterfaces leaves the class not implementing its own interface.
            "transitive",
            "interface A { fun f(): String = \"A.f\" }\n             interface B : A\n             class C : B\n             fun box(): String { val c: A = C(); return if (c.f() == \"A.f\") \"OK\" else \"fail\" }\n",
            "OK",
        ),
        (
            // The class already carries an erasure bridge with this signature; a forwarder beside it
            // makes two methods with one name and descriptor, and the class will not load.
            "generic override",
            "interface I<T> { fun f(t: T): String = \"I.f\" }\n             class C : I<String> { override fun f(t: String): String = \"C.f\" }\n             fun box(): String = if (C().f(\"x\") == \"C.f\") \"OK\" else \"fail\"\n",
            "OK",
        ),
        (
            // An enum reaches emission through a different function than an ordinary class.
            "enum",
            "interface I { fun f(): String = \"I.f\" }\n             enum class E : I { A, B }\n             fun box(): String = if (E.A.f() == \"I.f\") \"OK\" else \"fail\"\n",
            "OK",
        ),
        (
            // `super.f()` is a non-virtual call to a body that now lives on the holder.
            "super call",
            "interface I { fun f(): String = \"I.f\" }\n             class C : I { override fun f(): String = \"C+\" + super.f() }\n             fun box(): String = if (C().f() == \"C+I.f\") \"OK\" else \"fail\"\n",
            "OK",
        ),
        (
            // Two same-arity overloads, one overridden: keying the skip on arity alone dropped the
            // OTHER one's forwarder, and the class stopped implementing its interface.
            "overload, one overridden",
            "interface I { fun f(x: String): String = \"I:$x\"\n                           fun f(x: Int): String = \"I:$x\" }\n             class C : I { override fun f(x: Int): String = \"C:$x\" }\n             fun box(): String { val c: I = C()\n               return if (c.f(\"a\") == \"I:a\" && c.f(1) == \"C:1\") \"OK\" else \"fail\" }\n",
            "OK",
        ),
        (
            // A member declared on `A` and overridden on `B` must forward to the MOST DERIVED
            // declaration. Taking whichever the walk reached first made the answer depend on the
            // order the class listed its supertypes.
            "diamond, most derived wins",
            "interface A { fun f(): String = \"A.f\" }\n             interface B : A { override fun f(): String = \"B.f\" }\n             interface D : A\n             class C : D, B\n             fun box(): String = if (C().f() == \"B.f\") \"OK\" else \"fail: \" + C().f()\n",
            "OK",
        ),
        (
            // A private interface member: its body moves to the holder as a PRIVATE static, it gets
            // no class forwarder, and the call inside the moved body must go to the holder too — an
            // `invokespecial` naming the interface from another class does not even verify.
            "private member",
            "interface I { private fun h(): String = \"h\"; fun f(): String = h() + \"!\" }\n             class C : I\n             fun box(): String = if (C().f() == \"h!\") \"OK\" else \"fail\"\n",
            "OK",
        ),
    ] {
        let classes = compile_source(JvmDefaultMode::Disable, source, "T");
        let box_class = common::find_box_class(&classes)
            .unwrap_or_else(|| panic!("{name}: no box class emitted"));
        let result = common::run_box(&classes, &box_class, &[common::stdlib_jar()])
            .unwrap_or_else(|| panic!("{name}: JVM unavailable for the disable behavior test"));
        assert_eq!(result, expected, "{name}");
    }
}

/// A private interface member's body is a PRIVATE static on the holder, and never a member of the
/// implementing class's ABI — both measured against kotlinc.
#[test]
fn a_private_interface_member_stays_private_under_disable() {
    let source = "interface I { private fun h(): String = \"h\"; fun f(): String = h() + \"!\" }\n                  class C : I\n";
    let ours = compile_source(JvmDefaultMode::Disable, source, "T");
    let holder = public_method_shape(&ours, "I$DefaultImpls");
    assert!(
        !holder.iter().any(|(name, _, _)| name == "h"),
        "a private holder static is not public: {holder:?}"
    );
    let implementer = public_method_shape(&ours, "C");
    assert!(
        !implementer.iter().any(|(name, _, _)| name == "h"),
        "a private interface member never reaches the class ABI: {implementer:?}"
    );
}

/// A dependency's declaration and physical realization are one provider record. The consumer's own
/// `-jvm-default` setting must not reinterpret a classpath interface: ordinary dispatch uses the
/// inherited class forwarder, `super.f()` and omitted defaults use the exact holder targets published
/// by the dependency.
#[test]
fn a_disable_dependency_is_consumed_through_its_recorded_realizations() {
    let work = common::scratch_dir().expect("allocate cross-module jvm-default fixture");
    let library = work.join("library");
    let application = work.join("application");
    compile_module_to(
        JvmDefaultMode::Disable,
        r#"package dep
            interface I {
                val x: Int get() = 3
                fun f(): String = "I.f"
                fun g(value: Int = 7): String = "g$value"
            }
        "#,
        "Library",
        &library,
        None,
    );
    compile_module_to(
        JvmDefaultMode::NoCompatibility,
        r#"package app
            import dep.I
            class Inherited : I
            class Explicit : I {
                override val x: Int get() = super.x + 1
                override fun f(): String = "E+" + super.f()
            }
            fun box(): String {
                val inherited: I = Inherited()
                return if (inherited.x == 3 && inherited.f() == "I.f" && inherited.g() == "g7" &&
                    Explicit().x == 4 && Explicit().f() == "E+I.f") "OK" else "fail"
            }
        "#,
        "Application",
        &application,
        Some(&library),
    );
    let mut classes = Vec::new();
    collect_classes(&library, &library, &mut classes);
    collect_classes(&application, &application, &mut classes);
    let box_class = common::find_box_class(&classes).expect("cross-module box class");
    let result = common::run_box(&classes, &box_class, &[common::stdlib_jar()])
        .expect("JVM unavailable for cross-module jvm-default test");
    assert_eq!(result, "OK");
    let _ = std::fs::remove_dir_all(work);
}

/// Sibling files are declarations from the same normalized module provider, not a special IR-only
/// classifier kind. A class emitted from one file must receive the property forwarder declared by
/// an interface in another file without searching that class's current-file IR.
#[test]
fn a_disable_property_forwarder_crosses_source_files() {
    let classes = compile_sources(
        JvmDefaultMode::Disable,
        &[
            ("Api", "interface I { val x: Int get() = 4 }"),
            (
                "Impl",
                "class C : I\nfun box(): String { val value: I = C(); return if (value.x == 4) \"OK\" else \"fail\" }",
            ),
        ],
    );
    let box_class = common::find_box_class(&classes).expect("cross-file box class");
    let result = common::run_box(&classes, &box_class, &[common::stdlib_jar()])
        .expect("JVM unavailable for cross-file jvm-default test");
    assert_eq!(result, "OK");
}

/// A compatibility holder is not evidence that an interface accessor lives there. Under `enable`
/// the interface method is concrete and must retain virtual dispatch, including when a consumer is
/// compiled in another mode. Calling through the interface type therefore reaches the override.
#[test]
fn an_enable_dependency_property_keeps_virtual_dispatch() {
    let work = common::scratch_dir().expect("allocate cross-module jvm-default fixture");
    let library = work.join("library");
    let application = work.join("application");
    compile_module_to(
        JvmDefaultMode::Enable,
        r#"package dep
            interface I { val x: Int get() = 3 }
        "#,
        "Library",
        &library,
        None,
    );
    compile_module_to(
        JvmDefaultMode::NoCompatibility,
        r#"package app
            import dep.I
            class C : I { override val x: Int get() = 9 }
            fun box(): String {
                val value: I = C()
                return if (value.x == 9) "OK" else "fail: " + value.x
            }
        "#,
        "Application",
        &application,
        Some(&library),
    );
    let mut classes = Vec::new();
    collect_classes(&library, &library, &mut classes);
    collect_classes(&application, &application, &mut classes);
    let box_class = common::find_box_class(&classes).expect("cross-module box class");
    let result = common::run_box(&classes, &box_class, &[common::stdlib_jar()])
        .expect("JVM unavailable for cross-module jvm-default test");
    assert_eq!(result, "OK");
    let _ = std::fs::remove_dir_all(work);
}
