//! The libraries `-Xfragment-dependency` and `-Xfragment-friend-dependency` give a common
//! fragment. kotlinc's session for that fragment loads each one as a metadata KLIB and reports what
//! it cannot load as `info: KLIB loader: …`. Since 2.5 an existing path that is not a KLIB (a jar
//! or a class directory) is that fragment's JVM classpath instead. A KLIB's declarations come from
//! the metadata of the JVM standard library already on the compilation classpath only when the KLIB
//! is the standard library's own common one; any other KLIB would need its declarations read, which
//! krusty does not do for a JVM compilation, so it is refused rather than left to resolve nothing.

use std::path::{Path, PathBuf};

use krusty::jvm::fragment_library::{classify_fragment_library, FragmentLibraryKind};
use krusty::kotlin_version::KotlinVersion;

use super::fragments::FragmentLibrary;

/// The `unique_name` of the standard library's common KLIB, whose declarations the JVM standard
/// library on the classpath already provides.
const COMMON_STDLIB: &str = "kotlin-stdlib-common";

/// What the fragments' libraries mean for the folded compilation.
#[derive(Debug, Default, Eq, PartialEq)]
pub(super) struct FragmentLibraries {
    /// kotlinc's `info:` lines, in loading order.
    pub(super) notes: Vec<String>,
    /// Libraries that are the JVM classpath of a common fragment.
    pub(super) classpath: Vec<PathBuf>,
    /// The entries of `classpath` named as friends, whose `internal` declarations are visible.
    pub(super) friends: Vec<PathBuf>,
    /// KLIBs krusty cannot read declarations from.
    pub(super) refusals: Vec<String>,
}

