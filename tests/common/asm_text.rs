//! ASM's textual view of class files and Java-regex occurrence counts, for the bytecode corpora.
//!
//! JetBrains' `codegen/bytecodeText` expectations are `java.util.regex` patterns counted over the
//! text ASM's `Textifier` prints for every generated class, and `codegen/bytecodeListing` is
//! rendered by a visitor over the same ASM model. Both oracles are reproduced with the ASM copy the
//! reference compiler distribution itself ships (`org.jetbrains.org.objectweb.asm` inside
//! `kotlin-compiler.jar`) rather than a reimplementation, so the text and the pattern semantics are
//! the reference test infrastructure's own. The listing visitor itself ships only in JetBrains' test
//! fixtures, so `asm_text/BytecodeListing.java` renders it line for line over that ASM.
//!
//! The server (`asm_text/AsmText.java`) is compiled once per source/JDK/compiler-jar identity and
//! runs as a pool of persistent JVMs; no request starts a process.

use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use super::common::{
    die_with_parent, java_home, kotlin_compiler_jar, producing_jdk, read_exact_deadline,
    server_pool, spawn_owned,
};

/// The server's sources, by file name.
const SOURCES: [(&str, &str); 2] = [
    ("AsmText.java", include_str!("asm_text/AsmText.java")),
    (
        "BytecodeListing.java",
        include_str!("asm_text/BytecodeListing.java"),
    ),
];

/// How long one request may take. A class or a corpus pattern is small; a stalled JVM is a failure,
/// not something to wait out.
const REQUEST_DEADLINE: Duration = Duration::from_secs(60);

/// One class file as ASM reads it.
pub struct ClassText {
    /// The source file its `SourceFile` attribute names, when it names one.
    pub source_file: Option<String>,
    /// The class as ASM's `Textifier` prints it.
    pub text: String,
}

/// Why [`textify`] or [`listing`] has no answer.
#[derive(Debug)]
pub enum TextifyError {
    /// ASM cannot read one of the classes: the class file is malformed.
    Unreadable(String),
    /// The server itself failed.
    Server(String),
}

/// Each class file in `classes`, in order, as ASM reads it.
pub fn textify(classes: &[&[u8]]) -> Result<Vec<ClassText>, TextifyError> {
    with_server(|server| server.textify(classes))
        .map_err(TextifyError::Server)?
        .map_err(TextifyError::Unreadable)
}

/// The `codegen/bytecodeListing` options a module's directives select.
#[derive(Clone, Copy, Debug)]
pub struct ListingOptions {
    /// `WITH_SIGNATURES`.
    pub with_signatures: bool,
    /// Not `IGNORE_ANNOTATIONS`.
    pub with_annotations: bool,
    /// Not `DONT_SORT_DECLARATIONS`.
    pub sort_declarations: bool,
}

/// One class file as JetBrains' bytecode listing renders it.
pub struct ClassListing {
    /// The source file its `SourceFile` attribute names, when it names one.
    pub source_file: Option<String>,
    /// Its JVM internal name.
    pub name: String,
    pub text: String,
}

/// Each class file in `classes`, in order, as JetBrains' `BytecodeListingTextCollectingVisitor`
/// renders it under `options`.
pub fn listing(
    classes: &[&[u8]],
    options: ListingOptions,
) -> Result<Vec<ClassListing>, TextifyError> {
    with_server(|server| server.listing(classes, options))
        .map_err(TextifyError::Server)?
        .map_err(TextifyError::Unreadable)
}

/// How many times each Java pattern `(needle)` is found in `text`, in order; `None` for a needle
/// that is not a valid pattern.
pub fn count_matches(text: &str, needles: &[&str]) -> Result<Vec<Option<usize>>, String> {
    with_server(|server| server.count(text, needles))?
}

