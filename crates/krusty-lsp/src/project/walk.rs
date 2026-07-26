use std::path::Path;

const IGNORED_DIRECTORIES: &[&str] = &[
    ".git",
    ".gradle",
    ".idea",
    "build",
    "node_modules",
    "out",
    "target",
];

pub(super) fn is_ignored_directory(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return true;
    };
    if name == "buildSrc" || name == "build-logic" {
        return false;
    }
    IGNORED_DIRECTORIES.contains(&name)
}
