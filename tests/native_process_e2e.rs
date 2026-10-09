//! The native process boundary: the arguments `main` receives and the lines standard input yields.
//!
//! These are the two inputs a program gets from outside rather than from its source, so a `box()`
//! cannot carry them. Each case starts a program under `tests/native_process/` with raw argument
//! bytes and raw bytes on its standard input, and compares its exit status, stdout and stderr
//! EXACTLY with the references':
//!
//! * Kotlin/Native, the target's own reference. Its answers are recorded under
//!   `tests/native_process/oracle/` and required: a case it is a reference for fails without one.
//!   Re-record them by running this module with `KRUSTY_RECORD_NATIVE_PROCESS_ORACLE` naming a
//!   Kotlin/Native distribution (`scripts/kotlin-native.sh` provisions one); each program is
//!   compiled with its `kotlinc-native` and run on the same bytes.
//! * The JVM, live: the program is compiled by the reference kotlinc and started by `java` under a
//!   UTF-8 locale, which decodes the arguments as UTF-8 with one U+FFFD per maximal ill-formed
//!   subpart, as Kotlin/Native does.
//!
//! Each case names which references hold for it, and every case has at least one. Two kinds of
//! input have only one:
//!
//! * Standard input of more than one line is the JVM's alone. Kotlin/Native's `readLine` is one
//!   `read(2)` of up to 4095 bytes with trailing CR and LF trimmed, which on a terminal is a line
//!   but on a pipe is whatever one read returns; krusty reads lines as the JVM and the common
//!   stdlib define them.
//! * Ill-formed standard input is Kotlin/Native's alone: the JVM's `readLine` raises
//!   `MalformedInputException` on it, and krusty decodes it as Kotlin/Native does, as it decodes an
//!   argument. Its case is one line, which Kotlin/Native reads whole.
//!
//! The programs print text as UTF-16 code units in decimal, so what is compared is exactly what the
//! program received, independent of how any of the three encodes its output. Arguments are raw
//! bytes, which only a Unix host can pass, and only a Unix host links a native program.

#![cfg(unix)]

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::{Mutex, OnceLock};

use super::common;
use super::common_core::native_backend::{native_image, run_native_image, NativeBox};

const RECORD_ORACLE: &str = "KRUSTY_RECORD_NATIVE_PROCESS_ORACLE";

fn programs_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/native_process")
}

/// One run: a program, the arguments and standard input it is started with, and which references
/// hold for it.
struct Case {
    name: &'static str,
    program: &'static str,
    arguments: Vec<Vec<u8>>,
    input: Vec<u8>,
    kotlin_native: bool,
    jvm: bool,
}

/// What a process ended with, in the form the oracle records it.
fn transcript(output: &Output) -> Vec<u8> {
    let status = match output.status.code() {
        Some(code) => format!("status {code}\n"),
        None => format!("status killed {:?}\n", output.status),
    };
    let mut transcript = status.into_bytes();
    transcript.extend_from_slice(format!("stdout {}\n", output.stdout.len()).as_bytes());
    transcript.extend_from_slice(&output.stdout);
    transcript.extend_from_slice(format!("\nstderr {}\n", output.stderr.len()).as_bytes());
    transcript.extend_from_slice(&output.stderr);
    transcript.push(b'\n');
    transcript
}

fn run(case: &Case) {
    let Some(target) = krusty::native::NativeTarget::host() else {
        return;
    };
    if !krusty::native::can_link(target) {
        return;
    }
    let native = transcript(&run_krusty_native(target, case));
    if case.kotlin_native {
        let oracle_path = programs_dir()
            .join("oracle")
            .join(format!("{}.out", case.name));
        if let Some(distribution) = std::env::var_os(RECORD_ORACLE) {
            let recorded = transcript(&run_kotlin_native(Path::new(&distribution), case));
            fs::create_dir_all(oracle_path.parent().expect("oracle directory"))
                .expect("create the oracle directory");
            fs::write(&oracle_path, recorded).expect("record the Kotlin/Native oracle");
        }
        let oracle = fs::read(&oracle_path).unwrap_or_else(|_| {
            panic!(
                "{}: no Kotlin/Native oracle at {}; record it with {RECORD_ORACLE}",
                case.name,
                oracle_path.display()
            )
        });
        assert_eq!(
            String::from_utf8_lossy(&native),
            String::from_utf8_lossy(&oracle),
            "{}: krusty's native program and Kotlin/Native's differ",
            case.name
        );
    }
    if case.jvm {
        let Some(jvm) = run_jvm(case) else {
            return;
        };
        assert_eq!(
            String::from_utf8_lossy(&native),
            String::from_utf8_lossy(&transcript(&jvm)),
            "{}: krusty's native program and the JVM's differ",
            case.name
        );
    }
}

