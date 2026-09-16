//! Exact Java access and visibility diagnostic parity with kotlinc.

use super::common;
use super::diagnostics_parity_support::{errors, ObservedError};

#[test]
fn java_package_private_member_diagnostics_match_kotlinc() {
    let Some((java_dir, _)) = common::javac_compile(
        &[(
            "p/Api.java".to_string(),
            "package p; public final class Api {\n\
                 String instanceField = \"\";\n\
                 static String staticField = \"\";\n\
                 Api() {}\n\
                 void instanceMethod() {}\n\
                 static void staticMethod() {}\n\
             }"
            .to_string(),
        )],
        &[],
    ) else {
        eprintln!("skipping: JDK unavailable");
        return;
    };
    let source = "package q\n\
                  fun rejected(api: p.Api) {\n\
                      api.instanceField\n\
                      p.Api.staticField\n\
                      api.instanceMethod()\n\
                      p.Api.staticMethod()\n\
                      p.Api()\n\
                      api.instanceField = \"x\"\n\
                      p.Api.staticField = \"x\"\n\
                  }\n";
    let result = common::compiler_diagnostics(
        &[("PackagePrivateMembers.kt", source)],
        std::slice::from_ref(&java_dir),
    );
    if let Some(root) = java_dir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }

    assert_ne!(result.krusty_code, 0, "krusty silently accepted source");
    assert_ne!(
        result.reference_code, 0,
        "kotlinc unexpectedly accepted source"
    );
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let mut kotlinc_errors = errors(&result.reference_stderr);
    krusty_errors.sort_by_key(|error| (error.line, error.column));
    kotlinc_errors.sort_by_key(|error| (error.line, error.column));
    let expected = vec![
        ObservedError {
            file: "PackagePrivateMembers.kt".to_string(),
            line: 3,
            column: 5,
            message: "cannot access 'field instanceField: String!': it is package-private in 'p.Api'."
                .to_string(),
        },
        ObservedError {
            file: "PackagePrivateMembers.kt".to_string(),
            line: 4,
            column: 7,
            message: "cannot access 'static field staticField: String!': it is package-private in 'p.Api'."
                .to_string(),
        },
        ObservedError {
            file: "PackagePrivateMembers.kt".to_string(),
            line: 5,
            column: 5,
            message: "cannot access 'fun instanceMethod(): Unit': it is package-private in 'p.Api'."
                .to_string(),
        },
        ObservedError {
            file: "PackagePrivateMembers.kt".to_string(),
            line: 6,
            column: 7,
            message: "cannot access 'static fun staticMethod(): Unit': it is package-private in 'p.Api'."
                .to_string(),
        },
        ObservedError {
            file: "PackagePrivateMembers.kt".to_string(),
            line: 7,
            column: 3,
            message: "cannot access 'constructor(): Api': it is package-private in 'p.Api'."
                .to_string(),
        },
        ObservedError {
            file: "PackagePrivateMembers.kt".to_string(),
            line: 8,
            column: 5,
            message: "cannot access 'field instanceField: String!': it is package-private in 'p.Api'."
                .to_string(),
        },
        ObservedError {
            file: "PackagePrivateMembers.kt".to_string(),
            line: 9,
            column: 7,
            message: "cannot access 'static field staticField: String!': it is package-private in 'p.Api'."
                .to_string(),
        },
    ];
    assert_eq!(krusty_errors, expected);
    assert_eq!(kotlinc_errors, expected);
}

