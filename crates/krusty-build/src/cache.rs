//! The cache key.
//!
//! A wrong cache hit ships a wrong binary with no diagnostic — the highest-severity failure mode in
//! the build design, and the one that is cheapest to get wrong. So the rule here is: **every input
//! that can change emitted bytes is in the key by default.** An input leaves the key only with a
//! test demonstrating output invariance under its change.
//!
//! The obvious key — compiler version, source hashes, dependency ABI hashes, target — collides in
//! at least seven ways, each verified against this tree. Every field below exists because of one:
//!
//! | Field | Why the obvious key was wrong |
//! |---|---|
//! | [`CacheKeyInputs::sources`] | Source ORDER and BASENAMES reach the bytes. A package's facade-name list accumulates in file-streaming order (`JvmState::module_packages`), so reordering changes `.kotlin_module`; and the CLI derives class-naming stems from `file_stem(path)`. Hence an ordered list of `(path, content)`, not a sorted multiset of contents. |
//! | [`CacheKeyInputs::classpath`] | Entry PATHS are semantically load-bearing, not just contents: `SerializationAbi::from_classpath` picks the `write$Self` versus `write$Self$<module>` mangling by parsing a jar's FILE NAME. Content hashes alone would collide across a rename. |
//! | [`CacheKeyInputs::compiler_flags`] | `-module-name` reaches `@Metadata.classModuleName`, the `META-INF/<module>.kotlin_module` file name, and that same serialization mangle. |
//! | [`CacheKeyInputs::friend_paths`] | Friendship is exact path-set membership (`src/jvm/classpath.rs`), so adding or removing an associate changes which `internal` declarations are visible — changing both acceptance and emission. |
//! | [`CacheKeyInputs::environment`] | `KRUSTY_NO_CLASS_METADATA` switches off per-class `@Metadata` outright; `KRUSTY_LANGUAGE_VERSION` selects the reference version. Both change emitted bytes from outside the command line. |
//! | [`CacheKeyInputs::jdk_identity`] | `-jvm-target` sets the class-file major, but the BOOTCLASSPATH decides what `java.*` resolves to and what supertypes look like. JDK 17 and 21 compile the same sources differently. |
//! | [`CacheKeyInputs::plugins`] | KSP processors are external jars that GENERATE sources (`src/plugins/ksp.rs` runs a fixpoint), and `KspToolchain` is build-resolved — so processor identity is a compile input the build layer alone can see. |
//!
//! One thing deliberately NOT reused: `src/jvm/classpath.rs`'s `path_identity`, which mixes
//! dev/ino/ctime/mtime. That is correct for an in-process memo and wrong for a build cache — a
//! `git checkout` restoring identical bytes changes ctime, so it would miss every cache hit while
//! also failing to notice a same-timestamp content change on a coarse filesystem.

use std::path::{Path, PathBuf};

use crate::abi::AbiFingerprint;
use crate::digest::{digest_bytes, Digest, Hasher};
use crate::model::ModuleId;

/// Environment variables known to change emitted output. Named explicitly rather than hashing the
/// whole environment, which would make every key machine-specific and defeat sharing.
///
/// `JAVA_HOME` is absent on purpose: it selects a JDK, and the JDK is keyed by
/// [`CacheKeyInputs::jdk_identity`] — its actual contents — rather than by the path that found it.
pub const OUTPUT_AFFECTING_ENVIRONMENT: &[&str] =
    &["KRUSTY_NO_CLASS_METADATA", "KRUSTY_LANGUAGE_VERSION"];

/// A file identified by BOTH its path and its content.
///
/// Both halves are load-bearing and for different reasons: content because that is what the
/// compiler reads, path because jar file names select code paths (see the module table) and source
/// basenames name emitted classes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDigest {
    pub path: PathBuf,
    /// Digest of the file's bytes.
    pub content: Digest,
}

impl FileDigest {
    pub fn new(path: impl Into<PathBuf>, content: Digest) -> Self {
        Self {
            path: path.into(),
            content,
        }
    }

    /// Digest the given bytes as this path's content.
    pub fn of_bytes(path: impl Into<PathBuf>, bytes: &[u8]) -> Self {
        Self::new(path, digest_bytes(bytes))
    }

    /// Digest a file on disk.
    pub fn of_file(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref();
        let bytes = std::fs::read(path)?;
        Ok(Self::of_bytes(path, &bytes))
    }

    fn render(&self, out: &mut String) {
        out.push_str(&format!("{}:{}\n", self.path.display(), self.content));
    }

