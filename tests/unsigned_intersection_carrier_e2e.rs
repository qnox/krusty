//! An unsigned class bound stays the JVM carrier when an interface bound is also declared.
//!
//! `fun <T : X, X : Comparable<UInt>> f(arg: T) where X : UInt` erases `arg` to `int`. The
//! interface constituent is only a generic-signature bound. Passing `3U` through `UInt.box-impl`
//! and then invoking `(I)` fails verification.

use super::bytecode_comparison_support::compare_with_kotlinc_plugin;
use super::common;

const SOURCE: &str = "\
fun <T : X, X : Comparable<UInt>> testContains(arg: T): Boolean where X : UInt = arg in 1U..10U

fun <T : X, X : UInt> testFor(arg: T): Int where X : Comparable<UInt> {
    var sum = 0
    for (i in arg..arg + 3U) sum += i.toInt()
    return sum
}

class Carrier {
    fun <T : X, X : Comparable<UInt>> member(arg: T): Boolean where X : UInt = arg in 1U..10U
}

fun <T : X, X : Comparable<UInt>> named(before: Int, arg: T): Boolean where X : UInt =
    before == 1 && arg in 1U..10U

fun <T : X, X : Comparable<UInt>> omittedBefore(skip: Int = 7, arg: T): Int where X : UInt =
    skip + arg.toInt()

fun <T : X, X : Comparable<UInt>> omittedAfter(arg: T, skip: Int = 7): Int where X : UInt =
    skip + arg.toInt()

fun box(): String {
    if (!testContains(3U)) return \"FAIL contains\"
    if (testContains(0U)) return \"FAIL contains zero\"
    if (testFor(0U) != 6) return \"FAIL for\"
    if (!Carrier().member<UInt, UInt>(3U)) return \"FAIL member\"
    if (!named(arg = 4U, before = 1)) return \"FAIL named\"
    if (named(arg = 0U, before = 1)) return \"FAIL named zero\"
    if (omittedBefore(arg = 3U) != 10) return \"FAIL default before\"
    if (omittedAfter(3U) != 10) return \"FAIL default after\"
    return \"OK\"
}
";

#[test]
fn an_unsigned_class_bound_is_passed_as_its_carrier() {
    common::expect_box_ok_with_stdlib(SOURCE, "UnsignedIntersectionCarrier");
}

#[test]
fn a_same_module_unsigned_class_bound_is_passed_as_its_carrier() {
    common::expect_box_ok_files_with_stdlib(
        &[
            (
                "Other.kt",
                "fun <T : X, X : Comparable<UInt>> fromOther(arg: T): Boolean where X : UInt = arg in 1U..10U\n",
            ),
            (
                "Caller.kt",
                "fun box(): String {\n    if (!fromOther(3U)) return \"FAIL\"\n    if (fromOther(0U)) return \"FAIL zero\"\n    return \"OK\"\n}\n",
            ),
        ],
        "UnsignedIntersectionCarrierModule",
    );
}

#[test]
fn a_classpath_unsigned_class_bound_is_passed_as_its_carrier() {
    let library = "\
fun <T : X, X : Comparable<UInt>> fromDependency(arg: T): Boolean where X : UInt = arg in 1U..10U
";
    let main = "\
fun box(): String {
    if (!fromDependency(3U)) return \"FAIL\"
    if (fromDependency(0U)) return \"FAIL zero\"
    return \"OK\"
}
";
    let result = common::expect_box_run_against_kotlinc(library, main)
        .expect("kotlinc dependency and krusty caller");
    assert_eq!(result.trim(), "OK");
}

