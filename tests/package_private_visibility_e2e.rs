//! Thorough coverage of Java package-private visibility as seen from Kotlin.
//!
//! Java package-private declarations (no access modifier) are not part of Kotlin's source-level
//! visibility model, but they must be honored at Java-interop boundaries. These tests exercise the
//! boundary for functions, properties/fields, constants, classes, constructors, nested types,
//! inheritance, and use from local scopes and Kotlin companion objects. Every same-package case
//! must compile and run; every cross-package case must be rejected with a package-private
//! diagnostic instead of a silent fallback or a misleading "unresolved reference".

use super::common;
use std::path::Path;

/// Compile a set of Java sources with the in-process javac helper. The first component of each
/// tuple is the relative source path (e.g. `"p/Util.java"`).
fn compile_java(java: &[(&str, &str)]) -> Option<common::JavacOutput> {
    let sources: Vec<(String, String)> = java
        .iter()
        .map(|(name, src)| (name.to_string(), src.to_string()))
        .collect();
    common::javac_compile(&sources, &[])
}

/// Remove the scratch src+classes tree created by `javac_compile`.
fn cleanup(classes_dir: &Path) {
    if let Some(root) = classes_dir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
}

/// Compile Java sources, compile Kotlin against the javac output dir, and run `box()` in a single
/// classloader. Asserts the result is `"OK"`.
fn run_mixed(java: &[(&str, &str)], kotlin: &str) {
    let Some((javadir, java_classes)) = compile_java(java) else {
        eprintln!("skipping: JDK unavailable");
        return;
    };
    let jdk = common::jdk_modules();
    let jars = common::classpath_jars_for(kotlin);
    let mut cp = jars.clone();
    cp.push(javadir.clone());
    let mut classes = match common::compile_in_process(kotlin, "MainKt", &cp, Some(jdk.as_path())) {
        Some(c) => c,
        None => {
            let diagnostics = common::front_end_diagnostics(kotlin, &cp, Some(jdk.as_path()));
            panic!(
                "krusty should compile Kotlin against the javac output dir; diagnostics: {diagnostics:?}"
            );
        }
    };
    cleanup(&javadir);
    classes.extend(java_classes);
    let box_class = common::find_box_class(&classes).expect("box() class");
    let got = common::run_box(&classes, &box_class, &jars).expect("box run");
    assert_eq!(got, "OK");
}

/// Compile Java sources and return the front-end diagnostics for the Kotlin source. This is the
/// cross-package rejection path.
fn mixed_diagnostics(java: &[(&str, &str)], kotlin: &str) -> Option<Vec<String>> {
    let (javadir, _) = compile_java(java)?;
    let jdk = common::jdk_modules();
    let mut classpath = common::classpath_jars_for(kotlin);
    classpath.push(javadir.clone());
    let diagnostics = common::front_end_diagnostics(kotlin, &classpath, Some(jdk.as_path()));
    cleanup(&javadir);
    Some(diagnostics)
}

