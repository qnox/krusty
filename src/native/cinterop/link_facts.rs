//! What linking a program against an interop library's C libraries needs, captured when the
//! library is made and carried inside it.
//!
//! A `.def` file names its C libraries in `linkerOpts` (`-lz`). When the interop library is built,
//! each one is found and read once ([`capture`]): the name the loader finds it by and the functions
//! it exports at their default versions, which is all krusty's linker needs (`ImportLibrary`). The
//! facts are written into the library, beside its metadata, under
//! `default/targets/<target>/imports` ([`entry`]), so a program built against the library later
//! links with no C library on the build machine: the program's link reads them back ([`decode`])
//! and never the C library itself. See `docs/NATIVE.md`, "What a link needs from a library".
//!
//! Today the library files are read from the build machine's own library directories, so an
//! interop library is made where its C libraries are installed.

use std::path::{Path, PathBuf};

use super::library::CinteropError;
use crate::native::linker::{Export, ExportKind, ImportLibrary, ProgramLinkError};
use crate::native::target::{Arch, NativeTarget};

/// The library-relative path of the captured facts for the Kotlin/Native target `konan_target`.
pub(crate) fn entry(konan_target: &str) -> String {
    format!("default/targets/{konan_target}/imports")
}

/// The `-l` and `-L` options of `linkerOpts`, in order.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct LinkerOptions {
    /// Each `-l` operand: a name (`z`, found as `libz.so`), or after `:` a file name.
    libraries: Vec<String>,
    /// Each `-L` directory, searched before the target's own library directories.
    search_dirs: Vec<PathBuf>,
}

impl LinkerOptions {
    /// Read linker options. Anything but `-l` and `-L` is refused: krusty's linker cannot honor a
    /// system linker's flag it does not implement.
    pub(crate) fn parse(words: &[String]) -> Result<Self, CinteropError> {
        let mut options = Self::default();
        let mut words = words.iter();
        while let Some(word) = words.next() {
            let mut operand = |flag: &str| match word.strip_prefix(flag) {
                Some("") => words.next().cloned().ok_or_else(|| {
                    CinteropError(format!("linker option `{flag}` needs an operand"))
                }),
                Some(rest) => Ok(rest.to_string()),
                None => unreachable!("the flag was matched"),
            };
            if word.starts_with("-l") {
                options.libraries.push(operand("-l")?);
            } else if word.starts_with("-L") {
                options.search_dirs.push(PathBuf::from(operand("-L")?));
            } else {
                return Err(CinteropError(format!(
                    "unsupported linker option `{word}`: krusty links C libraries named by `-l`, \
                     found in `-L` directories"
                )));
            }
        }
        Ok(options)
    }
}

