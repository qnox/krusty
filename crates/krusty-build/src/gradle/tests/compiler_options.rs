//! Compiler-option and free-argument scenarios of the Kotlin compiler slice: what the plugin refuses
//! before asking krusty, what it leaves to kotlinc, and how free arguments follow the structured
//! options, as KGP passes them.

use super::*;

/// Run every compiler-option scenario against the compiler-slice `fixture`, whose `compiler/util`
/// module writes its Kotlin classes to `kotlin_classes` and whose `core/util.runtime` module
/// compiles `runtime_source`.
pub(super) fn check(fixture: &IntegrationFixture, kotlin_classes: &Path, runtime_source: &Path) {
    let root = &fixture.root;
    let log = &fixture.log;
    let build = || fixture.build();
    // A request the plugin cannot derive krusty's arguments from is valid for kotlinc: the
    // check reports it without asking krusty, and the compile is left to kotlinc. The
    // probe stops after the check, since several of these requests name files that do not
    // exist.
    for (case, expected) in [
                (
                    "structured-option",
                    "krusty does not support compilerOptions.extraWarnings",
                ),
                (
                    "reserved-free-argument",
                    "freeCompilerArg '-d=forbidden' conflicts with the plugin-owned destination; configure the structured Gradle input instead",
                ),
                (
                    "plugin-free-argument",
                    "freeCompilerArg '-Xplugin=forbidden.jar' conflicts with compiler plugin configuration; configure the structured Gradle input instead",
                ),
                (
                    "compiler-version",
                    "Kotlin compiler 2.4.0 differs from Kotlin Gradle plugin 2.4.10",
                ),
                (
                    "free-end-of-options",
                    "freeCompilerArg '--' would make the arguments after it sources; add sources to the task's source set instead",
                ),
                (
                    "free-argfile",
                    "freeCompilerArg '@overrides.args' is an argument file, which could hide plugin-owned arguments; pass its arguments as freeCompilerArgs instead",
                ),
                (
                    "free-source",
                    "freeCompilerArg 'Extra.kt' would be compiled as a source; add sources to the task's source set instead",
                ),
                (
                    "classpath-alias",
                    "freeCompilerArg '-cp' conflicts with the task libraries classpath; configure the structured Gradle input instead",
                ),
                (
                    "compiler-plugin-alias",
                    "freeCompilerArg '-Xcompiler-plugin=forbidden.jar' conflicts with compiler plugin configuration; configure the structured Gradle input instead",
                ),
                (
                    "plugin-option-free-argument",
                    "freeCompilerArg '-P' conflicts with compiler plugin configuration; configure the structured Gradle input instead",
                ),
                (
                    "reference-version-free-argument",
                    "freeCompilerArg '-Xkotlin-reference-version=2.4.0' conflicts with the Kotlin Gradle plugin version; configure the structured Gradle input instead",
                ),
                (
                    "dependency-policy-free-argument",
                    "freeCompilerArg '-no-stdlib=false' conflicts with the plugin-owned dependency policy; configure the structured Gradle input instead",
                ),
                (
                    "dev-compiler-version",
                    "Kotlin compiler 2.4.10-dev-7885 differs from Kotlin Gradle plugin 2.4.10",
                ),
                (
                    "empty-opt-in",
                    "compilerOptions.optIn contains an empty marker",
                ),
                (
                    "duplicate-opt-in",
                    "compilerOptions.optIn contains duplicate marker 'krusty.fixture.ExperimentalFirstApi'",
                ),
            ] {
                clear_invocation_log(log);
                clear_invocation_log(&fixture.probes);
                let output = build()
                    .property("krusty.negative", case)
                    .tasks([":compiler:util:compileKotlinKrustyCheck"])
                    .run_output()
                    .unwrap_or_else(|error| panic!("{case}: {error}"));
                assert_left_to_kotlinc(
                    &output,
                    ":compiler:util:compileKotlin",
                    &[expected.to_string()],
                );
                assert!(
                    !fixture.probes.exists(),
                    "{case}: the plugin refuses without asking krusty"
                );
                assert!(!log.exists(), "{case} must not compile with krusty");
            }

    // End to end, such a compile is kotlinc's: the replacement is skipped and the Kotlin
    // task writes the classes. Extra warnings are a request kotlinc compiles as it is.
    clear_invocation_log(log);
    clear_invocation_log(&fixture.probes);
    let _ = std::fs::remove_dir_all(kotlin_classes);
    let output = build()
        .property("krusty.negative", "structured-option")
        .tasks([":compiler:util:compileKotlin"])
        .run_output()
        .unwrap_or_else(|error| panic!("structured-option: {error}"));
    assert_left_to_kotlinc(
        &output,
        ":compiler:util:compileKotlin",
        &["krusty does not support compilerOptions.extraWarnings".to_string()],
    );
    assert!(
        !fixture.probes.exists(),
        "the plugin refuses extra warnings without asking krusty"
    );
    assert!(
        !log.exists(),
        "kotlinc, not krusty, compiles a refused request"
    );
    assert!(
        kotlin_classes
            .join("org/jetbrains/kotlin/util/UtilKt.class")
            .is_file(),
        "kotlinc wrote the module's classes"
    );

    // Free arguments follow the arguments derived from the structured options, as KGP passes
    // them to kotlinc, so one that restates such an option overrides it there too. The
    // compile stays krusty's.
    for (case, free) in [
        ("progressive-free-argument", &["-progressive"][..]),
        ("progressive-alias", &["-Xprogressive"]),
        ("module-name-free-argument", &["-module-name=overridden"]),
        ("jvm-default-conflict", &["-jvm-default=disable"]),
        (
            "opt-in-overlap",
            &["-opt-in=krusty.fixture.ExperimentalFirstApi"],
        ),
    ] {
        clear_invocation_log(log);
        build()
            .property("krusty.negative", case)
            .tasks([":compiler:util:compileKotlin"])
            .run()
            .unwrap_or_else(|error| panic!("{case}: {error}"));
        let invocation = single_module_invocation(log, "kotlin-compiler-util");
        let structured = invocation
            .iter()
            .position(|argument| argument == "-module-name")
            .unwrap_or_else(|| panic!("{case}: structured -module-name: {invocation:?}"));
        let free_start = invocation
            .windows(free.len())
            .rposition(|window| window == free)
            .unwrap_or_else(|| panic!("{case}: free {free:?}: {invocation:?}"));
        assert!(
            structured < free_start,
            "{case}: free arguments follow the structured ones: {invocation:?}"
        );
    }

    // A Kotlin JVM target that differs from the Java one fails the build, as it does with
    // kotlinc under the default validation mode.
    clear_invocation_log(log);
    let rendered = build()
        .property("krusty.negative", "jvm-target")
        .tasks([":compiler:util:compileKotlin"])
        .run()
        .expect_err("negative case jvm-target succeeded")
        .to_string();
    assert_eq!(
        rendered
            .lines()
            .filter(|line| line.trim() == "> Kotlin JVM target 17 differs from Java target 11")
            .count(),
        1,
        "jvm-target: {rendered}",
    );
    assert!(!log.exists(), "jvm-target must fail before execing krusty");

    // A compiler plugin krusty does not implement leaves the compile to kotlinc, reported.
    clear_invocation_log(log);
    clear_invocation_log(&fixture.probes);
    let output = build()
        .property("krusty.negative", "compiler-plugin")
        .tasks([":compiler:util:compileKotlin"])
        .run_output()
        .unwrap_or_else(|error| panic!("compiler-plugin: {error}"));
    assert_lombok_left_to_kotlinc(&output, &fixture.probes, ":compiler:util:compileKotlin");
    assert!(
        !log.exists(),
        "kotlinc, not krusty, compiles a refused plugin request"
    );

    // `compilerOptions.progressiveMode` is a structured input like `javaParameters`: the
    // plugin forwards it as kotlinc's `-progressive`, which krusty honors because it models
    // every progressive feature of the selected release.
    clear_invocation_log(log);
    build()
        .property("krusty.negative", "progressive-mode")
        .tasks([":compiler:util:compileKotlin"])
        .run()
        .unwrap_or_else(|error| panic!("progressive-mode: {error}"));
    let invocation = single_module_invocation(log, "kotlin-compiler-util");
    assert_eq!(
        invocation
            .iter()
            .filter(|argument| argument.as_str() == "-progressive")
            .count(),
        1,
        "progressive-mode: {invocation:?}"
    );

    // Free arguments reach krusty verbatim: the plugin keeps no copy of kotlinc's argument
    // table. An argument krusty does not implement leaves the compile to kotlinc, reported.
    clear_invocation_log(log);
    let output = build()
        .property("krusty.negative", "jspecify-free-argument")
        .tasks([":compiler:util:compileKotlin"])
        .run_output()
        .unwrap_or_else(|error| panic!("jspecify-free-argument: {error}"));
    assert_left_to_kotlinc(
        &output,
        ":compiler:util:compileKotlin",
        &[
            "krusty does not implement the kotlinc argument '-Xjspecify-annotations=strict'"
                .to_string(),
        ],
    );
    assert!(
        !log.exists(),
        "kotlinc, not krusty, compiles a refused argument"
    );

    // A malformed value is no refusal: the support check rejects it with the same parser
    // and the same one error line as a direct CLI or Bazel-worker invocation, so neither
    // compiler runs and the task's destination is left untouched.
    for (case, argument, expected) in [
        (
            "explicit-api-bad-mode",
            "-Xexplicit-api=sideways",
            "krusty: error: unknown value for parameter -Xexplicit-api: 'sideways'. \
                     Value should be one of {disable, strict, warning}"
                .to_owned(),
        ),
        (
            "jdk-release-free-argument",
            "-Xjdk-release=21",
            "krusty: error: '-Xjdk-release=21' option conflicts with '-jvm-target 17'. \
                     Please remove the '-jvm-target' option"
                .to_owned(),
        ),
        (
            "warning-level-bad-severity",
            "-Xwarning-level=REDUNDANT_CLI_ARG:loud",
            "krusty: error: invalid severity 'loud' in \
                     -Xwarning-level=REDUNDANT_CLI_ARG:loud; supported severities: error, \
                     warning, disabled"
                .to_owned(),
        ),
        (
            "warning-level-missing-colon",
            "-Xwarning-level=REDUNDANT_CLI_ARG",
            "krusty: error: invalid value 'REDUNDANT_CLI_ARG' for -Xwarning-level: \
                     expected <NAME>:<error|warning|disabled>"
                .to_owned(),
        ),
    ] {
        clear_invocation_log(log);
        clear_invocation_log(&fixture.probes);
        let _ = std::fs::remove_dir_all(kotlin_classes);
        let rendered = match build()
            .property("krusty.negative", case)
            .tasks([":compiler:util:compileKotlin"])
            .run()
        {
            Ok(()) => panic!("negative case {case} succeeded"),
            Err(error) => error.to_string(),
        };
        let check = single_invocation(&fixture.probes);
        assert_eq!(
            check
                .iter()
                .filter(|actual| actual.as_str() == argument)
                .count(),
            1,
            "{case}: {check:?}",
        );
        assert_eq!(
            task_failure(&rendered, ":compiler:util:compileKotlinKrustyCheck"),
            [expected.as_str()],
            "{case}: {rendered}",
        );
        assert!(!log.exists(), "{case} must fail before compiling");
        assert!(
            !kotlin_classes.exists(),
            "{case} wrote to {}",
            kotlin_classes.display()
        );
    }

    // Gradle forwards standard version values without duplicating kotlinc's
    // release-specific version table in the plugin. The shared compiler settings boundary
    // rejects values unavailable in the selected 2.4.10 compiler after seeing the exact
    // standard argument.
    for (case, option, kind) in [
        ("old-language-version", "-language-version", "language"),
        ("old-api-version", "-api-version", "API"),
    ] {
        clear_invocation_log(log);
        clear_invocation_log(&fixture.probes);
        let _ = std::fs::remove_dir_all(kotlin_classes);
        let rendered = build()
            .property("krusty.negative", case)
            .tasks([":compiler:util:compileKotlin"])
            .run()
            .expect_err("negative case succeeded")
            .to_string();
        assert_eq!(
            task_failure(&rendered, ":compiler:util:compileKotlinKrustyCheck"),
            [
                format!("krusty: error: unknown {kind} version: 1.9"),
                format!(
                    "Supported {kind} versions: 2.0 (deprecated), 2.1 (deprecated), 2.2, \
                             2.3, 2.4, 2.5 (experimental)"
                ),
            ],
            "{case}: {rendered}",
        );
        assert!(!log.exists(), "{case} must fail before compiling");
        assert!(
            !kotlin_classes.exists(),
            "{case} wrote to {}",
            kotlin_classes.display()
        );
        let invocation = single_invocation(&fixture.probes);
        assert_eq!(
            invocation
                .windows(2)
                .filter(|pair| pair == &[option, "1.9"])
                .count(),
            1,
            "{case}: {invocation:?}",
        );
    }

    // A well-formed named policy is forwarded to and applied by the compiler. Disabling a
    // warning does not change classfile bytes, so this compile succeeds normally.
    clear_invocation_log(log);
    build()
        .property("krusty.negative", "warning-level")
        .tasks([":compiler:util:compileKotlin"])
        .run()
        .unwrap_or_else(|error| panic!("warning-level forwarding: {error}"));
    let forwarded = single_invocation(log);
    assert_eq!(
        forwarded
            .iter()
            .filter(|argument| {
                argument.as_str() == "-Xwarning-level=REDUNDANT_CLI_ARG:disabled"
            })
            .count(),
        1,
        "{forwarded:?}"
    );

    // The standard Gradle languageVersion reaches the compiler unchanged. No krusty-only
    // property and no metadata-only substitution is part of the contract.
    clear_invocation_log(log);
    std::fs::write(
                runtime_source,
                "package org.jetbrains.kotlin.util\nfun runtimeMarker() = \"runtime\"\nfun languageLevel22() = 1\n",
            )
            .expect("edit runtime for languageVersion 2.2");
    build()
        .property("krusty.negative", "language-version-2-2")
        .tasks([":core:util.runtime:compileKotlin"])
        .run()
        .unwrap_or_else(|error| panic!("languageVersion 2.2: {error}"));
    let language_2_2_run = single_invocation(log);
    assert_eq!(
        language_2_2_run
            .windows(2)
            .filter(|pair| pair == &["-language-version", "2.2"])
            .count(),
        1,
        "{language_2_2_run:?}",
    );
    assert_eq!(
        language_2_2_run
            .windows(2)
            .filter(|pair| pair == &["-api-version", "2.2"])
            .count(),
        1,
        "{language_2_2_run:?}",
    );
    assert!(
        language_2_2_run
            .iter()
            .all(|argument| !argument.starts_with("-Xmetadata-version")),
        "{language_2_2_run:?}",
    );

    // `-Werror` and `allWarningsAsErrors` both reach krusty as one warning policy. This
    // module intentionally carries two unresolved opt-in markers, so each form must fail
    // with the complete ordered compiler diagnostic stream. Setting both is idempotent,
    // like kotlinc's command line, rather than a transport conflict.
    let diagnostic_log = root.join("krusty-diagnostics.txt");
    let expected_werror_diagnostics = "warning: opt-in requirement marker 'krusty.fixture.ExperimentalFirstApi' is unresolved. Make sure it's present in the module dependencies.\n\
warning: opt-in requirement marker 'krusty.fixture.ExperimentalSecondApi' is unresolved. Make sure it's present in the module dependencies.\n\
error: warnings found and -Werror specified\n";
    for case in ["all-warnings-as-errors", "free-werror", "werror-overlap"] {
        clear_invocation_log(log);
        let _ = std::fs::remove_file(&diagnostic_log);
        let result = build()
            .property("krusty.negative", case)
            .tasks([":compiler:util:compileKotlin"])
            .run();
        let status = match result {
            Err(GradleError::Failed { status, .. }) => status,
            other => {
                panic!("{case}: expected the warning-bearing module to fail, got {other:?}")
            }
        };
        assert_eq!(status, "exit status: 1", "{case}");
        assert_eq!(
            std::fs::read_to_string(&diagnostic_log).expect("recorded krusty diagnostics"),
            expected_werror_diagnostics,
            "{case}",
        );
        let invocation = single_module_invocation(log, "kotlin-compiler-util");
        assert_eq!(
            invocation
                .iter()
                .filter(|argument| argument.as_str() == "-Werror")
                .count(),
            1,
            "{case}: {invocation:?}"
        );
    }

    // A named level remains an override when the structured global policy is enabled.
    // `-Xcontext-parameters` is redundant at language level 2.4, so success proves that
    // the transported `:warning` exception was applied rather than promoted by `-Werror`.
    clear_invocation_log(log);
    let _ = std::fs::remove_file(&diagnostic_log);
    build()
        .property("krusty.negative", "werror-warning-exception")
        .tasks([":compiler:util:compileKotlin"])
        .run()
        .unwrap_or_else(|error| panic!("werror warning exception: {error}"));
    assert_eq!(
                std::fs::read_to_string(&diagnostic_log).expect("recorded krusty diagnostics"),
                "warning: the argument '-Xcontext-parameters' is redundant for the current language version 2.4.\n",
                "werror warning exception diagnostics",
            );
    let invocation = single_module_invocation(log, "kotlin-compiler-util");
    for expected in [
        "-Werror",
        "-Xcontext-parameters",
        "-Xwarning-level=REDUNDANT_CLI_ARG:warning",
    ] {
        assert_eq!(
            invocation
                .iter()
                .filter(|argument| argument.as_str() == expected)
                .count(),
            1,
            "{expected}: {invocation:?}"
        );
    }
}
