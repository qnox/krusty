//! Java-source interop tests compile Java fixtures through the shared harness, add those classes to
//! krusty's classpath, and run the Java and Kotlin outputs together.

use super::common;

/// javac_compile alone: one Java class in, its `.class` bytes out.
#[test]
fn javac_compile_returns_class_bytes() {
    let Some(out) = common::javac_compile(
        &[(
            "J.java".to_string(),
            "public class J { public static String ok() { return \"OK\"; } }".to_string(),
        )],
        &[],
    ) else {
        eprintln!("skipping: JDK unavailable");
        return;
    };
    let (dir, classes) = out;
    assert_eq!(classes.len(), 1, "one class expected, got {classes:?}");
    assert_eq!(classes[0].0, "J");
    // Class-file magic.
    assert_eq!(&classes[0].1[..4], &[0xCA, 0xFE, 0xBA, 0xBE]);
    assert!(dir.join("J.class").is_file());
    cleanup(&dir);
}

/// Nested types come back too, named by their relative path stem.
#[test]
fn javac_compile_collects_nested_classes() {
    let Some((_dir, classes)) = common::javac_compile(
        &[(
            "Outer.java".to_string(),
            "public class Outer { public interface Inner { void run(); } }".to_string(),
        )],
        &[],
    ) else {
        eprintln!("skipping: JDK unavailable");
        return;
    };
    let mut names: Vec<&str> = classes.iter().map(|(n, _)| n.as_str()).collect();
    names.sort();
    assert_eq!(names, ["Outer", "Outer$Inner"]);
    cleanup(&_dir);
}

/// Remove a `javac_compile` scratch tree (the classes dir's parent holds both `src/` and
/// `classes/`).
fn cleanup(classes_dir: &std::path::Path) {
    if let Some(root) = classes_dir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
}

/// A public Java instance field is a Kotlin property, including when it is inherited and its
/// declared generic type is specialized by the receiver's superclass chain. Keep this in the
/// compiler harness: the contract is frontend typing plus executable JVM lowering, not LSP wiring.
#[test]
fn public_java_instance_fields_are_typed_and_executable() {
    let java_classpath = common::classpath_jars_for("");
    let Some((java_dir, _)) = common::javac_compile(
        &[
            (
                "p/Box.java".to_string(),
                "package p; public class Box<T extends CharSequence> { public final T value; public final kotlin.jvm.functions.Function0<Integer> callback = () -> 1; public Box() { this.value = null; } public Box(T value) { this.value = value; } public T getValue() { throw new AssertionError(\"getter must not win over field\"); } public String getCallback() { return \"getter\"; } }"
                    .to_string(),
            ),
            (
                "p/StringBox.java".to_string(),
                "package p; public final class StringBox extends Box<String> { public StringBox(String value) { super(value); } }"
                    .to_string(),
            ),
        ],
        &java_classpath,
    ) else {
        eprintln!("skipping: JDK unavailable");
        return;
    };
    let source = r#"
class SourceBox : p.Box<String>()
class ShadowBox : p.Box<String>() { val value: Int = 7 }

fun nullableValueLength(box: p.Box<String>?): Int = box?.value?.length ?: 0
fun directValue(box: p.Box<String>): String = box.value

fun box(): String {
    val direct: String = directValue(p.StringBox("O"))
    val inherited: String = p.StringBox("K").value
    // `SourceBox()` leaves the Java field null. The field is platform-typed, so a NON-null target
    // here is a narrowing kotlinc guards — measured: it throws `NullPointerException: value must not
    // be null` rather than binding null. This case is about the field being READ and executed, so it
    // keeps the nullable target that admits the null; the guard itself is covered by
    // `platform_call_assertions_e2e`.
    val sourceInherited: String? = SourceBox().value
    return if (ShadowBox().value == 7) direct + inherited + sourceInherited else "FAIL"
}
"#;
    let jdk = common::jdk_modules();
    let mut classpath = common::classpath_jars_for(source);
    classpath.push(java_dir.clone());
    let diagnostics = common::front_end_diagnostics(source, &classpath, Some(jdk.as_path()));
    assert!(
        diagnostics.is_empty(),
        "public Java fields should resolve as Kotlin properties: {diagnostics:?}"
    );
    let result = common::compile_and_run_box(source, "Main", &classpath, Some(jdk.as_path()));
    let nullable_diagnostics = common::front_end_diagnostics(
        "fun invalid(box: p.Box<String>?): Int = box.callback()",
        &classpath,
        Some(jdk.as_path()),
    );
    cleanup(&java_dir);

    assert_eq!(result.as_deref(), Some("OKnull"));
    assert_eq!(
        nullable_diagnostics,
        ["only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Box<String>?'."]
    );
}