/// Write a set of emitted class files into a directory tree, preserving package subdirectories.
fn write_classes_to_dir(dir: &Path, classes: &[(String, Vec<u8>)]) {
    for (name, bytes) in classes {
        let path = dir.join(format!("{name}.class"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, bytes).unwrap();
    }
}

/// Build a Kotlin library against javac output, write it to a directory, then compile a Kotlin
/// main against that library. This exercises package-private access across a module boundary when
/// the Kotlin main shares the same package as the Java owner.
fn run_with_kotlin_lib(java: &[(&str, &str)], lib_src: &str, main_src: &str) {
    let Some((javadir, java_classes)) = compile_java(java) else {
        eprintln!("skipping: JDK unavailable");
        return;
    };
    let jdk = common::jdk_modules();
    let jars = common::classpath_jars_for(main_src);

    let root = std::env::temp_dir().join(format!("ppv_lib_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let libdir = root.join("lib");

    let mut lib_cp = jars.clone();
    lib_cp.push(javadir.clone());
    let lib_classes = common::compile_in_process(lib_src, "Lib", &lib_cp, Some(jdk.as_path()))
        .expect("lib Kotlin should compile against javac output");
    write_classes_to_dir(&libdir, &lib_classes);

    let mut main_cp = jars.clone();
    main_cp.push(libdir);
    main_cp.push(javadir.clone());
    let mut main_classes =
        match common::compile_in_process(main_src, "MainKt", &main_cp, Some(jdk.as_path())) {
            Some(c) => c,
            None => {
                let diagnostics =
                    common::front_end_diagnostics(main_src, &main_cp, Some(jdk.as_path()));
                panic!("main Kotlin should compile against lib; diagnostics: {diagnostics:?}");
            }
        };

    cleanup(&javadir);
    let _ = std::fs::remove_dir_all(&root);
    main_classes.extend(lib_classes);
    main_classes.extend(java_classes);
    let box_class = common::find_box_class(&main_classes).expect("box() class");
    let got = common::run_box(&main_classes, &box_class, &jars).expect("box run");
    assert_eq!(got, "OK");
}

/// Assert that a cross-package rejection diagnostic mentions the member and package-private access.
fn assert_package_private_rejection(diagnostics: &[String], name: &str) {
    assert!(
        diagnostics
            .iter()
            .any(|d| d.contains("package-private") && d.contains(name)),
        "expected a package-private diagnostic mentioning '{name}', got {diagnostics:?}"
    );
}

// ---------------------------------------------------------------------------
// Static functions and constants
// ---------------------------------------------------------------------------

#[test]
fn same_package_static_function_and_constant() {
    run_mixed(
        &[(
            "p/Util.java",
            "package p; public class Util { static String secret() { return \"OK\"; } static final String TAG = \"OK\"; }",
        )],
        r#"
            package p
            fun box(): String {
                val a = Util.secret()
                val b = Util.TAG
                return if (a == "OK" && b == "OK") "OK" else "FAIL"
            }
        "#,
    );
}

#[test]
fn cross_package_static_function_rejected() {
    let diagnostics = mixed_diagnostics(
        &[(
            "p/Util.java",
            "package p; public class Util { static String secret() { return \"OK\"; } }",
        )],
        "package q\nfun bad() { p.Util.secret() }\n",
    )
    .expect("javac");
    assert_package_private_rejection(&diagnostics, "secret");
}

#[test]
fn cross_package_static_constant_rejected() {
    let diagnostics = mixed_diagnostics(
        &[(
            "p/Util.java",
            "package p; public class Util { static final String TAG = \"OK\"; }",
        )],
        "package q\nfun bad() { p.Util.TAG }\n",
    )
    .expect("javac");
    assert_package_private_rejection(&diagnostics, "TAG");
}

// ---------------------------------------------------------------------------
// Instance functions and fields-as-properties
// ---------------------------------------------------------------------------

#[test]
fn same_package_instance_method() {
    run_mixed(
        &[(
            "p/Helper.java",
            "package p; public class Helper { String echo(String s) { return s; } }",
        )],
        "package p\nfun box(): String = Helper().echo(\"OK\")\n",
    );
}

#[test]
fn cross_package_instance_method_rejected() {
    let diagnostics = mixed_diagnostics(
        &[(
            "p/Helper.java",
            "package p; public class Helper { String echo(String s) { return s; } }",
        )],
        "package q\nfun bad(h: p.Helper) { h.echo(\"x\") }\n",
    )
    .expect("javac");
    assert_package_private_rejection(&diagnostics, "echo");
}

#[test]
fn same_package_instance_field_as_property() {
    run_mixed(
        &[(
            "p/Holder.java",
            "package p; public class Holder { String value = \"OK\"; }",
        )],
        r#"
            package p
            val topProp: String = Holder().value
            fun box(): String = if (topProp == "OK") "OK" else "FAIL"
        "#,
    );
}

#[test]
fn cross_package_instance_field_as_property_rejected() {
    let diagnostics = mixed_diagnostics(
        &[(
            "p/Holder.java",
            "package p; public class Holder { String value = \"OK\"; }",
        )],
        "package q\nfun bad(h: p.Holder) { h.value }\n",
    )
    .expect("javac");
    assert_package_private_rejection(&diagnostics, "value");
}

// ---------------------------------------------------------------------------
// Constructors
// ---------------------------------------------------------------------------

#[test]
fn same_package_package_private_constructor() {
    run_mixed(
        &[(
            "p/Gate.java",
            "package p; public class Gate { Gate() {} public String open() { return \"OK\"; } }",
        )],
        "package p\nfun box(): String = Gate().open()\n",
    );
}

// ---------------------------------------------------------------------------
// Nested classes
// ---------------------------------------------------------------------------

#[test]
fn same_package_nested_class() {
    run_mixed(
        &[(
            "p/Outer.java",
            r#"
                package p;
                public class Outer {
                    public static class Inner {
                        public Inner() {}
                        public String value() { return "OK"; }
                    }
                    public Inner make() { return new Inner(); }
                }
            "#,
        )],
        r#"
            package p
            fun box(): String {
                val a = Outer.Inner().value()
                val b = Outer().make().value()
                return if (a == "OK" && b == "OK") "OK" else "FAIL"
            }
        "#,
    );
}

#[test]
fn cross_package_nested_class_rejected() {
    let diagnostics = mixed_diagnostics(
        &[(
            "p/Outer.java",
            r#"
                package p;
                public class Outer {
                    static class Inner {
                        public Inner() {}
                        public String value() { return "OK"; }
                    }
                }
            "#,
        )],
        "package q\nfun bad() { p.Outer.Inner() }\n",
    )
    .expect("javac");
    assert_package_private_rejection(&diagnostics, "Inner");
}

// ---------------------------------------------------------------------------
// Inheritance
// ---------------------------------------------------------------------------

#[test]
fn same_package_inherited_instance_method() {
    run_mixed(
        &[(
            "p/Base.java",
            "package p; public class Base { String baseValue() { return \"OK\"; } }",
        )],
        "package p\nclass Derived : Base()\nfun box(): String = Derived().baseValue()\n",
    );
}

#[test]
fn cross_package_inherited_instance_method_rejected() {
    // A Java subclass in another package does not make an inherited package-private method visible.
    let diagnostics = mixed_diagnostics(
        &[
            (
                "p/Base.java",
                "package p; public class Base { String baseValue() { return \"OK\"; } }",
            ),
            (
                "q/Derived.java",
                "package q; public class Derived extends p.Base {}",
            ),
        ],
        "package q\nfun bad(d: Derived) { d.baseValue() }\n",
    )
    .expect("javac");
    assert_package_private_rejection(&diagnostics, "baseValue");
}

#[test]
fn same_package_inherited_static_constant() {
    run_mixed(
        &[(
            "p/Base.java",
            "package p; public class Base { static final String LABEL = \"OK\"; }",
        )],
        r#"
            package p
            class Derived : Base()
            fun box(): String = Base.LABEL
        "#,
    );
}

// ---------------------------------------------------------------------------
// Package-private top-level classes
// ---------------------------------------------------------------------------

#[test]
fn same_package_package_private_class_with_public_members() {
    run_mixed(
        &[(
            "p/Internal.java",
            r#"
                package p;
                class Internal {
                    public static String staticValue() { return "OK"; }
                    public String instanceValue() { return "OK"; }
                }
            "#,
        )],
        r#"
            package p
            fun box(): String {
                val a = Internal.staticValue()
                val b = Internal().instanceValue()
                return if (a == "OK" && b == "OK") "OK" else "FAIL"
            }
        "#,
    );
}

#[test]
fn cross_package_package_private_class_rejected() {
    let diagnostics = mixed_diagnostics(
        &[(
            "p/Internal.java",
            "package p; class Internal { public static String value() { return \"OK\"; } }",
        )],
        "package q\nfun bad() { p.Internal.value() }\n",
    )
    .expect("javac");
    assert_package_private_rejection(&diagnostics, "Internal");
}

#[test]
fn cross_package_package_private_class_type_ref_rejected() {
    let diagnostics = mixed_diagnostics(
        &[(
            "p/Internal.java",
            "package p; class Internal { public static String value() { return \"OK\"; } }",
        )],
        "package q\nfun bad(): p.Internal? = null\n",
    )
    .expect("javac");
    assert_package_private_rejection(&diagnostics, "Internal");
}

// ---------------------------------------------------------------------------
// Interfaces and enums
// ---------------------------------------------------------------------------

#[test]
fn same_package_package_private_interface() {
    run_mixed(
        &[("p/IDo.java", "package p; interface IDo { String doIt(); }")],
        r#"
            package p
            class Impl : IDo {
                override fun doIt(): String = "OK"
            }
            fun box(): String = Impl().doIt()
        "#,
    );
}

#[test]
fn same_package_package_private_enum() {
    run_mixed(
        &[("p/Status.java", "package p; enum Status { OK, FAIL }")],
        "package p\nfun box(): String = if (Status.OK.name == \"OK\") \"OK\" else \"FAIL\"\n",
    );
}

#[test]
fn cross_package_package_private_enum_rejected() {
    let diagnostics = mixed_diagnostics(
        &[("p/Status.java", "package p; enum Status { OK, FAIL }")],
        "package q\nfun bad() { p.Status.OK }\n",
    )
    .expect("javac");
    assert_package_private_rejection(&diagnostics, "Status");
}

// ---------------------------------------------------------------------------
// Overloads and false-positive guards
// ---------------------------------------------------------------------------

#[test]
fn public_overload_is_accessible_even_with_package_private_sibling() {
    // A public overload must not be hidden by a package-private overload with the same name, and
    // the package-private overload must not poison the public one with a package-private diagnostic.
    run_mixed(
        &[(
            "p/Overloads.java",
            "package p; public class Overloads { public String call() { return \"OK\"; } String call(int x) { return \"pp\"; } }",
        )],
        "package q\nfun box(): String = p.Overloads().call()\n",
    );
}

#[test]
fn package_private_overload_with_public_sibling_gets_ordinary_error() {
    // When a public overload exists but the call does not match it, the package-private sibling
    // should not produce a misleading "cannot access" diagnostic; the ordinary resolution failure
    // is sufficient.
    let diagnostics = mixed_diagnostics(
        &[(
            "p/Overloads.java",
            "package p; public class Overloads { public String call() { return \"public\"; } String call(int x) { return \"pp\"; } }",
        )],
        "package q\nfun bad(o: p.Overloads) { o.call(1) }\n",
    )
    .expect("javac");
    assert!(
        diagnostics.iter().any(|d| d.contains("call")),
        "expected a resolution failure mentioning call, got {diagnostics:?}"
    );
}

// ---------------------------------------------------------------------------
// Module boundary
// ---------------------------------------------------------------------------

#[test]
fn same_package_access_across_kotlin_module_boundary() {
    // A Kotlin library in the same package as a Java owner can access package-private members; a
    // second Kotlin module in that same package can consume the library and the Java members.
    run_with_kotlin_lib(
        &[(
            "p/Secret.java",
            "package p; public class Secret { static String value() { return \"OK\"; } }",
        )],
        "package p\nfun fetch(): String = Secret.value()\n",
        "package p\nfun box(): String = fetch()\n",
    );
}

// ---------------------------------------------------------------------------
// Local scopes and companion objects
// ---------------------------------------------------------------------------

#[test]
fn same_package_access_from_local_function_and_property() {
    run_mixed(
        &[(
            "p/Api.java",
            "package p; public class Api { String local() { return \"OK\"; } }",
        )],
        r#"
            package p
            fun box(): String {
                fun helper(): String = Api().local()
                val captured: String = Api().local()
                return if (helper() == "OK" && captured == "OK") "OK" else "FAIL"
            }
        "#,
    );
}

#[test]
fn same_package_access_from_companion_object() {
    run_mixed(
        &[(
            "p/Api.java",
            "package p; public class Api { String companionValue() { return \"OK\"; } }",
        )],
        r#"
            package p
            class K {
                companion object {
                    fun read(): String = Api().companionValue()
                }
            }
            fun box(): String = K.read()
        "#,
    );
}
