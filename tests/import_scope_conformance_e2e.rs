//! Import-scoped resolution of TOP-LEVEL functions and EXTENSIONS conforms to kotlinc: an unqualified
//! call binds ONLY to a same-package / imported / default function — NOT to an arbitrary classpath
//! function of that name. A dependency library declares both in a NON-default package `mylib`; the
//! consumer must import them, exactly as kotlinc requires.

use super::common;

const LIB: &str = "package mylib\n\
    fun provide(): String = \"OK\"\n\
    fun String.tagged(): String = this + \"!\"\n";

fn assert_unresolved_against_library(
    main: &str,
    column: usize,
    name: &str,
    receiver: Option<&str>,
) {
    let library = common::kotlinc_library(LIB).expect("kotlinc compiles the dependency");
    let result =
        common::compiler_diagnostics(&[("Main.kt", main)], &[library, common::stdlib_jar()]);
    let expected = [common::CompilerError {
        file: "Main.kt".to_string(),
        line: 1,
        column,
        message: krusty::diagnostic_wording::unresolved_reference_on(name, receiver),
    }];
    assert_eq!(result.krusty_code, 1, "{}", result.krusty_stderr);
    assert_eq!(result.reference_code, 1, "{}", result.reference_stderr);
    assert_eq!(common::compiler_errors(&result.krusty_stdout), []);
    assert_eq!(common::compiler_errors(&result.krusty_stderr), expected);
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
}

#[test]
fn unimported_top_level_function_is_unresolved() {
    // No `import mylib.provide` — kotlinc requires it, so a bare `provide()` must NOT resolve to the
    // classpath function (the over-permissive whole-classpath lookup would have found it).
    let main = "fun box(): String = provide()\n";
    assert_unresolved_against_library(main, 21, "provide", None);
}

#[test]
fn imported_top_level_function_resolves_and_runs() {
    let main = "import mylib.provide\n\
        fun box(): String = provide()\n";
    // A rejected source must FAIL here, not skip: the toolchain is the only legitimate `None`.
    let Some(out) = common::expect_box_run_against("scope_tl_pos", LIB, main) else {
        return;
    };
    assert_eq!(
        out, "OK",
        "an imported top-level function resolves and runs"
    );
}

#[test]
fn unimported_extension_is_unresolved() {
    let main = "fun box(): String = \"x\".tagged()\n";
    assert_unresolved_against_library(main, 25, "tagged", Some("String"));
}

#[test]
fn imported_extension_resolves_and_runs() {
    let main = "import mylib.tagged\n\
        fun box(): String = if (\"O\".tagged() == \"O!\") \"OK\" else \"NO\"\n";
    let Some(out) = common::expect_box_run_against("scope_ext_pos", LIB, main) else {
        return;
    };
    assert_eq!(out, "OK", "an imported extension resolves and runs");
}

/// Each file of a package resolves signatures through its OWN imports. Two sibling files alias
/// different classes to the same name, and every inferred signature (a function's return, a
/// property's type, one declaration inferred from another) must bind the alias of the file that
/// declares it, however many signatures of the other file were inferred first.
#[test]
fn each_file_infers_its_signatures_through_its_own_imports() {
    let sources = [
        (
            "First.kt",
            "package pick.first\nclass Item { fun tag() = \"first\" }\n",
        ),
        (
            "Second.kt",
            "package pick.second\nclass Item { fun tag() = \"second\" }\n",
        ),
        (
            "UseFirst.kt",
            "package pick.use\n\
             import pick.first.Item as Picked\n\
             fun first() = Picked()\n\
             val firstAgain = first()\n\
             fun firstTag() = firstAgain.tag()\n",
        ),
        (
            "UseSecond.kt",
            "package pick.use\n\
             import pick.second.Item as Picked\n\
             fun second() = Picked()\n\
             val secondAgain = second()\n\
             fun secondTag() = secondAgain.tag()\n",
        ),
        (
            "Box.kt",
            "package pick.use\n\
             fun box(): String {\n\
             val one: pick.first.Item = first()\n\
             val two: pick.second.Item = second()\n\
             return if (one.tag() + firstTag() + two.tag() + secondTag() == \"firstfirstsecondsecond\") \"OK\" else \"fail\"\n\
             }\n",
        ),
    ];
    common::expect_box_ok_files_with_stdlib(&sources, "per-file import aliases in signatures");
}

/// Two explicit imports of one name from different packages (`import a.contain`, `import
/// b.contain`) both put their callables at the explicit-import level; overload resolution picks
/// by receiver. Neither import replaces the other, from a dependency or from this module.
const SAME_NAME_IMPORTS_MAIN: &str = "import same.lists.contain\n\
    import same.text.contain\n\
    fun box(): String {\n\
        val list = listOf(1, 2) contain 2\n\
        val text = \"abc\" contain \"b\"\n\
        return if (list == \"list\" && text == \"text\") \"OK\" else \"$list $text\"\n\
    }\n";