/// Public and protected Java fields remain writable when a superclass also exposes bean accessors.
#[test]
fn java_instance_field_writes_public_and_protected() {
    run_mixed(
        &[
            (
                "p/Grand.java",
                "package p; public class Grand { protected String prot; protected String accessed; protected int count; }",
            ),
            (
                "p/Base.java",
                "package p; public class Base extends Grand { public String pub; public int publicCount; public String getPub() { return \"getter\"; } public String getProt() { return \"getter\"; } public String getAccessed() { return accessed; } public void setAccessed(String value) { accessed = value; } public int getCount() { return 99; } public int getPublicCount() { return 99; } }",
            ),
        ],
        r#"
class Sub : p.Base() {
    fun setAll() {
        prot = "p"
        this.prot = "q"
        publicCount++
        ++publicCount
        count++
        ++count
    }
    fun readAll(): String = pub + prot + publicCount + count
    fun expressionUpdates(): String {
        publicCount = 1
        count = 1
        val oldPublic = publicCount++
        val newPublic = ++publicCount
        val oldProtected = count++
        val newProtected = ++count
        return "$oldPublic$newPublic$oldProtected$newProtected"
    }
}
fun box(): String {
    val sub = Sub()
    sub.pub = "a"
    sub.setAll()
    sub.publicCount++
    sub.accessed = "z"
    val read = sub.readAll() + sub.accessed
    val expressions = Sub().expressionUpdates()
    return if (read == "aq32z" && expressions == "1313") "OK" else "FAIL:$read:$expressions"
}
"#,
    );
}

/// Java package-private visibility is honored from Kotlin in the SAME package: a package-private
/// class's package-private static member is callable (`UiThreadPriority.adjust()` in intellij's
/// `platform-impl/bootstrap`, called from `ui.kt` in the same package). Kotlin has no
/// package-private keyword, but the compiler maps Java package-private to package-scoped access.
#[test]
fn package_private_java_members_are_accessible_within_the_same_package() {
    run_mixed(
        &[(
            "p/Api.java",
            "package p; public final class Api {\n\
                 String field = \"F\";\n\
                 static String staticField = \"S\";\n\
                 Api() {}\n\
                 String instanceMethod() { return \"I\"; }\n\
                 static String staticMethod() { return \"M\"; }\n\
             }",
        )],
        "package p\n\
         fun box(): String {\n\
             val api = Api()\n\
             api.field = \"A\"\n\
             Api.staticField = \"B\"\n\
             val result = api.field + Api.staticField + api.instanceMethod() + Api.staticMethod()\n\
             return if (result == \"ABIM\") \"OK\" else result\n\
         }\n",
    );
}

#[test]
fn package_private_java_field_does_not_hide_public_method_property() {
    let source = "fun box(): String {\n\
             val values = java.util.HashMap<String, String>()\n\
             values[\"key\"] = \"value\"\n\
             return if (values.size == 1) \"OK\" else values.size.toString()\n\
         }\n";
    assert_eq!(common::checker_diags_with_stdlib(source), Some(Vec::new()));
    let result = common::compile_and_run_with_stdlib(source, "Main")
        .expect("HashMap.size must select the public method property");
    assert_eq!(result, "OK");
}

#[test]
fn package_private_java_static_rejected_cross_package() {
    let diagnostics = mixed_diagnostics(
        &[(
            "p/Helper.java",
            "package p; final class Helper { static void adjust() { } }",
        )],
        "package q\nfun bad() { p.Helper.adjust() }\n",
    )
    .expect("javac");
    assert_eq!(
        diagnostics,
        [
            "cannot access 'class Helper : Any': it is package-private in file.",
            "cannot access 'static fun adjust(): Unit': it is package-private in 'p.Helper'."
        ]
    );
}

#[test]
fn package_private_java_static_field_read_within_same_package() {
    run_mixed(
        &[(
            "p/Pub.java",
            "package p; public class Pub { static int count = 7; }",
        )],
        "package p\n\
         fun box(): String = if (Pub.count + p.Pub.count == 14) \"OK\" else \"FAIL\"\n",
    );
}

