//! The native process boundary: the arguments `main` receives and the lines standard input yields.
//!
//! These are the two inputs a program gets from outside rather than from its source, so a `box()`
//! cannot carry them: each test here starts the linked executable with real arguments and a real
//! pipe on its standard input, and pins everything it printed. Arguments are raw bytes, which only
//! a Unix host can pass, and only a Unix host links a native program.

#![cfg(unix)]

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt as _;

use super::common_core::native_backend::{native_image, run_native_image, NativeBox};

/// Lower `src` as a program, start it with `arguments` and `input` on standard input, and require
/// it to end cleanly having printed exactly `expected`. Skips only when this build cannot reach the
/// host target.
fn expect_main(src: &str, stem: &str, arguments: &[&[u8]], input: &[u8], expected: &str) {
    let Some(target) = krusty::native::NativeTarget::host() else {
        return;
    };
    if !krusty::native::can_link(target) {
        return;
    }
    let image = match native_image(&[(stem, src)], target) {
        Ok(image) => image,
        Err(NativeBox::Unavailable) => return,
        Err(NativeBox::Declined(reason)) => {
            panic!("{stem}: the native backend must lower this program: {reason}")
        }
        Err(failure) => panic!("{stem}: {}", failure.as_failure()),
    };
    let arguments: Vec<&OsStr> = arguments
        .iter()
        .map(|bytes| OsStr::from_bytes(bytes))
        .collect();
    let output = run_native_image(&image, stem, &arguments, input)
        .unwrap_or_else(|failure| panic!("{stem}: {}", failure.as_failure()));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stem}: it ended abnormally: {}; stdout {stdout:?}; stderr {:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(stdout, expected, "{stem}");
}

const PRINT_ARGUMENTS: &str = "fun main(args: Array<String>) {\n\
    \x20   println(args.size)\n\
    \x20   for (argument in args) println(\"[\" + argument + \"] \" + argument.length)\n\
    }\n";

#[test]
fn main_receives_the_arguments_without_the_program_name() {
    expect_main(
        PRINT_ARGUMENTS,
        "NativeArguments",
        &[b"one", b"", b"two words", "ünï €".as_bytes()],
        b"",
        "4\n[one] 3\n[] 0\n[two words] 9\n[ünï €] 5\n",
    );
}

#[test]
fn main_with_no_arguments_receives_an_empty_array() {
    expect_main(PRINT_ARGUMENTS, "NativeNoArguments", &[], b"", "0\n");
}

#[test]
fn an_ill_formed_argument_reads_as_replacement_characters() {
    // Each maximal ill-formed subpart is one U+FFFD: a stray continuation byte, a lead byte with
    // nothing after it, a truncated three-byte sequence, and a UTF-16 surrogate encoded as bytes,
    // whose three bytes are three subparts because no well-formed sequence begins `ED A0`.
    expect_main(
        PRINT_ARGUMENTS,
        "NativeIllFormedArguments",
        &[b"a\x80b", b"c\xC3", b"d\xE2\x82", b"e\xED\xA0\x80f"],
        b"",
        "4\n[a\u{FFFD}b] 3\n[c\u{FFFD}] 2\n[d\u{FFFD}] 2\n[e\u{FFFD}\u{FFFD}\u{FFFD}f] 5\n",
    );
}

#[test]
fn a_vararg_main_receives_the_same_arguments() {
    expect_main(
        "fun main(vararg args: String) { for (argument in args) println(argument) }\n",
        "NativeVarargArguments",
        &[b"x", b"y"],
        b"",
        "x\ny\n",
    );
}

#[test]
fn main_with_arguments_is_the_entry_when_both_forms_are_declared() {
    expect_main(
        "fun main() { println(\"without\") }\n\
         fun main(args: Array<String>) { println(\"with \" + args.size) }\n",
        "NativeBothMains",
        &[b"a"],
        b"",
        "with 1\n",
    );
}

const ECHO_LINES: &str = "fun main() {\n\
    \x20   while (true) {\n\
    \x20       val line = readLine() ?: break\n\
    \x20       println(\"[\" + line + \"] \" + line.length)\n\
    \x20   }\n\
    \x20   println(\"end \" + readlnOrNull())\n\
    }\n";

#[test]
fn read_line_splits_on_newline_and_crlf_and_keeps_an_unterminated_last_line() {
    expect_main(
        ECHO_LINES,
        "NativeReadLine",
        &[],
        "first\nsecond\r\n\nlone\rcarriage\nü€\nlast".as_bytes(),
        "[first] 5\n[second] 6\n[] 0\n[lone\rcarriage] 13\n[ü€] 2\n[last] 4\nend null\n",
    );
}

#[test]
fn read_line_at_the_end_of_empty_input_is_null() {
    expect_main(ECHO_LINES, "NativeReadLineEmpty", &[], b"", "end null\n");
}

#[test]
fn a_line_longer_than_the_read_buffer_is_one_line() {
    let long = "x".repeat(10_000);
    let input = format!("{long}\nshort\n");
    expect_main(
        ECHO_LINES,
        "NativeReadLineLong",
        &[],
        input.as_bytes(),
        &format!("[{long}] 10000\n[short] 5\nend null\n"),
    );
}

#[test]
fn an_ill_formed_line_reads_as_replacement_characters() {
    expect_main(
        ECHO_LINES,
        "NativeReadLineIllFormed",
        &[],
        b"a\xFFb\n",
        "[a\u{FFFD}b] 3\nend null\n",
    );
}

#[test]
fn readln_raises_at_the_end_of_input() {
    // `ReadAfterEOFException` is internal to the standard library, so a program can only catch it
    // as what it extends.
    expect_main(
        "fun main() {\n\
         \x20   println(readln())\n\
         \x20   try {\n\
         \x20       readln()\n\
         \x20       println(\"unreachable\")\n\
         \x20   } catch (e: RuntimeException) {\n\
         \x20       println(\"caught \" + e.message)\n\
         \x20   }\n\
         }\n",
        "NativeReadln",
        &[],
        b"only\n",
        "only\ncaught EOF has already been reached\n",
    );
}