fn source(program: &str) -> String {
    fs::read_to_string(programs_dir().join(format!("{program}.kt")))
        .unwrap_or_else(|error| panic!("{program}.kt: {error}"))
}

fn arguments(case: &Case) -> Vec<&OsStr> {
    case.arguments
        .iter()
        .map(|bytes| OsStr::from_bytes(bytes))
        .collect()
}

fn run_krusty_native(target: krusty::native::NativeTarget, case: &Case) -> Output {
    let image = match native_image(&[(case.program, &source(case.program))], target) {
        Ok(image) => image,
        Err(NativeBox::Declined(reason)) => panic!(
            "{}: the native backend must lower {}: {reason}",
            case.name, case.program
        ),
        Err(failure) => panic!("{}: {}", case.name, failure.as_failure()),
    };
    run_native_image(&image, case.name, &arguments(case), &case.input)
        .unwrap_or_else(|failure| panic!("{}: {}", case.name, failure.as_failure()))
}

/// Compile `case`'s program with Kotlin/Native's own compiler and run it on the case's bytes.
fn run_kotlin_native(distribution: &Path, case: &Case) -> Output {
    let scratch = common::scratch_dir()
        .expect("scratch directory")
        .join("kotlin-native-process");
    fs::create_dir_all(&scratch).expect("create the Kotlin/Native scratch directory");
    let executable = scratch.join(format!("{}.kexe", case.program));
    if !executable.is_file() {
        let output = std::process::Command::new(distribution.join("bin/kotlinc-native"))
            .arg(programs_dir().join(format!("{}.kt", case.program)))
            .arg("-o")
            .arg(scratch.join(case.program))
            .output()
            .expect("run kotlinc-native");
        assert!(
            output.status.success(),
            "{}: kotlinc-native failed:\n{}",
            case.program,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    run_native_image(
        &fs::read(&executable).expect("read the Kotlin/Native executable"),
        case.name,
        &arguments(case),
        &case.input,
    )
    .unwrap_or_else(|failure| panic!("{}: {}", case.name, failure.as_failure()))
}

/// Compile `case`'s program with the reference kotlinc, once per program, and start it with
/// `java`. `None` when the JVM toolchain is unavailable.
fn run_jvm(case: &Case) -> Option<Output> {
    static CLASSES: OnceLock<Mutex<HashMap<&'static str, PathBuf>>> = OnceLock::new();
    let classes = {
        let mut compiled = CLASSES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match compiled.get(case.program) {
            Some(classes) => classes.clone(),
            None => {
                let classes = common::scratch_dir()
                    .expect("scratch directory")
                    .join("jvm-process")
                    .join(case.program);
                let kotlinc = vec![
                    programs_dir()
                        .join(format!("{}.kt", case.program))
                        .to_string_lossy()
                        .into_owned(),
                    "-d".to_string(),
                    classes.to_string_lossy().into_owned(),
                ];
                let (code, stderr) = common::kotlinc_compile(&kotlinc)?;
                assert_eq!(code, 0, "{}: kotlinc failed:\n{stderr}", case.program);
                compiled.insert(case.program, classes.clone());
                classes
            }
        }
    };
    let main_class = main_class(case.program);
    common::run_jvm_main(
        &[classes, common::stdlib_jar()],
        &main_class,
        &arguments(case),
        &case.input,
    )
}

/// The JVM class a file's top-level `main` lands in: `snake_case.kt` is `Snake_caseKt`.
fn main_class(program: &str) -> String {
    let mut characters = program.chars();
    let first = characters.next().expect("a program name");
    format!("{}{}Kt", first.to_ascii_uppercase(), characters.as_str())
}

fn case(name: &'static str, program: &'static str, arguments: &[&[u8]], input: &[u8]) -> Case {
    Case {
        name,
        program,
        arguments: arguments.iter().map(|bytes| bytes.to_vec()).collect(),
        input: input.to_vec(),
        kotlin_native: true,
        jvm: true,
    }
}

fn jvm_only(mut case: Case) -> Case {
    case.kotlin_native = false;
    case
}

fn kotlin_native_only(mut case: Case) -> Case {
    case.jvm = false;
    case
}

// ---- arguments ----------------------------------------------------------------------------------

#[test]
fn main_receives_the_arguments_without_the_program_name() {
    run(&case(
        "arguments",
        "print_arguments",
        &[
            b"one",
            b"",
            b"two words",
            "ünï €".as_bytes(),
            "😀".as_bytes(),
        ],
        b"",
    ));
}

#[test]
fn main_with_no_arguments_receives_an_empty_array() {
    run(&case("no_arguments", "print_arguments", &[], b""));
}

/// Every kind of maximal ill-formed subpart: a stray continuation byte; a lead byte with nothing
/// after it; truncated three- and four-byte sequences; a UTF-16 surrogate encoded as bytes (`ED A0`
/// begins no well-formed sequence, so each byte is its own subpart); an overlong `/`; a sequence
/// past U+10FFFF; bytes that are never UTF-8; and a well-formed sequence right after an ill-formed
/// one.
#[test]
fn ill_formed_arguments_read_as_one_replacement_per_maximal_subpart() {
    run(&case(
        "ill_formed_arguments",
        "print_arguments",
        &[
            b"a\x80b",
            b"c\xC3",
            b"d\xE2\x82",
            b"e\xF0\x9F\x98",
            b"f\xED\xA0\x80g",
            b"\xC0\xAF",
            b"\xE0\x80\xAF",
            b"\xF4\x90\x80\x80",
            b"\xFE\xFF",
            b"\xE2\x82\xC3\xA9",
        ],
        b"",
    ));
}

#[test]
fn a_vararg_main_receives_the_same_arguments() {
    run(&case(
        "vararg_arguments",
        "vararg_arguments",
        &[b"x", b"y"],
        b"",
    ));
}

#[test]
fn a_main_taking_a_nullable_array_receives_the_arguments() {
    run(&case(
        "nullable_arguments",
        "nullable_arguments",
        &[b"p", b"q"],
        b"",
    ));
}

#[test]
fn main_with_arguments_is_the_entry_when_both_forms_are_declared() {
    run(&case("both_mains", "both_mains", &[b"a"], b""));
}

#[test]
fn overloads_beside_the_entry_are_ordinary_functions() {
    run(&case(
        "entry_beside_overloads",
        "entry_beside_overloads",
        &[b"a", b"b"],
        b"",
    ));
}

// ---- standard input -----------------------------------------------------------------------------

#[test]
fn read_line_splits_on_newline_and_crlf_and_keeps_an_unterminated_last_line() {
    run(&jvm_only(case(
        "lines",
        "echo_lines",
        &[],
        "first\nsecond\r\n\nlone\rcarriage\n\r\nü€😀\nlast".as_bytes(),
    )));
}

#[test]
fn read_line_at_the_end_of_empty_input_is_null() {
    run(&case("empty_input", "echo_lines", &[], b""));
}

#[test]
fn a_line_that_is_only_a_carriage_return_at_the_end_keeps_it() {
    run(&jvm_only(case(
        "trailing_carriage_return",
        "echo_lines",
        &[],
        b"a\r",
    )));
}

/// The runtime reads standard input 4096 bytes at a time. A line ending, a CRLF pair, and a
/// multi-byte sequence each split across that boundary are one line ending, one pair and one
/// character; and a line longer than the buffer is one line.
#[test]
fn line_endings_and_characters_split_across_the_read_buffer_are_whole() {
    let mut input = Vec::new();
    // `\r` is byte 4095 and `\n` byte 4096.
    input.extend(std::iter::repeat_n(b'x', 4095));
    input.extend_from_slice(b"\r\n");
    // Back on a boundary: this line's `\n` is the last byte of the second buffer.
    let at = input.len();
    input.extend(std::iter::repeat_n(b'y', 8191 - at));
    input.push(b'\n');
    // A three-byte `€` whose first byte ends the third buffer.
    let at = input.len();
    input.extend(std::iter::repeat_n(b'z', 12287 - at));
    input.extend_from_slice("€\n".as_bytes());
    // A line three buffers long, then an unterminated last line.
    input.extend(std::iter::repeat_n(b'w', 3 * 4096 + 17));
    input.extend_from_slice(b"\nend");
    run(&jvm_only(case(
        "buffer_boundaries",
        "echo_lines",
        &[],
        &input,
    )));
}

/// The same subparts as the arguments', on one line: Kotlin/Native decodes an ill-formed line as it
/// decodes an argument, where the JVM raises.
#[test]
fn ill_formed_input_reads_as_one_replacement_per_maximal_subpart() {
    run(&kotlin_native_only(case(
        "ill_formed_input",
        "echo_lines",
        &[],
        b"a\x80b c\xC3 d\xE2\x82 e\xF0\x9F\x98 f\xED\xA0\x80g \xC0\xAF \xE0\x80\xAF \xF4\x90\x80\x80 \xFE\xFF \xE2\x82\xC3\xA9\n",
    )));
}

#[test]
fn readln_raises_at_the_end_of_input() {
    run(&case("read_after_end", "read_after_end", &[], b"only\n"));
}