/// Run `body` on a pooled server. The outer error is the server's own failure; the inner result is
/// the request's answer.
fn with_server<R>(
    body: impl Fn(&mut Server) -> std::io::Result<Result<R, String>>,
) -> Result<Result<R, String>, String> {
    static POOL: OnceLock<server_pool::Pool<Server>> = OnceLock::new();
    let classpath = server_classpath()?;
    let pool = POOL.get_or_init(server_pool::Pool::new);
    pool.with_server(
        server_pool::server_pool_cap(),
        || Server::start(classpath),
        |server| match body(server) {
            Ok(answer) => Ok(answer),
            Err(error) => {
                // The JVM died or stalled mid-request: replace it so the next request starts clean.
                if let Some(fresh) = Server::start(classpath) {
                    *server = fresh;
                }
                Err(format!("the ASM text server failed: {error}"))
            }
        },
    )
    .ok_or_else(|| "the ASM text server could not start".to_string())?
}

/// The compiled server and the distribution's `kotlin-compiler.jar`, as one class path.
fn server_classpath() -> Result<&'static str, String> {
    static CLASSPATH: OnceLock<Result<String, String>> = OnceLock::new();
    CLASSPATH
        .get_or_init(|| {
            let jar = kotlin_compiler_jar().ok_or(
                "the reference kotlinc distribution (its kotlin-compiler.jar) is not provisioned",
            )?;
            let classes = compile_server(&jar)?;
            Ok(format!("{}:{}", classes.display(), jar.display()))
        })
        .as_deref()
        .map_err(Clone::clone)
}

