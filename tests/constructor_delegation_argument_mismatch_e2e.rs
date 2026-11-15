//! A constructor reached by `super(...)`, `this(...)`, a class-header base call, or an enum entry
//! checks its arguments like any call: kotlinc reports each argument that does not fit the only
//! candidate's parameter, at that argument, with the ordinary argument mismatch.

use super::common;

#[test]
fn constructor_delegation_argument_mismatches_are_reported_like_kotlinc() {
    const SRC: &str = "open class Holder(val value: String)\n\
class Header : Holder(1)\n\
class Secondary : Holder {\n\
constructor() : super(1)\n\
}\n\
class Delegating(val value: String) {\n\
constructor() : this(1)\n\
}\n\
enum class Kind(val value: String) { A(1) }\n\
class Two(val count: Int, val text: String)\n\
val plain = Holder(1)\n\
val two = Two(1, 2)\n\
class PairHeader : Pair(1, 2)\n\
open class Pair(val first: String, val second: Int)\n";
    const INT_FOR_STRING: &str =
        "argument type mismatch: actual type is 'Int', but 'String' was expected.";
    let sources = [("Main.kt", SRC)];
    let reference_args = common::language_directives::kotlinc_args(SRC);
    let expected = ["2:23", "4:23", "7:22", "9:40", "11:20", "12:18", "13:25"]
        .into_iter()
        .map(|position| format!("Main.kt:{position}: {INT_FOR_STRING}"))
        .collect::<Vec<_>>();
    assert_eq!(
        common::reference_error_blocks(&sources, &reference_args),
        expected
    );
    common::assert_error_blocks_match_kotlinc(&sources, &reference_args);
}