    fn absorb(&self, hasher: &mut Hasher, tag: &str) {
        hasher.field(tag, self.path.as_os_str().as_encoded_bytes());
        hasher.nested("content", self.content);
    }
}

/// Everything that decides a module's output.
///
/// Ordered `Vec`s throughout, never sets: where order does not matter the caller may sort, but the
/// key must be able to express that it does.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CacheKeyInputs {
    /// Compiler identity — version plus a build id, so a rebuilt compiler with the same version
    /// string does not silently reuse artifacts from the old one.
    pub compiler: String,
    /// Canonical compiler flags, in a stable order, including `-module-name`, `-jvm-target`,
    /// `-Xjvm-default`, opt-ins and language version.
    pub compiler_flags: Vec<String>,
    /// Present allowlisted environment entries as `(name, raw OS bytes)`. Absence, an empty value,
    /// and non-UTF-8 values remain distinct. See [`OUTPUT_AFFECTING_ENVIRONMENT`].
    pub environment: Vec<(String, Vec<u8>)>,
    /// JDK identity: its version string plus a digest of `lib/modules` (or the `ct.sym` release).
    pub jdk_identity: String,
    /// Sources in compile order.
    pub sources: Vec<FileDigest>,
    /// Compile classpath in build-tool order.
    pub classpath: Vec<FileDigest>,
    /// Friend outputs, whose `internal` declarations this module may see.
    pub friend_paths: Vec<FileDigest>,
    /// Direct dependencies' ABI fingerprints. ABI hashes rather than source hashes is what makes
    /// avoidance transitive, and is precisely what krusty lacks today.
    pub dependency_abis: Vec<(ModuleId, AbiFingerprint)>,
    /// Plugin/processor jars.
    pub plugins: Vec<FileDigest>,
    /// Plugin/processor options, in declaration order.
    pub plugin_options: Vec<String>,
    /// Target triple, or the JVM target for a JVM build.
    pub target: String,
}

impl CacheKeyInputs {
    /// Read the allowlisted environment from the current process.
    pub fn environment_from_process() -> Vec<(String, Vec<u8>)> {
        environment_from_lookup(|name| std::env::var_os(name))
    }

    /// Canonical rendering. The key is a hash of this, so it is also what a human reads when asking
    /// why two builds that "look the same" did not share a cache entry.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("compiler\n{}\n", self.compiler));
        out.push_str("flags\n");
        for flag in &self.compiler_flags {
            out.push_str(flag);
            out.push('\n');
        }
        out.push_str("environment\n");
        for (name, value) in &self.environment {
            out.push_str(&format!("{name}=0x{}\n", hex_bytes(value)));
        }
        out.push_str(&format!("jdk\n{}\n", self.jdk_identity));
        out.push_str("sources\n");
        for source in &self.sources {
            source.render(&mut out);
        }
        out.push_str("classpath\n");
        for entry in &self.classpath {
            entry.render(&mut out);
        }
        out.push_str("friends\n");
        for entry in &self.friend_paths {
            entry.render(&mut out);
        }
        out.push_str("dependencies\n");
        for (id, abi) in &self.dependency_abis {
            out.push_str(&format!("{id}={abi}\n"));
        }
        out.push_str("plugins\n");
        for plugin in &self.plugins {
            plugin.render(&mut out);
        }
        for option in &self.plugin_options {
            out.push_str(&format!("option:{option}\n"));
        }
        out.push_str(&format!("target\n{}\n", self.target));
        out
    }

    /// The key.
    ///
    /// Hashes length-delimited fields rather than [`Self::render`]: the rendering exists so a human
    /// can see why two builds differ, and is NOT an injective encoding — a flag or path containing
    /// a newline could render exactly like two of them. See `crate::digest`.
    pub fn key(&self) -> CacheKey {
        let mut hasher = Hasher::new();
        hasher.text("compiler", &self.compiler);
        hasher.count("flags", self.compiler_flags.len());
        for flag in &self.compiler_flags {
            hasher.text("flag", flag);
        }
        hasher.count("environment", self.environment.len());
        for (name, value) in &self.environment {
            hasher.text("env-name", name);
            hasher.field("env-value", value);
        }
        hasher.text("jdk", &self.jdk_identity);
        hasher.count("sources", self.sources.len());
        for source in &self.sources {
            source.absorb(&mut hasher, "source");
        }
        hasher.count("classpath", self.classpath.len());
        for entry in &self.classpath {
            entry.absorb(&mut hasher, "classpath");
        }
        hasher.count("friends", self.friend_paths.len());
        for entry in &self.friend_paths {
            entry.absorb(&mut hasher, "friend");
        }
        hasher.count("dependencies", self.dependency_abis.len());
        for (id, abi) in &self.dependency_abis {
            hasher.text("dependency", id.as_str());
            hasher.nested("abi", abi.digest());
        }
        hasher.count("plugins", self.plugins.len());
        for plugin in &self.plugins {
            plugin.absorb(&mut hasher, "plugin");
        }
        hasher.count("plugin-options", self.plugin_options.len());
        for option in &self.plugin_options {
            hasher.text("plugin-option", option);
        }
        hasher.text("target", &self.target);
        CacheKey(hasher.finish())
    }
}

