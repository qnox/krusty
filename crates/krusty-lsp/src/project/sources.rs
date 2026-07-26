use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::model::ProjectModel;

const MAX_INVENTORY_ENTRIES: usize = 32 * 1024;

#[derive(Default)]
pub struct ProjectSources {
    cache: Option<Cache>,
}

struct Cache {
    roots: Vec<PathBuf>,
    excluded_paths: Vec<PathBuf>,
    documents: Vec<(String, String)>,
    bytes: usize,
}

impl ProjectSources {
    pub fn invalidate(&mut self) {
        self.cache = None;
    }

    pub fn load(
        &mut self,
        model: &ProjectModel,
        documents: &[(&str, &str)],
        open_uris: &[&str],
        max_bytes: usize,
    ) -> Result<&[(String, String)], String> {
        let source_paths = documents
            .iter()
            .filter_map(|(uri, _)| url::Url::parse(uri).ok()?.to_file_path().ok())
            .collect::<HashSet<_>>();
        let excluded_paths = open_uris
            .iter()
            .filter_map(|uri| url::Url::parse(uri).ok()?.to_file_path().ok())
            .collect::<HashSet<_>>();
        let open_bytes = documents
            .iter()
            .try_fold(0usize, |total, (_, source)| total.checked_add(source.len()))
            .filter(|total| *total <= max_bytes)
            .ok_or_else(|| size_limit_message(max_bytes))?;
        let remaining = max_bytes - open_bytes;

        let mut root_policies = BTreeMap::new();
        for source_path in &source_paths {
            if let Some(module) = model.module_for_source(source_path) {
                for root in &module.source_roots {
                    let broad_root = root.path == model.root || root.path == module.base_directory;
                    root_policies
                        .entry(root.path.clone())
                        .and_modify(|broad| *broad |= broad_root)
                        .or_insert(broad_root);
                }
            }
        }
        let roots = root_policies.keys().cloned().collect::<Vec<_>>();

        let mut excluded_paths = excluded_paths.into_iter().collect::<Vec<_>>();
        excluded_paths.sort();
        let cache_matches = self
            .cache
            .as_ref()
            .is_some_and(|cache| cache.roots == roots && cache.excluded_paths == excluded_paths);
        if cache_matches {
            let cache = self.cache.as_ref().unwrap();
            if cache.bytes > remaining {
                return Err(size_limit_message(max_bytes));
            }
            return Ok(&cache.documents);
        }

        let mut remaining_entries = MAX_INVENTORY_ENTRIES;
        let mut paths = Vec::new();
        for (root, ignore_workspace_directories) in &root_policies {
            paths.extend(find_sources(
                root,
                *ignore_workspace_directories,
                &mut remaining_entries,
            )?);
        }
        paths.retain(|path| excluded_paths.binary_search(path).is_err());
        paths.sort();
        paths.dedup();

        let (documents, bytes) = load_documents(paths, remaining, max_bytes)?;
        self.cache = Some(Cache {
            roots,
            excluded_paths,
            documents,
            bytes,
        });
        Ok(&self.cache.as_ref().unwrap().documents)
    }
}

fn load_documents(
    paths: Vec<PathBuf>,
    budget: usize,
    max_bytes: usize,
) -> Result<(Vec<(String, String)>, usize), String> {
    let mut remaining = budget;
    let mut inventory = Vec::new();
    for path in paths {
        let metadata = fs::metadata(&path).map_err(|_| read_error_message())?;
        let Ok(bytes) = usize::try_from(metadata.len()) else {
            return Err(size_limit_message(max_bytes));
        };
        if bytes > remaining {
            return Err(size_limit_message(max_bytes));
        }
        remaining -= bytes;
        inventory.push(path);
    }

    let mut bytes = 0usize;
    let mut documents = Vec::with_capacity(inventory.len());
    for path in inventory {
        let source = fs::read_to_string(&path).map_err(|_| read_error_message())?;
        let Some(next_bytes) = bytes.checked_add(source.len()) else {
            return Err(size_limit_message(max_bytes));
        };
        if next_bytes > budget {
            return Err(size_limit_message(max_bytes));
        }
        bytes = next_bytes;
        let uri = url::Url::from_file_path(path).map_err(|_| read_error_message())?;
        documents.push((uri.into(), source));
    }
    Ok((documents, bytes))
}

fn size_limit_message(max_bytes: usize) -> String {
    format!(
        "module source set exceeds analysis limit (maximum {} MiB); semantic diagnostics suppressed",
        max_bytes / (1024 * 1024)
    )
}

fn inventory_limit_message() -> String {
    format!(
        "module source inventory exceeds analysis limit (maximum {MAX_INVENTORY_ENTRIES} entries); semantic diagnostics suppressed"
    )
}

fn read_error_message() -> String {
    "module source set contains an unreadable source; semantic diagnostics suppressed".to_string()
}

