//! Which project a command stands in, and which modules it holds: `project.yaml` and the module
//! files it lists, read as the Kotlin Toolchain reads them (`StandaloneAmperProjectContext`).
//!
//! The root is the nearest directory, walking up from the start, that holds a project file. A
//! module file met on the way that this project does not include makes its own directory a
//! single-module project. The module list is the root's own `module.yaml`, then each `modules:`
//! entry in order (a plain path, or a glob matched with [`crate::glob`]), without duplicates.
//!
//! Every diagnostic the toolchain reports here is reported with its message, severity and
//! position. Beyond the toolchain, krusty refuses `project.amper`/`module.amper` files, a `modules`
//! entry starting with `//` or `/`, a `mavenPlugins` list, and any symbolic link on the way to a
//! module (see [`crate::inventory`]).

use std::path::{Path, PathBuf};

use crate::diagnostic::{Diagnostic, Diagnostics, Severity};
use crate::glob::{self, Glob};
use crate::inventory::{self, Kind, WalkLimits};
use crate::reading::{read_document, FileReader, Value};
use crate::yaml::NodeId;

pub const PROJECT_FILE: &str = "project.yaml";
pub const MODULE_FILE: &str = "module.yaml";
const AMPER_PROJECT_FILE: &str = "project.amper";
const AMPER_MODULE_FILE: &str = "module.amper";

/// A project's root and module files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    pub root: PathBuf,
    pub project_file: Option<PathBuf>,
    /// Module files, the root's own first, then in `modules:` order.
    pub modules: Vec<PathBuf>,
    /// Module files of the local plugins `plugins:` makes available.
    pub plugins: Vec<PathBuf>,
}

/// Whether `path` is a `kind`, looked at without following a symbolic link; a link is an error.
fn is(path: &Path, kind: Kind) -> Result<bool, String> {
    inventory::kind(path)
        .map(|found| found == Some(kind))
        .map_err(|error| error.to_string())
}

/// Whether anything is at `path`, looked at without following a symbolic link; a link is an error.
fn exists(path: &Path) -> Result<bool, String> {
    inventory::kind(path)
        .map(|found| found.is_some())
        .map_err(|error| error.to_string())
}

/// The directory to start from, made absolute without resolving symbolic links in it.
fn absolute(start: &Path) -> Result<PathBuf, String> {
    if start.is_absolute() {
        Ok(start.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(start))
            .map_err(|error| format!("cannot read the current directory: {error}"))
    }
}

/// Find and read the project that `start` (a directory) belongs to. `Ok(None)` when no project or
/// module file is above it. Problems are added to `diagnostics`; the project is returned even when
/// some are errors, so every problem can be reported before giving up.
pub fn load(start: &Path, diagnostics: &mut Diagnostics) -> Result<Option<Project>, String> {
    let start = absolute(start)?;
    let mut closest_module: Option<PathBuf> = None;
    let mut root: Option<PathBuf> = None;
    for directory in start.ancestors() {
        for refused in [AMPER_PROJECT_FILE, AMPER_MODULE_FILE] {
            if exists(&directory.join(refused))? {
                return Err(format!(
                    "{}: krusty-toolchain reads YAML project files only",
                    directory.join(refused).display()
                ));
            }
        }
        if is(&directory.join(PROJECT_FILE), Kind::File)? {
            root = Some(directory.to_path_buf());
            break;
        }
        if closest_module.is_none() && is(&directory.join(MODULE_FILE), Kind::File)? {
            closest_module = Some(directory.join(MODULE_FILE));
        }
    }
    let Some(root) = root.or_else(|| {
        closest_module
            .as_ref()
            .and_then(|file| file.parent().map(Path::to_path_buf))
    }) else {
        return Ok(None);
    };
    let mut own = Diagnostics::default();
    let mut project = read_project(&root, &mut own)?;
    if let Some(module) = closest_module.filter(|module| !project.modules.contains(module)) {
        // The project above does not include the module we stand in: that module is its own
        // single-module project, and the project file's problems are not this command's.
        let directory = module.parent().expect("a module file has a directory");
        own = Diagnostics::default();
        project = read_project(directory, &mut own)?;
    }
    for diagnostic in own.iter() {
        diagnostics.push(diagnostic.clone());
    }
    Ok(Some(project))
}

