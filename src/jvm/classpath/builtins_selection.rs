//! Selection and provenance of one package's `.kotlin_builtins` fragment.
//!
//! Parsed bytes are reusable across classpaths. Compiler authority is not: it belongs to the exact
//! selected input entry that is this compilation's stdlib.

use super::*;

impl Classpath {
    /// A parsed `.kotlin_builtins` fragment by package id (class internal-name id →
    /// supertypes+members), together with the exact input that won classpath selection.
    pub(super) fn try_selected_builtins_file_for_package(
        &self,
        package: TypeName,
    ) -> Result<SelectedBuiltinsFile, std::sync::Arc<BuiltinsLoadError>> {
        let tree = self.package_tree();
        let catalog_complete = tree.incomplete_entries.is_empty();
        if catalog_complete {
            if let Some(selected) = self.builtins.borrow().get(&package) {
                return Ok(selected.clone());
            }
        }
        let path = Self::builtins_path_for_package(package);
        let declared_indices = tree
            .node_for_name(package)
            .map_or_else(Vec::new, |node| node.builtins_jars.clone());
        let mut indices = declared_indices.clone();
        indices.extend(tree.incomplete_entries.iter().copied());
        indices.sort_unstable();
        indices.dedup();
        let mut found = None;
        for i in indices {
            let Some(entry) = self.entries.get(i) else {
                continue;
            };
            let declared = declared_indices.contains(&i);
            // Per-entry global cache first: the parse of one entry's fragment is independent of the
            // classpath composition. The classpath-order walk still decides which entry wins.
            let global = if catalog_complete {
                self.entry_builtins_caches[i].as_ref()
            } else {
                None
            };
            let global_slot = global.map(|cache| {
                cache
                    .write()
                    .unwrap()
                    .entry(package)
                    .or_insert_with(|| std::sync::Arc::new(std::sync::Mutex::new(None)))
                    .clone()
            });
            let read_and_parse = || {
                // Split permanent absence (cacheable) from a failed read: publishing a transient
                // failure as process-global absence would strip builtins from later compilations.
                let read = match entry {
                    Entry::Dir(dir) => match std::fs::read(dir.join(&path)) {
                        Ok(bytes) => EntryReadResult::Data(bytes),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                            EntryReadResult::Absent
                        }
                        Err(error) => EntryReadResult::Failed(error.to_string()),
                    },
                    Entry::Jar(jar) => self.jar_entry_read(jar, &path),
                    Entry::Jimage(_) | Entry::CtSym { .. } => EntryReadResult::Absent,
                };
                match read {
                    EntryReadResult::Data(bytes) => {
                        match super::super::metadata::parse_builtins(&bytes) {
                            Ok(decoded) => (
                                Ok(Some(std::sync::Arc::new(BuiltinsFile::from_package(
                                    decoded,
                                )))),
                                true,
                            ),
                            Err(error) => (
                                Err(std::sync::Arc::new(BuiltinsLoadError::Decode {
                                    entry: entry.path().to_path_buf(),
                                    resource: path.clone(),
                                    source: error,
                                })),
                                true,
                            ),
                        }
                    }
                    EntryReadResult::Absent if declared => (
                        Err(std::sync::Arc::new(BuiltinsLoadError::Read {
                            entry: entry.path().to_path_buf(),
                            resource: path.clone(),
                            detail: "selected builtins resource is absent".to_string(),
                        })),
                        false,
                    ),
                    EntryReadResult::Absent => (Ok(None), true),
                    EntryReadResult::Failed(error) if declared => (
                        Err(std::sync::Arc::new(BuiltinsLoadError::Read {
                            entry: entry.path().to_path_buf(),
                            resource: path.clone(),
                            detail: error,
                        })),
                        false,
                    ),
                    // A catalog-incomplete entry was speculative. Its read failure is already
                    // reported by the classpath diagnostic and does not select this resource.
                    EntryReadResult::Failed(_) => (Ok(None), false),
                }
            };
            let parsed = match global_slot {
                Some(slot) => {
                    let mut cached = slot.lock().unwrap();
                    if let Some(hit) = cached.as_ref() {
                        hit.clone()
                    } else {
                        let (parsed, cacheable) = read_and_parse();
                        if cacheable {
                            *cached = Some(parsed.clone());
                        }
                        parsed
                    }
                }
                None => read_and_parse().0,
            };
            match parsed? {
                Some(file) => {
                    found = Some((i, file));
                    break;
                }
                None => continue,
            }
        }
        let selected = found.map_or_else(
            || SelectedBuiltinsFile {
                owner: None,
                file: std::sync::Arc::new(BuiltinsFile::from_package(
                    super::super::metadata::BuiltinPackage::default(),
                )),
            },
            |(owner, file)| SelectedBuiltinsFile {
                file,
                owner: Some(owner),
            },
        );
        if catalog_complete {
            self.builtins.borrow_mut().insert(package, selected.clone());
        }
        Ok(selected)
    }

    fn selected_builtins_file_for_package(&self, package: TypeName) -> SelectedBuiltinsFile {
        self.try_selected_builtins_file_for_package(package)
            .unwrap_or_else(|error| panic!("validated Kotlin builtins became unreadable: {error}"))
    }

    pub(super) fn builtins_file_for_package(
        &self,
        package: TypeName,
    ) -> std::sync::Arc<BuiltinsFile> {
        self.selected_builtins_file_for_package(package).file
    }

    /// Selected declaration bytes and whether that exact input is the compilation's stdlib.
    pub(super) fn compiler_builtin_package(
        &self,
        package: TypeName,
    ) -> (std::sync::Arc<BuiltinsFile>, bool) {
        let selected = self.selected_builtins_file_for_package(package);
        let compiler_builtin_owner = self
            .builtins_stdlib_entry()
            .is_some_and(|expected| selected.owner == Some(expected));
        (selected.file, compiler_builtin_owner)
    }

    /// The input whose `.kotlin_builtins` declarations are the compiler's builtins. A Kotlin
    /// distribution's stdlib is identified by its paired common KLIB, so an earlier copy cannot
    /// claim it. A dependency-resolved stdlib (`kotlin-stdlib-2.3.20.jar` from a repository) has no
    /// sibling KLIB; like kotlinc, its builtins are then those of the entry that wins selection of
    /// the `kotlin` package fragment.
    fn builtins_stdlib_entry(&self) -> Option<usize> {
        if self.common_expectation_klib().is_some() {
            return self.paired_stdlib_entry();
        }
        self.selected_builtins_file_for_package(crate::types::wk::kotlin_package())
            .owner
    }
}
