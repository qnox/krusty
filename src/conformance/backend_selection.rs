//! Kotlin's ordinary K2 box-suite backend compatibility and mute rules.
//! Target compatibility inherits parent backends; exact exclusions and mutes do not.

fn values<'a>(source: &'a str, directive: &'a str) -> impl Iterator<Item = &'a str> {
    source
        .lines()
        .filter_map(move |line| {
            line.trim()
                .strip_prefix("//")?
                .trim_start()
                .strip_prefix(directive)?
                .strip_prefix(':')
        })
        .flat_map(|payload| {
            payload.split(|character: char| character == ',' || character.is_whitespace())
        })
        .filter(|value| !value.is_empty())
}

// Callers pass the compatible chain from the broad parent to the concrete target.
fn exact_backend<'a>(names: &'a [&str]) -> Option<&'a str> {
    names.last().copied()
}

/// Whether TARGET_BACKEND admits this target or one of its compatible parents.
/// Kotlin stops compatibility recursion before ANY, so TARGET_BACKEND: ANY does not
/// select a concrete backend. Repeated directives contribute to one list.
pub fn backend_targeted(source: &str, names: &[&str]) -> bool {
    let mut targets = values(source, "TARGET_BACKEND").peekable();
    targets.peek().is_none() || targets.any(|target| target != "ANY" && names.contains(&target))
}

/// DONT_TARGET_EXACT_BACKEND matches the concrete backend, without parent inheritance.
pub fn dont_targets_exact_backend(source: &str, names: &[&str]) -> bool {
    values(source, "DONT_TARGET_EXACT_BACKEND").any(|target| Some(target) == exact_backend(names))
}

/// Source compatibility, independent of backend expected-failure mutes.
pub fn target_backend_applicable(source: &str, names: &[&str]) -> bool {
    backend_targeted(source, names) && !dont_targets_exact_backend(source, names)
}

/// Ordinary K2 box-suite mutes match the concrete target or ANY.
/// K1 and special output-multimodule suites have separate ignore directives. A source
/// MODULE declaration does not select that special output-multimodule suite.
pub fn backend_muted(source: &str, names: &[&str]) -> bool {
    ["IGNORE_BACKEND", "IGNORE_BACKEND_K2"]
        .iter()
        .any(|directive| {
            values(source, directive)
                .any(|target| target == "ANY" || Some(target) == exact_backend(names))
        })
        || dont_targets_exact_backend(source, names)
}

pub fn backend_applicable(source: &str, names: &[&str]) -> bool {
    backend_targeted(source, names) && !backend_muted(source, names)
}

/// Target-invariant relevance checks from Kotlin's Native codegen-box provider.
/// Cache, GC and test-mode conditions belong to their corresponding execution configurations.
pub fn native_box_configuration_applicable(source: &str) -> bool {
    !["FULL_JDK", "JVM_TARGET", "DIAGNOSTICS"]
        .iter()
        .any(|name| super::directive(source, name))
        && !values(source, "API_VERSION").any(|version| version == "1.4")
        && !values(source, "LANGUAGE_VERSION").any(|version| matches!(version, "1.3" | "1.4"))
        && !source
            .lines()
            .any(|line| line.trim() == "// DISABLE_NATIVE")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_disable_without_conditions_is_unconditional() {
        assert!(!native_box_configuration_applicable(
            "// DISABLE_NATIVE\nfun box() = \"OK\""
        ));
        assert!(native_box_configuration_applicable(
            "// DISABLE_NATIVE: gcScheduler=AGGRESSIVE\nfun box() = \"OK\""
        ));
    }

    #[test]
    fn repeated_targets_and_whitespace_form_one_backend_list() {
        assert!(backend_applicable(
            " // TARGET_BACKEND: NATIVE\n // TARGET_BACKEND: JVM_IR WASM_JS",
            &["JVM", "JVM_IR"]
        ));
        assert!(!backend_applicable(
            " // TARGET_BACKEND: JVM\n // DONT_TARGET_EXACT_BACKEND: JVM_IR",
            &["JVM", "JVM_IR"]
        ));
    }
}
