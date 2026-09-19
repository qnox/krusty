//! Transactional per-entry indexes derived from authoritative Kotlin metadata.
//!
//! A malformed metadata-bearing declaration aborts the whole entry build. Callers may therefore
//! publish a successful result atomically without confusing an invalid declaration with an absent
//! alias or extension.

use super::{
    descriptor_parts, enter_directory, push_id_dedup, Entry, EntryExt, JarPackages, TypeIndex,
};
use crate::jvm::classreader::{parse_class, ClassInfo, ReadError};
use crate::name_tree::{NameId, NameTree};
use crate::types::{type_name, TypeName};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

pub(super) type ClassMetadataLoadError = (TypeName, ReadError);

fn parse_metadata_class(
    bytes: &[u8],
    internal: &str,
) -> Result<Option<ClassInfo>, ClassMetadataLoadError> {
    match parse_class(bytes) {
        Ok(class) => Ok(Some(class)),
        Err(error @ ReadError::BadKotlinMetadata(_)) => Err((type_name(internal), error)),
        Err(_) => Ok(None),
    }
}

/// A lean per-class record for building the extension index — only what's needed to follow facade
/// superclass chains and index static methods (no fields, no instance methods).
struct ClassLite {
    is_public: bool,
    super_class: Option<NameId>,
    /// `(name, descriptor, generic-signature, is_public)` of each static method (excl `<init>`/`<clinit>`).
    /// Non-public ones (`@InlineOnly`) are kept for the inliner; the flag gates normal resolution.
    statics: Vec<(String, String, Option<String>, bool)>,
    /// JVM names of functions `@Metadata` marks as genuine TOP-LEVEL (NO extension receiver). A top-level
    /// generic whose first parameter erases to `Object` (`assertEquals<T>(T, T, String)`) is otherwise
    /// indistinguishable in bytecode from an extension, so a name that is ONLY ever top-level must NOT be
    /// keyed by its first parameter in `by_recv`. Name-keyed (not name+desc): `@Metadata` often omits the
    /// method descriptor (`jvm_desc=None`).
    toplevel_names: HashSet<String>,
    /// JVM names `@Metadata` marks as EXTENSIONS (receiver of any kind — class OR type parameter). A name
    /// that is an extension anywhere is NEVER excluded from `by_recv` (so `takeIf`/`uppercase` stay indexed).
    ext_names: HashSet<String>,
}

fn collect_class_bytes(
    bytes: &[u8],
    internal: &str,
    names: &mut NameTree,
    all: &mut HashMap<NameId, ClassLite>,
) -> Result<(), ClassMetadataLoadError> {
    let Some(class) = parse_metadata_class(bytes, internal)? else {
        return Ok(());
    };
    let this_class = names.insert(&class.this_class());
    let super_class = class.super_class().map(|name| names.insert(&name));
    let statics = class
        .methods
        .iter()
        .filter(|method| method.is_static() && !method.name.starts_with('<'))
        .map(|method| {
            (
                method.name.clone(),
                method.descriptor.clone(),
                method.signature.clone(),
                method.is_public(),
            )
        })
        .collect();
    let mut toplevel_names = HashSet::new();
    let mut ext_names = HashSet::new();
    for function in crate::jvm::metadata::package_functions(&class)
        .iter()
        .chain(crate::jvm::metadata::class_functions(&class).iter())
    {
        if function.is_extension() {
            ext_names.insert(function.jvm_name.clone());
        } else {
            toplevel_names.insert(function.jvm_name.clone());
        }
    }
    all.insert(
        this_class,
        ClassLite {
            is_public: class.is_public(),
            super_class,
            statics,
            toplevel_names,
            ext_names,
        },
    );
    Ok(())
}

fn collect_dir(
    directory: &Path,
    names: &mut NameTree,
    all: &mut HashMap<NameId, ClassLite>,
) -> Result<(), ClassMetadataLoadError> {
    let mut ancestors = HashSet::new();
    collect_dir_visited(directory, directory, names, all, &mut ancestors)
}

fn collect_dir_visited(
    root: &Path,
    directory: &Path,
    names: &mut NameTree,
    all: &mut HashMap<NameId, ClassLite>,
    ancestors: &mut HashSet<PathBuf>,
) -> Result<(), ClassMetadataLoadError> {
    let Ok(Some(canonical)) = enter_directory(directory, ancestors) else {
        return Ok(());
    };
    let Ok(entries) = std::fs::read_dir(directory) else {
        ancestors.remove(&canonical);
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_dir_visited(root, &path, names, all, ancestors)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "class")
        {
            if let Ok(bytes) = std::fs::read(&path) {
                let Some(internal) = path
                    .strip_prefix(root)
                    .ok()
                    .and_then(Path::to_str)
                    .map(|path| path.replace('\\', "/"))
                    .and_then(|path| class_internal_from_entry(&path).map(str::to_owned))
                else {
                    continue;
                };
                collect_class_bytes(&bytes, &internal, names, all)?;
            }
        }
    }
    ancestors.remove(&canonical);
    Ok(())
}

