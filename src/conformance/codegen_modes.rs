//! The JVM code-generation modes one box-test compilation unit selects.
//!
//! `// LAMBDAS:`, `// SAM_CONVERSIONS:`, and `// JVM_DEFAULT_MODE:` change which classes a unit
//! emits (lambda and SAM-wrapper classes, `$DefaultImpls` holders), so the box gate's krusty
//! compile and the reference-compiler oracle must select them from the same sources with the same
//! precedence. Both select through [`UnitCodegenModes::of_unit`]: a mode is read from the Kotlin
//! sources of the unit being compiled, never broadcast from one unit's directive to another.

use crate::jvm::ir_emit::{JvmDefaultMode, LambdaMode, LambdaModes};

use super::{
    jvm_default_mode, lambda_mode, needs_unmodeled_jvm_default_mode, needs_unmodeled_lambda_mode,
    needs_unmodeled_sam_conversion_mode, sam_conversion_mode,
};

/// The code-generation modes of one compilation unit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UnitCodegenModes {
    pub jvm_default: JvmDefaultMode,
    pub lambdas: LambdaModes,
}

impl UnitCodegenModes {
    /// The modes a unit compiles under, from its own Kotlin sources in compilation order. Within one
    /// source the last recognized directive counts; across sources, the first source that pins a
    /// non-default value selects that mode. Absent any, the unit keeps kotlinc's defaults.
    pub fn of_unit<'a>(sources: impl IntoIterator<Item = &'a str>) -> Self {
        Self::of_unit_in_case("", sources)
    }

    /// The modes a unit of a case compiles under. Each kind of mode the unit's own sources select
    /// as [`Self::of_unit`] does, whenever any of them carries that kind's directive, even one
    /// naming kotlinc's default; a kind none of them mentions takes `case_region`'s, the directives
    /// Kotlin's test infrastructure applies to every module.
    pub fn of_unit_in_case<'a>(
        case_region: &str,
        sources: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        let sources: Vec<&str> = sources.into_iter().collect();
        UnitCodegenModes {
            jvm_default: unit_mode(case_region, &sources, jvm_default_mode),
            lambdas: LambdaModes {
                lambdas: unit_mode(case_region, &sources, lambda_mode),
                sam_conversions: unit_mode(case_region, &sources, sam_conversion_mode),
            },
        }
    }

    /// The reference `kotlinc` arguments that select these modes. A default mode adds no argument,
    /// so a unit without a pinned mode compiles with exactly kotlinc's default invocation.
    pub fn kotlinc_args(self) -> Vec<String> {
        let mut args = Vec::new();
        let jvm_default = match self.jvm_default {
            JvmDefaultMode::Enable => None,
            JvmDefaultMode::NoCompatibility => Some("no-compatibility"),
            JvmDefaultMode::Disable => Some("disable"),
        };
        if let Some(mode) = jvm_default {
            args.extend(["-jvm-default".to_string(), mode.to_string()]);
        }
        if self.lambdas.lambdas == LambdaMode::Class {
            args.push("-Xlambdas=class".into());
        }
        if self.lambdas.sam_conversions == LambdaMode::Class {
            args.push("-Xsam-conversions=class".into());
        }
        args
    }
}

fn unit_mode<T: Copy + Default + PartialEq>(
    case_region: &str,
    sources: &[&str],
    mode: impl Fn(&str) -> Option<T>,
) -> T {
    let pinned: Vec<T> = sources.iter().filter_map(|source| mode(source)).collect();
    if pinned.is_empty() {
        return mode(case_region).unwrap_or_default();
    }
    pinned
        .into_iter()
        .find(|selected| *selected != T::default())
        .unwrap_or_default()
}

/// The first code-generation mode directive whose value names no known mode, trimmed. A unit whose
/// mode cannot be read must not be compiled under a guessed default.
pub fn unsupported_codegen_mode_directive(src: &str) -> Option<&str> {
    src.lines().map(str::trim).find(|line| {
        needs_unmodeled_lambda_mode(line)
            || needs_unmodeled_sam_conversion_mode(line)
            || needs_unmodeled_jvm_default_mode(line)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_unit_without_directives_keeps_kotlincs_defaults() {
        let modes = UnitCodegenModes::of_unit(["fun box() = \"OK\"\n"]);
        assert_eq!(modes, UnitCodegenModes::default());
        assert_eq!(modes.kotlinc_args(), Vec::<String>::new());
    }

    #[test]
    fn each_pinned_mode_maps_to_its_kotlinc_spelling() {
        let modes = UnitCodegenModes::of_unit([
            "// LAMBDAS: CLASS\n// SAM_CONVERSIONS: CLASS\n// JVM_DEFAULT_MODE: no-compatibility\n",
        ]);
        assert_eq!(
            modes,
            UnitCodegenModes {
                jvm_default: JvmDefaultMode::NoCompatibility,
                lambdas: LambdaModes {
                    lambdas: LambdaMode::Class,
                    sam_conversions: LambdaMode::Class,
                },
            }
        );
        assert_eq!(
            modes.kotlinc_args(),
            vec![
                "-jvm-default".to_string(),
                "no-compatibility".to_string(),
                "-Xlambdas=class".to_string(),
                "-Xsam-conversions=class".to_string(),
            ]
        );
        assert_eq!(
            UnitCodegenModes::of_unit(["// JVM_DEFAULT_MODE: disable\n"]).kotlinc_args(),
            vec!["-jvm-default".to_string(), "disable".to_string()]
        );
    }

    #[test]
    fn an_explicit_default_mode_adds_no_argument() {
        let modes = UnitCodegenModes::of_unit([
            "// LAMBDAS: INDY\n// SAM_CONVERSIONS: INDY\n// JVM_DEFAULT_MODE: enable\n",
        ]);
        assert_eq!(modes, UnitCodegenModes::default());
        assert_eq!(modes.kotlinc_args(), Vec::<String>::new());
    }

    #[test]
    fn the_first_source_pinning_a_mode_selects_it_for_the_unit() {
        let modes = UnitCodegenModes::of_unit([
            "fun a() {}\n",
            "// JVM_DEFAULT_MODE: disable\n// LAMBDAS: CLASS\n",
            "// JVM_DEFAULT_MODE: no-compatibility\n",
        ]);
        assert_eq!(modes.jvm_default, JvmDefaultMode::Disable);
        assert_eq!(modes.lambdas.lambdas, LambdaMode::Class);
        assert_eq!(modes.lambdas.sam_conversions, LambdaMode::Indy);
    }

    #[test]
    fn an_unrecognized_mode_value_is_reported() {
        assert_eq!(
            unsupported_codegen_mode_directive("fun box() = \"OK\"\n// LAMBDAS: CLASS\n"),
            None
        );
        assert_eq!(
            unsupported_codegen_mode_directive("// LAMBDAS: sideways\nfun box() = \"OK\"\n"),
            Some("// LAMBDAS: sideways")
        );
        assert_eq!(
            unsupported_codegen_mode_directive("// SAM_CONVERSIONS:\n"),
            Some("// SAM_CONVERSIONS:")
        );
        assert_eq!(
            unsupported_codegen_mode_directive(
                "// MODULE: lib\n  // JVM_DEFAULT_MODE: all\n// FILE: a.kt\n"
            ),
            Some("// JVM_DEFAULT_MODE: all")
        );
    }
}