/// A module is named after its directory, and no two modules may share a name.
pub fn check_unique_names(project: &Project, diagnostics: &mut Diagnostics) {
    let name = |file: &PathBuf| {
        file.parent()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
    };
    let mut reported: Vec<String> = Vec::new();
    for file in &project.modules {
        let Some(module) = name(file) else { continue };
        if reported.contains(&module) {
            continue;
        }
        let declared: Vec<String> = project
            .modules
            .iter()
            .filter(|other| name(other).as_ref() == Some(&module))
            .map(|other| other.display().to_string())
            .collect();
        if declared.len() > 1 {
            diagnostics.push(Diagnostic::project_error(format!(
                "Module name '{module}' is not unique, it's declared in:\n{}",
                declared.join("\n")
            )));
            reported.push(module);
        }
    }
}

/// Read the project whose root is `root` (`--project-dir`), without looking above it. `Ok(None)`
/// when `root` holds neither a project file nor a module file.
pub fn load_root(root: &Path, diagnostics: &mut Diagnostics) -> Result<Option<Project>, String> {
    let root = absolute(root)?;
    for refused in [AMPER_PROJECT_FILE, AMPER_MODULE_FILE] {
        if exists(&root.join(refused))? {
            return Err(format!(
                "{}: krusty-toolchain reads YAML project files only",
                root.join(refused).display()
            ));
        }
    }
    if !is(&root.join(PROJECT_FILE), Kind::File)? && !is(&root.join(MODULE_FILE), Kind::File)? {
        return Ok(None);
    }
    read_project(&root, diagnostics).map(Some)
}

/// Read the project rooted at `root`.
fn read_project(root: &Path, diagnostics: &mut Diagnostics) -> Result<Project, String> {
    let root_module = root.join(MODULE_FILE);
    let root_module = is(&root_module, Kind::File)?.then_some(root_module);
    let project_file = root.join(PROJECT_FILE);
    let mut modules: Vec<PathBuf> = root_module.iter().cloned().collect();
    let mut plugins = Vec::new();
    if !is(&project_file, Kind::File)? {
        return Ok(Project {
            root: root.to_path_buf(),
            project_file: None,
            modules,
            plugins,
        });
    }
    let Some(document) = read_document(&project_file, diagnostics) else {
        return Ok(Project {
            root: root.to_path_buf(),
            project_file: Some(project_file),
            modules,
            plugins,
        });
    };
    let mut reader = FileReader::new(&project_file, &document, diagnostics);
    let mut entries: Vec<(NodeId, &str)> = Vec::new();
    let mut modules_node = None;
    let mut plugin_entries: Vec<(NodeId, &str)> = Vec::new();
    if let Some(top) = document.root() {
        match reader.value(top) {
            Value::Mapping(pairs) => {
                for &(key, value) in pairs {
                    match reader.key(key) {
                        "modules" => {
                            modules_node = Some(value);
                            entries = reader.strings(value);
                        }
                        "plugins" => plugin_entries = plugin_paths(&mut reader, value),
                        "mavenPlugins" => reader.error(
                            key,
                            "krusty-toolchain does not implement Maven plugins (`mavenPlugins`)",
                        ),
                        _ => reader.unknown_property(key, value),
                    }
                }
            }
            Value::Missing | Value::Null => {}
            _ => reader.mismatch(top, "Project {..}"),
        }
    }
    reader.finish();
    for (node, entry) in &entries {
        for module in resolve_entry(&mut reader, root, *node, entry)? {
            if !modules.contains(&module) {
                modules.push(module);
            }
        }
    }
    let file_position = document.root().map(|top| reader.position(top));
    if modules.is_empty() {
        reader.diagnostics.push(Diagnostic::warning(
            Severity::Warning,
            &project_file,
            file_position,
            "Project has no modules: no root module file and no modules listed in the project file",
        ));
    }
    let listed: Vec<&str> = entries
        .iter()
        .map(|(_, entry)| entry.strip_prefix("./").unwrap_or(entry))
        .collect();
    let mut sorted = listed.clone();
    sorted.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    if let Some(node) = modules_node.filter(|_| sorted != listed) {
        let position = reader.position(node);
        reader.diagnostics.push(Diagnostic::warning(
            Severity::WeakWarning,
            &project_file,
            Some(position),
            "It is recommended to sort the `modules` list alphabetically. This reduces the chance of Git conflicts and makes it easier to visually locate a module in the list.",
        ));
    }
    for (node, entry) in plugin_entries {
        let shown = display_relative(root, &plugin_directory(root, &project_file, entry));
        let module_file = plugin_directory(root, &project_file, entry).join(MODULE_FILE);
        let found = match is(&module_file, Kind::File) {
            Ok(found) => found,
            Err(message) => {
                reader.error(node, message);
                continue;
            }
        };
        if !found {
            reader.error(node, format!("Plugin module `{shown}` is not found"));
        } else if !modules.contains(&module_file) {
            reader.error(
                node,
                format!("Plugin module `{shown}` is not included in the project `modules` list"),
            );
        } else {
            plugins.push(module_file);
        }
    }
    Ok(Project {
        root: root.to_path_buf(),
        project_file: Some(project_file),
        modules,
        plugins,
    })
}

