//! `krusty compilation-fingerprint <kotlinc arguments>`: one digest of everything a JVM compilation
//! with those arguments reads, so a build tool can keep the previous output when it is unchanged.
//!
//! The inputs are the ones the compilation itself selects, through the same reading: the arguments
//! after `@argfile` expansion, the expanded source files, and every entry of the effective
//! classpath (the implicit standard library and the selected JDK image included) plus the companion
//! files [`JvmCompilationInputInventory`] selects from it, the friend paths, and the compiler plugin
//! jars. The compiler binary is an input too, and so is every environment variable in
//! [`OUTPUT_ENVIRONMENT`], by its presence and value.
//!
//! Every input enters by its contents, read afresh. A directory or a jar enters by
//! [`krusty_build::dependency_view`], so an upstream module's body-only edit leaves a dependent's
//! fingerprint unchanged whether it reaches the dependent as classes or as a jar; any other file
//! enters by its bytes; a missing entry as missing.
//!
//! `krusty classpath-fingerprint <kotlinc arguments>` takes the same reading of the classpath side
//! alone ([`classpath_fingerprint`]), for a build tool that already keys a compilation on its
//! sources, options and compiler and needs only a dependency key that ignores body-only edits.

use std::path::{Path, PathBuf};

use krusty::jvm::compilation_inputs::{
    is_archive_entry, JvmCompilationInputInventory, OUTPUT_ENVIRONMENT,
};
use krusty_build::dependency_view::{archive_contents_fingerprint, directory_fingerprint};
use krusty_build::{digest_contents, digest_file, digest_files, Digest, Hasher};

use crate::{cli, kotlinc_arguments};

/// Bumped whenever the inputs read here change, so a digest from an older compiler never matches.
const FORMAT: &str = "krusty compilation fingerprint 4";

/// [`FORMAT`] for [`classpath_fingerprint`].
const CLASSPATH_FORMAT: &str = "krusty classpath fingerprint 1";

/// The fingerprint of the compilation `argv` describes in this process's environment, or why it
/// cannot be taken. A command line the compilation itself would reject has no fingerprint.
pub fn fingerprint(argv: Vec<String>) -> Result<Digest, String> {
    fingerprint_in(argv, |name| std::env::var_os(name))
}

