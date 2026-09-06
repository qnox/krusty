//! Several vararg ELEMENTS written positionally, followed by a NAMED argument for a later
//! defaulted parameter: `"a,b".split(",", ";", limit = 2)`. kotlinc packs the leading elements
//! into the vararg and binds `limit` by name. A single element before the named argument already
//! worked; two or more must map the same way.

use super::common;

fn stdlib_diagnostics(src: &str) -> Vec<String> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    common::front_end_diagnostics(src, std::slice::from_ref(&stdlib), Some(jdk.as_path()))
}

#[test]
fn one_vararg_element_before_a_named_argument_resolves() {
    let d = stdlib_diagnostics(
        "package sample\nprivate val parts = \"a,b\".split(\",\", ignoreCase = false)\nfun n(): Int = parts.size\n",
    );
    assert_eq!(d, Vec::<String>::new());
}

#[test]
fn two_vararg_elements_before_a_named_argument_resolve() {
    let d = stdlib_diagnostics(
        "package sample\nprivate val parts = \"a,b;c\".split(\",\", \";\", limit = 2)\nfun n(): Int = parts.size\n",
    );
    assert_eq!(d, Vec::<String>::new());
}

#[test]
fn two_vararg_elements_before_a_named_argument_resolve_in_a_body() {
    let d = stdlib_diagnostics(
        "package sample\nfun n(): Int = \"a,b;c\".split(\",\", \";\", limit = 2).size\n",
    );
    assert_eq!(d, Vec::<String>::new());
}

#[test]
fn two_vararg_elements_before_a_named_argument_resolve_on_a_module_function() {
    let d = stdlib_diagnostics(
        "package sample\n\
         fun option(vararg names: String, help: String = \"\"): String = names.joinToString() + help\n\
         private val target = option(\"--target\", \"-t\", help = \"Target\")\n\
         fun n(): Int = target.length\n",
    );
    assert_eq!(d, Vec::<String>::new());
}

#[test]
fn a_vararg_extra_of_another_type_still_declines_in_signature_position() {
    // `names` is a String vararg: an Int extra is not its element. The signature pass must not
    // pretend the call applies by dropping the extra; Pass 2 reports the real applicability error
    // (kotlinc's message), and the deferred decline report stays quiet because that error sits
    // inside the declaration.
    let d = stdlib_diagnostics(
        "package sample\n\
         fun option(vararg names: String, help: String = \"\"): String = names.joinToString() + help\n\
         private val target = option(\"--target\", 2, help = \"Target\")\n\
         fun n(): Int = target.length\n",
    );
    assert_eq!(
        d,
        vec![
            "argument type mismatch: actual type is 'Int', but 'String' was expected.".to_string()
        ],
        "an Int extra on a String vararg must be reported once, as kotlinc does"
    );
}

#[test]
fn two_vararg_elements_alone_resolve_on_a_module_function_with_a_non_final_vararg() {
    // Positional elements only: the trailing defaulted parameter is omitted. The signature pass
    // must expand the vararg at ITS slot rather than assuming the vararg is last.
    let d = stdlib_diagnostics(
        "package sample\n\
         fun option(vararg names: String, help: String = \"\"): String = names.joinToString() + help\n\
         private val target = option(\"--target\", \"-t\")\n\
         fun n(): Int = target.length\n",
    );
    assert_eq!(d, Vec::<String>::new());
}

#[test]
fn vararg_elements_before_a_named_required_argument_resolve_on_a_module_function() {
    let d = stdlib_diagnostics(
        "package sample\n\
         fun mid(vararg names: String, count: Int): Int = names.size + count\n\
         fun both() = mid(\"a\", \"b\", count = 1)\n\
         fun one() = mid(\"a\", count = 1)\n\
         fun n(): Int = both() + one()\n",
    );
    assert_eq!(d, Vec::<String>::new());
}

/// The acceptance criterion is bytes: the signature pass selecting the right overload must leave
/// the emitted class identical to kotlinc's, on the JDK-25 target a Gradle toolchain builds for.
fn assert_byte_identical(name: &str, src: &str, class: &str) {
    match common::byte_diff_against_kotlinc_cp_target(
        name,
        src,
        class,
        &[common::stdlib_jar()],
        Some("25"),
    ) {
        None => eprintln!("skip ({name}: reference toolchain unavailable)"),
        Some(Ok(())) => {}
        Some(Err(diff)) => panic!("{name}: class bytes differ from kotlinc:\n{diff}"),
    }
}

#[test]
fn packed_vararg_arrays_are_byte_identical() {
    // Packing goes through a local, as kotlinc does: `anewarray; astore n; aload n; …; aload n`.
    // `split(",", ";", limit = 2)` itself is still kept out of the byte test by a separate
    // residue — kotlinc `checkcast`s a `String` receiver to the `CharSequence` extension
    // receiver — so the stdlib shapes here are the packing ones.
    assert_byte_identical(
        "PackedVarargArrays",
        "fun names() = listOf(\"a\", \"b\")\n\
         fun sizes() = intArrayOf(1, 2)\n\
         fun labels() = arrayOf(\"x\", \"y\")\n\
         fun count(): Int = names().size + sizes().size + labels().size\n",
        "PackedVarargArraysKt",
    );
}

#[test]
fn module_vararg_elements_before_named_argument_are_byte_identical() {
    assert_byte_identical(
        "VarargElementsNamedModule",
        "fun option(vararg names: String, help: String = \"\"): String = help + names.size\n\
         fun target() = option(\"--target\", \"-t\", help = \"Target\")\n\
         fun plain() = option(\"--plain\", \"-p\")\n\
         fun count(): Int = target().length + plain().length\n",
        "VarargElementsNamedModuleKt",
    );
}