#[test]
fn java_unbounded_wildcard_diagnostic_is_a_star_projection() {
    let (java_dir, _) = common::javac_compile(
        &[
            (
                "fixtures/W.java".to_string(),
                "package fixtures; public final class W<T> {}".to_string(),
            ),
            (
                "fixtures/Pair.java".to_string(),
                "package fixtures; public final class Pair<A, B> {}".to_string(),
            ),
            (
                "fixtures/Wildcards.java".to_string(),
                "package fixtures; import java.lang.CharSequence; public final class Wildcards {\n\
                     public static W<?> unbounded() { return null; }\n\
                     public static W<? extends CharSequence> covariant() { return null; }\n\
                     public static W<? super Integer> contravariant() { return null; }\n\
                     public static Pair<String, ?> nested() { return null; }\n\
                 }"
                .to_string(),
            ),
        ],
        &[],
    )
    .expect("JDK javac must compile the wildcard fixture");
    let source = "import fixtures.Wildcards\n\
                  fun unbounded(): String = Wildcards.unbounded()\n\
                  fun covariant(): String = Wildcards.covariant()\n\
                  fun contravariant(): String = Wildcards.contravariant()\n\
                  fun nested(): String = Wildcards.nested()\n";
    let result = common::compiler_diagnostics(
        &[("JavaWildcardDiagnostics.kt", source)],
        std::slice::from_ref(&java_dir),
    );
    if let Some(root) = java_dir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
    assert_eq!((result.krusty_code, result.reference_code), (1, 1));
    let mut krusty = errors(&result.krusty_stderr);
    krusty.extend(errors(&result.krusty_stdout));
    let reference = errors(&result.reference_stderr);
    let expected = vec![
        ObservedError {
            file: "JavaWildcardDiagnostics.kt".to_string(),
            line: 2,
            column: 27,
            message: "return type mismatch: expected 'String', actual 'W<*>!'.".to_string(),
        },
        ObservedError {
            file: "JavaWildcardDiagnostics.kt".to_string(),
            line: 3,
            column: 27,
            message: "return type mismatch: expected 'String', actual 'W<out CharSequence!>!'."
                .to_string(),
        },
        ObservedError {
            file: "JavaWildcardDiagnostics.kt".to_string(),
            line: 4,
            column: 31,
            message: "return type mismatch: expected 'String', actual 'W<in Int!>!'.".to_string(),
        },
        ObservedError {
            file: "JavaWildcardDiagnostics.kt".to_string(),
            line: 5,
            column: 24,
            message: "return type mismatch: expected 'String', actual 'Pair<String!, *>!'."
                .to_string(),
        },
    ];
    assert_eq!(krusty, expected);
    assert_eq!(reference, expected);
}

#[test]
fn protected_java_member_receiver_diagnostics_match_kotlinc() {
    let Some((java_dir, _)) = common::javac_compile(
        &[(
            "fixtures/Parent.java".to_string(),
            "package fixtures; public class Parent { protected String value() { return \"hidden\"; } }"
                .to_string(),
        )],
        &[],
    ) else {
        return;
    };
    let source = "package consumer\nimport fixtures.Parent\nclass Child : Parent() { fun read(parent: Parent): String = parent.value() }";
    let result = common::compiler_diagnostics(
        &[("ProtectedReceiver.kt", source)],
        std::slice::from_ref(&java_dir),
    );
    if let Some(root) = java_dir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let expected = vec![ObservedError {
        file: "ProtectedReceiver.kt".to_string(),
        line: 3,
        column: 68,
        message: "cannot access 'fun value(): String!': it is protected in 'fixtures.Parent'."
            .to_string(),
    }];
    assert_eq!(krusty_errors, expected);
    assert_eq!(errors(&result.reference_stderr), expected);
}

#[test]
fn protected_inherited_java_constructor_diagnostic_matches_kotlinc() {
    let Some((java_dir, _)) = common::javac_compile(
        &[(
            "fixtures/Parent.java".to_string(),
            "package fixtures; public class Parent { protected static class Category { protected Category() {} } }"
                .to_string(),
        )],
        &[],
    ) else {
        return;
    };
    let source = "import fixtures.Parent\n\
                  class Child : Parent() {\n\
                      fun value(): Any = Category()\n\
                  }";
    let result = common::compiler_diagnostics(
        &[("ProtectedNestedConstructor.kt", source)],
        std::slice::from_ref(&java_dir),
    );
    if let Some(root) = java_dir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
    let expected = vec![ObservedError {
        file: "ProtectedNestedConstructor.kt".to_string(),
        line: 3,
        column: 20,
        message: "cannot access 'constructor(): Parent.Category': it is protected in 'fixtures.Parent.Category'."
            .to_string(),
    }];
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    assert_eq!(krusty_errors, expected);
    assert_eq!(errors(&result.reference_stderr), expected);
}

