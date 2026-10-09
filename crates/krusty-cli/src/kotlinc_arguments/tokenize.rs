//! kotlinc's command-line parser (`parsePreprocessedCommandLineArguments`), driven by a release's
//! [`Catalog`]. It decides which tokens are arguments, which are sources, what value each argument
//! takes, and every syntax-level error and warning kotlinc reports, in kotlinc's order and words.
//! What an argument *means* is applied by the caller.

use super::catalog::{ArgumentSpec, Catalog, ValueKind};

/// One argument occurrence, in command-line order.
#[derive(Clone, Debug, PartialEq)]
pub struct Occurrence<'c> {
    pub spec: &'c ArgumentSpec,
    pub value: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Bool(bool),
    String(String),
    /// The elements this occurrence contributes to an array argument, already split.
    List(Vec<String>),
}

/// What kotlinc records as an argument's explicit values (`CommonToolArguments.explicitArguments`):
/// one entry per occurrence for a scalar, and for an array one list that every occurrence extends.
#[derive(Clone, Debug, PartialEq)]
pub enum Recorded {
    Scalars(Vec<String>),
    List(Vec<String>),
}

/// The syntax problems kotlinc collects in `ArgumentParseErrors`, kept as its message texts.
#[derive(Clone, Debug, Default, PartialEq)]
struct Problems {
    unknown_args: Vec<String>,
    unknown_extra_flags: Vec<String>,
    obsolete_form: Vec<String>,
    /// Deprecated spelling to canonical name, first-use order, each spelling once.
    deprecated: Vec<(String, String)>,
    without_value: Vec<String>,
    bad_boolean: Vec<String>,
    feature_boolean_with_value: Vec<String>,
    bad_feature_value: Vec<(String, Vec<String>)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tokenized<'c> {
    pub occurrences: Vec<Occurrence<'c>>,
    /// Arguments in first-occurrence order with their recorded values.
    pub explicit: Vec<(&'c ArgumentSpec, Recorded)>,
    /// Sources: every non-option token, and every token after `--`.
    pub free: Vec<String>,
    problems: Problems,
    argfile_problems: Vec<String>,
}

impl<'c> Tokenized<'c> {
    /// kotlinc's `validateArgumentsAllErrors`: any one stops the invocation before compiling.
    pub fn errors(&self) -> Vec<String> {
        let problems = &self.problems;
        let mut errors = Vec::new();
        for argument in &problems.without_value {
            errors.push(format!("No value passed for argument {argument}"));
        }
        for argument in &problems.bad_boolean {
            errors.push(format!(
                "Incorrect value for boolean argument '{}'. Only 'true' and 'false' are allowed.",
                before(argument, '=')
            ));
        }
        for argument in &problems.feature_boolean_with_value {
            errors.push(format!(
                "No value is expected for argument '{}'.",
                before(argument, '=')
            ));
        }
        for (argument, allowed) in &problems.bad_feature_value {
            let mut parts = argument.split('=');
            let name = parts.next().unwrap_or_default();
            let value = parts.next().unwrap_or_default();
            let allowed = allowed
                .iter()
                .map(|value| format!("'{value}'"))
                .collect::<Vec<_>>()
                .join(", ");
            errors.push(format!(
                "Incorrect value for argument '{name}'. Actual value: '{value}', but allowed values: {allowed}."
            ));
        }
        for argument in &problems.unknown_args {
            errors.push(format!("Invalid argument: {argument}"));
        }
        errors
    }

    /// The warnings of kotlinc's `reportArgumentParseProblems`, in its order. The unsafe
    /// `-XXLanguage` notice and language-feature name checks need kotlinc's feature table and are
    /// not part of this list.
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        for (spec, recorded) in &self.explicit {
            let Recorded::Scalars(values) = recorded else {
                continue;
            };
            let first = &values[0];
            if values.len() > 1 && values.iter().any(|value| value != first) {
                warnings.push(format!(
                    "Argument '{}' is passed multiple times: '{}'. The last value will be used.",
                    spec.name,
                    values.join("', '")
                ));
            }
        }
        let problems = &self.problems;
        for flag in &problems.unknown_extra_flags {
            warnings.push(format!(
                "Flag is not supported by this version of the compiler: {flag}"
            ));
        }
        for argument in &problems.obsolete_form {
            warnings.push(format!(
                "Advanced option value is passed in an obsolete form. Please use the '=' character to specify the value: {argument}=..."
            ));
        }
        warnings.extend(self.deprecations());
        warnings.extend(self.argfile_problems.iter().cloned());
        warnings
    }

    /// The warnings that name a deprecated spelling. The argument still takes effect under its
    /// current name, unlike the other warnings, whose argument is ignored.
    pub fn deprecations(&self) -> Vec<String> {
        self.problems
            .deprecated
            .iter()
            .map(|(deprecated, name)| {
                format!("Argument {deprecated} is deprecated. Please use {name} instead")
            })
            .collect()
    }
}

fn before(text: &str, delimiter: char) -> &str {
    text.split_once(delimiter).map_or(text, |(head, _)| head)
}

