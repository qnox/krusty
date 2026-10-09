//! The kotlinc command line a box test's directives select.
//!
//! Kotlin's test infrastructure configures each test's compiler from directives in the test source.
//! The box gate compiles every unit twice, with krusty and with the reference kotlinc, and the two
//! compilations must be configured identically. [`unit_kotlinc_arguments`] is the one translation
//! from directives to kotlinc arguments: the reference compiler receives the arguments verbatim and
//! krusty reads them through its kotlinc-compatible command line, so a directive krusty does not
//! implement is refused exactly as the same argument would be on a real build.

use super::{directive, UnitCodegenModes};

/// The kotlinc arguments one compilation unit of a case compiles under: the case-wide arguments of
/// [`case_kotlinc_arguments`], then the code-generation modes the unit's own Kotlin sources select
/// ([`UnitCodegenModes::of_unit`]).
pub fn unit_kotlinc_arguments<'a>(
    case: &str,
    unit: impl IntoIterator<Item = &'a str>,
) -> Result<Vec<String>, String> {
    let mut arguments = case_kotlinc_arguments(case)?;
    arguments.extend(UnitCodegenModes::of_unit(unit).kotlinc_args());
    Ok(arguments)
}

/// The kotlinc arguments the case-wide directives of `src` select, in directive order per kind:
///
/// | directive | argument |
/// |---|---|
/// | `// LANGUAGE: +A -B` | `-XXLanguage:+A`, `-XXLanguage:-B` |
/// | `// API_VERSION: X` | `-api-version X` (see below) |
/// | `// OPT_IN: a.B, c.D` | `-opt-in=a.B`, `-opt-in=c.D` |
/// | `// EXPLICIT_API_MODE: STRICT` | `-Xexplicit-api=strict` |
/// | `// ALLOW_KOTLIN_PACKAGE`, or a `package kotlin…` declaration | `-Xallow-kotlin-package` |
/// | `// JVM_TARGET: X` | `-jvm-target X` |
/// | `// STRING_CONCAT: X` | `-Xstring-concat=X` |
/// | `// ASSERTIONS_MODE: X` | `-Xassertions=X` |
/// | `// WHEN_EXPRESSIONS: X` | `-Xwhen-expressions=x` |
///
/// The per-unit `// LAMBDAS:`, `// SAM_CONVERSIONS:` and `// JVM_DEFAULT_MODE:` directives belong
/// to [`unit_kotlinc_arguments`]; a value of one that names no mode fails closed here, so no unit
/// compiles under a guessed default.
///
/// An `// API_VERSION:` below 2.0 cannot be represented by kotlinc 2.4's public command line:
/// Kotlin's own test runner installs it directly in the compiler configuration. Refuse that case
/// here until the differential harness has the same typed configuration channel; silently omitting
/// it would compile a different program at the default API level.
///
/// A case declaring a type in the reserved `kotlin` package needs `-Xallow-kotlin-package` whether
/// or not it says so: several corpus cases declare one without the directive, and kotlinc rejects
/// them without the argument.
pub fn case_kotlinc_arguments(src: &str) -> Result<Vec<String>, String> {
    if let Some(directive) = super::unsupported_codegen_mode_directive(src) {
        return Err(format!(
            "box directive `{directive}` names no code-generation mode"
        ));
    }
    let mut arguments: Vec<String> = values(src, "LANGUAGE")
        .flat_map(|payload| payload.split([' ', ',', '\t']))
        .filter(|token| !token.is_empty())
        .map(|token| format!("-XXLanguage:{token}"))
        .collect();
    if let Some(version) = last_value(src, "API_VERSION") {
        if predates_kotlin_2(version) {
            return Err(format!(
                "box directive `// API_VERSION: {version}` requires the typed compiler-configuration channel"
            ));
        }
        arguments.extend(["-api-version".to_string(), version.to_string()]);
    }
    arguments.extend(
        values(src, "OPT_IN")
            .flat_map(|payload| payload.split([' ', ',', '\t']))
            .filter(|marker| !marker.is_empty())
            .map(|marker| format!("-opt-in={marker}")),
    );
    if let Some(mode) = last_value(src, "EXPLICIT_API_MODE") {
        arguments.push(format!("-Xexplicit-api={}", mode.to_ascii_lowercase()));
    }
    if directive(src, "ALLOW_KOTLIN_PACKAGE") || declares_reserved_kotlin_package(src) {
        arguments.push("-Xallow-kotlin-package".to_string());
    }
    if let Some(target) = last_value(src, "JVM_TARGET") {
        arguments.extend(["-jvm-target".to_string(), target.to_string()]);
    }
    if let Some(mode) = last_value(src, "STRING_CONCAT") {
        arguments.push(format!("-Xstring-concat={mode}"));
    }
    if let Some(mode) = last_value(src, "ASSERTIONS_MODE") {
        arguments.push(format!("-Xassertions={mode}"));
    }
    if let Some(mode) = last_value(src, "WHEN_EXPRESSIONS") {
        arguments.push(format!("-Xwhen-expressions={}", mode.to_ascii_lowercase()));
    }
    Ok(arguments)
}

