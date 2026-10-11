//! A Kotlin program that calls a C library through a `.def` file, built and run with no C
//! compiler: `krusty cinterop` reads the `.def` file and the library's headers and writes the
//! interop library, the native code generator lowers each call of a C function to a direct C
//! call, and krusty's linker imports the function from the shared library the `.def` file's
//! `linkerOpts` name, whose facts the interop library carries.
//!
//! zlib is the library, read from the host: its header and `libz.so`. A host without them skips,
//! except on CI, which must have both. Every expected answer is Kotlin/Native's for the same
//! program against its own `platform.zlib`, which is checked when a Kotlin/Native distribution is
//! provisioned (`KRUSTY_KOTLIN_NATIVE`).

use std::path::{Path, PathBuf};
use std::process::Command;

use krusty::native::{InteropLibraries, NativeTarget};

use super::common::native_backend::{native_interop_outcome, NativeBox};
use super::common::{kotlin_native_box_with_arguments, krusty_binary, scratch_dir};

/// Kotlin/Native's own `platform.zlib` definition for Linux, without its dependency on
/// `platform.posix` and the inline helpers of its code section.
const ZLIB_DEF: &str = "\
package = platform.zlib
headers = zconf.h zlib.h
headerFilter = zlib.h
compilerOpts = -DByte=uByte -DBytef=uBytef -D_ANSI_SOURCE -D_POSIX_C_SOURCE=199309 -D_BSD_SOURCE -D_XOPEN_SOURCE=700
linkerOpts = -lz
";

/// Kotlin/Native requires the opt-in that every interop declaration carries.
const OPT_IN: &str = "-opt-in=kotlinx.cinterop.ExperimentalForeignApi";

/// The host, when this build can link for it and the host has zlib's header and shared library.
fn zlib_host() -> Option<NativeTarget> {
    let target = NativeTarget::host().filter(|target| krusty::native::can_link(*target))?;
    let multiarch = format!("{}-linux-gnu", target.arch.triple_name());
    let has_library = ["/lib", "/usr/lib"].iter().any(|root| {
        Path::new(root).join(&multiarch).join("libz.so").is_file()
            || Path::new(root).join("libz.so").is_file()
            || Path::new(&format!("{root}64")).join("libz.so").is_file()
    });
    if Path::new("/usr/include/zlib.h").is_file() && has_library {
        return Some(target);
    }
    assert!(
        std::env::var_os("CI").is_none(),
        "CI must have zlib's header and shared library (zlib1g-dev) installed"
    );
    None
}

/// Run `krusty cinterop` on the zlib definition, writing the library below `directory`.
fn krusty_cinterop(directory: &Path, arguments: &[&str]) -> std::process::Output {
    let def = directory.join("zlib.def");
    std::fs::write(&def, ZLIB_DEF).expect("write the .def file");
    Command::new(krusty_binary())
        .arg("cinterop")
        .arg("-def")
        .arg(&def)
        .args(arguments)
        .current_dir(directory)
        .output()
        .expect("run krusty cinterop")
}

