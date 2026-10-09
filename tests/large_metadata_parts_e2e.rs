//! Kotlin metadata whose `d1` payload outgrows one `CONSTANT_Utf8` entry (65535 bytes), as a large
//! generated model or client class does. kotlinc writes the payload as several `d1` strings; a single
//! oversized string wrapped its 16-bit length and left a constant pool no JVM can read. A consumer
//! compiled against the library also reads the metadata back across the parts.
use super::common;

fn library() -> String {
    let mut source = String::from("package lib\nclass Wide {\n");
    for property in 0..4000 {
        source.push_str(&format!("    val property{property}: Int = {property}\n"));
    }
    source.push_str("}\n");
    source
}

#[test]
fn metadata_longer_than_one_utf8_entry_is_written_in_parts() {
    const MAIN: &str = "import lib.Wide\n\
        fun box(): String {\n\
        \x20 val sum = Wide().property3999 + Wide().property0\n\
        \x20 return if (sum == 3999) \"OK\" else \"fail: $sum\"\n\
        }\n";
    let jdk = common::jdk_modules();
    let lib = common::compile_lib("large_metadata_parts", &library()).expect("library compiles");
    let answer = common::compile_and_run_box(
        MAIN,
        "Main",
        &[lib, common::stdlib_jar(), jdk.clone()],
        Some(jdk.as_path()),
    );
    assert_eq!(answer.as_deref(), Some("OK"));
}
