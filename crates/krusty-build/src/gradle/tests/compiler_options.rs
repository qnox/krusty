//! Compiler-option and free-argument scenarios of the Kotlin compiler slice: what the plugin refuses
//! before asking krusty, what it leaves to kotlinc, and how free arguments follow the structured
//! options, as KGP passes them.

use super::*;

/// How kotlinc fails a compile whose malformed value krusty rejected too: the build tools API
/// reports the value's error on the compiler's log and fails the work generically.
const KOTLINC_COMPILATION_FAILURE: [&str; 2] = [
    "A failure occurred while executing org.jetbrains.kotlin.compilerRunner.btapi.BuildToolsApiCompilationWork",
    " > Compilation error. See log for more details",
];
/// How kotlinc fails a compile whose API version it no longer supports on Gradle 9, where KGP
/// reports the diagnostic as a Gradle problem that fails the work. Older Gradle fails it
/// generically, as [`KOTLINC_COMPILATION_FAILURE`].
const KOTLINC_UNSUPPORTED_API_VERSION_PROBLEM: [&str; 4] = [
    "A failure occurred while executing org.jetbrains.kotlin.compilerRunner.btapi.BuildToolsApiCompilationWork",
    " > Kotlin compiler: UNSUPPORTED_LANGUAGE_VERSION",
    "     Kotlin compiler error",
    "       API version 1.9 is no longer supported; use version 2.0 or greater instead.",
];

