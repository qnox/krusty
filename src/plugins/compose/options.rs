//! The Compose compiler plugin's command-line options, as `ComposeCommandLineProcessor` declares
//! and `ComposePluginRegistrar` defaults them.

use crate::plugins::cli::PluginOption;

/// The options krusty's Compose pass honours. Every other declared option is accepted only at the
/// setting that leaves the generated code as krusty emits it (see [`ComposeOptions::UNIMPLEMENTED`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComposeOptions {
    /// `sourceInformation`: call `sourceInformation` with each group's source locations. kotlinc
    /// leaves it off unless the option says `true`.
    pub source_information: bool,
    /// `traceMarkersEnabled`: bracket each restartable body with composition trace events. On
    /// unless the option says otherwise.
    pub trace_markers: bool,
    /// `generateFunctionKeyMetaAnnotations`: annotate each composable function with its group key.
    /// Without the option kotlinc annotates whenever the runtime's `FunctionKeyMeta` is not
    /// retained at run time, which it is not in any runtime that declares it with binary retention.
    pub function_key_meta: bool,
}

impl ComposeOptions {
    /// Every key `ComposeCommandLineProcessor` declares.
    pub const KEYS: &'static [&'static str] = &[
        "liveLiterals",
        "liveLiteralsEnabled",
        "generateFunctionKeyMetaClasses",
        "generateFunctionKeyMetaAnnotations",
        "sourceInformation",
        "metricsDestination",
        "reportsDestination",
        "featureFlag",
        "intrinsicRemember",
        "nonSkippingGroupOptimization",
        "suppressKotlinVersionCompatibilityCheck",
        "generateDecoys",
        "strongSkipping",
        "experimentalStrongSkipping",
        "stabilityConfigurationPath",
        "traceMarkersEnabled",
        "skipIrLoweringIfRuntimeNotFound",
        "targetRuntimeVersion",
    ];

    /// Settings that change the generated code in a way krusty's pass does not implement: live
    /// literals, function key meta classes, decoys (which kotlinc itself rejects), and turning off
    /// one of the feature flags kotlinc enables by default.
    pub const UNIMPLEMENTED: &'static [(&'static str, &'static str)] = &[
        ("liveLiterals", "true"),
        ("liveLiteralsEnabled", "true"),
        ("generateFunctionKeyMetaClasses", "true"),
        ("generateDecoys", "true"),
        ("intrinsicRemember", "false"),
        ("nonSkippingGroupOptimization", "false"),
        ("strongSkipping", "false"),
        ("experimentalStrongSkipping", "false"),
        ("featureFlag", "-StrongSkipping"),
        ("featureFlag", "-IntrinsicRemember"),
        ("featureFlag", "-OptimizeNonSkippingGroups"),
        ("featureFlag", "-PausableComposition"),
    ];

    /// Configure from the options the compilation gives Compose, in either syntax. A later setting
    /// of a key replaces an earlier one, as kotlinc's configuration does.
    pub fn from_options(options: &[PluginOption]) -> Self {
        let mut configured = Self {
            source_information: false,
            trace_markers: true,
            function_key_meta: true,
        };
        for option in options {
            let enabled = option.value == "true";
            match option.key.as_str() {
                "sourceInformation" => configured.source_information = enabled,
                "traceMarkersEnabled" => configured.trace_markers = enabled,
                "generateFunctionKeyMetaAnnotations" => configured.function_key_meta = enabled,
                _ => {}
            }
        }
        configured
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn option(key: &str, value: &str) -> PluginOption {
        PluginOption {
            id: crate::plugins::cli::COMPOSE_PLUGIN_ID.to_string(),
            key: key.to_string(),
            value: value.to_string(),
        }
    }

    #[test]
    fn defaults_match_the_registrar() {
        assert_eq!(
            ComposeOptions::from_options(&[]),
            ComposeOptions {
                source_information: false,
                trace_markers: true,
                function_key_meta: true,
            }
        );
    }

    #[test]
    fn each_honoured_option_reads_true_only_from_the_exact_spelling() {
        let configured = ComposeOptions::from_options(&[
            option("sourceInformation", "true"),
            option("traceMarkersEnabled", "TRUE"),
            option("generateFunctionKeyMetaAnnotations", "false"),
        ]);
        assert_eq!(
            configured,
            ComposeOptions {
                source_information: true,
                trace_markers: false,
                function_key_meta: false,
            }
        );
    }
}