/// Parse an already argfile-expanded command line against one release's arguments.
pub fn tokenize<'c>(
    catalog: &'c Catalog,
    arguments: Vec<String>,
    argfile_problems: Vec<String>,
) -> Tokenized<'c> {
    let mut problems = Problems::default();
    let mut occurrences = Vec::new();
    let mut explicit: Vec<(&ArgumentSpec, Recorded)> = Vec::new();
    let mut free = Vec::new();
    let mut free_started = false;
    let mut tokens = arguments.into_iter();

    while let Some(argument) = tokens.next() {
        if free_started {
            free.push(argument);
            continue;
        }
        if argument == "--" {
            free_started = true;
            continue;
        }
        let delimiter = if argument.starts_with("-XXLanguage") {
            ':'
        } else {
            '='
        };
        let key = before(&argument, delimiter);
        let Some(spec) = catalog.lookup(key) else {
            if argument.starts_with("-X") {
                problems.unknown_extra_flags.push(argument);
            } else if argument.starts_with('-') {
                problems.unknown_args.push(argument);
            } else {
                free.push(argument);
            }
            continue;
        };
        // `-shortName=value` is not a spelling kotlinc accepts.
        if key != argument && spec.short_name.as_deref() == Some(key) {
            problems.unknown_args.push(argument);
            continue;
        }
        if spec.obsolete {
            // Reported as unknown, but its value is still consumed.
            problems.unknown_args.push(argument.clone());
        }
        if spec.deprecated_name.as_deref() == Some(key)
            && !problems.deprecated.iter().any(|(name, _)| name == key)
        {
            problems
                .deprecated
                .push((key.to_string(), spec.name.clone()));
        }
        if spec.name == argument && spec.is_advanced() && spec.kind != ValueKind::Bool {
            problems.obsolete_form.push(argument.clone());
        }

        let attached = |name: &str| {
            argument
                .strip_prefix(name)
                .and_then(|rest| rest.strip_prefix(delimiter))
                .map(str::to_string)
        };
        let value = if spec.kind == ValueKind::Bool {
            let value = match attached(&spec.name) {
                None => true,
                Some(text) => {
                    if spec.changes_language_features() {
                        problems.feature_boolean_with_value.push(argument.clone());
                    }
                    match text.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => {
                            if !spec.changes_language_features() {
                                problems.bad_boolean.push(argument.clone());
                            }
                            true
                        }
                    }
                }
            };
            record_scalar(&mut explicit, spec, value.to_string());
            Value::Bool(value)
        } else {
            let text = if let Some(text) = attached(&spec.name) {
                let legal = spec.legal_values();
                if !legal.is_empty() && !legal.contains(&text.as_str()) {
                    problems.bad_feature_value.push((
                        argument.clone(),
                        legal.iter().map(|value| value.to_string()).collect(),
                    ));
                }
                text
            } else if let Some(text) = spec.deprecated_name.as_deref().and_then(attached) {
                text
            } else if let Some(next) = tokens.next() {
                next
            } else {
                problems.without_value.push(argument);
                break;
            };
            if spec.kind == ValueKind::String {
                record_scalar(&mut explicit, spec, text.clone());
                Value::String(text)
            } else {
                let elements = spec.delimiter.split(&text);
                record_elements(&mut explicit, spec, &elements);
                Value::List(elements)
            }
        };
        occurrences.push(Occurrence { spec, value });
    }

    Tokenized {
        occurrences,
        explicit,
        free,
        problems,
        argfile_problems,
    }
}

fn record_scalar<'c>(
    explicit: &mut Vec<(&'c ArgumentSpec, Recorded)>,
    spec: &'c ArgumentSpec,
    value: String,
) {
    match explicit
        .iter_mut()
        .find(|(known, _)| std::ptr::eq(*known, spec))
    {
        Some((_, Recorded::Scalars(values))) => values.push(value),
        Some((_, Recorded::List(_))) => unreachable!("an argument has one value kind"),
        None => explicit.push((spec, Recorded::Scalars(vec![value]))),
    }
}