/// The kotlinc arguments only the reference compile of a case receives: `// RETURN_VALUE_CHECKER_MODE:
/// FULL|CHECKER|DISABLED` selects `-Xreturn-value-checker=full|check|disable`, so kotlinc accepts the
/// case's `@MustUseReturnValues`/`@IgnorableReturnValue` annotations.
///
/// krusty does not model the return-value checker and refuses the argument, yet the checker only
/// reports unused results and never changes the case's runtime `box()`. Keeping the argument off
/// krusty's command line keeps such a case applicable; it moves to [`case_kotlinc_arguments`] once
/// krusty applies the argument. An unknown mode fails closed rather than reference-compiling under
/// a guessed default.
pub fn reference_only_kotlinc_arguments(src: &str) -> Result<Vec<String>, String> {
    let Some(mode) = last_value(src, "RETURN_VALUE_CHECKER_MODE") else {
        return Ok(Vec::new());
    };
    // The directive spells kotlinc's `ReturnValueCheckerMode` enum constants.
    let value = match mode {
        "FULL" => "full",
        "CHECKER" => "check",
        "DISABLED" => "disable",
        unknown => {
            return Err(format!(
                "box directive `// RETURN_VALUE_CHECKER_MODE: {unknown}` names no mode"
            ))
        }
    };
    Ok(vec![format!("-Xreturn-value-checker={value}")])
}

/// The trimmed payload of every `// <name>:` directive line, in source order.
fn values<'a>(src: &'a str, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
    src.lines().filter_map(move |line| {
        line.trim_start()
            .strip_prefix("// ")?
            .strip_prefix(name)?
            .strip_prefix(':')
            .map(str::trim)
    })
}

/// The first word of the last `// <name>:` directive's payload: Kotlin's test runner keeps the
/// last value of a single-valued directive.
fn last_value<'a>(src: &'a str, name: &'a str) -> Option<&'a str> {
    values(src, name)
        .filter_map(|payload| payload.split_whitespace().next())
        .last()
}

/// Whether a `major.minor` version names a Kotlin 1.x release.
fn predates_kotlin_2(version: &str) -> bool {
    version
        .split_once('.')
        .and_then(|(major, _)| major.parse::<u32>().ok())
        .is_some_and(|major| major < 2)
}