/// Load `libraries` as kotlinc's fragment sessions do. `release` is the Kotlin release being
/// reproduced: before 2.5 a fragment's session ignored what it could not load as a KLIB.
pub(super) fn load(libraries: &[FragmentLibrary], release: KotlinVersion) -> FragmentLibraries {
    let mut loaded = FragmentLibraries::default();
    for library in libraries {
        let path = Path::new(&library.path);
        match classify_fragment_library(path) {
            FragmentLibraryKind::Missing => loaded
                .notes
                .push(format!("KLIB loader: Library not found: {}", library.path)),
            FragmentLibraryKind::NotKlib => {
                loaded.notes.push(format!(
                    "KLIB loader: Not a Kotlin library, or a library with invalid format: {}",
                    library.path
                ));
                if release >= KotlinVersion::V2_5_0_RC && !library.hidden_from.is_empty() {
                    loaded.refusals.push(format!(
                        "krusty folds the fragments into one compilation, so it cannot keep the \
                         JVM classpath entry {} of fragment `{}` out of fragment(s) {} that do \
                         not refine it",
                        library.path,
                        library.fragment,
                        library.hidden_from.join(", ")
                    ));
                } else if release >= KotlinVersion::V2_5_0_RC {
                    if !loaded.classpath.iter().any(|seen| seen == path) {
                        loaded.classpath.push(path.to_path_buf());
                    }
                    if library.friend && !loaded.friends.iter().any(|seen| seen == path) {
                        loaded.friends.push(path.to_path_buf());
                    }
                }
            }
            FragmentLibraryKind::Klib { unique_name } => {
                if unique_name.as_deref() != Some(COMMON_STDLIB) {
                    loaded.refusals.push(format!(
                        "krusty does not read the declarations of the KLIB {} that fragment `{}` \
                         depends on; only the standard library's common KLIB is supported",
                        library.path, library.fragment
                    ));
                }
            }
        }
    }
    loaded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "krusty_fragment_libraries_{name}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch directory");
        dir
    }

    fn dependency(fragment: &str, path: &Path, friend: bool) -> FragmentLibrary {
        FragmentLibrary {
            fragment: fragment.to_string(),
            path: path.display().to_string(),
            friend,
            hidden_from: Vec::new(),
        }
    }

    /// An unpacked KLIB: the manifest and the module header are all `KlibArchive` reads here.
    fn klib(directory: &Path, unique_name: &str) -> PathBuf {
        let root = directory.join(unique_name);
        std::fs::create_dir_all(root.join("default/linkdata")).expect("create klib");
        std::fs::write(
            root.join("default/manifest"),
            format!("unique_name={unique_name}\n"),
        )
        .expect("write manifest");
        std::fs::write(root.join("default/linkdata/module"), b"").expect("write module");
        root
    }

    #[test]
    fn a_missing_library_is_reported_and_otherwise_ignored() {
        let missing = scratch("missing").join("nowhere/");
        let loaded = load(
            &[dependency("common", &missing, false)],
            KotlinVersion::newest(),
        );
        assert_eq!(
            loaded,
            FragmentLibraries {
                notes: vec![format!(
                    "KLIB loader: Library not found: {}",
                    missing.display()
                )],
                ..FragmentLibraries::default()
            }
        );
    }

    #[test]
    fn a_class_directory_or_jar_is_the_fragments_jvm_classpath_from_2_5() {
        let directory = scratch("classes");
        let jar = directory.join("dep.jar");
        std::fs::write(&jar, b"not a zip").expect("write jar");
        let friend = directory.join("internal");
        std::fs::create_dir_all(&friend).expect("create class directory");
        let libraries = [
            dependency("common", &directory, false),
            dependency("common", &jar, false),
            dependency("common", &friend, true),
        ];
        let invalid = |path: &Path| {
            format!(
                "KLIB loader: Not a Kotlin library, or a library with invalid format: {}",
                path.display()
            )
        };
        let notes = vec![invalid(&directory), invalid(&jar), invalid(&friend)];

        assert_eq!(
            load(&libraries, KotlinVersion::V2_5_0_RC),
            FragmentLibraries {
                notes: notes.clone(),
                classpath: vec![directory, jar, friend.clone()],
                friends: vec![friend],
                refusals: Vec::new(),
            }
        );
        // kotlinc 2.4 reported the same notes and left the library out of the fragment's session.
        assert_eq!(
            load(&libraries, KotlinVersion::V2_4_21),
            FragmentLibraries {
                notes,
                ..FragmentLibraries::default()
            }
        );
    }

    #[test]
    fn a_classpath_entry_hidden_from_a_source_fragment_is_refused() {
        let directory = scratch("hidden");
        let mut library = dependency("middle", &directory, false);
        library.hidden_from = vec!["common".to_string()];
        let loaded = load(&[library], KotlinVersion::V2_5_0_RC);
        assert_eq!(
            loaded,
            FragmentLibraries {
                notes: vec![format!(
                    "KLIB loader: Not a Kotlin library, or a library with invalid format: {}",
                    directory.display()
                )],
                refusals: vec![format!(
                    "krusty folds the fragments into one compilation, so it cannot keep the \
                     JVM classpath entry {} of fragment `middle` out of fragment(s) common that \
                     do not refine it",
                    directory.display()
                )],
                ..FragmentLibraries::default()
            }
        );
    }

    #[test]
    fn the_common_standard_library_klib_is_loaded_silently() {
        let directory = scratch("stdlib");
        let stdlib = klib(&directory, COMMON_STDLIB);
        assert_eq!(
            load(
                &[dependency("common", &stdlib, false)],
                KotlinVersion::newest()
            ),
            FragmentLibraries::default()
        );
    }

    #[test]
    fn another_klib_is_refused_because_its_declarations_are_not_read() {
        let directory = scratch("other");
        let other = klib(&directory, "other-library");
        assert_eq!(
            load(
                &[dependency("common", &other, true)],
                KotlinVersion::newest()
            ),
            FragmentLibraries {
                refusals: vec![format!(
                    "krusty does not read the declarations of the KLIB {} that fragment \
                     `common` depends on; only the standard library's common KLIB is supported",
                    other.display()
                )],
                ..FragmentLibraries::default()
            }
        );
    }
}