fn collect_jar(
    jar: &Path,
    packages: &JarPackages,
    names: &mut NameTree,
    all: &mut HashMap<NameId, ClassLite>,
) -> Result<(), ClassMetadataLoadError> {
    let Ok(file) = File::open(jar) else {
        return Ok(());
    };
    let Ok(mut archive) = zip::ZipArchive::new(file) else {
        return Ok(());
    };
    for entry_id in 0..archive.len() {
        let internal = archive
            .name_for_index(entry_id)
            .and_then(class_internal_from_entry)
            .filter(|internal| ext_scan_wanted(internal, packages))
            .map(str::to_owned);
        let Some(internal) = internal else {
            continue;
        };
        let Ok(mut entry) = archive.by_index(entry_id) else {
            continue;
        };
        let mut bytes = Vec::new();
        if entry.read_to_end(&mut bytes).is_ok() {
            collect_class_bytes(&bytes, &internal, names, all)?;
        }
    }
    Ok(())
}

/// Scan one classpath entry into its extension contribution. The result is returned only after the
/// complete entry succeeds, so the caller can publish it atomically.
pub(super) fn build_entry_ext(
    entry: &Entry,
    packages: &JarPackages,
) -> Result<EntryExt, ClassMetadataLoadError> {
    let mut names = NameTree::default();
    let mut all: HashMap<NameId, ClassLite> = HashMap::new();
    match entry {
        Entry::Dir(directory) => collect_dir(directory, &mut names, &mut all)?,
        Entry::Jar(jar) => collect_jar(jar, packages, &mut names, &mut all)?,
        Entry::Jimage(_) | Entry::CtSym { .. } => {}
    }
    let mut extensions = EntryExt::default();
    for class in all.values() {
        extensions
            .toplevel_names
            .extend(class.toplevel_names.iter().cloned());
        extensions.ext_names.extend(class.ext_names.iter().cloned());
    }
    for (&root, root_class) in &all {
        let mut root_id = None;
        let mut current = Some(root);
        let mut visited = HashSet::new();
        while let Some(class_name) = current {
            if !visited.insert(class_name) {
                break;
            }
            let Some(class) = all.get(&class_name) else {
                break;
            };
            for (method_name, descriptor, _signature, public) in &class.statics {
                if !root_class.is_public && *public {
                    continue;
                }
                let Some((first_parameter, _return_descriptor)) = descriptor_parts(descriptor)
                else {
                    continue;
                };
                let owner = *root_id
                    .get_or_insert_with(|| extensions.owner_names.insert_from(&names, root));
                push_id_dedup(&mut extensions.by_name, method_name, owner);
                if let Some(receiver) = first_parameter {
                    extensions
                        .by_recv_raw
                        .entry(receiver)
                        .or_default()
                        .push((method_name.clone(), owner));
                }
            }
            current = class.super_class;
        }
    }
    Ok(extensions)
}

/// `Xxx.class` entry name (jar/jimage path) → internal name, or `None` if not an indexable class.
fn class_internal_from_entry(name: &str) -> Option<&str> {
    name.strip_suffix(".class").filter(|name| !name.is_empty())
}

fn ext_scan_wanted(internal: &str, packages: &JarPackages) -> bool {
    packages.contains_facade(internal)
        || internal
            .rsplit('/')
            .next()
            .unwrap_or(internal)
            .contains("Kt")
}

fn type_alias_scan_wanted(internal: &str, packages: &JarPackages) -> bool {
    packages.contains_facade(internal) || is_type_aliases_kt(internal)
}

fn parse_aliases_from_bytes(
    bytes: &[u8],
    internal: &str,
    index: &mut TypeIndex,
) -> Result<(), ClassMetadataLoadError> {
    let Some(class) = parse_metadata_class(bytes, internal)? else {
        return Ok(());
    };
    for alias in crate::jvm::metadata::metadata_type_aliases(&class) {
        let name = type_name(&alias.name);
        index.type_aliases.insert(name, type_name(&alias.target));
        index.alias_expansions.insert(
            name,
            (
                type_name(&alias.target),
                alias.formals.clone(),
                alias.expansion,
                alias.expansion_spelling.clone(),
            ),
        );
    }
    Ok(())
}

/// A Kotlin file facade (`*Kt`) is where a top-level type alias is recorded.
fn is_type_aliases_kt(internal: &str) -> bool {
    internal
        .rsplit('/')
        .next()
        .unwrap_or(internal)
        .ends_with("Kt")
}

pub(super) fn build_entry_types(
    entry: &Entry,
    packages: &JarPackages,
) -> Result<TypeIndex, ClassMetadataLoadError> {
    let mut index = TypeIndex::default();
    match entry {
        Entry::Dir(directory) => scan_types_dir(directory, &mut index)?,
        Entry::Jar(jar) => scan_types_jar(jar, packages, &mut index)?,
        Entry::Jimage(_) | Entry::CtSym { .. } => {}
    }
    Ok(index)
}

