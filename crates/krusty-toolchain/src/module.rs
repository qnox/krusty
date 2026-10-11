//! A module file's header: what the module produces (`product:`) and its `description:`.
//!
//! The other properties a module file may hold are recognised here, so a misspelt property is
//! reported as the toolchain reports it, but their values are read by the phases that use them.
//! krusty-toolchain reads JVM applications, libraries and plugins only; any other product type is
//! refused where it is written.

use std::path::{Path, PathBuf};

use crate::diagnostic::{Diagnostic, Diagnostics, Severity};
use crate::inventory::{self, Kind};
use crate::reading::{read_document, FileReader, Value};
use crate::yaml::NodeId;

/// The product types the toolchain knows, in its order (the order its messages list them in).
const PRODUCT_TYPES: [&str; 13] = [
    "lib",
    "jvm/app",
    "jvm/lib",
    "kmp/lib",
    "jvm/amper-plugin",
    "android/app",
    "ios/app",
    "macos/app",
    "linux/app",
    "windows/app",
    "wasm-js/app",
    "wasm-wasi/app",
    "js/app",
];

/// Properties of a module file other than `product` and `description`. `dependencies` and
/// `settings` may also be written `test-…` and with an `@platform` qualifier.
const OTHER_PROPERTIES: [&str; 10] = [
    "aliases",
    "pluginInfo",
    "apply",
    "repositories",
    "plugins",
    "mavenPlugins",
    "tasks",
    "layout",
    "dependencies",
    "settings",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// A JVM product type.
pub enum ProductType {
    App,
    Lib,
    /// A toolchain plugin. Listed, but not built: krusty-toolchain does not run plugins.
    AmperPlugin,
}

impl ProductType {
    pub fn name(self) -> &'static str {
        match self {
            Self::App => "jvm/app",
            Self::Lib => "jvm/lib",
            Self::AmperPlugin => "jvm/amper-plugin",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleHeader {
    pub file: PathBuf,
    /// The module's name: its directory's name.
    pub name: String,
    pub product: ProductType,
    pub description: Option<String>,
}

/// The type a `product:` value must have, rendered as the toolchain renders it.
fn product_schema_type() -> String {
    let shorthands: Vec<String> = PRODUCT_TYPES
        .iter()
        .map(|name| format!("\"{name}\""))
        .collect();
    format!("ModuleProduct ( {} | {{..}} )", shorthands.join(" | "))
}

/// Read the header of the module file `file`. `None` when a problem was reported.
pub fn read_header(file: &Path, diagnostics: &mut Diagnostics) -> Option<ModuleHeader> {
    let name = file
        .parent()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())?;
    let document = read_document(file, diagnostics)?;
    let errors_before = diagnostics.errors();
    let mut reader = FileReader::new(file, &document, diagnostics);
    let mut product = None;
    let mut description = None;
    let mut product_seen = false;
    let top = document.root();
    match top.map(|top| (top, reader.value(top))) {
        None => {}
        Some((_, Value::Missing | Value::Null)) => {}
        // The toolchain reads a module's product first, and nothing else of a module without one.
        Some((_, Value::Mapping(pairs)))
            if !pairs.iter().any(|&(key, _)| reader.key(key) == "product") => {}
        Some((_, Value::Mapping(pairs))) => {
            for &(key, value) in pairs {
                match reader.key(key) {
                    "product" => {
                        product_seen = true;
                        product = read_product(&mut reader, value);
                    }
                    "description" => description = read_description(&mut reader, value),
                    _ if other_property(&mut reader, key) => reader.skip(value),
                    _ => reader.unknown_property(key, value),
                }
            }
        }
        Some((top, _)) => reader.mismatch(top, "MinimalModule {..}"),
    }
    reader.finish();
    if !product_seen {
        let message =
            "Product type definition is missing. Define a product using a `product:` declaration.";
        // Placed at the module's mapping, or at the whole file when there is none.
        match top.filter(|&top| matches!(reader.value(top), Value::Mapping(_))) {
            Some(top) => reader.error(top, message),
            None => reader
                .diagnostics
                .push(Diagnostic::error(file, None, message)),
        }
    }
    if diagnostics.errors() != errors_before {
        return None;
    }
    Some(ModuleHeader {
        file: file.to_path_buf(),
        name,
        product: product?,
        description: description.flatten(),
    })
}

/// Whether `key` is one of the module properties read elsewhere. `dependencies` and `settings` may
/// be written `test-…`, and with an `@jvm` qualifier, which on a JVM module means what it does
/// without one. Any other qualifier is refused.
fn other_property(reader: &mut FileReader<'_>, key: NodeId) -> bool {
    let text = reader.key(key);
    let (name, qualifier) = match text.split_once('@') {
        Some((name, qualifier)) => (name, Some(qualifier)),
        None => (text, None),
    };
    let (name, test) = match name.strip_prefix("test-") {
        Some(name) => (name, true),
        None => (name, false),
    };
    let fragment_property = matches!(name, "dependencies" | "settings");
    let known = OTHER_PROPERTIES.contains(&name)
        || qualifier.is_some() && matches!(name, "product" | "description");
    if test && !fragment_property || !known {
        return false;
    }
    match qualifier {
        None | Some("jvm") if fragment_property => {}
        None => {}
        Some(qualifier) => reader.error(
            key,
            format!("krusty-toolchain reads JVM modules only; it refuses the qualifier `@{qualifier}` on `{name}`"),
        ),
    }
    true
}

fn read_description(reader: &mut FileReader<'_>, node: NodeId) -> Option<Option<String>> {
    match reader.value(node) {
        Value::Missing | Value::Null => Some(None),
        Value::Scalar(text) => Some(Some(text.to_string())),
        _ => {
            reader.mismatch(node, "string | null");
            None
        }
    }
}

fn read_product(reader: &mut FileReader<'_>, node: NodeId) -> Option<ProductType> {
    match reader.value(node) {
        Value::Scalar(_) => product_type(reader, node),
        Value::Mapping(pairs) => {
            let mut product = None;
            let mut typed = false;
            let mut platforms = None;
            for &(key, value) in pairs {
                match reader.key(key) {
                    "type" => {
                        typed = true;
                        product = product_type(reader, value);
                    }
                    "platforms" => platforms = Some(value),
                    _ => reader.unknown_property(key, value),
                }
            }
            if !typed {
                reader.error(node, "No value for required property `type`.");
                return None;
            }
            let product = product?;
            match platforms {
                Some(list) if jvm_only(reader, product, list) => Some(product),
                Some(_) => None,
                None => Some(product),
            }
        }
        _ => {
            reader.mismatch(node, &product_schema_type());
            None
        }
    }
}

fn product_type(reader: &mut FileReader<'_>, node: NodeId) -> Option<ProductType> {
    let Value::Scalar(name) = reader.value(node) else {
        reader.mismatch(node, &product_schema_type());
        return None;
    };
    match name {
        "jvm/app" => Some(ProductType::App),
        "jvm/lib" => Some(ProductType::Lib),
        "jvm/amper-plugin" => {
            let directory = reader.file.parent().expect("a module file has a directory");
            let plugin_file = inventory::kind(&directory.join("plugin.yaml"));
            if let Err(error) = &plugin_file {
                reader.error(node, error.to_string());
            } else if plugin_file.ok().flatten() != Some(Kind::File) {
                let span = reader.span(node);
                reader.diagnostics.push(Diagnostic::warning(
                    Severity::Warning,
                    &reader.file,
                    Some(span),
                    "`plugin.yaml` file is missing in the plugins module directory, so it will have no effect when enabled",
                ));
            }
            Some(ProductType::AmperPlugin)
        }
        known if PRODUCT_TYPES.contains(&known) => {
            reader.error(
                node,
                format!("krusty-toolchain reads `jvm/app`, `jvm/lib` and `jvm/amper-plugin` modules only, not `{known}`"),
            );
            None
        }
        unknown => {
            let expected: Vec<String> = PRODUCT_TYPES
                .iter()
                .map(|name| format!("`{name}`"))
                .collect();
            reader.error(
                node,
                format!(
                    "Unknown value `{unknown}`. Expected one of: [{}]",
                    expected.join(", ")
                ),
            );
            None
        }
    }
}

/// A JVM product's `platforms:` may list only `jvm`.
fn jvm_only(reader: &mut FileReader<'_>, product: ProductType, list: NodeId) -> bool {
    let items = reader.strings(list);
    let mut fine = !items.is_empty();
    if items.is_empty() {
        reader.error(
            list,
            "krusty-toolchain reads `platforms` of a JVM product only as `[jvm]`",
        );
    }
    for (item, platform) in items {
        if platform != "jvm" {
            fine = false;
            reader.error(
                item,
                format!(
                    "Product type `{}` does not support platform `{platform}`.\nSupported platforms are: [`jvm`].",
                    product.name()
                ),
            );
        }
    }
    fine
}

/// The headers of every module of a project, in the project's order. `None` when a problem was
/// reported in any of them.
pub fn read_headers(
    modules: &[PathBuf],
    diagnostics: &mut Diagnostics,
) -> Option<Vec<ModuleHeader>> {
    let headers: Vec<Option<ModuleHeader>> = modules
        .iter()
        .map(|file| read_header(file, diagnostics))
        .collect();
    headers.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "krusty-toolchain-module-{name}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_plugin_file_that_is_a_symbolic_link_is_refused() {
        let temp = TempDir::new("linked-plugin");
        let outside = TempDir::new("linked-plugin-target");
        std::fs::write(outside.0.join("plugin.yaml"), "").unwrap();
        let file = temp.0.join("module.yaml");
        std::fs::write(&file, "product: jvm/amper-plugin\n").unwrap();
        std::os::unix::fs::symlink(outside.0.join("plugin.yaml"), temp.0.join("plugin.yaml"))
            .unwrap();
        let mut diagnostics = Diagnostics::default();
        assert_eq!(read_header(&file, &mut diagnostics), None);
        let reported: Vec<String> = diagnostics.iter().map(ToString::to_string).collect();
        assert_eq!(
            reported,
            [format!(
                "{}:1:10: ERROR: krusty-toolchain does not follow symbolic links: {}",
                file.display(),
                temp.0.join("plugin.yaml").display()
            )]
        );
    }
}