#[test]
fn protected_java_field_receiver_diagnostics_match_kotlinc() {
    let Some((java_dir, _)) = common::javac_compile(
        &[(
            "fixtures/Parent.java".to_string(),
            "package fixtures; public class Parent { protected String field = \"\"; }".to_string(),
        )],
        &[],
    ) else {
        return;
    };
    let source = "package consumer\n\
                  import fixtures.Parent\n\
                  class Child : Parent() {\n\
                      fun read(parent: Parent): String = parent.field\n\
                      fun write(parent: Parent) { parent.field = \"\" }\n\
                  }";
    let result = common::compiler_diagnostics(
        &[("ProtectedFieldReceiver.kt", source)],
        std::slice::from_ref(&java_dir),
    );
    if let Some(root) = java_dir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let kotlinc_errors = errors(&result.reference_stderr);
    let expected = vec![
        ObservedError {
            file: "ProtectedFieldReceiver.kt".to_string(),
            line: 4,
            column: 43,
            message: "cannot access 'field field: String!': it is protected in 'fixtures.Parent'."
                .to_string(),
        },
        ObservedError {
            file: "ProtectedFieldReceiver.kt".to_string(),
            line: 5,
            column: 36,
            message: "cannot access 'field field: String!': it is protected in 'fixtures.Parent'."
                .to_string(),
        },
    ];
    assert_eq!(krusty_errors.len(), 2);
    assert_eq!(kotlinc_errors.len(), 2);
    assert_eq!(krusty_errors, expected);
    assert_eq!(kotlinc_errors, expected);
}

#[test]
fn java_field_write_rejection_diagnostics_match_kotlinc() {
    let Some((java_dir, _)) = common::javac_compile(
        &[(
            "fixtures/Parent.java".to_string(),
            "package fixtures; public class Parent { public final String finalField = \"\"; protected String protectedField = \"\"; }".to_string(),
        )],
        &[],
    ) else {
        return;
    };
    let source = "package consumer\n\
                  import fixtures.Parent\n\
                  fun finalWrite(parent: Parent) { parent.finalField = \"\" }\n\
                  fun protectedWrite(parent: Parent) { parent.protectedField = \"\" }";
    let result = common::compiler_diagnostics(
        &[("JavaFieldWriteRejections.kt", source)],
        std::slice::from_ref(&java_dir),
    );
    if let Some(root) = java_dir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let kotlinc_errors = errors(&result.reference_stderr);
    let expected = vec![
        ObservedError {
            file: "JavaFieldWriteRejections.kt".to_string(),
            line: 3,
            column: 41,
            message: "'val' cannot be reassigned.".to_string(),
        },
        ObservedError {
            file: "JavaFieldWriteRejections.kt".to_string(),
            line: 4,
            column: 45,
            message: "cannot access 'field protectedField: String!': it is protected in 'fixtures.Parent'."
                .to_string(),
        },
    ];
    assert_eq!(krusty_errors.len(), 2);
    assert_eq!(kotlinc_errors.len(), 2);
    assert_eq!(krusty_errors, expected);
    assert_eq!(kotlinc_errors, expected);
}

