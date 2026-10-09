//! Settings written where they have no effect (`IncorrectSettingsSectionFactory`): a
//! platform-agnostic setting under a platform qualifier, a setting for another platform, and a
//! setting for another product type.

use super::Reported;
use crate::diagnostic::{Diagnostic, Diagnostics, Severity};
use crate::module::ProductType;
use crate::tree::{Files, Node, Trace, Value};

/// Check every property written in `node`, a module's or template's tree, outermost first.
pub(super) fn check(
    node: &Node,
    product: ProductType,
    files: &Files,
    reported: &mut Reported,
    diagnostics: &mut Diagnostics,
) {
    match &node.value {
        Value::Mapping { entries, .. } => {
            for entry in entries {
                let (Some(property), Trace::File { file, position }) =
                    (entry.property, &entry.key_trace)
                else {
                    continue;
                };
                if entry.value.trace.is_default() {
                    continue;
                }
                let path = files.path(*file);
                let key = &entry.key;
                if property.agnostic && entry.value.contexts.jvm {
                    let message = format!(
                        "Setting `{key}` can only be used without any @platform qualifier."
                    );
                    reported.push(
                        Diagnostic::error(path, Some(*position), message),
                        diagnostics,
                    );
                }
                if !property.platforms.includes_jvm() {
                    let message = format!(
                        "Setting `{key}` doesn't have any effect on platform `jvm`. It only applies to {}",
                        property.platforms.listed()
                    );
                    let warning =
                        Diagnostic::warning(Severity::Warning, path, Some(*position), message);
                    reported.push(warning, diagnostics);
                }
                if property.jvm_app_only && product != ProductType::JvmApp {
                    let message = format!(
                        "Setting `{key}` cannot be applied to product type `{}`. Supported product types are: `jvm/app`",
                        product.name()
                    );
                    let warning =
                        Diagnostic::warning(Severity::Warning, path, Some(*position), message);
                    reported.push(warning, diagnostics);
                }
            }
            for entry in entries {
                check(&entry.value, product, files, reported, diagnostics);
            }
        }
        Value::List(children) => {
            for child in children {
                check(child, product, files, reported, diagnostics);
            }
        }
        _ => {}
    }
}
