//! `krusty compilation-fingerprint <kotlinc arguments>`: one digest of everything a JVM compilation
//! with those arguments reads, so a build tool can keep the previous output when it is unchanged.
//!
//! The inputs are the ones the compilation itself selects, through the same reading: the arguments
//! after `@argfile` expansion, the expanded source files, and every entry of the effective
//! classpath (the implicit standard library and the selected JDK image included) plus the companion
//! files [`JvmCompilationInputInventory`] selects from it, the friend paths, and the compiler plugin
//! jars. The compiler binary is an input too.
//!
//! Every input enters by its contents, read afresh. A directory enters by
//! [`krusty_build::dependency_view`], so an upstream module's body-only edit leaves a dependent's
//! fingerprint unchanged; a file enters by its bytes; a missing entry as missing.

use std::path::Path;

use krusty::jvm::compilation_inputs::JvmCompilationInputInventory;
use krusty_build::dependency_view::directory_fingerprint;
use krusty_build::{digest_files, Digest, Hasher};

use crate::{cli, kotlinc_arguments};

/// Bumped whenever the inputs read here change, so a digest from an older compiler never matches.
const FORMAT: &str = "krusty compilation fingerprint 2";

/// The fingerprint of the compilation `argv` describes, or why it cannot be taken. A command line
/// the compilation itself would reject has no fingerprint.
pub fn fingerprint(argv: Vec<String>) -> Result<Digest, String> {
    let mut argfile_problems = Vec::new();
    let expanded = kotlinc_arguments::argfile::expand(argv.clone(), &mut argfile_problems);
    let options = cli::parse(argv);
    let rejected = argfile_problems
        .iter()
        .chain(&options.argument_errors)
        .chain(&options.errors)
        .chain(&options.configuration_errors)
        .chain(&options.plugins.errors)
        .cloned()
        .collect::<Vec<_>>();
    if !rejected.is_empty() {
        return Err(rejected.join("\n"));
    }
    let effective_classpath = options.effective_classpath()?;
    let inventory = JvmCompilationInputInventory::from_effective_classpath(&effective_classpath);

    let compiler = std::env::current_exe()
        .map_err(|error| format!("cannot locate the compiler binary: {error}"))?;
    let plugins = &options.plugins;
    let entries = inventory
        .effective_classpath()
        .iter()
        .map(|entry| ("classpath", entry.as_path()))
        .chain(inventory.implicit_files().map(|file| ("implicit", file)))
        .chain(
            options
                .friend_paths
                .iter()
                .map(|entry| ("friend", entry.as_path())),
        )
        .chain(
            plugins
                .plugin_jars
                .iter()
                .map(|jar| ("plugin", Path::new(jar))),
        )
        .chain(
            plugins
                .compiler_plugins
                .iter()
                .flat_map(|plugin| plugin.classpath.iter())
                .map(|jar| ("compiler-plugin", Path::new(jar))),
        )
        .collect::<Vec<_>>();
    let mut entry_contents = entries
        .iter()
        .map(|(_, entry)| {
            if entry.is_dir() {
                Contents::Directory(None)
            } else if entry.exists() {
                Contents::File(None)
            } else {
                Contents::Missing
            }
        })
        .collect::<Vec<_>>();

    // One budget of threads for the whole fingerprint. Every file (the compiler, the sources, the
    // JDK image and the jars) is one queue of pieces on it; each directory then uses it in turn.
    let workers = std::thread::available_parallelism().map_or(1, usize::from);
    let entry_files = entries
        .iter()
        .zip(&entry_contents)
        .filter(|(_, contents)| matches!(contents, Contents::File(_)))
        .map(|((_, entry), _)| *entry);
    let files = std::iter::once(compiler.as_path())
        .chain(options.sources.iter().map(Path::new))
        .chain(entry_files)
        .collect::<Vec<_>>();
    let mut file_digests = files
        .iter()
        .zip(digest_files(&files, workers))
        .map(|(file, digest)| {
            digest.map_err(|error| format!("cannot read {}: {error}", file.display()))
        });
    let mut next_file = || file_digests.next().expect("one digest per file");
    let compiler_digest = next_file()?;
    let source_digests = options
        .sources
        .iter()
        .map(|_| next_file())
        .collect::<Result<Vec<_>, _>>()?;
    for ((_, entry), contents) in entries.iter().zip(&mut entry_contents) {
        match contents {
            Contents::File(digest) => *digest = Some(next_file()?),
            Contents::Directory(digest) => {
                *digest = Some(
                    directory_fingerprint(entry)
                        .map_err(|error| format!("cannot read {}: {error}", entry.display()))?,
                )
            }
            Contents::Missing => {}
        }
    }

    let mut hasher = Hasher::new();
    hasher.text("format", FORMAT);
    hasher.nested("compiler", compiler_digest);
    hasher.count("arguments", expanded.len());
    for argument in &expanded {
        hasher.text("argument", argument);
    }
    hasher.count("sources", options.sources.len());
    for (source, digest) in options.sources.iter().zip(source_digests) {
        hasher.text("source", source);
        hasher.nested("contents", digest);
    }
    hasher.count("entries", entries.len());
    for ((role, entry), contents) in entries.iter().zip(entry_contents) {
        hasher.text(role, &entry.to_string_lossy());
        match contents {
            Contents::Directory(digest) => hasher.nested("directory", digest.expect("read")),
            Contents::File(digest) => hasher.nested("file", digest.expect("read")),
            Contents::Missing => hasher.text("missing", ""),
        };
    }
    Ok(hasher.finish())
}