fn find_sources(
    root: &Path,
    ignore_workspace_directories: bool,
    remaining_entries: &mut usize,
) -> Result<Vec<PathBuf>, String> {
    let mut pending = vec![root.to_path_buf()];
    let mut sources = Vec::new();
    while let Some(directory) = pending.pop() {
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => return Err(read_error_message()),
        };
        for entry in entries {
            let entry = entry.map_err(|_| read_error_message())?;
            let Some(next_remaining) = remaining_entries.checked_sub(1) else {
                return Err(inventory_limit_message());
            };
            *remaining_entries = next_remaining;
            let path = entry.path();
            let kind = entry.file_type().map_err(|_| read_error_message())?;
            if kind.is_dir()
                && (!ignore_workspace_directories || !super::walk::is_ignored_directory(&path))
            {
                pending.push(path);
            } else if kind.is_file() && krusty::source::is_supported_path(&path) {
                sources.push(path);
            }
        }
    }
    Ok(sources)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::project::{Module, ModuleId, ProjectModel, ProviderKind, SourceRoot};

    const MAX_BYTES: usize = 32 * 1024 * 1024;
    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_path(label: &str) -> PathBuf {
        let unique = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "krusty-lsp-{label}-{}-{unique}",
            std::process::id()
        ))
    }

    fn file_uri(path: &Path) -> String {
        url::Url::from_file_path(path).unwrap().into()
    }

    #[test]
    fn load_excludes_every_open_document_from_disk_sources() {
        let directory = temp_path("open-source-exclusion");
        fs::create_dir_all(&directory).unwrap();
        let active = directory.join("Active.kt");
        let blocked = directory.join("Blocked.kt");
        let support = directory.join("Support.kt");
        fs::write(&active, "val staleActive = 1").unwrap();
        fs::write(&blocked, "val staleBlocked = 1").unwrap();
        fs::write(&support, "val support = 1").unwrap();
        let mut module = Module::new(ModuleId::new(":", "main"), directory.clone());
        module.source_roots = vec![SourceRoot::source(directory.clone())];
        let model =
            ProjectModel::new(directory.clone(), ProviderKind::None).with_modules(vec![module]);
        let active_uri = file_uri(&active);
        let blocked_uri = file_uri(&blocked);
        let documents = [(active_uri.as_str(), "val active = 1")];
        let open_uris = [active_uri.as_str(), blocked_uri.as_str()];
        let mut sources = ProjectSources::default();

        let loaded = sources
            .load(&model, &documents, &open_uris, MAX_BYTES)
            .unwrap();

        fs::remove_dir_all(directory).ok();
        assert_eq!(
            loaded,
            [(file_uri(&support), "val support = 1".to_string())]
        );
    }

    #[test]
    fn oversized_sources_are_rejected_before_they_are_read() {
        let path = temp_path("oversized-support").with_extension("kt");
        let file = fs::File::create(&path).unwrap();
        file.set_len(MAX_BYTES as u64 + 1).unwrap();

        let result = load_documents(vec![path.clone()], MAX_BYTES, MAX_BYTES);

        fs::remove_file(path).ok();
        assert_eq!(result.unwrap_err(), size_limit_message(MAX_BYTES));
    }

    #[test]
    fn inventory_is_entry_bounded() {
        let directory = temp_path("support-inventory");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("First.kt"), "").unwrap();
        fs::write(directory.join("Second.kt"), "").unwrap();
        let mut remaining_entries = 1;

        let result = find_sources(&directory, false, &mut remaining_entries);

        fs::remove_dir_all(directory).ok();
        assert_eq!(result.unwrap_err(), inventory_limit_message());
    }

    #[test]
    fn broad_roots_skip_output_directories() {
        let directory = temp_path("support-output");
        fs::create_dir_all(directory.join("target/cache")).unwrap();
        fs::create_dir_all(directory.join("src")).unwrap();
        for index in 0..8 {
            fs::write(directory.join(format!("target/cache/{index}.kt")), "").unwrap();
        }
        fs::write(directory.join("src/Feature.kt"), "").unwrap();
        let mut remaining_entries = 3;

        let result = find_sources(&directory, true, &mut remaining_entries);

        fs::remove_dir_all(&directory).ok();
        assert_eq!(result.unwrap(), vec![directory.join("src/Feature.kt")]);
    }

    #[test]
    fn explicit_source_roots_keep_output_named_packages() {
        let directory = temp_path("source-package");
        fs::create_dir_all(directory.join("build")).unwrap();
        fs::write(directory.join("build/Feature.kt"), "").unwrap();
        let mut remaining_entries = 2;

        let result = find_sources(&directory, false, &mut remaining_entries);

        fs::remove_dir_all(&directory).ok();
        assert_eq!(result.unwrap(), vec![directory.join("build/Feature.kt")]);
    }

    #[test]
    fn unreadable_source_suppresses_semantic_diagnostics() {
        let path = temp_path("invalid-support").with_extension("kt");
        fs::write(&path, [0xff]).unwrap();

        let result = load_documents(vec![path.clone()], MAX_BYTES, MAX_BYTES);

        fs::remove_file(path).ok();
        assert_eq!(result.unwrap_err(), read_error_message());
    }
}