const SAME_NAME_IMPORTS_LIB: [(&str, &str); 2] = [
    (
        "Lists.kt",
        "package same.lists\n\
         infix fun <T> Collection<T>.contain(element: T): String = if (element in this) \"list\" else \"-\"\n",
    ),
    (
        "Text.kt",
        "package same.text\n\
         infix fun String.contain(part: String): String = if (part in this) \"text\" else \"-\"\n",
    ),
];

#[test]
fn same_name_explicit_imports_from_a_dependency_are_all_candidates() {
    let build = common::compile_libs_build("same_name_imports", &SAME_NAME_IMPORTS_LIB)
        .expect("krusty compiles the dependency");
    let lib = build
        .reference_out()
        .expect("kotlinc compiles the dependency")
        .to_path_buf();
    let jdk = common::jdk_modules();
    let classpath = [common::stdlib_jar(), lib.clone()];
    let krusty =
        common::compile_and_run_box(SAME_NAME_IMPORTS_MAIN, "Main", &classpath, Some(&jdk))
            .expect("krusty resolves both imports");
    let reference = common::kotlinc_box_files_result_with_classpath(
        &[("Main.kt", SAME_NAME_IMPORTS_MAIN)],
        "MainKt",
        &[lib],
    );
    assert_eq!(krusty, "OK");
    assert_eq!(reference, "OK");
}

#[test]
fn same_name_explicit_imports_from_this_module_are_all_candidates() {
    let sources = [
        SAME_NAME_IMPORTS_LIB[0],
        SAME_NAME_IMPORTS_LIB[1],
        ("Main.kt", SAME_NAME_IMPORTS_MAIN),
    ];
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let krusty = common::compile_and_run_box_files(&sources, &[stdlib], Some(&jdk))
        .expect("krusty resolves both same-module imports");
    let reference = common::kotlinc_box_files_result(&sources, "MainKt");
    assert_eq!(krusty, "OK");
    assert_eq!(reference, "OK");
}

