//! The `@Metadata` of a suspend lambda's class records the lambda's function (`<anonymous>`): its
//! receiver, each value parameter under its source name, and its result, as kotlinc writes it for
//! reflection.

use super::common;

const SRC: &str = "class C\n\
class Box<T>(val value: T)\n\
suspend fun pause() {}\n\
fun a(c: suspend (Int) -> String) {}\n\
fun b(c: suspend C.(String, Long) -> Int) {}\n\
fun g(c: suspend () -> Box<String>) {}\n\
fun use() {\n\
    a { pause(); \"s\" }\n\
    a { v -> pause(); \"v\" }\n\
    b { s, _ -> pause(); 1 }\n\
    g { pause(); Box(\"\") }\n\
    val n: suspend (C?) -> Unit = { pause() }\n\
}\n";

fn expect_class_matches(class: &str) {
    common::metadata_header_diff_against_kotlinc_cp(
        "LambdaMetadata",
        SRC,
        class,
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .expect("the lambda class's @Metadata is kotlinc's");
}

#[test]
fn an_implicit_it_parameter_is_recorded_as_it() {
    expect_class_matches("LambdaMetadataKt$use$1");
}

#[test]
fn a_named_parameter_is_recorded_under_its_name() {
    expect_class_matches("LambdaMetadataKt$use$2");
}

#[test]
fn a_receiver_and_an_unused_parameter_are_recorded() {
    expect_class_matches("LambdaMetadataKt$use$3");
}

#[test]
fn a_generic_result_is_recorded_with_its_argument() {
    expect_class_matches("LambdaMetadataKt$use$4");
}

#[test]
fn a_lambda_bound_to_a_local_records_its_nullable_parameter() {
    expect_class_matches("LambdaMetadataKt$use$n$1");
}
