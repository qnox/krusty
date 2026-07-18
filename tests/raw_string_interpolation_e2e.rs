//! Raw (triple-quoted) string interpolation: `"""$x/${e}"""`. krusty's lexer previously REJECTED any
//! `$`/`${}` inside a raw string ("raw string interpolation is not supported"), dropping the whole
//! file — pervasive in the generated httpclient `*Client.kt` files, which build request URLs as
//! `"""$baseUrl/repos/${owner}/${repo}"""`. The lexer now lexes a raw template (verbatim chunks, no
//! escape processing, triple-quote delimiter) into the same token stream as a regular template.
use super::common;

#[test]
fn raw_string_interpolation_and_verbatim_chunks() {
    // `$base` and `${id + 1}` interpolate; the `\n` in a raw chunk stays a literal backslash-n
    // (no escape processing), unlike a regular string where `\n` is a newline.
    const SRC: &str = "fun box(): String {\n\
        \x20 val base = \"http://x\"\n\
        \x20 val id = 7\n\
        \x20 val url = \"\"\"$base/zen/${id + 1}\"\"\"\n\
        \x20 val raw = \"\"\"a\\nb\"\"\"\n\
        \x20 return if (url == \"http://x/zen/8\" && raw == \"a\\\\nb\") \"OK\" else \"FAIL:$url|$raw\"\n\
        }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "Main").expect("raw string interpolation"),
        "OK"
    );
}
