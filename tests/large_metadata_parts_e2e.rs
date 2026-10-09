//! Kotlin metadata whose `d1` payload outgrows one `CONSTANT_Utf8` entry (65535 bytes), as a large
//! generated model or client class does. kotlinc writes the payload as several `d1` strings; a single
//! oversized string wrapped its 16-bit length and left a constant pool no JVM can read. A consumer
//! compiled against the library also reads the metadata back across the parts.
use super::common;
use std::fmt::Write as _;

const FUNCTION_COUNT: usize = 80;
const PARAMETER_COUNT: usize = 240;

fn library() -> String {
    // A few abstract methods with many parameters carry much more metadata per emitted JVM method
    // than thousands of properties. This still crosses the same d1/CONSTANT_Utf8 boundary while
    // avoiding an 8,000-accessor class whose coverage-instrumented compile took over 26 minutes.
    let mut source = String::from("package lib\ninterface Wide {\n");
    for function in 0..FUNCTION_COUNT {
        write!(source, "    fun method{function}(").expect("write method header");
        for parameter in 0..PARAMETER_COUNT {
            if parameter != 0 {
                source.push_str(", ");
            }
            write!(source, "p{parameter}: Int").expect("write method parameter");
        }
        source.push_str("): Int\n");
    }
    source.push_str("}\n");
    source
}

fn consumer() -> String {
    let mut source = String::from("import lib.Wide\nfun read(value: Wide?): Int = value?.method");
    write!(source, "{}(", FUNCTION_COUNT - 1).expect("write selected method");
    for parameter in 0..PARAMETER_COUNT {
        if parameter != 0 {
            source.push_str(", ");
        }
        source.push('0');
    }
    source.push_str(
        ") ?: 3999\n\
        fun box(): String {\n\
        \x20 val result = read(null)\n\
        \x20 return if (result == 3999) \"OK\" else \"fail: $result\"\n\
        }\n",
    );
    source
}

#[test]
fn metadata_longer_than_one_utf8_entry_is_written_in_parts() {
    let jdk = common::jdk_modules();
    let lib = common::compile_lib("large_metadata_parts", &library()).expect("library compiles");
    let main = consumer();
    let answer = common::compile_and_run_box(
        &main,
        "Main",
        &[lib, common::stdlib_jar(), jdk.clone()],
        Some(jdk.as_path()),
    );
    assert_eq!(answer.as_deref(), Some("OK"));
}

/// `lib/Wide` is byte for byte what kotlinc writes, against the recorded kotlinc dump: the same
/// number of `d1` parts with the same boundaries, and the same constant pool around them. The
/// payload must still outgrow one entry, or the comparison would not reach the partition. The
/// consumer test above separately covers reading the parts back across modules.
#[test]
fn metadata_parts_are_byte_identical_to_kotlinc() {
    let source = library();
    let stdlib = [common::stdlib_jar()];
    let krusty = common::compile_in_process(&source, "Wide", &stdlib, None)
        .expect("krusty compiles the library")
        .into_iter()
        .find_map(|(name, bytes)| (name == "lib/Wide").then_some(bytes))
        .expect("krusty emits lib/Wide");
    let parts =
        common::kotlin_metadata::kotlin_metadata_d1_parts(&krusty).expect("lib/Wide carries d1");
    assert!(parts.len() > 1, "d1 fits one entry: {} part", parts.len());
    common::byte_diff_against_kotlinc_cp("Wide", &source, "lib/Wide", &stdlib)
        .expect("reference kotlinc is provisioned")
        .expect("lib/Wide byte-identical to kotlinc");
}