#[test]
fn unsigned_class_bound_call_instructions_match_kotlinc() {
    let compared = compare_with_kotlinc_plugin(
        "UnsignedIntersectionCarrier",
        SOURCE,
        "UnsignedIntersectionCarrierKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("kotlinc and krusty both compile the intersection carrier");
    let box_method = exact_method(&compared.reference_bytes, "box", "()Ljava/lang/String;");
    let krusty_box = exact_method(&compared.krusty_bytes, "box", "()Ljava/lang/String;");
    assert_eq!(krusty_box, box_method, "box(): flags, name and descriptor");
    // Named arguments spill in source order before they are moved into parameter order, so `box`
    // is not kotlinc's constant-reordered body. Its invoke sequence is the exact carrier contract.
    assert_eq!(
        invoke_instructions(&exact_instructions(
            &compared.krusty,
            &box_method.1,
            &box_method.2
        )),
        invoke_instructions(&exact_instructions(
            &compared.reference,
            &box_method.1,
            &box_method.2
        )),
        "box() invoke sequence"
    );
    for (prefix, descriptor) in [("omittedBefore-", "(II)I"), ("omittedAfter-", "(II)I")] {
        let reference = exact_method(&compared.reference_bytes, prefix, descriptor);
        let krusty = exact_method(&compared.krusty_bytes, prefix, descriptor);
        assert_eq!(krusty, reference, "{prefix}: flags, name and descriptor");
        assert_eq!(
            exact_instructions(&compared.krusty, &reference.1, descriptor),
            exact_instructions(&compared.reference, &reference.1, descriptor),
            "{prefix} instructions"
        );
    }
    // `in` lowers through `Integer.compareUnsigned`; kotlinc's body calls `uintCompare` and
    // reorders constants. The carrier contract is the shared `int` descriptor and these exact
    // invokes, none of which box `UInt`.
    for (prefix, descriptor, compares) in [
        (
            "testContains-",
            "(I)Z",
            vec![
                "invokestatic # // Method java/lang/Integer.compareUnsigned:(II)I",
                "invokestatic # // Method java/lang/Integer.compareUnsigned:(II)I",
            ],
        ),
        (
            "named-",
            "(II)Z",
            vec![
                "invokestatic # // Method java/lang/Integer.compareUnsigned:(II)I",
                "invokestatic # // Method java/lang/Integer.compareUnsigned:(II)I",
            ],
        ),
        (
            "testFor-",
            "(I)I",
            vec![
                "invokestatic # // Method kotlin/UInt.\"constructor-impl\":(I)I",
                "invokestatic # // Method kotlin/UnsignedKt.uintCompare:(II)I",
            ],
        ),
    ] {
        let reference = exact_method(&compared.reference_bytes, prefix, descriptor);
        let krusty = exact_method(&compared.krusty_bytes, prefix, descriptor);
        assert_eq!(krusty, reference, "{prefix}: flags, name and descriptor");
        assert_eq!(
            invoke_instructions(&exact_instructions(
                &compared.krusty,
                &reference.1,
                descriptor
            )),
            compares,
            "{prefix} unsigned compares"
        );
    }
}

fn invoke_instructions(instructions: &[String]) -> Vec<&str> {
    instructions
        .iter()
        .filter(|line| line.contains("invoke"))
        .map(String::as_str)
        .collect()
}

fn exact_method(bytes: &[u8], prefix: &str, descriptor: &str) -> (u16, String, String) {
    let class = krusty::jvm::classreader::parse_class(bytes).expect("readable class file");
    let matches = class
        .methods
        .iter()
        .filter(|method| method.name.starts_with(prefix) && method.descriptor == descriptor)
        .collect::<Vec<_>>();
    let [method] = matches.as_slice() else {
        panic!("expected exactly one {prefix}*{descriptor}, found {matches:?}")
    };
    (
        method.access,
        method.name.clone(),
        method.descriptor.clone(),
    )
}

fn exact_instructions(disassembly: &str, name: &str, descriptor: &str) -> Vec<String> {
    let lines = disassembly.lines().map(str::trim).collect::<Vec<_>>();
    let blocks = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| is_javap_method_header(line))
        .filter(|(_, line)| {
            line.split_once('(')
                .and_then(|(head, _)| head.split_whitespace().last())
                == Some(name)
        })
        .filter_map(|(start, _)| {
            let end = lines[start + 1..]
                .iter()
                .position(|line| is_javap_method_header(line))
                .map_or(lines.len(), |offset| start + 1 + offset);
            lines[start + 1..end]
                .iter()
                .any(|line| *line == format!("descriptor: {descriptor}"))
                .then_some((start, end))
        })
        .collect::<Vec<_>>();
    let [(start, end)] = blocks.as_slice() else {
        panic!("expected exactly one {name}{descriptor} disassembly, found {blocks:?}")
    };
    lines[*start + 1..*end]
        .iter()
        .filter_map(|line| {
            let (pc, instruction) = line.split_once(": ")?;
            pc.parse::<u32>().ok()?;
            Some(
                instruction
                    .split_whitespace()
                    .map(|token| if token.starts_with('#') { "#" } else { token })
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        })
        .collect()
}

fn is_javap_method_header(line: &str) -> bool {
    line.ends_with(';') && line.contains('(') && !line.contains(':') && !line.starts_with('#')
}