fn record_elements<'c>(
    explicit: &mut Vec<(&'c ArgumentSpec, Recorded)>,
    spec: &'c ArgumentSpec,
    elements: &[String],
) {
    match explicit
        .iter_mut()
        .find(|(known, _)| std::ptr::eq(*known, spec))
    {
        Some((_, Recorded::List(values))) => values.extend_from_slice(elements),
        Some((_, Recorded::Scalars(_))) => unreachable!("an argument has one value kind"),
        None => explicit.push((spec, Recorded::List(elements.to_vec()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use krusty::kotlin_version::KotlinVersion;

    fn parse(version: KotlinVersion, arguments: &[&str]) -> Tokenized<'static> {
        let catalog = Catalog::for_version(version).unwrap();
        tokenize(
            catalog,
            arguments
                .iter()
                .map(|argument| argument.to_string())
                .collect(),
            Vec::new(),
        )
    }

    fn values(tokenized: &Tokenized) -> Vec<(String, Value)> {
        tokenized
            .occurrences
            .iter()
            .map(|occurrence| (occurrence.spec.name.clone(), occurrence.value.clone()))
            .collect()
    }

    const NEWEST: KotlinVersion = KotlinVersion::V2_4_20;

    /// Measured on kotlinc 2.4.20: each line of the expected output is what kotlinc printed.
    #[test]
    fn unknown_tokens_follow_kotlincs_three_way_split() {
        let parsed = parse(NEWEST, &["A.kt", "-foo", "-Xfoo=1", "-Xbar"]);
        assert_eq!(parsed.free, vec!["A.kt"]);
        assert_eq!(parsed.errors(), vec!["Invalid argument: -foo"]);
        assert_eq!(
            parsed.warnings(),
            vec![
                "Flag is not supported by this version of the compiler: -Xfoo=1",
                "Flag is not supported by this version of the compiler: -Xbar",
            ]
        );
    }

    #[test]
    fn every_value_form_kotlinc_accepts() {
        let parsed = parse(
            NEWEST,
            &[
                "-module-name=m1",
                "-module-name",
                "m2",
                "-cp",
                "a:b",
                "-Xexplicit-api",
                "warning",
                "-Werror=false",
                "-Xfriend-paths=x,y",
                "-Xfriend-paths=z",
            ],
        );
        assert_eq!(parsed.errors(), Vec::<String>::new());
        assert_eq!(
            values(&parsed),
            vec![
                ("-module-name".into(), Value::String("m1".into())),
                ("-module-name".into(), Value::String("m2".into())),
                ("-classpath".into(), Value::String("a:b".into())),
                ("-Xexplicit-api".into(), Value::String("warning".into())),
                ("-Werror".into(), Value::Bool(false)),
                (
                    "-Xfriend-paths".into(),
                    Value::List(vec!["x".into(), "y".into()])
                ),
                ("-Xfriend-paths".into(), Value::List(vec!["z".into()])),
            ]
        );
        assert_eq!(
            parsed.warnings(),
            vec![
                "Argument '-module-name' is passed multiple times: 'm1', 'm2'. The last value will be used.",
                "Advanced option value is passed in an obsolete form. Please use the '=' character to specify the value: -Xexplicit-api=...",
            ]
        );
        assert_eq!(
            parsed.explicit.last().map(|(_, recorded)| recorded.clone()),
            Some(Recorded::List(vec!["x".into(), "y".into(), "z".into()]))
        );
    }

    #[test]
    fn syntax_errors_use_kotlincs_words_and_order() {
        let parsed = parse(
            NEWEST,
            &[
                "-cp=x",
                "-Werror=maybe",
                "-Xcontext-parameters=true",
                "-Xname-based-destructuring=disable",
                "-module-name",
            ],
        );
        assert_eq!(
            parsed.errors(),
            vec![
                "No value passed for argument -module-name",
                "Incorrect value for boolean argument '-Werror'. Only 'true' and 'false' are allowed.",
                "No value is expected for argument '-Xcontext-parameters'.",
                "Incorrect value for argument '-Xname-based-destructuring'. Actual value: 'disable', but allowed values: 'only-syntax', 'name-mismatch', 'complete'.",
                "Invalid argument: -cp=x",
            ]
        );
    }

    #[test]
    fn deprecated_names_map_to_the_current_argument_with_a_warning() {
        let parsed = parse(NEWEST, &["-Xopt-in=a.B", "-Xopt-in=c.D"]);
        assert_eq!(
            values(&parsed),
            vec![
                ("-opt-in".into(), Value::List(vec!["a.B".into()])),
                ("-opt-in".into(), Value::List(vec!["c.D".into()])),
            ]
        );
        assert_eq!(
            parsed.warnings(),
            vec!["Argument -Xopt-in is deprecated. Please use -opt-in instead"]
        );
    }

    #[test]
    fn everything_after_the_separator_is_a_source() {
        let parsed = parse(NEWEST, &["--", "-d", "-x.kt"]);
        assert_eq!(parsed.free, vec!["-d", "-x.kt"]);
        assert!(parsed.occurrences.is_empty());
    }

    #[test]
    fn language_feature_arguments_split_on_the_colon_and_keep_the_value_whole() {
        let parsed = parse(NEWEST, &["-XXLanguage:+A,-B"]);
        assert_eq!(
            values(&parsed),
            vec![("-XXLanguage".into(), Value::List(vec!["+A,-B".into()]))]
        );
    }

    /// 2.4.0 reports an obsolete argument as unknown; 2.4.20 parses the removed argument.
    #[test]
    fn removed_arguments_follow_the_reference_release() {
        let old = parse(KotlinVersion::V2_4_0, &["-Xuse-k2"]);
        assert_eq!(old.errors(), vec!["Invalid argument: -Xuse-k2"]);
        let new = parse(NEWEST, &["-Xuse-k2"]);
        assert_eq!(new.errors(), Vec::<String>::new());
        assert_eq!(new.occurrences.len(), 1);
    }
}