/// The `plugins:` entries: local module paths, `./x` or `//x`.
fn plugin_paths<'a>(reader: &mut FileReader<'a>, node: NodeId) -> Vec<(NodeId, &'a str)> {
    let Value::Sequence(items) = reader.value(node) else {
        reader.mismatch(node, "sequence [UnscopedModuleDependency]");
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|&item| match reader.value(item) {
            Value::Scalar(text) if text.starts_with('.') || text.starts_with('/') => {
                Some((item, text))
            }
            _ => {
                reader.error(
                    item,
                    "krusty-toolchain reads a `plugins` entry only as a local module path (`./path` or `//path`)",
                );
                None
            }
        })
        .collect()
}

fn plugin_directory(root: &Path, project_file: &Path, entry: &str) -> PathBuf {
    let base = project_file.parent().unwrap_or(root);
    let joined = match entry.strip_prefix("//") {
        Some(rest) => root.join(rest),
        None => base.join(entry),
    };
    lexical_normalize(&joined)
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

fn display_relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// The module files one `modules:` entry names.
fn resolve_entry(
    reader: &mut FileReader<'_>,
    root: &Path,
    node: NodeId,
    entry: &str,
) -> Result<Vec<PathBuf>, String> {
    if entry.starts_with('/') {
        reader.error(
            node,
            format!("krusty-toolchain refuses the module path `{entry}`: a `modules` entry is a path relative to the project root, without a leading `/` or `//`"),
        );
        return Ok(Vec::new());
    }
    if glob::has_glob_characters(entry) {
        return resolve_glob(reader, root, node, entry);
    }
    let resolved = match inventory::resolve_inside(root, Path::new(entry)) {
        Ok(resolved) => resolved,
        Err(error) => {
            reader.error(node, error.to_string());
            return Ok(Vec::new());
        }
    };
    let directory = resolved.clone().unwrap_or_else(|| root.join(entry));
    let module_file = directory.join(MODULE_FILE);
    let (found, has_module_file, has_amper_file) = match look_at_module(&directory) {
        Ok(looked) => looked,
        Err(message) => {
            reader.error(node, message);
            return Ok(Vec::new());
        }
    };
    if found.is_none() {
        reader.error(node, format!("Unresolved module path `{entry}`"));
        return Ok(Vec::new());
    }
    if found != Some(Kind::Directory) {
        reader.error(node, format!("`{entry}` is not a directory"));
        return Ok(Vec::new());
    }
    if !has_module_file {
        if has_amper_file {
            return Err(format!(
                "{}: krusty-toolchain reads YAML project files only",
                directory.join(AMPER_MODULE_FILE).display()
            ));
        }
        reader.error(
            node,
            format!("Directory `{entry}` doesn't contain a Kotlin module file"),
        );
        return Ok(Vec::new());
    }
    if directory == root {
        let position = reader.position(node);
        reader.diagnostics.push(Diagnostic::warning(
            Severity::WeakWarning,
            &reader.file,
            Some(position),
            "The root module is included by default",
        ));
        return Ok(Vec::new());
    }
    if resolved.is_none() {
        reader.error(
            node,
            format!("Directory `{entry}` is not under the project root"),
        );
        return Ok(Vec::new());
    }
    Ok(vec![module_file])
}

/// What a `modules:` entry's directory is, and whether it holds a module file and a refused
/// `module.amper`.
fn look_at_module(directory: &Path) -> Result<(Option<Kind>, bool, bool), String> {
    Ok((
        inventory::kind(directory).map_err(|error| error.to_string())?,
        is(&directory.join(MODULE_FILE), Kind::File)?,
        exists(&directory.join(AMPER_MODULE_FILE))?,
    ))
}

fn resolve_glob(
    reader: &mut FileReader<'_>,
    root: &Path,
    node: NodeId,
    entry: &str,
) -> Result<Vec<PathBuf>, String> {
    if let Err(error) = Glob::compile(entry) {
        reader.error(node, format!("Invalid glob pattern `{entry}`: {error}"));
        return Ok(Vec::new());
    }
    if entry.contains("**") {
        reader.error(
            node,
            format!("Unsupported `**` in module glob pattern `{entry}`. Use multiple single-level `*` segments instead to specify the depth exactly."),
        );
        return Ok(Vec::new());
    }
    let normalized = glob::normalize(entry);
    let matcher = Glob::compile(&normalized).map_err(|error| {
        format!("the normalized glob `{normalized}` of `{entry}` does not compile: {error}")
    })?;
    // Without `**` a match is at most as deep as the pattern has segments, so only the literal
    // directories before the first glob segment, and that many levels below them, are walked.
    let segments: Vec<&str> = normalized.split('/').collect();
    let literal = segments
        .iter()
        .take_while(|segment| !glob::has_glob_characters(segment) && **segment != "..")
        .count()
        .min(segments.len().saturating_sub(1));
    let prefix = segments[..literal].join("/");
    let base = if prefix.is_empty() {
        root.to_path_buf()
    } else {
        match inventory::resolve_inside(root, Path::new(&prefix)) {
            Ok(Some(path)) => path,
            Ok(None) => return Ok(Vec::new()),
            Err(error) => {
                reader.error(node, error.to_string());
                return Ok(Vec::new());
            }
        }
    };
    let mut found = Vec::new();
    let base_kind = match inventory::kind(&base) {
        Ok(kind) => kind,
        Err(error) => {
            reader.error(node, error.to_string());
            return Ok(Vec::new());
        }
    };
    if base_kind == Some(Kind::Directory) {
        let limits = WalkLimits {
            depth: normalized.matches('/').count() + 2 - literal,
            entries: WalkLimits::SOURCES.entries,
        };
        let entries = match inventory::walk(&base, limits, false) {
            Ok(entries) => entries,
            Err(error) => {
                reader.error(node, error.to_string());
                return Ok(Vec::new());
            }
        };
        for item in entries {
            if item.kind != Kind::File {
                continue;
            }
            let (directory, name) = match item.relative.rsplit_once('/') {
                Some((directory, name)) => (directory.to_string(), name),
                None => (String::new(), item.relative.as_str()),
            };
            let relative = match (prefix.is_empty(), directory.is_empty()) {
                (true, _) => directory,
                (false, true) => prefix.clone(),
                (false, false) => format!("{prefix}/{directory}"),
            };
            if !matcher.matches(&relative) {
                continue;
            }
            match name {
                MODULE_FILE => found.push(root.join(&relative).join(MODULE_FILE)),
                AMPER_MODULE_FILE => {
                    return Err(format!(
                        "{}: krusty-toolchain reads YAML project files only",
                        root.join(&relative).join(AMPER_MODULE_FILE).display()
                    ))
                }
                _ => {}
            }
        }
    }
    if found.is_empty() {
        let position = reader.position(node);
        reader.diagnostics.push(Diagnostic::warning(
            Severity::WeakWarning,
            &reader.file,
            Some(position),
            format!("Glob pattern `{entry}` doesn't match any Kotlin module directory under the project root"),
        ));
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "krusty-toolchain-project-{name}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }

        fn write(&self, relative: &str, text: &str) {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const MODULE: &str = "product: jvm/lib\n";

    fn reported(root: &Path, diagnostics: &Diagnostics) -> Vec<String> {
        let prefix = format!("{}/", root.display());
        diagnostics
            .iter()
            .map(|diagnostic| diagnostic.to_string().replace(&prefix, ""))
            .collect()
    }

    #[test]
    fn the_project_is_found_from_a_directory_below_its_root() {
        let temp = TempDir::new("discovery");
        temp.write("project.yaml", "modules: [libs/a]\n");
        temp.write("libs/a/module.yaml", MODULE);
        temp.write("libs/a/src/main.kt", "");
        let mut diagnostics = Diagnostics::default();
        let project = load(&temp.0.join("libs/a/src"), &mut diagnostics)
            .unwrap()
            .unwrap();
        assert_eq!(project.root, temp.0);
        assert_eq!(project.modules, [temp.0.join("libs/a/module.yaml")]);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn a_module_the_project_above_does_not_list_is_its_own_project() {
        let temp = TempDir::new("unlisted");
        // The project above has a problem; it is not this command's.
        temp.write("project.yaml", "modules: [missing]\n");
        temp.write("tools/x/module.yaml", MODULE);
        let mut diagnostics = Diagnostics::default();
        let project = load(&temp.0.join("tools/x"), &mut diagnostics)
            .unwrap()
            .unwrap();
        assert_eq!(project.root, temp.0.join("tools/x"));
        assert_eq!(project.project_file, None);
        assert_eq!(project.modules, [temp.0.join("tools/x/module.yaml")]);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn no_project_is_found_without_a_project_or_module_file() {
        let temp = TempDir::new("nothing");
        let mut diagnostics = Diagnostics::default();
        // The temporary directory's ancestors hold no project files.
        assert_eq!(load(&temp.0, &mut diagnostics).unwrap(), None);
        assert_eq!(load_root(&temp.0, &mut diagnostics).unwrap(), None);
    }

    #[test]
    fn amper_language_files_are_refused() {
        let temp = TempDir::new("amper");
        temp.write("project.yaml", "modules: [a]\n");
        temp.write("a/module.amper", "product: jvm/lib\n");
        let mut diagnostics = Diagnostics::default();
        assert_eq!(
            load(&temp.0, &mut diagnostics).unwrap_err(),
            format!(
                "{}: krusty-toolchain reads YAML project files only",
                temp.0.join("a/module.amper").display()
            )
        );
        temp.write("project.amper", "");
        assert_eq!(
            load(&temp.0.join("a"), &mut diagnostics).unwrap_err(),
            format!(
                "{}: krusty-toolchain reads YAML project files only",
                temp.0.join("a/module.amper").display()
            )
        );
    }

    #[test]
    fn root_anchored_and_absolute_entries_are_refused() {
        let temp = TempDir::new("anchored");
        temp.write("project.yaml", "modules:\n  - //libs/a\n  - /libs/a\n");
        temp.write("libs/a/module.yaml", MODULE);
        let mut diagnostics = Diagnostics::default();
        load(&temp.0, &mut diagnostics).unwrap();
        assert_eq!(
            reported(&temp.0, &diagnostics),
            [
                "project.yaml:2:5: ERROR: krusty-toolchain refuses the module path `//libs/a`: a `modules` entry is a path relative to the project root, without a leading `/` or `//`",
                "project.yaml:3:5: ERROR: krusty-toolchain refuses the module path `/libs/a`: a `modules` entry is a path relative to the project root, without a leading `/` or `//`",
                "project.yaml:1:1: WARNING: Project has no modules: no root module file and no modules listed in the project file",
            ]
        );
    }

    #[test]
    fn maven_plugins_are_refused() {
        let temp = TempDir::new("maven-plugins");
        temp.write("project.yaml", "modules: [a]\nmavenPlugins: []\n");
        temp.write("a/module.yaml", MODULE);
        let mut diagnostics = Diagnostics::default();
        load(&temp.0, &mut diagnostics).unwrap();
        assert_eq!(
            reported(&temp.0, &diagnostics),
            ["project.yaml:2:1: ERROR: krusty-toolchain does not implement Maven plugins (`mavenPlugins`)"]
        );
    }

    #[test]
    fn a_directory_outside_the_root_is_reported() {
        let temp = TempDir::new("outside");
        let outside = TempDir::new("outside-module");
        outside.write("module.yaml", MODULE);
        let name = outside
            .0
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        temp.write("project.yaml", &format!("modules: [../{name}]\n"));
        let mut diagnostics = Diagnostics::default();
        load(&temp.0, &mut diagnostics).unwrap();
        assert_eq!(
            reported(&temp.0, &diagnostics),
            [
                format!("project.yaml:1:11: ERROR: Directory `../{name}` is not under the project root"),
                "project.yaml:1:1: WARNING: Project has no modules: no root module file and no modules listed in the project file".to_string(),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symbolic_link_on_the_way_to_a_module_is_refused() {
        let temp = TempDir::new("links");
        let outside = TempDir::new("links-target");
        outside.write("m/module.yaml", MODULE);
        temp.write(
            "project.yaml",
            "modules:\n  - \"globbed/*\"\n  - linked/m\n",
        );
        std::os::unix::fs::symlink(&outside.0, temp.0.join("linked")).unwrap();
        std::fs::create_dir(temp.0.join("globbed")).unwrap();
        std::os::unix::fs::symlink(outside.0.join("m"), temp.0.join("globbed/m")).unwrap();
        let mut diagnostics = Diagnostics::default();
        load(&temp.0, &mut diagnostics).unwrap();
        assert_eq!(
            reported(&temp.0, &diagnostics),
            [
                "project.yaml:2:5: ERROR: krusty-toolchain does not follow symbolic links: globbed/m",
                "project.yaml:3:5: ERROR: krusty-toolchain does not follow symbolic links: linked",
                "project.yaml:1:1: WARNING: Project has no modules: no root module file and no modules listed in the project file",
            ]
        );
    }

    /// `name` in `directory`, made a symbolic link to a real file of the same name outside.
    #[cfg(unix)]
    fn link_to_outside(outside: &TempDir, directory: &Path, name: &str, text: &str) {
        outside.write(name, text);
        std::fs::create_dir_all(directory).unwrap();
        std::os::unix::fs::symlink(outside.0.join(name), directory.join(name)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_build_file_that_is_a_symbolic_link_is_refused() {
        let refused = |path: &Path| {
            format!(
                "krusty-toolchain does not follow symbolic links: {}",
                path.display()
            )
        };
        for name in [PROJECT_FILE, AMPER_PROJECT_FILE, AMPER_MODULE_FILE] {
            let temp = TempDir::new(&format!("linked-{name}"));
            let outside = TempDir::new(&format!("linked-{name}-target"));
            link_to_outside(&outside, &temp.0, name, "modules: [a]\n");
            let mut diagnostics = Diagnostics::default();
            assert_eq!(
                load(&temp.0, &mut diagnostics).unwrap_err(),
                refused(&temp.0.join(name)),
                "{name}"
            );
            assert!(diagnostics.is_empty());
        }
        // A module file met on the way up from where the command starts.
        let temp = TempDir::new("linked-module-above");
        let outside = TempDir::new("linked-module-above-target");
        link_to_outside(&outside, &temp.0.join("a"), MODULE_FILE, MODULE);
        std::fs::create_dir_all(temp.0.join("a/src")).unwrap();
        let mut diagnostics = Diagnostics::default();
        assert_eq!(
            load(&temp.0.join("a/src"), &mut diagnostics).unwrap_err(),
            refused(&temp.0.join("a/module.yaml"))
        );
        assert!(diagnostics.is_empty());
        // A module file a `modules:` entry leads to, and one a glob finds.
        let temp = TempDir::new("linked-module-listed");
        let outside = TempDir::new("linked-module-listed-target");
        temp.write("project.yaml", "modules: [a, \"libs/*\"]\n");
        link_to_outside(&outside, &temp.0.join("a"), MODULE_FILE, MODULE);
        link_to_outside(&outside, &temp.0.join("libs/b"), MODULE_FILE, MODULE);
        let mut diagnostics = Diagnostics::default();
        load(&temp.0, &mut diagnostics).unwrap();
        assert_eq!(
            reported(&temp.0, &diagnostics),
            [
                "project.yaml:1:11: ERROR: krusty-toolchain does not follow symbolic links: a/module.yaml",
                "project.yaml:1:14: ERROR: krusty-toolchain does not follow symbolic links: libs/b/module.yaml",
                "project.yaml:1:1: WARNING: Project has no modules: no root module file and no modules listed in the project file",
            ]
        );
    }

    #[test]
    fn a_glob_reads_only_as_deep_as_it_can_match() {
        let temp = TempDir::new("glob-depth");
        temp.write("project.yaml", "modules: [\"libs/*\"]\n");
        temp.write("libs/a/module.yaml", MODULE);
        // Below the glob's depth nothing is read, so a link there is not met.
        std::fs::create_dir_all(temp.0.join("libs/a/deep/er")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/", temp.0.join("libs/a/deep/er/root")).unwrap();
        let mut diagnostics = Diagnostics::default();
        let project = load(&temp.0, &mut diagnostics).unwrap().unwrap();
        assert!(
            diagnostics.is_empty(),
            "{:?}",
            reported(&temp.0, &diagnostics)
        );
        assert_eq!(project.modules, [temp.0.join("libs/a/module.yaml")]);
    }
}