pub(super) fn build_entry_package_types(
    entry: &Entry,
    packages: &JarPackages,
    package: TypeName,
) -> Result<TypeIndex, ClassMetadataLoadError> {
    let mut index = TypeIndex::default();
    match entry {
        Entry::Dir(directory) => {
            if let Some(package_entry) = packages.entry_name(package) {
                for &facade in &package_entry.facades {
                    let internal = packages.names.render(facade);
                    let path = directory.join(format!("{internal}.class"));
                    if let Ok(bytes) = std::fs::read(path) {
                        parse_aliases_from_bytes(&bytes, &internal, &mut index)?;
                    }
                }
            }
        }
        Entry::Jar(jar) => {
            // Archive entry names are an external classfile format and therefore require text.
            scan_types_jar_package(jar, packages, &package.render(), &mut index)?
        }
        Entry::Jimage(_) | Entry::CtSym { .. } => {}
    }
    Ok(index)
}

fn scan_types_dir(directory: &Path, index: &mut TypeIndex) -> Result<(), ClassMetadataLoadError> {
    let mut ancestors = HashSet::new();
    scan_types_dir_rooted(directory, directory, index, &mut ancestors)
}

fn scan_types_dir_rooted(
    root: &Path,
    directory: &Path,
    index: &mut TypeIndex,
    ancestors: &mut HashSet<PathBuf>,
) -> Result<(), ClassMetadataLoadError> {
    let Ok(Some(canonical)) = enter_directory(directory, ancestors) else {
        return Ok(());
    };
    let Ok(entries) = std::fs::read_dir(directory) else {
        ancestors.remove(&canonical);
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_types_dir_rooted(root, &path, index, ancestors)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "class")
        {
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let relative = relative.to_string_lossy().replace('\\', "/");
            let Some(internal) = class_internal_from_entry(&relative) else {
                continue;
            };
            if is_type_aliases_kt(internal) {
                if let Ok(bytes) = std::fs::read(&path) {
                    parse_aliases_from_bytes(&bytes, internal, index)?;
                }
            }
        }
    }
    ancestors.remove(&canonical);
    Ok(())
}

fn scan_types_jar(
    jar: &Path,
    packages: &JarPackages,
    index: &mut TypeIndex,
) -> Result<(), ClassMetadataLoadError> {
    let Ok(file) = File::open(jar) else {
        return Ok(());
    };
    let Ok(mut archive) = zip::ZipArchive::new(file) else {
        return Ok(());
    };
    for entry_id in 0..archive.len() {
        let internal = archive
            .name_for_index(entry_id)
            .and_then(class_internal_from_entry)
            .filter(|internal| type_alias_scan_wanted(internal, packages))
            .map(str::to_owned);
        let Some(internal) = internal else {
            continue;
        };
        let Ok(mut entry) = archive.by_index(entry_id) else {
            continue;
        };
        let mut bytes = Vec::new();
        if entry.read_to_end(&mut bytes).is_ok() {
            parse_aliases_from_bytes(&bytes, &internal, index)?;
        }
    }
    Ok(())
}

fn scan_types_jar_package(
    jar: &Path,
    packages: &JarPackages,
    package: &str,
    index: &mut TypeIndex,
) -> Result<(), ClassMetadataLoadError> {
    let Ok(file) = File::open(jar) else {
        return Ok(());
    };
    let Ok(mut archive) = zip::ZipArchive::new(file) else {
        return Ok(());
    };
    for entry_id in 0..archive.len() {
        let internal = archive
            .name_for_index(entry_id)
            .and_then(class_internal_from_entry)
            .filter(|internal| {
                packages.declares_facade(package, internal)
                    || (internal.rsplit_once('/').map_or("", |(parent, _)| parent) == package
                        && is_type_aliases_kt(internal))
            })
            .map(str::to_owned);
        let Some(internal) = internal else {
            continue;
        };
        let Ok(mut entry) = archive.by_index(entry_id) else {
            continue;
        };
        let mut bytes = Vec::new();
        if entry.read_to_end(&mut bytes).is_ok() {
            parse_aliases_from_bytes(&bytes, &internal, index)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_index_scans_cataloged_and_conventional_facades() {
        let module = crate::metadata::module::build_kotlin_module(&[(
            "p".to_string(),
            vec!["Utils".to_string(), "HelpersKt".to_string()],
        )]);
        let mut packages = JarPackages::default();
        super::super::record_kotlin_module(&module, &mut packages);

        assert!(ext_scan_wanted("p/Utils", &packages));
        assert!(type_alias_scan_wanted("p/Utils", &packages));
        assert!(ext_scan_wanted("p/HelpersKt", &packages));
        assert!(!ext_scan_wanted("p/Regular", &packages));

        let empty = JarPackages::default();
        assert!(ext_scan_wanted("p/HelpersKt", &empty));
        assert!(!ext_scan_wanted("p/Regular", &empty));
    }
}
