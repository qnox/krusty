//! kotlinc's generic `Signature` for a type with a `Nothing` argument.
//!
//! A type is written raw when one of its own arguments is `Nothing?`, or `Nothing` for a type
//! parameter not declared `in`; an enclosing type keeps its arguments. `Nothing` for an `in`
//! parameter is a star. A class header left with no generic structure has no `Signature`.

use super::common;

const SOURCE: &str = r#"interface Box<out T>
interface Sink<in T>
interface Inv<T>

class RawBox : Box<Nothing>
class StarSink : Sink<Nothing>
class NestedRaw : Inv<List<Nothing?>>

fun positions(x: Box<Nothing>, y: Sink<Nothing>, z: Inv<String?>): Box<Box<Nothing>>? = null
fun nullable(x: List<Nothing?>): Sink<Nothing>? = null
"#;

/// Each method's and the class's `Signature`, beside the declaration it belongs to.
fn signatures(bytes: &[u8]) -> Vec<String> {
    let work = common::scratch_dir().expect("a scratch directory");
    let path = work.join("Signatures.class");
    std::fs::write(&path, bytes).expect("class file");
    let text = common::javap(&["-p", "-v", &path.to_string_lossy()]).expect("javap runs");
    let _ = std::fs::remove_dir_all(work);
    text.lines()
        .map(str::trim)
        .filter(|line| {
            line.starts_with("Signature:")
                || line.starts_with("public")
                || line.starts_with("final")
        })
        .map(|line| {
            line.split_whitespace()
                .map(|token| if token.starts_with('#') { "#" } else { token })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

#[test]
fn nothing_arguments_are_signed_as_kotlinc_signs_them() {
    for class in ["RawBox", "StarSink", "NestedRaw", "SignaturesKt"] {
        let pair = common::ModuleClassPair::compile(&[("Signatures.kt", SOURCE)], class);
        assert_eq!(
            signatures(&pair.krusty),
            signatures(&pair.kotlinc),
            "{class}"
        );
    }
}