/// The zlib interop library, built by `krusty cinterop` and opened for `target`.
fn zlib_library(directory: &Path, target: NativeTarget) -> InteropLibraries {
    let built = krusty_cinterop(directory, &["-o", "zlib"]);
    assert!(
        built.status.success(),
        "krusty cinterop failed: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&built.stdout), "");
    let library: PathBuf = directory.join("zlib.klib");
    InteropLibraries::open(&[library], target)
        .unwrap_or_else(|error| panic!("the interop library opens: {error}"))
}

/// What the native program answers, built against the zlib interop library.
fn native_answer(source: &str) -> Option<NativeBox> {
    let target = zlib_host()?;
    let directory = scratch_dir().expect("a scratch directory");
    let zlib = zlib_library(&directory, target);
    let outcome = native_interop_outcome(&[("ZlibCalls", source)], Some(&zlib), target);
    let _ = std::fs::remove_dir_all(&directory);
    Some(outcome)
}

/// zlib's pure functions of integers and its constant macros: each call is a direct C call of the
/// named symbol, whose `uLong`, `uInt` and `z_off_t` arguments and results cross as the C ABI
/// passes them, and each constant is the value the interop library declares.
#[test]
fn a_program_calls_zlib_through_an_interop_library_krusty_built() {
    let source = "\
import platform.zlib.*

fun box(): String =
    \"${compressBound(100u)} ${compressBound(0u)} ${adler32_combine(1u, 1u, 0)} \" +
        \"${crc32_combine(0u, 0u, 0)} $Z_OK $Z_BUF_ERROR $Z_BEST_COMPRESSION\"
";
    let expected = "113 13 1 0 0 -5 9";
    if let Some(answer) = kotlin_native_box_with_arguments(source, &[OPT_IN.to_string()]) {
        assert_eq!(answer, expected, "Kotlin/Native's answer");
    }
    let Some(outcome) = native_answer(source) else {
        return;
    };
    match outcome {
        NativeBox::Answered(answer) => assert_eq!(answer, expected),
        NativeBox::Declined(reason) => {
            panic!("the native backend must lower this program: {reason}")
        }
        failure => panic!("the native program ran wrong: {}", failure.as_failure()),
    }
}

/// A C function whose result is a pointer has no representation in this generator yet, so the
/// call is declined by name instead of handing a C pointer to Kotlin as a collected reference.
#[test]
fn a_c_call_returning_a_pointer_is_declined() {
    let source = "\
import platform.zlib.*

fun box(): String {
    val version = zlibVersion()
    return if (version != null) \"OK\" else \"no version\"
}
";
    let Some(outcome) = native_answer(source) else {
        return;
    };
    match outcome {
        NativeBox::Declined(reason) => assert_eq!(
            reason,
            "krusty: the native backend does not support a call of the C function `zlibVersion` \
             passing `kotlinx.cinterop.CPointer?`, which has no C representation here yet"
        ),
        NativeBox::Answered(answer) => panic!("it was emitted, answering {answer:?}"),
        failure => panic!("the native program ran wrong: {}", failure.as_failure()),
    }
}

/// `krusty cinterop` writes the library Kotlin/Native's `cinterop` would: a packed `.klib` named
/// after `-o`, whose manifest repeats the `.def` file and names the target, and which carries the
/// facts of the `libz` it found for `-lz`.
#[test]
fn krusty_cinterop_writes_a_packed_interop_library_with_its_link_facts() {
    let Some(target) = zlib_host() else {
        return;
    };
    let directory = scratch_dir().expect("a scratch directory");
    let built = krusty_cinterop(&directory, &["-o", "out/zlib", "-target", "host"]);
    assert!(
        built.status.success(),
        "krusty cinterop failed: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    let archive = krusty::klib::KlibArchive::open(&directory.join("out/zlib.klib"))
        .expect("a packed library");
    let manifest = archive.manifest().expect("a manifest");
    let konan = target.konan_name().expect("a Kotlin/Native target");
    assert_eq!(manifest.unique_name(), Some("zlib"));
    assert_eq!(manifest.get("interop"), Some("true"));
    assert_eq!(manifest.get("package"), Some("platform.zlib"));
    assert_eq!(manifest.get("linkerOpts"), Some("-lz"));
    assert_eq!(manifest.get("includedHeaders"), Some("usr/include/zlib.h"));
    assert_eq!(manifest.targets(), vec![konan]);
    let zlib = InteropLibraries::open(&[directory.join("out/zlib.klib")], target)
        .expect("the library opens");
    let [libz] = zlib.imports() else {
        panic!("one imported library, not {}", zlib.imports().len());
    };
    assert_eq!(libz.soname(), "libz.so.1");
    assert!(libz.export("compressBound").is_some());

    let refused = krusty_cinterop(&directory, &["-o", "x", "-staticLibrary", "libz.a"]);
    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&refused.stderr),
        "error: option -staticLibrary is not supported by krusty cinterop\n\
         info: use krusty cinterop -help for more information\n"
    );
    let _ = std::fs::remove_dir_all(&directory);
}