/// Compile `Conflict.kt`, which imports `a.Same` and `b.Same`, beside `module` sources and against
/// `dependencies`, and assert kotlinc's complete ledger, multi-line messages included: the two
/// conflicting-import errors followed by `use_sites` (line, column, message) in `Conflict.kt`.
fn assert_conflicting_classifier_imports(
    module: &[(&str, &str)],
    dependencies: &[std::path::PathBuf],
    conflict: &str,
    use_sites: &[(usize, usize, &str)],
) {
    let sources = module
        .iter()
        .copied()
        .chain([("Conflict.kt", conflict)])
        .collect::<Vec<_>>();
    let classpath = dependencies
        .iter()
        .cloned()
        .chain([common::stdlib_jar()])
        .collect::<Vec<_>>();
    let result = common::compiler_diagnostics(&sources, &classpath);
    let conflicting = "conflicting import: imported name 'Same' is ambiguous.";
    let expected = [(1, 10, conflicting), (2, 10, conflicting)]
        .into_iter()
        .chain(use_sites.iter().copied())
        .flat_map(|(line, column, message)| {
            let mut lines = message.lines();
            let header = format!(
                "Conflict.kt:{line}:{column}: {}",
                lines.next().unwrap_or_default()
            );
            std::iter::once(header)
                .chain(lines.map(|line| format!("| {line}")))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(result.krusty_code, 1, "{}", result.krusty_stderr);
    assert_eq!(result.reference_code, 1, "{}", result.reference_stderr);
    assert_eq!(common::compiler_errors(&result.krusty_stdout), []);
    assert_eq!(
        common::rendered_error_blocks(&result.krusty_stderr),
        expected
    );
    assert_eq!(
        common::rendered_error_blocks(&result.reference_stderr),
        expected
    );
}

const SAME_CLASSES: [(&str, &str); 2] = [
    ("A.kt", "package a\nclass Same\n"),
    ("B.kt", "package b\nclass Same\n"),
];

const CLASS_AMBIGUITY: &str =
    "overload resolution ambiguity between candidates:\nclass Same : Any\nclass Same : Any";

#[test]
fn conflicting_classifier_imports_report_the_complete_kotlinc_ledger() {
    // A type reference lists the conflicting classifiers; a constructor call binds no classifier
    // and reports its name unresolved (kotlinc 2.4.20).
    assert_conflicting_classifier_imports(
        &SAME_CLASSES,
        &[],
        "import a.Same\nimport b.Same\nval value: Same = Same()\n",
        &[
            (3, 12, CLASS_AMBIGUITY),
            (3, 19, "unresolved reference 'Same'."),
        ],
    );
}

#[test]
fn conflicting_classifier_imports_are_ambiguous_in_body_type_positions() {
    assert_conflicting_classifier_imports(
        &SAME_CLASSES,
        &[],
        "import a.Same\nimport b.Same\nfun g() {\n    val x: Same? = null\n    val y = x is Same\n}\n",
        &[(4, 12, CLASS_AMBIGUITY), (5, 18, CLASS_AMBIGUITY)],
    );
}

#[test]
fn conflicting_classifier_imports_select_the_only_complete_type_path() {
    // Only `a.Same` declares `N`, so the qualified type binds it; the import list still conflicts.
    assert_conflicting_classifier_imports(
        &[
            ("A.kt", "package a\nclass Same { class N }\n"),
            ("B.kt", "package b\nclass Same\n"),
        ],
        &[],
        "import a.Same\nimport b.Same\nfun f(x: Same.N) {}\n",
        &[],
    );
}

#[test]
fn conflicting_class_and_typealias_imports_list_the_alias_declaration() {
    let dependency = common::kotlinc_library("package b\ntypealias Same = Any\n")
        .expect("kotlinc compiles the dependency");
    assert_conflicting_classifier_imports(
        &[("A.kt", "package a\nclass Same\n")],
        &[dependency],
        "import a.Same\nimport b.Same\nfun g() {\n    val x: Same? = null\n    val y = Same()\n}\n",
        &[
            (
                4,
                12,
                "overload resolution ambiguity between candidates:\nclass Same : Any\ntypealias Same = Any",
            ),
            (5, 13, "unresolved reference 'Same'."),
        ],
    );
}

#[test]
fn conflicting_generic_classifier_imports_render_type_parameters() {
    let dependency = common::kotlinc_library("package b\ntypealias Same<T> = List<T>\n")
        .expect("kotlinc compiles the dependency");
    assert_conflicting_classifier_imports(
        &[("A.kt", "package a\nclass Same<T>\n")],
        &[dependency],
        "import a.Same\nimport b.Same\nfun g() {\n    val x: Same<String>? = null\n}\n",
        &[(
            4,
            12,
            "overload resolution ambiguity between candidates:\nclass Same<T> : Any\ntypealias Same<T> = List<T>",
        )],
    );
}

#[test]
fn conflicting_same_module_typealias_imports_keep_both_declarations() {
    assert_conflicting_classifier_imports(
        &[
            ("A.kt", "package a\ntypealias Same = String\n"),
            ("B.kt", "package b\ntypealias Same = Int\n"),
        ],
        &[],
        "import a.Same\nimport b.Same\nfun use(value: Same?) = value\n",
        &[(3, 16, "overload resolution ambiguity between candidates:\ntypealias Same = String\ntypealias Same = Int")],
    );
}

#[test]
fn conflicting_class_and_same_module_typealias_keep_both_declarations() {
    assert_conflicting_classifier_imports(
        &[
            ("A.kt", "package a\nclass Same\n"),
            ("B.kt", "package b\ntypealias Same = Int\n"),
        ],
        &[],
        "import a.Same\nimport b.Same\nfun use(value: Same?) = value\n",
        &[(3, 16, "overload resolution ambiguity between candidates:\nclass Same : Any\ntypealias Same = Int")],
    );
}

#[test]
fn conflicting_imports_are_reported_when_only_the_signature_fails() {
    assert_conflicting_classifier_imports(
        &[
            ("A.kt", "package a\ntypealias Same = String\n"),
            ("B.kt", "package b\ntypealias Same = Int\n"),
        ],
        &[],
        "import a.Same\nimport b.Same\nfun use(value: Same): Missing = value\n",
        &[
            (
                3,
                16,
                "overload resolution ambiguity between candidates:\ntypealias Same = String\ntypealias Same = Int",
            ),
            (3, 23, "unresolved reference 'Missing'."),
        ],
    );
}

fn assert_mixed_imports_run(main: &str) {
    let sources = [
        (
            "Functions.kt",
            "package mixed.functions\nfun selected(): String = \"OK\"\nfun chosen(): String = \"OK\"\n",
        ),
        (
            "Object.kt",
            "package mixed.objects\nobject selected { operator fun invoke(): String = \"OK\" }\n",
        ),
        (
            "Property.kt",
            "package mixed.properties\nval chosen: String = \"not callable\"\n",
        ),
        ("Main.kt", main),
    ];
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let krusty = common::compile_and_run_box_files(&sources, &[stdlib], Some(&jdk))
        .expect("krusty resolves the callable facet");
    let reference = common::kotlinc_box_files_result(&sources, "MainKt");
    assert_eq!(krusty, "OK");
    assert_eq!(reference, "OK");
}

#[test]
fn callable_and_object_imports_are_order_independent() {
    for main in [
        "import mixed.functions.selected\nimport mixed.objects.selected\nfun box(): String = selected()\n",
        "import mixed.objects.selected\nimport mixed.functions.selected\nfun box(): String = selected()\n",
    ] {
        assert_mixed_imports_run(main);
    }
}

#[test]
fn callable_and_property_imports_are_order_independent() {
    for main in [
        "import mixed.functions.chosen\nimport mixed.properties.chosen\nfun box(): String = chosen()\n",
        "import mixed.properties.chosen\nimport mixed.functions.chosen\nfun box(): String = chosen()\n",
    ] {
        assert_mixed_imports_run(main);
    }
}
