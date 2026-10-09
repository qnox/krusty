//! `show settings`: a module's effective settings as YAML, every value followed by where it comes
//! from (`YamlSerializer` and `SettingsFilter`).

use crate::configuration::Configuration;
use crate::module::{ModuleHeader, ProductType};
use crate::tree::{Files, Node, Trace, Value};

const INDENT: &str = "  ";

/// The settings of the `shown` modules among `modules` (configured as `configured`), in the
/// project's order, each named when the project has several modules.
pub fn modules_settings(
    modules: &[ModuleHeader],
    configured: &[Configuration],
    shown: impl Fn(&ModuleHeader) -> bool,
) -> String {
    let mut output = String::new();
    for (module, configuration) in modules.iter().zip(configured) {
        let Some(main) = configuration.main.as_ref().filter(|_| shown(module)) else {
            continue;
        };
        if modules.len() > 1 {
            output.push_str(&module_banner(&module.name));
        }
        if let Some(main_settings) = main.get("settings") {
            output.push_str(&settings(
                main_settings,
                module.product,
                &configuration.files.files,
            ));
        }
    }
    output
}

/// The line naming a module before its settings when a project has several modules: padded to the
/// toolchain's terminal width when its output is not a terminal, and followed by a blank line of
/// the same width.
fn module_banner(name: &str) -> String {
    const WIDTH: usize = 1500;
    let line = format!("Module: {name}");
    let padding = WIDTH.saturating_sub(line.chars().count());
    format!("{line}{}\n{}\n", " ".repeat(padding), " ".repeat(WIDTH))
}

/// The settings of a JVM module's main fragment, headed `settings@jvm:` and followed by a blank
/// line.
fn settings(settings: &Node, product: ProductType, files: &Files) -> String {
    let printer = Printer { product, files };
    format!(
        "settings@jvm:\n{}\n\n",
        prepend_indent(&printer.value(settings), INDENT)
    )
}

struct Printer<'a> {
    product: ProductType,
    files: &'a Files,
}

/// Kotlin's `String.prependIndent`: blank lines become the indent (or stay, when longer).
fn prepend_indent(text: &str, indent: &str) -> String {
    text.split('\n')
        .map(|line| {
            if line.trim().is_empty() {
                if line.chars().count() < indent.chars().count() {
                    indent.to_string()
                } else {
                    line.to_string()
                }
            } else {
                format!("{indent}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl Printer<'_> {
    fn comment(&self, trace: &Trace) -> String {
        let description = match trace {
            Trace::Default => "default".to_string(),
            Trace::File { file, .. } => self.files.name(*file),
            Trace::Derived {
                description,
                source,
            } => match source {
                Some(file) => format!("{description} @ {}", self.files.name(*file)),
                None => description.clone(),
            },
        };
        format!("  # {description}")
    }

    fn value(&self, node: &Node) -> String {
        let comment = self.comment(&node.trace);
        match &node.value {
            Value::Null => format!("null{comment}"),
            Value::Boolean(value) => format!("{value}{comment}"),
            Value::Int(value) => format!("{value}{comment}"),
            Value::String(value) => format!("{value}{comment}"),
            Value::Enum(_, value) => format!("{value}{comment}"),
            // The toolchain joins the path's names, so an absolute path loses its root.
            Value::Path(path) => {
                let names: Vec<String> = path
                    .iter()
                    .filter(|name| *name != std::path::MAIN_SEPARATOR_STR)
                    .map(|name| name.to_string_lossy().into_owned())
                    .collect();
                format!("{}{comment}", names.join("/"))
            }
            Value::List(children) if children.is_empty() => format!("[]{comment}"),
            Value::List(children) => children
                .iter()
                .map(|child| {
                    let item = prepend_indent(&self.value(child), "  ");
                    format!("- {}", &item[2..])
                })
                .collect::<Vec<_>>()
                .join("\n"),
            Value::Mapping {
                object: None,
                entries,
            } if entries.is_empty() => {
                format!("{{}}{comment}")
            }
            Value::Mapping {
                object: None,
                entries,
            } => entries
                .iter()
                .map(|entry| self.key_value(&format!("\"{}\"", entry.key), &entry.value))
                .collect::<Vec<_>>()
                .join("\n"),
            Value::Mapping {
                object: Some(_),
                entries,
            } => {
                let mut shown: Vec<_> = entries
                    .iter()
                    .filter(|entry| {
                        entry.property.is_some_and(|property| {
                            !property.hidden
                                && (!property.jvm_app_only || self.product == ProductType::App)
                                && property.platforms.includes_jvm()
                        })
                    })
                    .collect();
                if shown.is_empty() {
                    return format!("{{}}{comment}");
                }
                shown.sort_by(|a, b| a.key.encode_utf16().cmp(b.key.encode_utf16()));
                shown
                    .iter()
                    .map(|entry| self.key_value(&entry.key, &entry.value))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            Value::Error | Value::Reference { .. } | Value::Opaque => {
                unreachable!("a complete tree holds values only")
            }
        }
    }

    fn key_value(&self, key: &str, value: &Node) -> String {
        let serialized = self.value(value);
        let scalar = !matches!(value.value, Value::List(_) | Value::Mapping { .. });
        if scalar || serialized.starts_with("[]") || serialized.starts_with("{}") {
            format!("{key}: {serialized}")
        } else {
            format!("{key}:\n{}", prepend_indent(&serialized, INDENT))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::module_banner;

    #[test]
    fn a_module_is_named_on_a_line_as_wide_as_the_toolchain_prints_it() {
        let banner = module_banner("app");
        let lines: Vec<&str> = banner.split_terminator('\n').collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], format!("Module: app{}", " ".repeat(1489)));
        assert_eq!(lines[1], " ".repeat(1500));
    }
}