/// Run every compiler-option scenario against the compiler-slice `fixture`, whose `compiler/util`
/// module writes its Kotlin classes to `kotlin_classes`.
pub(super) fn check(fixture: &IntegrationFixture, kotlin_classes: &Path) {
    let root = &fixture.root;
    let log = &fixture.log;
    let build = || fixture.build();
    let refusals_record = &root.join("compiler/util/build/krusty/compileKotlin/refusals.txt");
    // A request the plugin cannot derive krusty's arguments from is valid for kotlinc: the
    // check reports it without asking krusty, and the compile is left to kotlinc. Every refusal
    // the plugin derives is pinned by its KotlincRequestTest; this build drives one free
    // argument through KGP's freeCompilerArgs.
    {
        let case = "plugin-free-argument";
        let expected = "freeCompilerArg '-Xplugin=forbidden.jar' conflicts with compiler plugin configuration; configure the structured Gradle input instead";
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
    // compile stays krusty's. KotlincRequestTest pins the order for every structured option.
    {
        let case = "module-name-free-argument";
        let free = &["-module-name=overridden"][..];
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

    // `-Pkrusty.fixKotlincDefects=true` asks krusty to fix kotlinc's defects: the compile stays
    // krusty's and carries krusty's own switch once, right after the reference version.
    {
        let case = "fix-kotlinc-defects";
        clear_invocation_log(log);
        build()
            .property("krusty.fixKotlincDefects", "true")
            .tasks([":compiler:util:compileKotlin"])
            .run()
            .unwrap_or_else(|error| panic!("{case}: {error}"));
        let invocation = single_module_invocation(log, "kotlin-compiler-util");
        let reference = format!("-Xkotlin-reference-version={}", fixture.kgp);
        assert_eq!(
            invocation
                .windows(2)
                .filter(|pair| pair == &[reference.as_str(), "-Xfix-kotlinc-defects"])
                .count(),
            1,
            "{case}: {invocation:?}"
        );
        assert_eq!(
            invocation
                .iter()
                .filter(|argument| argument.as_str() == "-Xfix-kotlinc-defects")
                .count(),
            1,
            "{case}: {invocation:?}"
        );
    }

    // A compile left to kotlinc keeps kotlinc's defects, and the check says so.
    {
        let case = "jspecify-free-argument";
        clear_invocation_log(log);
        let output = build()
            .property("krusty.negative", case)
            .property("krusty.fixKotlincDefects", "true")
            .tasks([":compiler:util:compileKotlin"])
            .run_output()
            .unwrap_or_else(|error| panic!("{case} with defect fixes: {error}"));
        assert_eq!(
            output
                .lines()
                .filter(|line| *line
                    == "krusty: task ':compiler:util:compileKotlin' keeps kotlinc's defects: \
                        krusty.fixKotlincDefects applies only to compiles krusty runs")
                .count(),
            1,
            "{case}: {output}"
        );
        assert!(
            !log.exists(),
            "kotlinc, not krusty, compiles a refused argument"
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

    // A Kotlin task with no Kotlin source compiles nothing, as KGP's: its Java sources and Java
    // target are neither compiled nor checked.
    clear_invocation_log(log);
    clear_invocation_log(&fixture.probes);
    build()
        .property("krusty.negative", "java-only-source-set")
        .tasks([":compiler:util:compileJavaOnlyKotlin"])
        .run()
        .unwrap_or_else(|error| panic!("java-only-source-set: {error}"));
    assert!(
        !log.exists(),
        "java-only-source-set: krusty compiles no Java-only source set"
    );

    // KGP compiles against the Kotlin task's own Java toolchain, whose JDK home is no public
    // KGP API: such a compile is kotlinc's, which compiles it against that JDK.
    clear_invocation_log(log);
    clear_invocation_log(&fixture.probes);
    let _ = std::fs::remove_dir_all(kotlin_classes);
    let output = build()
        .property("krusty.negative", "kotlin-java-toolchain")
        .tasks([":compiler:util:compileKotlin"])
        .run_output()
        .unwrap_or_else(|error| panic!("kotlin-java-toolchain: {error}"));
    let gradle_java = output
        .lines()
        .find_map(|line| line.strip_prefix("fixture: Gradle Java "))
        .and_then(|major| major.trim().parse::<u32>().ok())
        .unwrap_or_else(|| panic!("kotlin-java-toolchain: no Gradle Java release\n{output}"));
    assert_left_to_kotlinc(
        &output,
        ":compiler:util:compileKotlin",
        &[format!(
            "Kotlin task Java toolchain {} differs from Gradle Java toolchain {gradle_java}, \
             whose JDK is the only one krusty receives",
            gradle_java - 1,
        )],
    );
    assert!(
        !fixture.probes.exists(),
        "kotlin-java-toolchain: the plugin refuses without asking krusty"
    );
    assert!(
        !log.exists(),
        "kotlin-java-toolchain: kotlinc, not krusty, compiles it"
    );
    assert!(
        kotlin_classes
            .join("org/jetbrains/kotlin/util/UtilKt.class")
            .is_file(),
        "kotlin-java-toolchain: kotlinc wrote the module's classes"
    );

    // A compiler plugin krusty implements only partly no longer leaves the compile to kotlinc: krusty
    // compiles it, with the plugin.
    clear_invocation_log(log);
    clear_invocation_log(&fixture.probes);
    let output = build()
        .property("krusty.negative", "compiler-plugin")
        .tasks([":compiler:util:compileKotlin"])
        .run_output()
        .unwrap_or_else(|error| panic!("compiler-plugin: {error}"));
    assert_assignment_compiled_by_krusty(&output, &fixture.probes, log, "Util.kt");

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

    // A malformed value krusty rejects never fails the build on krusty's word: the support
    // check records krusty's error line, the same as a direct CLI or Bazel-worker invocation
    // reports, and leaves the compile to kotlinc, which rejects the value itself. krusty never
    // compiles. The CLI's own tests pin each malformed value's message.
    {
        let case = "explicit-api-bad-mode";
        let argument = "-Xexplicit-api=sideways";
        let expected = "krusty: error: unknown value for parameter -Xexplicit-api: 'sideways'. \
                        Value should be one of {disable, strict, warning}";
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
            std::fs::read_to_string(refusals_record).expect("refusals record"),
            format!("krusty rejects the command line\n{expected}\n"),
            "{case}: {rendered}",
        );
        assert_eq!(
            task_failure(&rendered, ":compiler:util:compileKotlin"),
            KOTLINC_COMPILATION_FAILURE,
            "{case}: {rendered}",
        );
        assert!(!log.exists(), "krusty never compiles {case}");
        // KGP creates the destination before kotlinc rejects the value; no compiler writes a class.
        assert!(
            !kotlin_classes.exists() || class_files_under(kotlin_classes).is_empty(),
            "{case} wrote to {}",
            kotlin_classes.display()
        );
    }

    // Gradle forwards standard version values without duplicating kotlinc's
    // release-specific version table in the plugin. The shared compiler settings boundary
    // rejects values unavailable in the selected compiler after seeing the exact
    // standard argument.
    {
        let (case, option, kind) = ("old-api-version", "-api-version", "API");
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
            std::fs::read_to_string(refusals_record).expect("refusals record"),
            format!(
                "krusty rejects the command line\nkrusty: error: unknown {kind} version: 1.9\n\
                 Supported {kind} versions: 2.0 (deprecated), 2.1 (deprecated), 2.2, 2.3, 2.4, \
                 2.5 (experimental), 2.6 (experimental)\n"
            ),
            "{case}: {rendered}",
        );
        assert_eq!(
            task_failure(&rendered, ":compiler:util:compileKotlin"),
            if gradle_version()
                .split('.')
                .next()
                .and_then(|major| major.parse::<u32>().ok())
                .is_some_and(|major| major >= 9)
            {
                &KOTLINC_UNSUPPORTED_API_VERSION_PROBLEM[..]
            } else {
                &KOTLINC_COMPILATION_FAILURE[..]
            },
            "{case}: {rendered}",
        );
        assert!(!log.exists(), "krusty never compiles {case}");
        assert!(
            !kotlin_classes.exists() || class_files_under(kotlin_classes).is_empty(),
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

    // `allWarningsAsErrors` reaches krusty as `-Werror`. This module intentionally carries two
    // unresolved opt-in markers, so the compile fails with the complete ordered compiler
    // diagnostic stream. KotlincRequestTest pins the free `-Werror` forms as one policy.
    let diagnostic_log = root.join("krusty-diagnostics.txt");
    let expected_werror_diagnostics = "warning: opt-in requirement marker 'krusty.fixture.ExperimentalFirstApi' is unresolved. Make sure it's present in the module dependencies.\n\
warning: opt-in requirement marker 'krusty.fixture.ExperimentalSecondApi' is unresolved. Make sure it's present in the module dependencies.\n\
error: warnings found and -Werror specified\n";
    {
        let case = "all-warnings-as-errors";
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
}