/// [`fingerprint`], reading the output-affecting environment variables through `variable`.
fn fingerprint_in(
    argv: Vec<String>,
    variable: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Result<Digest, String> {
    let (expanded, options) = parsed(argv)?;
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
    let files = std::iter::once(compiler.as_path())
        .chain(options.sources.iter().map(Path::new))
        .collect::<Vec<_>>();
    // A jar enters this fingerprint by its bytes; only `classpath-fingerprint` reads one by its
    // dependency view.
    let entry_reads = entries
        .iter()
        .map(|(_, entry)| (*entry, Reading::Bytes))
        .collect::<Vec<_>>();
    let (file_digests, entry_contents) = read(&files, &entry_reads, None)?;
    let (compiler_digest, source_digests) = file_digests
        .split_first()
        .expect("the compiler is read first");

    let mut hasher = Hasher::new();
    hasher.text("format", FORMAT);
    hasher.nested("compiler", *compiler_digest);
    hasher.count("environment", OUTPUT_ENVIRONMENT.len());
    for name in OUTPUT_ENVIRONMENT {
        hasher.text("variable", name);
        match variable(name) {
            Some(value) => hasher.field("set", value.as_encoded_bytes()),
            None => hasher.text("unset", ""),
        };
    }
    hasher.count("arguments", expanded.len());
    for argument in &expanded {
        hasher.text("argument", argument);
    }
    hasher.count("sources", options.sources.len());
    for (source, digest) in options.sources.iter().zip(source_digests) {
        hasher.text("source", source);
        hasher.nested("contents", *digest);
    }
    hasher.count("entries", entries.len());
    for ((role, entry), contents) in entries.iter().zip(entry_contents) {
        hasher.text(role, &entry.to_string_lossy());
        contents.absorb(&mut hasher);
    }
    Ok(hasher.finish())
}

/// The fingerprint of what a JVM compilation with `argv` observes of its dependencies: every entry
/// of the effective classpath, in lookup order, with the companion files selected from it, and
/// which of those entries are friends. Sources, options and the compiler are left to the caller,
/// which keys the compilation on them by its own means; a build tool that keys it on this digest
/// in place of the classpath files skips a dependent when a dependency's edit is body-only.
///
/// An entry enters by its contents, as in [`fingerprint`], except that a jar enters by its
/// [`archive_contents_fingerprint`], the view a class directory enters by: a dependency's body-only edit
/// leaves the digest unchanged whether it reaches this compilation as classes or as a jar. An
/// entry enters by its position, never by its path, so the digest is the same in another checkout.
/// A friend path enters by the position of the classpath entry it names, since that equality is how
/// the compilation applies it; one naming no entry enters by its contents.
///
/// Reading a jar's view parses every class in it. With `view_cache`, the view of each jar is kept
/// in that directory under the digest of the jar's bytes and of this compiler, so a classpath of
/// unchanged library jars is read once and then only digested.
pub fn classpath_fingerprint(
    argv: Vec<String>,
    view_cache: Option<&Path>,
) -> Result<Digest, String> {
    let (_, options) = parsed(argv)?;
    let effective_classpath = options.effective_classpath()?;
    let inventory = JvmCompilationInputInventory::from_effective_classpath(&effective_classpath);
    let classpath = inventory.effective_classpath();
    let implicit = inventory.implicit_files().collect::<Vec<_>>();
    let outside = options
        .friend_paths
        .iter()
        .filter(|friend| !classpath.contains(friend))
        .map(PathBuf::as_path)
        .collect::<Vec<_>>();
    let entries = classpath
        .iter()
        .map(|entry| (entry.as_path(), Reading::Dependency))
        .chain(implicit.iter().map(|file| (*file, Reading::Bytes)))
        .chain(outside.iter().map(|entry| (*entry, Reading::Dependency)))
        .collect::<Vec<_>>();
    let views = ArchiveViews::new(view_cache)?;
    let (_, contents) = read(&[], &entries, Some(&views))?;
    let mut contents = contents.into_iter();

    let mut hasher = Hasher::new();
    hasher.text("format", CLASSPATH_FORMAT);
    hasher.count("classpath", classpath.len());
    for entry in contents.by_ref().take(classpath.len()) {
        entry.absorb(&mut hasher);
    }
    hasher.count("implicit", implicit.len());
    for file in contents.by_ref().take(implicit.len()) {
        file.absorb(&mut hasher);
    }
    hasher.count("friends", options.friend_paths.len());
    for friend in &options.friend_paths {
        match classpath.iter().position(|entry| entry == friend) {
            Some(position) => {
                hasher.count("classpath entry", position);
            }
            None => {
                hasher.text("outside the classpath", "");
                contents
                    .next()
                    .expect("one read per friend outside the classpath")
                    .absorb(&mut hasher);
            }
        }
    }
    Ok(hasher.finish())
}

/// How a jar entry is read: as a dependency the compilation resolves against, by its
/// [`archive_contents_fingerprint`], or by its bytes (the JDK image, or a plugin jar, whose code runs). A
/// directory enters by its [`directory_fingerprint`] either way.
#[derive(Clone, Copy)]
enum Reading {
    Dependency,
    Bytes,
}

/// Bumped whenever a cached archive view's meaning changes, so an older cache entry never matches.
const VIEW_CACHE_FORMAT: &str = "krusty archive view cache 1";

/// The archive views of one fingerprint, read through an optional cache directory.
struct ArchiveViews {
    /// The cache directory and the digest of this compiler, whose class reader the views come from.
    cache: Option<(PathBuf, Digest)>,
}

impl ArchiveViews {
    fn new(cache: Option<&Path>) -> Result<Self, String> {
        let Some(cache) = cache else {
            return Ok(Self { cache: None });
        };
        let compiler = std::env::current_exe()
            .map_err(|error| format!("cannot locate the compiler binary: {error}"))?;
        let compiler = digest_file(&compiler)
            .map_err(|error| format!("cannot read {}: {error}", compiler.display()))?;
        Ok(Self {
            cache: Some((cache.to_path_buf(), compiler)),
        })
    }

    /// How the jar at `path` enters a fingerprint: by its [`archive_contents_fingerprint`], or by
    /// its bytes when the zip reader rejects it. The jar is read once, and both its cache key and
    /// its view come from those bytes, so a jar replaced while it is read cannot publish one
    /// version's view under the other's key. A cache entry is published by renaming a complete
    /// file into place, and an unreadable one is read afresh.
    fn contents(&self, path: &Path) -> std::io::Result<Contents> {
        let bytes = std::fs::read(path)?;
        let digest = digest_contents(&bytes);
        let view = || match archive_contents_fingerprint(&bytes) {
            Ok(view) => Ok(Some(view)),
            // An archive the zip reader rejects enters by its bytes, as a class file the class
            // reader rejects does: the key stays conservative, and the compilation reports it.
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => Ok(None),
            Err(error) => Err(error),
        };
        let view = match &self.cache {
            None => view()?,
            Some((cache, compiler)) => {
                let key = Hasher::new()
                    .text("format", VIEW_CACHE_FORMAT)
                    .nested("compiler", *compiler)
                    .nested("archive", digest)
                    .finish();
                let entry = cache.join(key.to_string());
                match std::fs::read_to_string(&entry)
                    .ok()
                    .and_then(|text| Digest::parse_hex(text.trim()))
                {
                    Some(view) => Some(view),
                    None => {
                        let view = view()?;
                        if let Some(view) = view {
                            std::fs::create_dir_all(cache)?;
                            let staging = cache.join(format!("{key}.{}.tmp", std::process::id()));
                            std::fs::write(&staging, view.to_string())?;
                            std::fs::rename(&staging, &entry)?;
                        }
                        view
                    }
                }
            }
        };
        Ok(view.map_or(Contents::File(digest), Contents::Archive))
    }
}

/// The digest of each of `files`, which must exist, and the [`Contents`] of each of `entries`, on
/// one budget of threads. Every file (the compiler, the sources, the JDK image and the jars) is one
/// queue of pieces on it; each directory then uses it in turn.
fn read(
    files: &[&Path],
    entries: &[(&Path, Reading)],
    views: Option<&ArchiveViews>,
) -> Result<(Vec<Digest>, Vec<Contents>), String> {
    #[derive(Clone, Copy)]
    enum Kind {
        Directory,
        Archive,
        File,
        Missing,
    }
    let kinds = entries
        .iter()
        .map(|&(entry, reading)| {
            if entry.is_dir() {
                Kind::Directory
            } else if views.is_some()
                && matches!(reading, Reading::Dependency)
                && entry.is_file()
                && is_archive_entry(entry)
            {
                Kind::Archive
            } else if entry.exists() {
                Kind::File
            } else {
                Kind::Missing
            }
        })
        .collect::<Vec<_>>();
    let workers = std::thread::available_parallelism().map_or(1, usize::from);
    let all_files = files
        .iter()
        .copied()
        .chain(
            entries
                .iter()
                .zip(&kinds)
                .filter(|(_, kind)| matches!(kind, Kind::File))
                .map(|((entry, _), _)| *entry),
        )
        .collect::<Vec<_>>();
    let mut digests =
        all_files
            .iter()
            .zip(digest_files(&all_files, workers))
            .map(|(file, digest)| {
                digest.map_err(|error| format!("cannot read {}: {error}", file.display()))
            });
    let file_digests = digests
        .by_ref()
        .take(files.len())
        .collect::<Result<Vec<_>, _>>()?;
    let archive_paths = entries
        .iter()
        .zip(&kinds)
        .filter(|(_, kind)| matches!(kind, Kind::Archive))
        .map(|((entry, _), _)| *entry)
        .collect::<Vec<_>>();
    let mut archives = match views {
        Some(views) => archive_contents(views, &archive_paths, workers),
        None => Vec::new(),
    }
    .into_iter();
    let contents = entries
        .iter()
        .zip(kinds)
        .map(|(&(entry, _), kind)| match kind {
            Kind::File => Ok(Contents::File(
                digests.next().expect("one digest per file")?,
            )),
            Kind::Directory => directory_fingerprint(entry)
                .map(Contents::Directory)
                .map_err(|error| format!("cannot read {}: {error}", entry.display())),
            Kind::Archive => archives.next().expect("one view per archive"),
            Kind::Missing => Ok(Contents::Missing),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((file_digests, contents))
}

/// The [`ArchiveViews::contents`] of each of `paths`, in order, on `workers` threads.
fn archive_contents(
    views: &ArchiveViews,
    paths: &[&Path],
    workers: usize,
) -> Vec<Result<Contents, String>> {
    let chunk = paths.len().div_ceil(workers.max(1)).max(1);
    std::thread::scope(|scope| {
        let handles = paths
            .chunks(chunk)
            .map(|chunk| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .map(|path| {
                            views
                                .contents(path)
                                .map_err(|error| format!("cannot read {}: {error}", path.display()))
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("an archive view worker panicked"))
            .collect()
    })
}

/// `argv` after `@argfile` expansion and the options it parses to, or every reason the compilation
/// would reject it.
fn parsed(argv: Vec<String>) -> Result<(Vec<String>, cli::Options), String> {
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
    Ok((expanded, options))
}

/// How an entry enters a fingerprint: a directory by its [`directory_fingerprint`], an archive by
/// its [`archive_contents_fingerprint`], any other file by its bytes, a missing entry as missing.
enum Contents {
    Directory(Digest),
    Archive(Digest),
    File(Digest),
    Missing,
}

impl Contents {
    fn absorb(self, hasher: &mut Hasher) {
        match self {
            Contents::Directory(digest) => hasher.nested("directory", digest),
            Contents::Archive(digest) => hasher.nested("archive", digest),
            Contents::File(digest) => hasher.nested("file", digest),
            Contents::Missing => hasher.text("missing", ""),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::{fingerprint, fingerprint_in};

    fn classpath_fingerprint(argv: Vec<String>) -> Result<krusty_build::Digest, String> {
        super::classpath_fingerprint(argv, None)
    }
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

    /// Turning per-class `@Metadata` off changes every emitted class, so with the command line and
    /// every file unchanged it still moves the fingerprint; so does its value, and unsetting it.
    #[test]
    fn output_affecting_environment_enters_the_fingerprint() {
        let scratch = Scratch::new("environment");
        let arguments = arguments(&scratch, &["-no-stdlib".to_string(), "-no-jdk".to_string()]);
        let with = |value: Option<&str>| {
            fingerprint_in(arguments.clone(), |name| {
                (name == "KRUSTY_NO_CLASS_METADATA")
                    .then_some(value)
                    .flatten()
                    .map(std::ffi::OsString::from)
            })
            .expect("fingerprint")
        };
        let unset = with(None);
        assert_eq!(with(None), unset);
        let set = with(Some("1"));
        assert_ne!(set, unset, "setting it");
        assert_ne!(with(Some("")), unset, "setting it empty");
        assert_ne!(with(Some("yes")), set, "its value");
    }

    fn classpath_arguments(classpath: &[&Path], friends: &[&Path]) -> Vec<String> {
        let mut arguments = vec!["-no-stdlib".to_string(), "-no-jdk".to_string()];
        let joined = std::env::join_paths(classpath).expect("join the classpath");
        arguments.push("-classpath".to_string());
        arguments.push(joined.to_string_lossy().into_owned());
        if !friends.is_empty() {
            let friends = friends
                .iter()
                .map(|friend| friend.display().to_string())
                .collect::<Vec<_>>();
            arguments.push(format!("-Xfriend-paths={}", friends.join(",")));
        }
        arguments
    }

    /// The classpath fingerprint reads entries by contents and position: the same dependencies in
    /// another checkout give the same digest, and a changed byte or a changed order moves it.
    #[test]
    fn the_classpath_enters_by_contents_and_order_not_by_path() {
        let scratch = Scratch::new("classpath");
        let jar = |root: &str, name: &str, contents: &[u8]| {
            scratch.write(&format!("{root}/{name}"), contents)
        };
        let one = jar("one", "a.jar", b"jar-a");
        let two = jar("one", "b.jar", b"jar-b");
        let base =
            classpath_fingerprint(classpath_arguments(&[&one, &two], &[])).expect("fingerprint");
        let elsewhere = [jar("two", "a.jar", b"jar-a"), jar("two", "b.jar", b"jar-b")];
        assert_eq!(
            classpath_fingerprint(classpath_arguments(&[&elsewhere[0], &elsewhere[1]], &[]))
                .expect("fingerprint"),
            base,
            "another checkout"
        );
        assert_ne!(
            classpath_fingerprint(classpath_arguments(&[&two, &one], &[])).expect("fingerprint"),
            base,
            "the lookup order"
        );
        replace_keeping_size_and_time(&two, b"jar-c");
        assert_ne!(
            classpath_fingerprint(classpath_arguments(&[&one, &two], &[])).expect("fingerprint"),
            base,
            "a changed byte"
        );
    }

    /// A friend path names a classpath entry; which entry it names moves the classpath fingerprint
    /// even when the entries' contents are the same, and so does a friend outside the classpath.
    #[test]
    fn friends_enter_by_the_entry_they_name() {
        let scratch = Scratch::new("friends");
        let first = scratch.write("first.jar", b"same");
        let second = scratch.write("second.jar", b"same");
        let outside = scratch.write("outside.jar", b"outside");
        let with = |friends: &[&Path]| {
            classpath_fingerprint(classpath_arguments(&[&first, &second], friends))
                .expect("fingerprint")
        };
        let none = with(&[]);
        let names_first = with(&[&first]);
        assert_ne!(names_first, none, "a friend");
        assert_ne!(
            with(&[&second]),
            names_first,
            "which entry the friend names"
        );
        let names_outside = with(&[&outside]);
        assert_ne!(names_outside, none, "a friend outside the classpath");
        replace_keeping_size_and_time(&outside, b"changed");
        assert_ne!(with(&[&outside]), names_outside, "its contents");
    }

    /// Writes a jar holding `entries`, in that order, each stored with `time`.
    fn write_jar(path: &Path, entries: &[(&str, &[u8])], time: zip::DateTime) {
        let mut writer = zip::ZipWriter::new(std::fs::File::create(path).expect("create a jar"));
        let options = zip::write::SimpleFileOptions::default().last_modified_time(time);
        for (name, contents) in entries {
            writer.start_file(*name, options).expect("start an entry");
            std::io::Write::write_all(&mut writer, contents).expect("write an entry");
        }
        writer.finish().expect("finish the jar");
    }

    /// A jar on the classpath enters by its entries, read as a class directory's files are: the
    /// same entries in another order or with other timestamps give the same digest, through the
    /// view cache as without it, and a changed entry moves it.
    #[test]
    fn a_jar_enters_by_its_entries() {
        let scratch = Scratch::new("jar-entries");
        let time = |year| zip::DateTime::from_date_and_time(year, 1, 1, 0, 0, 0).expect("a time");
        let first = scratch.0.join("first.jar");
        let second = scratch.0.join("second.jar");
        let changed = scratch.0.join("changed.jar");
        write_jar(&first, &[("a.txt", b"a"), ("b/c.txt", b"c")], time(2001));
        write_jar(&second, &[("b/c.txt", b"c"), ("a.txt", b"a")], time(2024));
        write_jar(&changed, &[("a.txt", b"a"), ("b/c.txt", b"d")], time(2001));
        assert_ne!(
            std::fs::read(&first).expect("read"),
            std::fs::read(&second).expect("read")
        );
        let cache = scratch.0.join("views");
        let digest = |jar: &Path, cache: Option<&Path>| {
            super::classpath_fingerprint(classpath_arguments(&[jar], &[]), cache)
                .expect("fingerprint")
        };
        let base = digest(&first, None);
        assert_eq!(digest(&second, None), base, "the archive's layout");
        assert_ne!(digest(&changed, None), base, "an entry's contents");
        assert_eq!(digest(&first, Some(&cache)), base, "a cache miss");
        assert_eq!(digest(&first, Some(&cache)), base, "a cache hit");
        assert_eq!(digest(&second, Some(&cache)), base, "another jar's miss");
        assert_ne!(digest(&changed, Some(&cache)), base, "a changed jar");

        // An entry that does not hold a view is read afresh and replaced.
        for entry in std::fs::read_dir(&cache).expect("list the cache") {
            std::fs::write(entry.expect("an entry").path(), b"torn").expect("tear an entry");
        }
        assert_eq!(digest(&first, Some(&cache)), base, "a torn entry");
    }

    /// A jar replaced at its path and then restored is keyed and viewed from the bytes of each
    /// read: the cache holds each version's view under that version's digest, so restoring the
    /// first version reads its own view back, never the replacement's.
    #[test]
    fn a_replaced_jar_never_takes_another_versions_view() {
        let scratch = Scratch::new("jar-replaced");
        let time = zip::DateTime::from_date_and_time(2001, 1, 1, 0, 0, 0).expect("a time");
        let jar = scratch.0.join("lib.jar");
        let cache = scratch.0.join("views");
        let views = super::ArchiveViews::new(Some(&cache)).expect("open the view cache");
        let view = |bytes: &[u8]| {
            krusty_build::dependency_view::archive_contents_fingerprint(bytes).expect("a view")
        };
        let read = |views: &super::ArchiveViews| match views.contents(&jar).expect("read the jar") {
            super::Contents::Archive(view) => view,
            _ => panic!("the jar enters by its view"),
        };
        write_jar(&jar, &[("a.txt", b"first")], time);
        let first = std::fs::read(&jar).expect("read");
        assert_eq!(read(&views), view(&first), "the first version's miss");
        write_jar(&jar, &[("a.txt", b"other")], time);
        let second = std::fs::read(&jar).expect("read");
        assert_eq!(
            first.len(),
            second.len(),
            "the versions differ only in contents"
        );
        assert_eq!(read(&views), view(&second), "the replacement's miss");
        std::fs::write(&jar, &first).expect("restore the first version");
        assert_eq!(read(&views), view(&first), "the restored version's hit");
        let entries = std::fs::read_dir(&cache).expect("list the cache").count();
        assert_eq!(entries, 2, "one entry per version");
    }

    /// A classpath file that is no archive the zip reader accepts enters by its bytes.
    #[test]
    fn an_unreadable_jar_enters_by_its_bytes() {
        let scratch = Scratch::new("jar-unreadable");
        let jar = scratch.write("broken.jar", b"not a zip");
        let base = super::classpath_fingerprint(classpath_arguments(&[&jar], &[]), None)
            .expect("fingerprint");
        replace_keeping_size_and_time(&jar, b"not a jar");
        assert_ne!(
            super::classpath_fingerprint(classpath_arguments(&[&jar], &[]), None)
                .expect("fingerprint"),
            base
        );
    }

    #[test]
    fn a_rejected_command_line_has_no_fingerprint() {
        let scratch = Scratch::new("rejected");
        let arguments = arguments(&scratch, &["-Xfragment-refines=jvm:common".to_string()]);
        assert!(fingerprint(arguments).is_err());
    }
}