/// How an entry enters the fingerprint, and once read, its digest.
enum Contents {
    Directory(Option<Digest>),
    File(Option<Digest>),
    Missing,
}

#[cfg(test)]
mod tests {
    use super::fingerprint;
    use std::path::{Path, PathBuf};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "krusty-compilation-fingerprint-{tag}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create the scratch directory");
            Self(path)
        }

        fn write(&self, relative: &str, contents: &[u8]) -> PathBuf {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("create a parent");
            std::fs::write(&path, contents).expect("write a file");
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Rewrites `path` with same-sized bytes and its previous modification time.
    fn replace_keeping_size_and_time(path: &Path, contents: &[u8]) {
        let modified = std::fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .expect("read the modification time");
        assert_eq!(
            std::fs::metadata(path).unwrap().len(),
            contents.len() as u64
        );
        std::fs::write(path, contents).expect("rewrite the file");
        std::fs::File::options()
            .write(true)
            .open(path)
            .and_then(|file| file.set_modified(modified))
            .expect("restore the modification time");
    }

    fn arguments(scratch: &Scratch, extra: &[String]) -> Vec<String> {
        let source = scratch.write("src/A.kt", b"package p\nfun a() = 1\n");
        let mut arguments = vec![
            source.display().to_string(),
            "-d".to_string(),
            scratch.0.join("out").display().to_string(),
        ];
        arguments.extend_from_slice(extra);
        arguments
    }

    /// The JDK image and the standard library's sibling KLIB are read without being named; a
    /// changed byte in either moves the fingerprint even when its size and time are kept.
    #[test]
    fn implicit_inputs_enter_by_their_contents() {
        let scratch = Scratch::new("implicit");
        scratch.write("jdk/release", b"JAVA_VERSION=\"25.0.1\"\n");
        let modules = scratch.write("jdk/lib/modules", b"image-one");
        let stdlib = scratch.write("kotlin/kotlin-stdlib.jar", b"stdlib");
        let klib = scratch.write("kotlin/kotlin-stdlib-wasm-js.klib", b"klib-one");
        let arguments = arguments(
            &scratch,
            &[
                "-no-stdlib".to_string(),
                format!("-jdk-home={}", scratch.0.join("jdk").display()),
                format!("-classpath={}", stdlib.display()),
            ],
        );
        let base = fingerprint(arguments.clone()).expect("fingerprint");
        assert_eq!(fingerprint(arguments.clone()).expect("fingerprint"), base);

        replace_keeping_size_and_time(&modules, b"image-two");
        let changed_image = fingerprint(arguments.clone()).expect("fingerprint");
        assert_ne!(changed_image, base, "the JDK image");

        replace_keeping_size_and_time(&klib, b"klib-two");
        assert_ne!(
            fingerprint(arguments).expect("fingerprint"),
            changed_image,
            "the standard library's companion KLIB"
        );
    }

    /// An argument file's arguments are inputs, so editing it moves the fingerprint.
    #[test]
    fn argument_files_are_read() {
        let scratch = Scratch::new("argfile");
        let argfile = scratch.write("options.args", b"-module-name=one\n");
        let arguments = arguments(
            &scratch,
            &[
                "-no-stdlib".to_string(),
                "-no-jdk".to_string(),
                format!("@{}", argfile.display()),
            ],
        );
        let base = fingerprint(arguments.clone()).expect("fingerprint");
        replace_keeping_size_and_time(&argfile, b"-module-name=two\n");
        assert_ne!(fingerprint(arguments).expect("fingerprint"), base);
    }

    #[test]
    fn a_rejected_command_line_has_no_fingerprint() {
        let scratch = Scratch::new("rejected");
        let arguments = arguments(&scratch, &["-Xfragment-refines=jvm:common".to_string()]);
        assert!(fingerprint(arguments).is_err());
    }
}
