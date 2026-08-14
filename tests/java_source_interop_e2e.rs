//! Java-source interop: a box test whose `// FILE:` blocks include `.java` sources (the corpus'
//! `codegen/box` Java-interop shape). The Java files are compiled by the persistent JavaRunner's
//! in-process javac (`common::javac_compile` — no per-test `javac` spawn), their output directory
//! joins krusty's compile classpath (loose-`.class` dir entries are already supported), and the
//! resulting classes run together with krusty's in one BoxRunner classloader.

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
    val sourceInherited: String = SourceBox().value
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
        ["only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'p.Box<String>?'."]
    );
}

/// The CIRCULAR direction (slice 2, Kotlin-first): Java extends a Kotlin class, Kotlin calls the
/// Java class. Pipeline: signature stubs from the Java source (`krusty::jvm::java_stub`, no
/// javac) → krusty compiles Kotlin against the stub dir → real javac compiles the Java against
/// krusty's output → both class sets run together. The stubs never reach the runtime.
#[test]
fn java_extends_kotlin_via_stub_pipeline() {
    let kotlin = r#"
open class A {
    open fun name(): String = "FAIL:A"
}

fun box(): String = J().name()
"#;
    let java = "public class J extends A { @Override public String name() { return \"OK\"; } }";
    let jdk = common::jdk_modules();
    let jars = common::classpath_jars_for(kotlin);

    // 1. Stubs: resolve `A` as a known Kotlin class, everything else via a real Classpath.
    let mut cp_paths = jars.clone();
    cp_paths.push(jdk.clone());
    let classpath = krusty::jvm::classpath::Classpath::new(cp_paths);
    let resolve = |cand: &str| cand == "A" || classpath.find(cand).is_some();
    let stubs = krusty::jvm::java_stub::stub_classes(
        &[("J.java".to_string(), java.to_string())],
        krusty::jvm::java_stub::StubMode::Strict,
        &resolve,
    )
    .expect("stub generation");

    let root = std::env::temp_dir().join(format!("krusty_stub_e2e_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let stubdir = root.join("stubs");
    let kotlindir = root.join("kotlin");
    for (name, bytes) in &stubs {
        let p = stubdir.join(format!("{name}.class"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, bytes).unwrap();
    }

    // 2. Kotlin against the stubs.
    let mut cp = jars.clone();
    cp.push(stubdir);
    let kotlin_classes =
        match common::compile_in_process(kotlin, "MainKt", &cp, Some(jdk.as_path())) {
            Some(c) => c,
            None => {
                let d = common::front_end_diagnostics(kotlin, &cp, Some(jdk.as_path()));
                let _ = std::fs::remove_dir_all(&root);
                panic!("krusty should compile Kotlin against the stub dir; diags: {d:?}");
            }
        };

    // 3. Real javac against krusty's output; the stub dir is NOT on javac's classpath.
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

    // 4. Run with the REAL classes only.
    let mut classes = kotlin_classes;
    classes.extend(java_classes);
    let box_class = common::find_box_class(&classes).expect("box() class");
    let got = common::run_box(&classes, &box_class, &jars).expect("box run");
    assert_eq!(got, "OK");
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

    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("PlatformApi.required")),
        "{diagnostics:?}"
    );
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.contains("PlatformApi.optional")),
        "{diagnostics:?}"
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
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
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
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
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
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("PackageType")),
        "{diagnostics:?}"
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
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("secret")),
        "{diagnostics:?}"
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
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("Category")),
        "{diagnostics:?}"
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
fn peer_inherited_nested_ambiguity_does_not_fall_back_to_source_type() {
    let Some(diagnostics) = mixed_diagnostics(
        &[
            (
                "fixtures/Left.java",
                "package fixtures; public interface Left { class Category {} }",
            ),
            (
                "fixtures/Right.java",
                "package fixtures; public interface Right { class Category {} }",
            ),
        ],
        r#"
            package fixtures
            class Category
            class Child(category: Category) : Left, Right
        "#,
    ) else {
        eprintln!("skipping: JDK unavailable");
        return;
    };

    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("Category")),
        "peer inherited classifiers must remain ambiguous: {diagnostics:?}"
    );
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
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("value")),
        "{diagnostics:?}"
    );
}

#[test]
fn dependency_internal_member_is_not_visible() {
    let Some(diagnostics) = common::diagnostics_against_ref(
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
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("hidden")),
        "{diagnostics:?}"
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