/// The circular direction: Java extends a Kotlin class while Kotlin calls the Java class. Both
/// sources enter the production frontend together; its JVM provider publishes Java declaration
/// headers during Pass 1, Kotlin is emitted, and only then does javac compile the real Java body.
#[test]
fn java_extends_kotlin_via_production_source_headers() {
    let kotlin = r#"
open class A {
    open fun name(): String = "FAIL:A"
}

fun box(): String = J().name()
"#;
    let java = "public class J extends A { @Override public String name() { return \"OK\"; } }";
    let jdk = common::jdk_modules();
    let jars = common::classpath_jars_for(kotlin);
    let mut cp_paths = jars.clone();
    cp_paths.push(jdk.clone());
    let classpath = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(cp_paths));
    let inputs = [
        krusty::source::SourceInput::kotlin(kotlin).with_file_stem("Main"),
        krusty::source::SourceInput::java(java).with_file_stem("J"),
    ];
    let stems = vec!["Main".to_string(), "J".to_string()];
    let mut diagnostics = krusty::diag::DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(
            classpath.clone(),
        )),
        &krusty::features::LangFeatures::new(),
        |files, symbols| krusty::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );
    let outputs = krusty::compiler::emit_analyzed(
        analysis,
        &stems,
        &krusty::jvm::JvmBackend::new(classpath),
        "main",
        &mut diagnostics,
    );
    assert!(
        !diagnostics.has_errors(),
        "mixed-source production frontend rejected the legal cycle: {:?}",
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| diagnostic.msg.as_str())
            .collect::<Vec<_>>()
    );
    let kotlin_classes = outputs
        .into_iter()
        .filter_map(|(path, bytes)| {
            path.strip_suffix(".class")
                .map(|name| (name.to_string(), bytes))
        })
        .collect::<Vec<_>>();

    let root = std::env::temp_dir().join(format!("krusty_stub_e2e_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let kotlindir = root.join("kotlin");

    // Real javac sees krusty's output; provider-owned Java headers never reach the runtime.
    for (name, bytes) in &kotlin_classes {
        let p = kotlindir.join(format!("{name}.class"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, bytes).unwrap();
    }
    let mut javac_cp = jars.clone();
    javac_cp.push(kotlindir.clone());
    let Some((javadir, java_classes)) =
        common::javac_compile(&[("J.java".to_string(), java.to_string())], &javac_cp)
    else {
        let _ = std::fs::remove_dir_all(&root);
        panic!("javac should compile J against krusty's emitted A");
    };
    cleanup(&javadir);
    let _ = std::fs::remove_dir_all(&root);

    // Run with the real classes only.
    let mut classes = kotlin_classes;
    classes.extend(java_classes);
    let box_class = common::find_box_class(&classes).expect("box() class");
    let got = common::run_box(&classes, &box_class, &jars).expect("box run");
    assert_eq!(got, "OK");
}

#[test]
fn kotlin_covariant_supertype_is_invariant_in_a_java_class_header() {
    let kotlin = r#"
        open class A<T> : Collection<T> {
            override val size: Int get() = 0
            override fun contains(element: T): Boolean = false
            override fun containsAll(elements: Collection<T>): Boolean = false
            override fun isEmpty(): Boolean = true
            override fun iterator(): Iterator<T> = emptyList<T>().iterator()
        }

        interface L<E> : List<E>
    "#;
    let Some(kotlin_classes) = common::compile_lib("java_invariant_supertype", kotlin) else {
        return;
    };
    let java = r#"
        public abstract class B<E> extends A<E> implements L<E> {
            public void insert(E value) { add(0, value); }
        }
    "#;
    let result = common::javac_compile(
        &[("B.java".to_string(), java.to_string())],
        &[kotlin_classes, common::stdlib_jar()],
    );
    assert!(result.is_some(), "javac must see L<E> as java.util.List<E>");
}

/// The `// MODULE:` chaining shape with a Java file in the DEPENDENCY module: `lib` is a Java
/// class plus Kotlin that uses it, `main` is Kotlin `box()` against lib's emitted dir on the
/// classpath — the same javac-first, dir-chaining flow `compile_module_test` performs per module.
#[test]
fn module_dependency_with_java_source() {
    let jdk = common::jdk_modules();
    let jars = common::classpath_jars_for("");
    // Module `lib`: J.java + lib.kt (Kotlin wrapping the Java class).
    let Some((javadir, java_classes)) = common::javac_compile(
        &[(
            "J.java".to_string(),
            "public class J { public static String part() { return \"O\"; } }".to_string(),
        )],
        &jars,
    ) else {
        eprintln!("skipping: JDK unavailable");
        return;
    };
    let mut libcp = jars.clone();
    libcp.push(javadir.clone());
    let lib_kotlin = common::compile_in_process(
        "class A { fun part(): String = J.part() + \"K\" }",
        "Lib",
        &libcp,
        Some(jdk.as_path()),
    )
    .expect("lib kotlin compiles against the javac dir");
    // Write lib's FULL output (kotlin + java classes) to one dir — the chained module classpath.
    let root = std::env::temp_dir().join(format!("krusty_modjava_e2e_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let libdir = root.join("lib");
    for (name, bytes) in lib_kotlin.iter().chain(java_classes.iter()) {
        let p = libdir.join(format!("{name}.class"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, bytes).unwrap();
    }
    if let Some(jroot) = javadir.parent() {
        let _ = std::fs::remove_dir_all(jroot);
    }
    // Module `main`: box() against lib's dir.
    let mut maincp = jars.clone();
    maincp.push(libdir);
    let main_classes = common::compile_in_process(
        "fun box(): String = A().part()",
        "MainKt",
        &maincp,
        Some(jdk.as_path()),
    )
    .expect("main compiles against lib's emitted dir");
    let mut classes = lib_kotlin;
    classes.extend(java_classes);
    classes.extend(main_classes);
    let _ = std::fs::remove_dir_all(&root);
    let box_class = common::find_box_class(&classes).expect("box class");
    let got = common::run_box(&classes, &box_class, &jars).expect("box run");
    assert_eq!(got, "OK");
}

/// Kotlin `box()` calling a static method on a javac-compiled Java class (the
/// `constants/numberLiteralCoercionToInferredType.kt` shape, minus the K2-ignored parts).
#[test]
fn kotlin_calls_java_static() {
    run_mixed(
        &[(
            "J.java",
            "public class J { public static String greet() { return \"OK\"; } }",
        )],
        "fun box(): String = J.greet()",
    );
}

/// Kotlin class extending a javac-compiled Java base and overriding its method (the
/// `fakeOverride/kt40180.kt` shape).
#[test]
fn kotlin_extends_java_base() {
    run_mixed(
        &[(
            "Base.java",
            "public class Base { public String foo(String s) { return \"FAIL:base\"; } }",
        )],
        r#"
class Derived : Base() {
    override fun foo(s: String): String = s
}

fun box(): String = Derived().foo("OK")
"#,
    );
}

#[test]
fn nullable_value_reaches_unannotated_java_parameters() {
    run_mixed(
        &[
            (
                "PlatformBase.java",
                r#"
public class PlatformBase {
    public String normalize(String value) {
        return value == null ? "OK" : value;
    }
    public static String normalizeStatic(String value) {
        return value == null ? "OK" : value;
    }
}
"#,
            ),
            (
                "PlatformContainer.java",
                r#"
public class PlatformContainer {
    public static class Value {
        private final String value;

        public Value(String value) {
            this.value = value;
        }

        public String normalize() {
            return value == null ? "OK" : value;
        }
    }
}
"#,
            ),
        ],
        r#"
class PlatformDerived : PlatformBase() {
    fun normalizeNullable(value: String?): String {
        val inherited = super.normalize(value)
        val direct = PlatformBase.normalizeStatic(value)
        val literal = PlatformBase.normalizeStatic(null)
        val constructed = PlatformContainer.Value(value).normalize()
        return if (inherited == "OK" && direct == "OK" && literal == "OK" && constructed == "OK") "OK" else "fail"
    }
}

fun box(): String = PlatformDerived().normalizeNullable(null)
"#,
    );
}

#[test]
fn java_parameter_annotations_control_nullable_arguments() {
    let java = [
        (
            "NotNull.java",
            r#"
package org.jetbrains.annotations;
import java.lang.annotation.*;
@Target(ElementType.PARAMETER)
@Retention(RetentionPolicy.CLASS)
public @interface NotNull {}
"#,
        ),
        (
            "Nullable.java",
            r#"
package org.jetbrains.annotations;
import java.lang.annotation.*;
@Target(ElementType.PARAMETER)
@Retention(RetentionPolicy.CLASS)
public @interface Nullable {}
"#,
        ),
        (
            "PlatformApi.java",
            r#"
import org.jetbrains.annotations.*;
public class PlatformApi {
    public static void required(@NotNull String value) {}
    public static void optional(@Nullable String value) {}
}
"#,
        ),
    ];
    let Some(diagnostics) = mixed_diagnostics(
        &java,
        r#"
fun call(value: String?) {
    PlatformApi.optional(value)
    PlatformApi.required(value)
}
"#,
    ) else {
        return;
    };

    assert_eq!(
        diagnostics,
        ["unresolved Java static 'PlatformApi.required' for given argument types"]
    );
}

fn compile_java(java: &[(&str, &str)]) -> Option<common::JavacOutput> {
    let sources = java
        .iter()
        .map(|(name, source)| (name.to_string(), source.to_string()))
        .collect::<Vec<_>>();
    common::javac_compile(&sources, &[])
}

fn mixed_diagnostics(java: &[(&str, &str)], kotlin: &str) -> Option<Vec<String>> {
    let (javadir, _) = compile_java(java)?;
    let jdk = common::jdk_modules();
    let mut classpath = common::classpath_jars_for(kotlin);
    classpath.push(javadir.clone());
    let diagnostics = common::front_end_diagnostics(kotlin, &classpath, Some(jdk.as_path()));
    if let Some(root) = javadir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
    Some(diagnostics)
}

#[test]
fn inherited_java_sam_target_wins_static_overload_specificity() {
    run_mixed(
        &[(
            "Test.java",
            r#"
public class Test {
    public interface MyRunnable extends Runnable {}
    public static void foo(MyRunnable value) {}
    public static void foo(Runnable value) { throw new AssertionError("less specific"); }
}
"#,
        )],
        r#"
// LANGUAGE: +EliminateAmbiguitiesOnInheritedSamInterfaces
fun box(): String {
    Test.foo {}
    return "OK"
}
"#,
    );
}

/// Compile the Java sources with javac, then the Kotlin source with krusty against the javac output
/// dir on the classpath, and run `box()` with both class sets in one loader. Asserts "OK".
fn run_mixed(java: &[(&str, &str)], kotlin: &str) {
    let Some((javadir, java_classes)) = compile_java(java) else {
        eprintln!("skipping: JDK unavailable");
        return;
    };
    let jdk = common::jdk_modules();
    // Gate-canonical jars (stdlib/test/annotations — Intrinsics must resolve at runtime); the javac
    // output dir joins only the COMPILE classpath. The run classpath stays jars-only so the pooled
    // BoxRunner JVM (keyed by classpath) is reused across tests — the Java classes ride along as
    // bytes in the in-memory loader.
    let jars: Vec<std::path::PathBuf> = common::classpath_jars_for(kotlin);
    let mut cp = jars.clone();
    cp.push(javadir.clone());
    let mut classes = match common::compile_in_process(kotlin, "MainKt", &cp, Some(jdk.as_path())) {
        Some(c) => c,
        None => {
            let d = common::front_end_diagnostics(kotlin, &cp, Some(jdk.as_path()));
            panic!("krusty should compile Kotlin against the javac output dir; diags: {d:?}");
        }
    };
    // Scratch src+classes tree done — everything needed is in memory now.
    if let Some(root) = javadir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
    classes.extend(java_classes);
    let box_class = common::find_box_class(&classes).expect("box() class");
    let got = common::run_box(&classes, &box_class, &jars).expect("box run");
    assert_eq!(got, "OK");
}

#[test]
fn inferred_return_worklist_tracks_java_synthetic_property_name() {
    let Some((javadir, _)) = compile_java(&[
        (
            "demo/SyntheticBase.java",
            "package demo; public class SyntheticBase {}",
        ),
        (
            "demo/SyntheticCatalog.java",
            "package demo; public class SyntheticCatalog { public SyntheticBase[] getEntries() { return new SyntheticBase[0]; } }",
        ),
    ]) else {
        eprintln!("skipping: JDK unavailable");
        return;
    };
    let jdk = common::jdk_modules();
    let sources = [
        "package demo\nfun box(): String = entryName()",
        "package demo\nfun entryName() = SyntheticCatalogImpl().entries[0].name",
        "package demo\nclass SyntheticEntry(val name: String) : SyntheticBase()\nclass SyntheticCatalogImpl : SyntheticCatalog() { override fun getEntries() = values(); fun values() = arrayOf(SyntheticEntry(\"OK\")) }",
    ];
    let mut classpath = common::classpath_jars_for(&sources.join("\n"));
    classpath.push(javadir.clone());
    let diagnostics =
        common::front_end_diagnostics_files(&sources, &classpath, Some(jdk.as_path()));
    if let Some(root) = javadir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
    // The signature pass declines this inferred return (a Java synthetic property read through
    // a module override), and the batch compiler used to emit NOTHING for the file while reporting
    // success. The decline is reported now; this pins the surfaced gap, not a clean compile.
    assert_eq!(
        diagnostics,
        vec![
            "krusty: cannot infer the return type of 'entryName'; add an explicit return type"
                .to_string()
        ]
    );
}

#[test]
fn inferred_return_worklist_tracks_unicode_java_synthetic_property_name() {
    let Some((javadir, _)) = compile_java(&[
        (
            "demo/SyntheticBase.java",
            "package demo; public class SyntheticBase {}",
        ),
        (
            "demo/UnicodeSyntheticCatalog.java",
            "package demo; public class UnicodeSyntheticCatalog { public SyntheticBase[] getÄpfel() { return new SyntheticBase[0]; } }",
        ),
    ]) else {
        eprintln!("skipping: JDK unavailable");
        return;
    };
    let jdk = common::jdk_modules();
    let sources = [
        "package demo\nfun unicodeBox(): String = unicodeEntryName()",
        "package demo\nfun unicodeEntryName() = UnicodeSyntheticCatalogImpl().äpfel[0].name",
        "package demo\nclass UnicodeSyntheticEntry(val name: String) : SyntheticBase()\nclass UnicodeSyntheticCatalogImpl : UnicodeSyntheticCatalog() { override fun getÄpfel() = values(); fun values() = arrayOf(UnicodeSyntheticEntry(\"OK\")) }",
    ];
    let mut classpath = common::classpath_jars_for(&sources.join("\n"));
    classpath.push(javadir.clone());
    let diagnostics =
        common::front_end_diagnostics_files(&sources, &classpath, Some(jdk.as_path()));
    if let Some(root) = javadir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
    // The signature pass declines this inferred return (a Java synthetic property read through
    // a module override), and the batch compiler used to emit NOTHING for the file while reporting
    // success. The decline is reported now; this pins the surfaced gap, not a clean compile.
    assert_eq!(
        diagnostics,
        vec!["krusty: cannot infer the return type of 'unicodeEntryName'; add an explicit return type".to_string()]
    );
}

#[test]
fn same_package_java_package_classifier_keeps_public_member_access() {
    run_mixed(
        &[(
            "fixtures/PackageType.java",
            r#"
                package fixtures;
                class PackageType {
                    public PackageType() {}
                    public String value() { return "O"; }
                    public static String staticValue() { return "K"; }
                }
            "#,
        )],
        r#"
            package fixtures
            fun box(): String = PackageType().value() + PackageType.staticValue()
        "#,
    );
}

#[test]
fn cross_package_java_package_classifier_constructor_is_rejected() {
    let Some(diagnostics) = mixed_diagnostics(
        &[(
            "fixtures/PackageType.java",
            r#"
                package fixtures;
                class PackageType {
                    public PackageType() {}
                }
            "#,
        )],
        r#"
            package consumer
            import fixtures.PackageType
            fun use(): Any = PackageType()
        "#,
    ) else {
        eprintln!("skipping: JDK unavailable");
        return;
    };
    assert_eq!(
        diagnostics,
        [
            "cannot access 'class PackageType : Any': it is package-private in file.",
            "cannot access 'class PackageType : Any': it is package-private in file.",
        ]
    );
}

#[test]
fn subclass_calls_protected_java_super_member() {
    run_mixed(
        &[(
            "fixtures/Parent.java",
            r#"
                package fixtures;
                public class Parent {
                    protected String value() { return "OK"; }
                }
            "#,
        )],
        r#"
            package consumer
            import fixtures.Parent
            class Child : Parent() {
                fun read(): String = super.value()
            }
            fun box(): String = Child().read()
        "#,
    );
}

#[test]
fn subclass_resolves_protected_nested_classifier_from_java_base() {
    run_mixed(
        &[(
            "fixtures/Parent.java",
            r#"
                package fixtures;
                public class Parent {
                    protected static class Category {
                        public Category() {}
                        public String value() { return "OK"; }
                    }
                }
            "#,
        )],
        r#"
            import fixtures.Parent
            class Child : Parent() {
                fun value(): String = Category().value()
            }
            fun box(): String = Child().value()
        "#,
    );
}

#[test]
fn override_uses_inherited_java_nested_enum_in_signature_and_body() {
    run_mixed(
        &[(
            "fixtures/Parent.java",
            r#"
                package fixtures;
                public class Parent {
                    public enum DialogStyle { NO_STYLE, COMPACT }
                    protected DialogStyle getStyle() { return DialogStyle.NO_STYLE; }
                }
            "#,
        )],
        r#"
            package consumer
            import fixtures.Parent
            class Child : Parent() {
                override fun getStyle(): DialogStyle = DialogStyle.COMPACT
                fun value(): String = getStyle().name
            }
            fun box(): String = if (Child().value() == "COMPACT") "OK" else "FAIL"
        "#,
    );
}

#[test]
fn inherited_classifier_does_not_expose_its_protected_member() {
    let Some(diagnostics) = mixed_diagnostics(
        &[(
            "fixtures/Parent.java",
            r#"
                package fixtures;
                public class Parent {
                    protected static class Category {
                        public Category() {}
                        protected String secret() { return "hidden"; }
                    }
                }
            "#,
        )],
        r#"
            import fixtures.Parent
            class Child : Parent() {
                fun value(): String = Category().secret()
            }
        "#,
    ) else {
        eprintln!("skipping: JDK unavailable");
        return;
    };
    assert_eq!(
        diagnostics,
        ["cannot access 'fun secret(): String!': it is protected in 'fixtures.Parent.Category'."]
    );
}

#[test]
fn inherited_classifier_does_not_expose_its_protected_constructor() {
    let Some(diagnostics) = mixed_diagnostics(
        &[(
            "fixtures/Parent.java",
            r#"
                package fixtures;
                public class Parent {
                    protected static class Category {
                        protected Category() {}
                        public String value() { return "hidden"; }
                    }
                }
            "#,
        )],
        r#"
            import fixtures.Parent
            class Child : Parent() {
                fun value(): String = Category().value()
            }
        "#,
    ) else {
        eprintln!("skipping: JDK unavailable");
        return;
    };
    assert_eq!(
        diagnostics,
        ["cannot access 'constructor(): Parent.Category': it is protected in 'fixtures.Parent.Category'."]
    );
}

#[test]
fn same_package_subclass_resolves_package_nested_classifier() {
    run_mixed(
        &[(
            "fixtures/Parent.java",
            r#"
                package fixtures;
                public class Parent {
                    static class Category {
                        public Category() {}
                        public String value() { return "OK"; }
                    }
                }
            "#,
        )],
        r#"
            package fixtures
            class Child : Parent() {
                fun value(): String = Category().value()
            }
            fun box(): String = Child().value()
        "#,
    );
}

#[test]
fn package_nested_classifier_does_not_shadow_cross_package_source_type() {
    run_mixed(
        &[(
            "fixtures/Parent.java",
            r#"
                package fixtures;
                public class Parent {
                    static class Category {}
                }
            "#,
        )],
        r#"
            import fixtures.Parent
            class Category(val value: String)
            class Child : Parent() {
                fun value(): String = Category("OK").value
            }
            fun box(): String = Child().value()
        "#,
    );
}

#[test]
fn primary_constructor_parameter_precedes_peer_inherited_nested_classifiers() {
    let java = [
        (
            "fixtures/Left.java",
            "package fixtures; public interface Left { class Category {} }",
        ),
        (
            "fixtures/Right.java",
            "package fixtures; public interface Right { class Category {} }",
        ),
    ];
    let kotlin = r#"
            package fixtures
            class Category(val value: String)
            class Child(val category: Category) : Left, Right
            fun box(): String = Child(Category("OK")).category.value
        "#;
    let Some((javadir, _)) = compile_java(&java) else {
        eprintln!("skipping: JDK unavailable");
        return;
    };
    let differential = common::compiler_diagnostics(
        &[("PrimaryConstructorHeaderScope.kt", kotlin)],
        std::slice::from_ref(&javadir),
    );
    cleanup(&javadir);
    assert_eq!(
        (differential.krusty_code, differential.reference_code),
        (0, 0),
        "primary-constructor header scope differs: krusty={} kotlinc={}",
        differential.krusty_stderr,
        differential.reference_stderr,
    );
    run_mixed(&java, kotlin);
}

#[test]
fn private_nested_classifier_from_java_base_does_not_shadow_source_type() {
    run_mixed(
        &[(
            "fixtures/Parent.java",
            r#"
                package fixtures;
                public class Parent {
                    private static class Category {}
                }
            "#,
        )],
        r#"
            import fixtures.Parent
            class Category(val value: String)
            class Child : Parent() {
                fun value(): String = Category("OK").value
            }
            fun box(): String = Child().value()
        "#,
    );
}

#[test]
fn dollar_named_top_level_class_is_not_an_inherited_classifier() {
    run_mixed(
        &[
            (
                "fixtures/Parent.java",
                "package fixtures; public class Parent {}",
            ),
            (
                "fixtures/Parent$Category.java",
                r#"
                    package fixtures;
                    public class Parent$Category {
                        public Parent$Category() {}
                        public String value() { return "FAIL"; }
                    }
                "#,
            ),
        ],
        r#"
            import fixtures.Parent
            class Category(val value: String)
            class Child : Parent() {
                fun value(): String = Category("OK").value
            }
            fun box(): String = Child().value()
        "#,
    );
}

#[test]
fn protected_java_member_is_not_visible_through_base_value() {
    let Some(diagnostics) = mixed_diagnostics(
        &[(
            "fixtures/Parent.java",
            r#"
                package fixtures;
                public class Parent {
                    protected String value() { return "hidden"; }
                }
            "#,
        )],
        r#"
            package consumer
            import fixtures.Parent
            class Child : Parent() {
                fun read(parent: Parent): String = parent.value()
            }
        "#,
    ) else {
        eprintln!("skipping: JDK unavailable");
        return;
    };
    assert_eq!(
        diagnostics,
        ["cannot access 'fun value(): String!': it is protected in 'fixtures.Parent'."]
    );
}

#[test]
fn dependency_internal_member_is_not_visible() {
    let Some(diagnostics) = common::diagnostics_against(
        "internal_member_visibility",
        "package fixtures\n\
         class PublicApi {\n\
             @PublishedApi internal fun hidden(): String = \"hidden\"\n\
         }",
        "package consumer\n\
         import fixtures.PublicApi\n\
         fun read(api: PublicApi): String = api.hidden()",
    ) else {
        return;
    };
    assert_eq!(
        diagnostics,
        ["cannot access 'hidden': it is internal in 'fixtures/PublicApi'"]
    );
}

#[test]
fn boxed_java_primitive_returns_work_in_kotlin_contexts() {
    run_mixed(
        &[(
            "BoxedSource.java",
            r#"
                package fixtures;
                public final class BoxedSource {
                    private final Integer value;
                    public BoxedSource(Integer value) { this.value = value; }
                    public Integer read() { return value; }
                    public static Integer readStatic() { return null; }
                    public Boolean positive() { return value != null && value > 0; }
                    public static Byte byteValue() { return (byte) 1; }
                    public static Short shortValue() { return (short) 2; }
                    public static Long longValue() { return 3L; }
                    public static Character charValue() { return 'x'; }
                    public static Float floatValue() { return 4.0f; }
                    public static Double doubleValue() { return 5.0; }
                }
            "#,
        )],
        r#"
            import fixtures.BoxedSource

            fun boxedReturn(): Int = BoxedSource(5).read()
            fun acceptInt(value: Int): Int = value
            tailrec fun boxedTail(n: Int): Int =
                if (BoxedSource(n).positive()) boxedTail(n - 1) else n
            tailrec fun boxedTailUnit(n: Int, result: IntArray) {
                if (BoxedSource(n).positive()) boxedTailUnit(n - 1, result)
                else result[0] = n
            }

            fun box(): String {
                val fallback = BoxedSource(null).read() ?: 7
                val present = BoxedSource(4).read() ?: 7
                val staticFallback = BoxedSource.readStatic() ?: 9
                val assigned: Int = BoxedSource(5).read()
                val returned = boxedReturn()
                val accepted = acceptInt(BoxedSource(6).read())
                val nullable: Int? = BoxedSource(null).read()
                val nullFallback = BoxedSource(null).read() ?: null
                val indexed = intArrayOf(11)[BoxedSource(0).read()]
                val conditional = if (BoxedSource(1).positive()) 12 else 0
                val selected = when {
                    BoxedSource(1).positive() -> 13
                    else -> 0
                }
                var remaining = 1
                while (BoxedSource(remaining).positive()) {
                    remaining--
                }
                var doRemaining = 1
                do {
                    doRemaining--
                } while (BoxedSource(doRemaining).positive())
                val tail = boxedTail(2)
                val tailUnit = intArrayOf(-1)
                boxedTailUnit(2, tailUnit)
                val byte: Byte = BoxedSource.byteValue()
                val short: Short = BoxedSource.shortValue()
                val long: Long = BoxedSource.longValue()
                val char: Char = BoxedSource.charValue()
                val float: Float = BoxedSource.floatValue()
                val double: Double = BoxedSource.doubleValue()
                return if (
                    fallback == 7 &&
                    present == 4 &&
                    staticFallback == 9 &&
                    assigned == 5 &&
                    returned == 5 &&
                    accepted == 6 &&
                    nullable == null &&
                    nullFallback == null &&
                    indexed == 11 &&
                    conditional == 12 &&
                    selected == 13 &&
                    remaining == 0 &&
                    doRemaining == 0 &&
                    tail == 0 &&
                    tailUnit[0] == 0 &&
                    byte == 1.toByte() &&
                    short == 2.toShort() &&
                    long == 3L &&
                    char == 'x' &&
                    float == 4.0f &&
                    double == 5.0
                ) "OK" else "fail"
            }
        "#,
    );
}

/// Expression-position static call on a same-(root-)package class must resolve like the ctor and
/// type positions do (the `imported_type_internal` fallback in the static-receiver path).
#[test]
fn root_package_static_call_matches_other_positions() {
    run_mixed(
        &[(
            "K.java",
            "public class K { public static String s() { return \"OK\"; } }",
        )],
        "fun box(): String = K.s()",
    );
}

/// A top-level VALUE named like the class shadows it in receiver position (Kotlin shadowing —
/// `value_root_shadows_classifier` must keep winning over the classpath fallback): `K.s()` then
/// resolves `s` against the String value and fails, it must NOT silently become the static call.
#[test]
fn value_shadows_classpath_class_in_static_receiver_position() {
    let Some((javadir, _)) = common::javac_compile(
        &[(
            "K.java".to_string(),
            "public class K { public static String s() { return \"FAIL:static\"; } }".to_string(),
        )],
        &[],
    ) else {
        eprintln!("skipping: JDK unavailable");
        return;
    };
    let jdk = common::jdk_modules();
    let mut cp = common::classpath_jars_for("");
    cp.push(javadir.clone());
    let src = "val K = \"value\"\nfun box(): String = K.s()";
    let d = common::front_end_diagnostics(src, &cp, Some(jdk.as_path()));
    if let Some(root) = javadir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
    assert!(
        !d.is_empty(),
        "value K must shadow class K; expected a resolution error, got clean compile"
    );
}
