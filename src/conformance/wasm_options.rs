//! Per-compilation Wasm options shared by the product and reference test invocations.

use crate::compilation_target::CompilationTarget;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WasmCompilerOptions {
    /// `None` keeps the selected release's default; the directive explicitly enables checks.
    array_range_checks: Option<bool>,
}

impl WasmCompilerOptions {
    /// Read the original case before FILE/MODULE splitting. Carry this same configuration to
    /// the reference compiler through its extra-arguments boundary. A split file may still hold
    /// the directive line as leading text, but that copy is a comment: it is never re-read.
    pub fn from_source(source: &str) -> Result<Self, String> {
        const DIRECTIVE: &str = "WASM_ENABLE_ARRAY_RANGE_CHECKS";
        let mut options = Self::default();
        for content in source
            .lines()
            .filter_map(|line| line.trim().strip_prefix("//"))
        {
            let content = content.trim();
            if content.split([' ', ':', '\t']).next() != Some(DIRECTIVE) {
                continue;
            }
            if content != DIRECTIVE {
                return Err(format!("box directive `// {content}` takes no value"));
            }
            options.array_range_checks = Some(true);
        }
        Ok(options)
    }

    pub fn array_range_checks(self) -> Option<bool> {
        self.array_range_checks
    }

    pub fn reference_arguments(self, target: CompilationTarget) -> &'static [&'static str] {
        match (target, self.array_range_checks) {
            (CompilationTarget::WasmJs | CompilationTarget::WasmWasi, Some(true)) => {
                &["-Xwasm-enable-array-range-checks"]
            }
            _ => &[],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_options_preserve_release_defaults_and_all_reference_arguments() {
        let options = WasmCompilerOptions::from_source("fun box() = \"OK\"\n").unwrap();
        assert_eq!(options.array_range_checks(), None);
        for target in [
            CompilationTarget::Jvm,
            CompilationTarget::Native,
            CompilationTarget::Js,
            CompilationTarget::WasmJs,
            CompilationTarget::WasmWasi,
        ] {
            assert_eq!(options.reference_arguments(target), &[] as &[&str]);
        }
    }

    #[test]
    fn one_written_option_selects_product_checks_and_only_wasm_reference_flags() {
        let options = WasmCompilerOptions::from_source(
            "// WASM_ENABLE_ARRAY_RANGE_CHECKS\nfun box() = \"OK\"\n",
        )
        .unwrap();
        assert_eq!(options.array_range_checks(), Some(true));
        for target in [CompilationTarget::WasmJs, CompilationTarget::WasmWasi] {
            assert_eq!(
                options.reference_arguments(target),
                ["-Xwasm-enable-array-range-checks"]
            );
        }
        for target in [
            CompilationTarget::Jvm,
            CompilationTarget::Native,
            CompilationTarget::Js,
        ] {
            assert_eq!(options.reference_arguments(target), &[] as &[&str]);
        }
    }

    #[test]
    fn a_value_on_the_enable_directive_is_rejected() {
        assert_eq!(
            WasmCompilerOptions::from_source("// WASM_ENABLE_ARRAY_RANGE_CHECKS: false\n"),
            Err(
                "box directive `// WASM_ENABLE_ARRAY_RANGE_CHECKS: false` takes no value"
                    .to_owned()
            )
        );
    }
}