fn environment_from_lookup(
    mut lookup: impl FnMut(&str) -> Option<std::ffi::OsString>,
) -> Vec<(String, Vec<u8>)> {
    OUTPUT_AFFECTING_ENVIRONMENT
        .iter()
        .filter_map(|name| {
            lookup(name).map(|value| {
                (
                    (*name).into(),
                    value.as_os_str().as_encoded_bytes().to_vec(),
                )
            })
        })
        .collect()
}

fn hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// A module's cache key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CacheKey(Digest);

impl CacheKey {
    pub fn digest(self) -> Digest {
        self.0
    }

    /// Rebuild a key from a previously rendered value. Not for minting keys: those come from
    /// [`CacheKeyInputs::key`], which is what guarantees every input is covered.
    pub fn from_digest(value: Digest) -> Self {
        Self(value)
    }
}

impl std::fmt::Display for CacheKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A labelled mutation of one keyed input, used by `every_input_participates_in_the_key`.
    type MutationCase = (&'static str, fn(&mut CacheKeyInputs));

    fn baseline() -> CacheKeyInputs {
        CacheKeyInputs {
            compiler: "krusty 2.4.10-build.1 (abc123)".into(),
            compiler_flags: vec!["-module-name".into(), "app".into(), "-jvm-target".into()],
            environment: vec![],
            jdk_identity: "21.0.10/modules:deadbeef".into(),
            sources: vec![
                FileDigest::of_bytes("/repo/app/Alpha.kt", b"1"),
                FileDigest::of_bytes("/repo/app/Beta.kt", b"2"),
            ],
            classpath: vec![FileDigest::of_bytes("/m2/kotlin-stdlib-2.4.10.jar", b"10")],
            friend_paths: vec![],
            dependency_abis: vec![(ModuleId::new("core"), fingerprint_of(7))],
            plugins: vec![],
            plugin_options: vec![],
            target: "jvm-17".into(),
        }
    }

    /// An `AbiFingerprint` with a chosen value, via the public constructor path.
    fn fingerprint_of(seed: u64) -> AbiFingerprint {
        use crate::abi::{fingerprint, AbiClass};
        fingerprint(&[AbiClass {
            name: format!("lib/C{seed}"),
            access: 33,
            super_name: None,
            interfaces: vec![],
            signature: None,
            retention: None,
            metadata: None,
            members: vec![],
        }])
    }

    /// Mutating any one input must change the key. Each closure is one of the seven collisions the
    /// module table documents.
    #[test]
    fn every_input_participates_in_the_key() {
        let cases: Vec<MutationCase> = vec![
            ("compiler", |i| i.compiler = "krusty 2.4.10-build.2".into()),
            ("flags", |i| {
                i.compiler_flags.push("-Xjvm-default=all".into())
            }),
            ("environment", |i| {
                i.environment
                    .push(("KRUSTY_NO_CLASS_METADATA".into(), b"1".to_vec()))
            }),
            ("jdk", |i| i.jdk_identity = "17.0.9/modules:feedface".into()),
            ("source content", |i| {
                i.sources[0].content = digest_bytes(b"99")
            }),
            ("source path", |i| {
                i.sources[0].path = PathBuf::from("/repo/app/Renamed.kt")
            }),
            ("classpath content", |i| {
                i.classpath[0].content = digest_bytes(b"99")
            }),
            ("classpath path", |i| {
                i.classpath[0].path = PathBuf::from("/m2/kotlin-stdlib-2.4.0.jar")
            }),
            ("friend paths", |i| {
                i.friend_paths.push(FileDigest::of_bytes("/out/main", b"5"))
            }),
            ("dependency abi", |i| {
                i.dependency_abis[0].1 = fingerprint_of(8)
            }),
            ("plugins", |i| {
                i.plugins
                    .push(FileDigest::of_bytes("/m2/ksp-processor.jar", b"3"))
            }),
            ("plugin options", |i| {
                i.plugin_options.push("verbose=true".into())
            }),
            ("target", |i| i.target = "jvm-21".into()),
        ];

        let base = baseline().key();
        for (label, mutate) in cases {
            let mut inputs = baseline();
            mutate(&mut inputs);
            assert_ne!(
                base,
                inputs.key(),
                "changing {label} must change the cache key — otherwise the cache serves a stale \
                 artifact and ships a wrong binary with no diagnostic"
            );
        }
    }

    /// Source ORDER is load-bearing: a package's facade-name list accumulates in file-streaming
    /// order, so reordering sources changes `.kotlin_module` bytes. A key over a sorted multiset
    /// would collide these two builds. Pinned by
    /// `tests/emission_determinism_e2e.rs::module_facade_order_follows_source_order_but_classes_do_not`.
    #[test]
    fn reordering_sources_changes_the_key() {
        let forward = baseline();
        let mut reversed = baseline();
        reversed.sources.reverse();
        assert_ne!(
            forward.key(),
            reversed.key(),
            "source order reaches .kotlin_module, so it must reach the key"
        );
    }

    /// Classpath order selects which entry wins a name, so it is equally load-bearing.
    #[test]
    fn reordering_the_classpath_changes_the_key() {
        let mut forward = baseline();
        forward
            .classpath
            .push(FileDigest::of_bytes("/m2/other-1.0.jar", b"11"));
        let mut reversed = forward.clone();
        reversed.classpath.reverse();
        assert_ne!(forward.key(), reversed.key());
    }

    #[test]
    fn identical_inputs_produce_identical_keys() {
        assert_eq!(baseline().key(), baseline().key());
        for _ in 0..8 {
            assert_eq!(baseline().key(), baseline().key());
        }
    }

    /// Two different modules' sources must not collide just because their contents match: the paths
    /// differ, and paths are in the key.
    #[test]
    fn same_content_at_different_paths_does_not_collide() {
        let mut a = baseline();
        let mut b = baseline();
        a.sources = vec![FileDigest::of_bytes("/repo/app/Main.kt", b"42")];
        b.sources = vec![FileDigest::of_bytes("/repo/lib/Main.kt", b"42")];
        assert_ne!(a.key(), b.key());
    }

    #[test]
    fn the_environment_allowlist_is_read_without_panicking() {
        // Reads the real process environment; the assertion is only that entries are allowlisted.
        for (name, _) in CacheKeyInputs::environment_from_process() {
            assert!(
                OUTPUT_AFFECTING_ENVIRONMENT.contains(&name.as_str()),
                "{name} is not on the allowlist"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn environment_identity_preserves_presence_empty_and_non_utf8_bytes() {
        use std::os::unix::ffi::OsStringExt;

        let absent = environment_from_lookup(|_| None);
        let empty = environment_from_lookup(|name| {
            (name == "KRUSTY_NO_CLASS_METADATA").then(std::ffi::OsString::new)
        });
        let raw = environment_from_lookup(|name| {
            (name == "KRUSTY_NO_CLASS_METADATA")
                .then(|| std::ffi::OsString::from_vec(vec![0xff, 0x00, b'x']))
        });
        assert!(absent.is_empty());
        assert_eq!(empty, vec![("KRUSTY_NO_CLASS_METADATA".into(), Vec::new())]);
        assert_eq!(
            raw,
            vec![("KRUSTY_NO_CLASS_METADATA".into(), vec![0xff, 0x00, b'x'])]
        );

        let mut absent_key = baseline();
        absent_key.environment = absent;
        let mut empty_key = baseline();
        empty_key.environment = empty;
        let mut raw_key = baseline();
        raw_key.environment = raw;
        assert_ne!(
            absent_key.key(),
            empty_key.key(),
            "presence changes emission"
        );
        assert_ne!(empty_key.key(), raw_key.key(), "raw OS bytes are preserved");
    }

    #[test]
    fn a_rendering_names_every_section() {
        let rendered = baseline().render();
        for section in [
            "compiler",
            "flags",
            "environment",
            "jdk",
            "sources",
            "classpath",
            "friends",
            "dependencies",
            "plugins",
            "target",
        ] {
            assert!(
                rendered.contains(section),
                "rendering must name the {section} section so a cache miss is explainable"
            );
        }
    }

    #[test]
    fn key_displays_as_fixed_width_hex() {
        let rendered = baseline().key().to_string();
        assert_eq!(rendered.len(), 64, "SHA-256 renders as 64 hex characters");
        assert!(rendered.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn digesting_a_missing_file_is_an_error_not_a_panic() {
        assert!(FileDigest::of_file("/nonexistent/krusty-build/probe").is_err());
    }
}