#[test]
fn package_private_java_classifier_constructor_diagnostics_match_kotlinc() {
    let Some((java_dir, _)) = common::javac_compile(
        &[(
            "javafixture/PackageBox.java".to_string(),
            "package javafixture; class PackageBox { PackageBox(int value) {} }".to_string(),
        )],
        &[],
    ) else {
        return;
    };
    let source = "package consumer\n\
                  import javafixture.PackageBox\n\
                  fun use(): Int { PackageBox(1); return 0 }\n";
    let result = common::compiler_diagnostics(
        &[("PackagePrivateClassifier.kt", source)],
        std::slice::from_ref(&java_dir),
    );
    if let Some(root) = java_dir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let mut kotlinc_errors = errors(&result.reference_stderr);
    krusty_errors.sort_by_key(|error| (error.line, error.column));
    kotlinc_errors.sort_by_key(|error| (error.line, error.column));
    let expected = vec![
        ObservedError {
            file: "PackagePrivateClassifier.kt".to_string(),
            line: 2,
            column: 20,
            message: "cannot access 'class PackageBox : Any': it is package-private in file."
                .to_string(),
        },
        ObservedError {
            file: "PackagePrivateClassifier.kt".to_string(),
            line: 3,
            column: 18,
            message: "cannot access 'constructor(p0: Int): PackageBox': it is package-private in 'javafixture.PackageBox'."
                .to_string(),
        },
    ];
    assert_eq!(krusty_errors, expected);
    assert_eq!(kotlinc_errors, expected);
}

#[test]
fn package_private_java_static_field_diagnostics_match_kotlinc() {
    let Some((java_dir, _)) = common::javac_compile(
        &[(
            "p/Pub.java".to_string(),
            "package p; public class Pub { static int count = 7; }".to_string(),
        )],
        &[],
    ) else {
        return;
    };
    let result = common::compiler_diagnostics(
        &[
            (
                "ClassifierImportStaticField.kt",
                "package q\nimport p.Pub\nfun classifierImport(): Int = Pub.count\n",
            ),
            (
                "QualifiedStaticField.kt",
                "package q\nfun qualified(): Int = p.Pub.count\n",
            ),
        ],
        std::slice::from_ref(&java_dir),
    );
    if let Some(root) = java_dir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let kotlinc_errors = errors(&result.reference_stderr);
    let message =
        "cannot access 'static field count: Int': it is package-private in 'p.Pub'.".to_string();
    let expected = vec![
        ObservedError {
            file: "ClassifierImportStaticField.kt".to_string(),
            line: 3,
            column: 35,
            message: message.clone(),
        },
        ObservedError {
            file: "QualifiedStaticField.kt".to_string(),
            line: 2,
            column: 30,
            message,
        },
    ];
    assert_eq!(krusty_errors, expected);
    assert_eq!(kotlinc_errors, expected);
    assert_eq!(krusty_errors.len(), 2);
    assert_eq!(kotlinc_errors.len(), 2);
}

#[test]
fn package_private_java_classifier_public_constructor_diagnostics_match_kotlinc() {
    let Some((java_dir, _)) = common::javac_compile(
        &[(
            "fixtures/PackageType.java".to_string(),
            "package fixtures; class PackageType { public PackageType() {} }".to_string(),
        )],
        &[],
    ) else {
        return;
    };
    let source = "package consumer\n\
                  import fixtures.PackageType\n\
                  fun use(): Any = PackageType()\n";
    let result = common::compiler_diagnostics(
        &[("PackagePrivatePublicConstructor.kt", source)],
        std::slice::from_ref(&java_dir),
    );
    if let Some(root) = java_dir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let mut kotlinc_errors = errors(&result.reference_stderr);
    krusty_errors.sort_by_key(|error| (error.line, error.column));
    kotlinc_errors.sort_by_key(|error| (error.line, error.column));
    let expected = vec![
        ObservedError {
            file: "PackagePrivatePublicConstructor.kt".to_string(),
            line: 2,
            column: 17,
            message: "cannot access 'class PackageType : Any': it is package-private in file."
                .to_string(),
        },
        ObservedError {
            file: "PackagePrivatePublicConstructor.kt".to_string(),
            line: 3,
            column: 18,
            message: "cannot access 'class PackageType : Any': it is package-private in file."
                .to_string(),
        },
    ];
    assert_eq!(krusty_errors, expected);
    assert_eq!(kotlinc_errors, expected);
}