/// Whether any source file declares the top-level package `kotlin` or a `kotlin.*` subpackage,
/// which kotlinc accepts only under `-Xallow-kotlin-package`.
fn declares_reserved_kotlin_package(src: &str) -> bool {
    src.lines().any(|line| {
        line.trim_start()
            .strip_prefix("package ")
            .map(str::trim)
            .is_some_and(|package| package == "kotlin" || package.starts_with("kotlin."))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(src: &str) -> Vec<String> {
        case_kotlinc_arguments(src).expect("the directives map to arguments")
    }

    #[test]
    fn a_case_without_directives_compiles_with_kotlincs_defaults() {
        assert_eq!(arguments("fun box() = \"OK\"\n"), Vec::<String>::new());
    }

    #[test]
    fn every_case_wide_directive_maps_to_its_kotlinc_argument() {
        let src = "// LANGUAGE: +ContextParameters -SomethingElse\n\
                   // LANGUAGE: +MultiPlatformProjects\n\
                   // API_VERSION: 2.3\n\
                   // OPT_IN: kotlin.ExperimentalMultiplatform, kotlin.contracts.ExperimentalContracts\n\
                   // EXPLICIT_API_MODE: STRICT\n\
                   // ALLOW_KOTLIN_PACKAGE\n\
                   // RETURN_VALUE_CHECKER_MODE: CHECKER\n\
                   // JVM_TARGET: 1.8\n\
                   // STRING_CONCAT: inline\n\
                   // ASSERTIONS_MODE: always-enable\n\
                   // WHEN_EXPRESSIONS: INDY\n\
                   fun box() = \"OK\"\n";
        assert_eq!(
            arguments(src),
            [
                "-XXLanguage:+ContextParameters",
                "-XXLanguage:-SomethingElse",
                "-XXLanguage:+MultiPlatformProjects",
                "-api-version",
                "2.3",
                "-opt-in=kotlin.ExperimentalMultiplatform",
                "-opt-in=kotlin.contracts.ExperimentalContracts",
                "-Xexplicit-api=strict",
                "-Xallow-kotlin-package",
                "-jvm-target",
                "1.8",
                "-Xstring-concat=inline",
                "-Xassertions=always-enable",
                "-Xwhen-expressions=indy",
            ]
        );
    }

    #[test]
    fn the_return_value_checker_mode_reaches_only_the_reference_compile() {
        let src = "// RETURN_VALUE_CHECKER_MODE: CHECKER\nfun box() = \"OK\"\n";
        assert_eq!(arguments(src), Vec::<String>::new());
        for (mode, argument) in [
            ("FULL", "-Xreturn-value-checker=full"),
            ("CHECKER", "-Xreturn-value-checker=check"),
            ("DISABLED", "-Xreturn-value-checker=disable"),
        ] {
            assert_eq!(
                reference_only_kotlinc_arguments(&format!(
                    "// RETURN_VALUE_CHECKER_MODE: {mode}\nfun box() = \"OK\"\n"
                )),
                Ok(vec![argument.to_string()])
            );
        }
        assert_eq!(
            reference_only_kotlinc_arguments("fun box() = \"OK\"\n"),
            Ok(Vec::new())
        );
    }

    #[test]
    fn a_kotlin_1_api_version_fails_closed_instead_of_selecting_the_default_api() {
        assert_eq!(
            case_kotlinc_arguments("// API_VERSION: 1.9\nfun box() = \"OK\"\n"),
            Err(
                "box directive `// API_VERSION: 1.9` requires the typed compiler-configuration channel"
                    .to_string()
            )
        );
        assert_eq!(
            case_kotlinc_arguments("// API_VERSION: 1.3\nfun box() = \"OK\"\n"),
            Err(
                "box directive `// API_VERSION: 1.3` requires the typed compiler-configuration channel"
                    .to_string()
            )
        );
    }

    #[test]
    fn a_kotlin_package_declaration_needs_the_allow_argument_without_the_directive() {
        assert_eq!(
            arguments("package kotlin.collections\nfun box() = \"OK\"\n"),
            ["-Xallow-kotlin-package"]
        );
        assert_eq!(
            arguments("package kotlinx.foo\nfun box() = \"OK\"\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn the_last_value_of_a_single_valued_directive_counts() {
        assert_eq!(
            arguments("// JVM_TARGET: 1.8\n// MODULE: lib\n// JVM_TARGET: 11\n"),
            ["-jvm-target", "11"]
        );
    }

    #[test]
    fn an_unknown_mode_fails_closed() {
        assert_eq!(
            reference_only_kotlinc_arguments("// RETURN_VALUE_CHECKER_MODE: SOMETIMES\n"),
            Err(
                "box directive `// RETURN_VALUE_CHECKER_MODE: SOMETIMES` names no mode".to_string()
            )
        );
        assert_eq!(
            case_kotlinc_arguments("// LAMBDAS: sideways\nfun box() = \"OK\"\n"),
            Err("box directive `// LAMBDAS: sideways` names no code-generation mode".to_string())
        );
    }

    #[test]
    fn a_unit_appends_the_modes_its_own_sources_select() {
        let case = "// LANGUAGE: +ContextParameters\n// MODULE: lib\n// LAMBDAS: CLASS\n";
        assert_eq!(
            unit_kotlinc_arguments(case, ["// LAMBDAS: CLASS\nfun a() {}\n"]).unwrap(),
            ["-XXLanguage:+ContextParameters", "-Xlambdas=class"]
        );
        assert_eq!(
            unit_kotlinc_arguments(case, ["fun b() {}\n"]).unwrap(),
            ["-XXLanguage:+ContextParameters"]
        );
    }
}