fn compile_server(jar: &Path) -> Result<PathBuf, String> {
    let java_home = java_home();
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in SOURCES
        .iter()
        .flat_map(|(name, source)| name.bytes().chain(source.bytes()))
        .chain(producing_jdk::driver_cache_key(&java_home))
        .chain(jar.as_os_str().as_encoded_bytes().iter().copied())
    {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("target/asm_text_{hash:016x}"));
    if dir.join("AsmText.class").is_file() {
        return Ok(dir);
    }
    // Compiled into a private directory and renamed into place, so a concurrent test process never
    // loads a half-written class.
    let staging = dir.with_extension(format!("staging-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|error| format!("create {staging:?}: {error}"))?;
    let mut javac = Command::new(Path::new(&java_home).join("bin/javac"));
    javac.arg("-cp").arg(jar).arg("-d").arg(&staging);
    for (name, source) in SOURCES {
        let path = staging.join(name);
        std::fs::write(&path, source).map_err(|error| format!("write {path:?}: {error}"))?;
        javac.arg(path);
    }
    let output = javac
        .output()
        .map_err(|error| format!("run javac: {error}"))?;
    if !output.status.success() {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!(
            "javac rejected the ASM text server:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    match std::fs::rename(&staging, &dir) {
        Ok(()) => Ok(dir),
        // Another process published the same classes first.
        Err(_) if dir.join("AsmText.class").is_file() => {
            let _ = std::fs::remove_dir_all(&staging);
            Ok(dir)
        }
        Err(error) => Err(format!("publish {dir:?}: {error}")),
    }
}

struct Server {
    _child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
}

impl Server {
    fn start(classpath: &str) -> Option<Self> {
        let mut command = Command::new(Path::new(&java_home()).join("bin/java"));
        command
            .args(["-XX:TieredStopAtLevel=1", "-XX:+UseSerialGC", "-Xmx512m"])
            .args(["-cp", classpath, "AsmText"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        die_with_parent(&mut command);
        let mut child = spawn_owned(command).ok()?;
        let stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;
        Some(Server {
            _child: child,
            stdin,
            stdout,
        })
    }

    fn textify(&mut self, classes: &[&[u8]]) -> std::io::Result<Result<Vec<ClassText>, String>> {
        self.stdin.write_all(b"T")?;
        self.write_len(classes.len())?;
        for class in classes {
            self.write_bytes(class)?;
        }
        self.stdin.flush()?;
        let deadline = Instant::now() + REQUEST_DEADLINE;
        if let Some(error) = self.read_status(deadline)? {
            return Ok(Err(error));
        }
        let texts = (0..classes.len())
            .map(|_| {
                let source_file = self.read_string(deadline)?;
                Ok(ClassText {
                    source_file: (!source_file.is_empty()).then_some(source_file),
                    text: self.read_string(deadline)?,
                })
            })
            .collect::<std::io::Result<_>>()?;
        Ok(Ok(texts))
    }

    fn listing(
        &mut self,
        classes: &[&[u8]],
        options: ListingOptions,
    ) -> std::io::Result<Result<Vec<ClassListing>, String>> {
        self.stdin.write_all(b"L")?;
        self.stdin.write_all(&[
            u8::from(options.with_signatures),
            u8::from(options.with_annotations),
            u8::from(options.sort_declarations),
        ])?;
        self.write_len(classes.len())?;
        for class in classes {
            self.write_bytes(class)?;
        }
        self.stdin.flush()?;
        let deadline = Instant::now() + REQUEST_DEADLINE;
        if let Some(error) = self.read_status(deadline)? {
            return Ok(Err(error));
        }
        let listings = (0..classes.len())
            .map(|_| {
                let source_file = self.read_string(deadline)?;
                Ok(ClassListing {
                    source_file: (!source_file.is_empty()).then_some(source_file),
                    name: self.read_string(deadline)?,
                    text: self.read_string(deadline)?,
                })
            })
            .collect::<std::io::Result<_>>()?;
        Ok(Ok(listings))
    }

    fn count(
        &mut self,
        text: &str,
        needles: &[&str],
    ) -> std::io::Result<Result<Vec<Option<usize>>, String>> {
        self.stdin.write_all(b"C")?;
        self.write_bytes(text.as_bytes())?;
        self.write_len(needles.len())?;
        for needle in needles {
            self.write_bytes(needle.as_bytes())?;
        }
        self.stdin.flush()?;
        let deadline = Instant::now() + REQUEST_DEADLINE;
        if let Some(error) = self.read_status(deadline)? {
            return Ok(Err(error));
        }
        let counts = (0..needles.len())
            .map(|_| {
                self.read_int(deadline)
                    .map(|count| usize::try_from(count).ok())
            })
            .collect::<std::io::Result<_>>()?;
        Ok(Ok(counts))
    }

    fn write_len(&mut self, len: usize) -> std::io::Result<()> {
        let len = u32::try_from(len)
            .map_err(|_| std::io::Error::other("a request part exceeds the protocol's length"))?;
        self.stdin.write_all(&len.to_be_bytes())
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.write_len(bytes.len())?;
        self.stdin.write_all(bytes)
    }

    fn read_int(&mut self, deadline: Instant) -> std::io::Result<i32> {
        let mut buf = [0u8; 4];
        read_exact_deadline(self.stdout.as_raw_fd(), &mut buf, deadline)?;
        Ok(i32::from_be_bytes(buf))
    }

    fn read_string(&mut self, deadline: Instant) -> std::io::Result<String> {
        let len = usize::try_from(self.read_int(deadline)?)
            .map_err(|_| std::io::Error::other("negative length from the ASM text server"))?;
        let mut buf = vec![0u8; len];
        read_exact_deadline(self.stdout.as_raw_fd(), &mut buf, deadline)?;
        String::from_utf8(buf).map_err(std::io::Error::other)
    }

    /// `None` when the request succeeded; the server's reason when it did not.
    fn read_status(&mut self, deadline: Instant) -> std::io::Result<Option<String>> {
        match self.read_int(deadline)? {
            0 => Ok(None),
            1 => self.read_string(deadline).map(Some),
            status => Err(std::io::Error::other(format!(
                "unknown status {status} from the ASM text server"
            ))),
        }
    }
}