/// Find and read each library `options` names, for `target`.
pub(crate) fn capture(
    options: &LinkerOptions,
    target: NativeTarget,
) -> Result<Vec<ImportLibrary>, CinteropError> {
    let dirs: Vec<PathBuf> = options
        .search_dirs
        .iter()
        .cloned()
        .chain(library_dirs(target.arch))
        .collect();
    let mut libraries = Vec::new();
    for name in &options.libraries {
        let file = match name.strip_prefix(':') {
            Some(file) => file.to_string(),
            None => format!("lib{name}.so"),
        };
        let Some(path) = dirs
            .iter()
            .map(|dir| dir.join(&file))
            .find(|path| path.is_file())
        else {
            return Err(CinteropError(format!(
                "cannot find `{file}` for `-l{name}` in {}",
                dirs.iter()
                    .map(|dir| dir.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        };
        libraries.push(read(&path, target.arch)?);
    }
    Ok(libraries)
}

/// The directories a C library for `arch` is installed in: the Debian multiarch ones first, then
/// those of other distributions.
fn library_dirs(arch: Arch) -> Vec<PathBuf> {
    let multiarch = format!("{}-linux-gnu", arch.triple_name());
    ["/lib", "/usr/lib", "/usr/local/lib"]
        .iter()
        .map(|root| Path::new(root).join(&multiarch))
        .chain(
            ["/lib64", "/usr/lib64", "/lib", "/usr/lib", "/usr/local/lib"]
                .iter()
                .map(PathBuf::from),
        )
        .collect()
}

fn read(path: &Path, arch: Arch) -> Result<ImportLibrary, CinteropError> {
    let what = path.display().to_string();
    let bytes = std::fs::read(path)
        .map_err(|error| CinteropError(format!("cannot read {what}: {error}")))?;
    ImportLibrary::from_elf_shared_object(&what, &bytes, arch, None)
        .map_err(|error| CinteropError(error.to_string()))
}

/// The facts as the library carries them: per library, a `library <soname>` line, then one
/// `function <name> [<version>]` or `data <name> [<version>]` line per export, in name order.
pub(crate) fn encode(libraries: &[ImportLibrary]) -> Vec<u8> {
    let mut out = String::new();
    for library in libraries {
        out.push_str(&format!("library {}\n", library.soname()));
        for (name, export) in library.exports() {
            let kind = match export.kind {
                ExportKind::Function => "function",
                ExportKind::Data => "data",
            };
            out.push_str(&format!("{kind} {name}"));
            if let Some(version) = &export.version {
                out.push_str(&format!(" {version}"));
            }
            out.push('\n');
        }
    }
    out.into_bytes()
}

/// Read the facts [`encode`] wrote. `what` names the library that carries them, in errors. A line
/// that is not one [`encode`] writes is refused, never skipped: the facts are all the link knows.
pub(crate) fn decode(bytes: &[u8], what: &str) -> Result<Vec<ImportLibrary>, ProgramLinkError> {
    let malformed = |detail: String| {
        ProgramLinkError::InvalidLibrary(format!("the link facts of {what} {detail}"))
    };
    let text = std::str::from_utf8(bytes).map_err(|_| malformed("are not UTF-8".to_string()))?;
    let mut libraries = Vec::new();
    let mut current: Option<(String, Vec<(String, Export)>)> = None;
    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        match line.split(' ').collect::<Vec<_>>().as_slice() {
            ["library", soname] => {
                if let Some((soname, exports)) = current.take() {
                    libraries.push(ImportLibrary::new(soname, exports)?);
                }
                current = Some((soname.to_string(), Vec::new()));
            }
            [kind @ ("function" | "data"), name, version @ ..] if version.len() <= 1 => {
                let Some((_, exports)) = current.as_mut() else {
                    return Err(malformed(format!(
                        "name an export before any library on line {line_number}"
                    )));
                };
                let kind = if *kind == "function" {
                    ExportKind::Function
                } else {
                    ExportKind::Data
                };
                exports.push((
                    name.to_string(),
                    Export {
                        kind,
                        version: version.first().map(|version| version.to_string()),
                    },
                ));
            }
            _ => {
                return Err(malformed(format!(
                    "have a malformed line {line_number}: {line:?}"
                )))
            }
        }
    }
    if let Some((soname, exports)) = current {
        libraries.push(ImportLibrary::new(soname, exports)?);
    }
    Ok(libraries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split(' ').map(str::to_string).collect()
    }

    #[test]
    fn linker_options_name_libraries_and_directories() {
        assert_eq!(
            LinkerOptions::parse(&words("-lz -L /opt/lib -lsqlite3 -L/usr/x -l :libm.so.6"))
                .expect("parses"),
            LinkerOptions {
                libraries: vec!["z".into(), "sqlite3".into(), ":libm.so.6".into()],
                search_dirs: vec!["/opt/lib".into(), "/usr/x".into()],
            }
        );
        assert_eq!(
            LinkerOptions::parse(&words("-lz -Wl,--as-needed")).expect_err("refused"),
            CinteropError(
                "unsupported linker option `-Wl,--as-needed`: krusty links C libraries named by \
                 `-l`, found in `-L` directories"
                    .to_string()
            )
        );
        assert_eq!(
            LinkerOptions::parse(&words("-L")).expect_err("refused"),
            CinteropError("linker option `-L` needs an operand".to_string())
        );
    }

    #[test]
    fn facts_round_trip_through_their_text_one_line_per_library_and_export() {
        let library = |soname: &str, exports: &[(&str, ExportKind, Option<&str>)]| {
            ImportLibrary::new(
                soname.to_string(),
                exports.iter().map(|(name, kind, version)| {
                    (
                        name.to_string(),
                        Export {
                            kind: *kind,
                            version: version.map(str::to_string),
                        },
                    )
                }),
            )
            .expect("a valid library")
        };
        let libraries = vec![
            library(
                "libz.so.1",
                &[
                    ("deflate", ExportKind::Function, Some("ZLIB_1.2.0")),
                    ("compressBound", ExportKind::Function, None),
                ],
            ),
            library(
                "libc.so.6",
                &[("environ", ExportKind::Data, Some("GLIBC_2.2.5"))],
            ),
        ];
        let text = encode(&libraries);
        assert_eq!(
            String::from_utf8(text.clone()).expect("UTF-8"),
            "library libz.so.1\nfunction compressBound\nfunction deflate ZLIB_1.2.0\n\
             library libc.so.6\ndata environ GLIBC_2.2.5\n"
        );
        assert_eq!(decode(&text, "zlib.klib").expect("decodes"), libraries);
        assert_eq!(decode(b"", "zlib.klib").expect("decodes"), Vec::new());
    }

    #[test]
    fn malformed_facts_are_refused_with_their_line() {
        let refused = |text: &[u8]| match decode(text, "zlib.klib").expect_err("refused") {
            ProgramLinkError::InvalidLibrary(detail) => detail,
            other => panic!("refused as {other:?}"),
        };
        assert_eq!(
            refused(b"function deflate\n"),
            "the link facts of zlib.klib name an export before any library on line 1"
        );
        assert_eq!(
            refused(b"library libz.so.1\nsymbol deflate\n"),
            "the link facts of zlib.klib have a malformed line 2: \"symbol deflate\""
        );
        assert_eq!(
            refused(b"library libz.so.1\nfunction deflate ZLIB_1.2.0 extra\n"),
            "the link facts of zlib.klib have a malformed line 2: \
             \"function deflate ZLIB_1.2.0 extra\""
        );
        assert_eq!(
            refused(b"library libz.so.1\nfunction deflate\nfunction deflate\n"),
            "libz.so.1 exports `deflate` twice"
        );
        assert_eq!(
            refused(b"library \xff\n"),
            "the link facts of zlib.klib are not UTF-8"
        );
    }
}
