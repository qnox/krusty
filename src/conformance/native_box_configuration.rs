//! Kotlin's Native codegen-box configuration rules: the target-invariant directives that keep a
//! generator-selected case out of every Linux Native execution configuration.

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

/// Kotlin's CInteropUtils predicate. The separator ends definition properties and starts C code.
pub fn native_def_contents_supported(source: &str, is_apple: bool) -> bool {
    if is_apple {
        return true;
    }
    for line in source.lines() {
        if line.starts_with("---") {
            return true;
        }
        let parts = line.split('=').collect::<Vec<_>>();
        if parts.len() == 2
            && parts[0].trim().eq_ignore_ascii_case("language")
            && parts[1].trim().eq_ignore_ascii_case("Objective-C")
        {
            return false;
        }
    }
    true
}

// Krusty's executable Native lane targets Linux; only FILE blocks ending in .def are properties.
fn native_def_files_supported(source: &str) -> bool {
    let mut definition = String::new();
    let mut in_def = false;
    for line in source.lines() {
        let file = super::directive_payload(line, "FILE");
        if file.is_some() || super::directive_payload(line, "MODULE").is_some() {
            if in_def && !native_def_contents_supported(&definition, false) {
                return false;
            }
            definition.clear();
            in_def = file.is_some_and(|file| file.trim().ends_with(".def"));
        } else if in_def {
            definition.push_str(line);
            definition.push('\n');
        }
    }
    !in_def || native_def_contents_supported(&definition, false)
}

// Target properties are invariant across the Linux Native execution configurations. A mode-
// dependent clause cannot remove a test from this shared inventory of those configurations.
fn native_target_property(property: &str) -> Option<&'static str> {
    match property {
        "isAppleTarget" | "targetSupportsCoreSymbolication" => Some("false"),
        "targetFamily" => Some("LINUX"),
        "targetArchitecture" => match std::env::consts::ARCH {
            "x86_64" => Some("X64"),
            "aarch64" => Some("ARM64"),
            _ => None,
        },
        "target" => match std::env::consts::ARCH {
            "x86_64" => Some("linux_x64"),
            "aarch64" => Some("linux_arm64"),
            _ => None,
        },
        _ => None,
    }
}

fn native_target_directive(source: &str, directive: &str) -> bool {
    source.lines().any(|line| {
        let Some(tail) = line
            .trim()
            .strip_prefix("//")
            .map(str::trim_start)
            .and_then(|line| line.strip_prefix(directive))
        else {
            return false;
        };
        if tail.is_empty() || tail.trim() == ":" {
            return true;
        }
        let Some(payload) = tail.strip_prefix(':') else {
            return false;
        };
        payload.split("&&").all(|condition| {
            let Some((property, expected)) = condition.trim().split_once('=') else {
                return false;
            };
            native_target_property(property.trim()).is_some_and(|actual| actual == expected.trim())
        })
    })
}

/// Target-invariant relevance checks from Kotlin's Native codegen-box provider.
/// Cache, GC and test-mode conditions belong to their corresponding execution configurations.
pub fn native_box_configuration_applicable(source: &str) -> bool {
    !["FULL_JDK", "JVM_TARGET", "DIAGNOSTICS"]
        .iter()
        .any(|name| super::directive(source, name))
        && !values(source, "API_VERSION").any(|version| version == "1.4")
        && !values(source, "LANGUAGE_VERSION").any(|version| matches!(version, "1.3" | "1.4"))
        && native_def_files_supported(source)
        && !native_target_directive(source, "DISABLE_NATIVE")
        && !native_target_directive(source, "IGNORE_NATIVE")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_def_selection_stops_at_file_boundaries_and_inline_c() {
        assert!(!native_box_configuration_applicable(
            "// MODULE: cinterop\n// FILE: interop.def\nlanguage = Objective-C\n// MODULE: main(cinterop)\nfun box() = \"OK\""
        ));
        assert!(native_box_configuration_applicable(
            "// FILE: test.kt\nval language = \"Objective-C\"\n// FILE: interop.def\nlanguage = C\n---\nlanguage = Objective-C"
        ));
        assert!(native_box_configuration_applicable(
            "// FILE: interop.def\nheaders = Test.h\n// FILE: Test.h\nlanguage = Objective-C"
        ));
    }

    #[test]
    fn native_disable_without_conditions_is_unconditional() {
        assert!(!native_box_configuration_applicable(
            "// DISABLE_NATIVE\nfun box() = \"OK\""
        ));
        assert!(native_box_configuration_applicable(
            "// DISABLE_NATIVE: gcScheduler=AGGRESSIVE\nfun box() = \"OK\""
        ));
    }
}
