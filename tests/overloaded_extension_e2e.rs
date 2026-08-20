//! Top-level extension overload selection and conflict diagnostics.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

fn assert_exact_unresolved_diagnostics(tag: &str, source: &str, expected: &[&str]) {
    let (reference_code, _) = common::kotlinc_source_result(tag, source);
    assert_ne!(reference_code, 0, "kotlinc accepted {tag}");
    assert_eq!(
        common::front_end_diagnostics_files_with_stdlib(&[source]),
        expected
            .iter()
            .map(|message| (*message).to_string())
            .collect::<Vec<_>>()
    );
}

#[test]
fn arity_overloaded_extension_on_user_class() {
    const SRC: &str = "\
class Box(val n: Int)\n\
fun Box.f() = f(1)\n\
fun Box.f(k: Int): Int = n + k\n\
fun box(): String =\n\
    if (Box(10).f() == 11 && Box(10).f(5) == 15) \"OK\" else \"FAIL\"\n";
    assert_eq!(run(SRC).expect("arity-overloaded extension"), "OK");
}

#[test]
fn arity_overloaded_extension_distinct_bodies() {
    const SRC: &str = "\
class S(val v: String)\n\
fun S.tag() = \"none\"\n\
fun S.tag(a: String) = a\n\
fun S.tag(a: String, b: String) = a + b\n\
fun box(): String {\n\
    val s = S(\"x\")\n\
    return if (s.tag() == \"none\" && s.tag(\"O\") == \"O\" && s.tag(\"O\", \"K\") == \"OK\")\n\
        \"OK\" else \"FAIL\"\n\
}\n";
    assert_eq!(run(SRC).expect("three-arity overloaded extension"), "OK");
}

#[test]
fn error_typed_conflict_key_ignores_unresolved_receivers() {
    assert_exact_unresolved_diagnostics(
        "UnresolvedExtensionReceivers",
        "fun MissingReceiverA.use() {}\nfun MissingReceiverB.use() {}\n",
        &[
            "unresolved reference 'MissingReceiverA'.",
            "unresolved reference 'MissingReceiverB'.",
        ],
    );
}

#[test]
fn error_typed_conflict_key_ignores_unresolved_extension_parameters() {
    assert_exact_unresolved_diagnostics(
        "UnresolvedExtensionParameters",
        "fun String.use(value: MissingParameterA) {}\n\
         fun String.use(value: MissingParameterB) {}\n",
        &[
            "unresolved reference 'MissingParameterA'.",
            "unresolved reference 'MissingParameterB'.",
        ],
    );
}

#[test]
fn error_typed_conflict_key_ignores_unresolved_ordinary_parameters() {
    assert_exact_unresolved_diagnostics(
        "UnresolvedOrdinaryParameters",
        "fun use(value: MissingParameterA) {}\n\
         fun use(value: MissingParameterB) {}\n",
        &[
            "unresolved reference 'MissingParameterA'.",
            "unresolved reference 'MissingParameterB'.",
        ],
    );
}

#[test]
fn error_typed_conflict_key_ignores_nested_unresolved_parameters() {
    assert_exact_unresolved_diagnostics(
        "NestedUnresolvedParameters",
        "fun use(value: List<MissingArgumentA>) {}\n\
         fun use(value: List<MissingArgumentB>) {}\n",
        &[
            "unresolved reference 'MissingArgumentA'.",
            "unresolved reference 'MissingArgumentB'.",
        ],
    );
}

#[test]
fn bounded_receiver_overloads_keep_distinct_erasure() {
    const MEMBER_SRC: &str = "\
interface Alpha\n\
interface Beta\n\
interface Catalog {\n\
    fun <T : Alpha> T.label(): String\n\
    fun <T : Beta> T.label(): String\n\
}\n";
    common::expect_front_end_ok_files_with_stdlib(&[MEMBER_SRC], "BoundedMemberReceiverOverloads");

    const SRC: &str = "\
interface Alpha\n\
interface Beta\n\
class First : Alpha\n\
class Second : Beta\n\
fun <T : Alpha> T.label(): String = \"A\"\n\
fun <T : Beta> T.label(): String = \"B\"\n\
fun box(): String = if (First().label() + Second().label() == \"AB\") \"OK\" else \"FAIL\"\n";
    common::expect_box_ok_with_stdlib(SRC, "BoundedReceiverOverloads");
}

#[test]
fn inline_arity_overloads_expand_the_checker_selected_body() {
    const SRC: &str = "\
class S\n\
inline fun S.tag() = \"zero\"\n\
inline fun S.tag(value: String) = value\n\
fun box(): String {\n\
    val s = S()\n\
    return if (s.tag() == \"zero\" && s.tag(\"OK\") == \"OK\") \"OK\" else \"FAIL\"\n\
}\n";
    assert_eq!(run(SRC).expect("inline extension overloads"), "OK");
}

#[test]
fn same_arity_overloads_lower_the_checker_selected_declaration() {
    const SRC: &str = "\
class S\n\
fun S.pick(value: Int): String = \"I\" + value\n\
fun S.pick(value: String): String = \"S\" + value\n\
fun box(): String {\n\
    val s = S()\n\
    return if (s.pick(2) == \"I2\" && s.pick(\"K\") == \"SK\") \"OK\" else \"FAIL\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("same-arity extension overloads must lower by source identity"),
        "OK"
    );
}

#[test]
fn unit_return_coercion_applies_only_to_lambda_literals() {
    const SRC: &str = "\
fun String.pick(block: () -> Unit): String = \"unit\"\n\
fun String.pick(value: Any): String = \"any\"\n\
fun box(): String {\n\
    val stored: () -> String = { \"value\" }\n\
    val literal = \"\".pick { \"value\" }\n\
    val functionValue = \"\".pick(stored)\n\
    return if (literal == \"unit\" && functionValue == \"any\") \"OK\" else \"$literal/$functionValue\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("Unit coercion must distinguish literals from function values"),
        "OK"
    );
}

#[test]
fn convention_calls_preserve_lambda_literal_unit_coercion() {
    const SRC: &str = "\
class Scope\n\
operator fun Scope.get(block: () -> Unit): String = \"unit\"\n\
operator fun Scope.get(value: Any): String = \"any\"\n\
operator fun Scope.plus(block: () -> Unit): String = \"unit\"\n\
operator fun Scope.plus(value: Any): String = \"any\"\n\
fun Scope.pick(block: () -> Unit): String = \"unit\"\n\
fun Scope.pick(value: Any): String = \"any\"\n\
operator fun Long?.compareTo(block: () -> Unit): Int = -1\n\
operator fun Long?.compareTo(value: Any): Int = 1\n\
fun safeLiteral(scope: Scope?): String? = scope?.pick { \"value\" }\n\
fun safeStored(scope: Scope?, block: () -> String): String? = scope?.pick(block)\n\
fun box(): String {\n\
    val scope = Scope()\n\
    val stored: () -> String = { \"value\" }\n\
    val actual = scope[{ \"value\" }] + scope[stored] +\n\
        (scope + ({ \"value\" })) + (scope + stored) +\n\
        safeLiteral(scope) + safeStored(scope, stored)\n\
    val nullable: Long? = 1L\n\
    val comparisons = nullable < ({ \"value\" }) && !(nullable < stored)\n\
    return if (actual == \"unitanyunitanyunitany\" && comparisons) \"OK\" else actual\n\
}\n";
    assert_eq!(
        run(SRC).expect("convention calls must preserve literal-lambda overload selection"),
        "OK"
    );
}

#[test]
fn convention_calls_lower_the_exact_cross_file_source_declaration() {
    let output = common::compile_and_run_files_with_stdlib(&[
        ("Model", "package model\nclass Box\n"),
        (
            "Imported",
            "package imported\n\
             import model.Box\n\
             operator fun Box.plus(value: Int): String = \"imported-plus\"\n\
             operator fun Box.get(index: Int): String = \"imported-get\"\n\
             fun Box.pick(value: Int): String = \"imported-pick\"\n\
             operator fun Long?.compareTo(other: Long?): Int = -1\n",
        ),
        (
            "Use",
            "package use\n\
             import model.Box\n\
             import imported.plus\n\
             import imported.get\n\
             import imported.pick\n\
             import imported.compareTo\n\
             operator fun Box.plus(value: Int): String = \"local-plus\"\n\
             operator fun Box.get(index: Int): String = \"local-get\"\n\
             fun Box.pick(value: Int): String = \"local-pick\"\n\
             operator fun Long?.compareTo(other: Long?): Int = 1\n\
             fun safePick(box: Box?): String? = box?.pick(1)\n\
             fun box(): String {\n\
                 val value: Long? = 1L\n\
                 val other: Long? = 2L\n\
                 val exact = Box() + 1 == \"imported-plus\" &&\n\
                     Box()[0] == \"imported-get\" &&\n\
                     safePick(Box()) == \"imported-pick\" && value < other\n\
                 return if (exact) \"OK\" else \"FAIL\"\n\
             }\n",
        ),
    ]);
    assert_eq!(output.as_deref(), Some("OK"));
}

#[test]
fn cross_file_generic_extension_boxes_a_primitive_receiver() {
    let output = common::compile_and_run_files_with_stdlib(&[
        ("Generic", "package generic\nfun <T> T.id(): T = this\n"),
        (
            "Use",
            "package use\n\
             import generic.id\n\
             fun box(): String = if (42.id() == 42) \"OK\" else \"FAIL\"\n",
        ),
    ]);
    assert_eq!(output.as_deref(), Some("OK"));
}

#[test]
fn identical_extension_signatures_in_distinct_packages_keep_source_identity() {
    let outputs = common::compile_in_process_files(
        &[
            (
                "A",
                "package a\n\
                 fun String.packageScoped() = 1\n\
                 fun useA(): Int = \"\".packageScoped()\n",
            ),
            (
                "B",
                "package b\n\
                 fun String.packageScoped() = \"B\"\n\
                 fun useB(): String = \"\".packageScoped()\n",
            ),
        ],
        &[],
        None,
    );
    assert!(
        outputs.is_some(),
        "cross-package extension declarations selected each other's signatures"
    );
}

#[test]
fn explicit_import_does_not_expose_unrelated_extensions_from_its_package() {
    let outputs = common::compile_in_process_files(
        &[
            (
                "A",
                "package a\n\
                 class Unrelated\n\
                 fun String.hiddenExtension(): Int = 1\n",
            ),
            (
                "B",
                "package b\n\
                 import a.Unrelated\n\
                 fun invalid(): Int = \"\".hiddenExtension()\n",
            ),
        ],
        &[],
        None,
    );
    assert!(
        outputs.is_none(),
        "an explicit class import exposed an unrelated package extension"
    );
}

#[test]
fn internal_extension_is_visible_across_files_in_one_module() {
    let output = common::compile_and_run_files_with_stdlib(&[
        (
            "Declaration",
            "package shared\n\
             internal fun String.moduleTag(): String = this\n",
        ),
        (
            "Use",
            "package shared\n\
             fun box(): String = \"OK\".moduleTag()\n",
        ),
    ]);
    assert_eq!(output.as_deref(), Some("OK"));
}

#[test]
fn internal_extension_is_not_visible_across_module_boundary() {
    let Some(library) = common::compile_lib(
        "internal_extension_boundary",
        "package dependency\ninternal fun String.secret(): String = this\n",
    ) else {
        eprintln!("skipping: provisioned kotlinc toolchain unavailable");
        return;
    };
    let outputs = common::compile_in_process_files(
        &[(
            "Use",
            "package consumer\n\
             import dependency.secret\n\
             fun box(): String = \"OK\".secret()\n",
        )],
        &[library],
        None,
    );
    assert!(
        outputs.is_none(),
        "a dependency module's internal extension must remain inaccessible"
    );
}

#[test]
fn top_level_callable_import_alias_invokes_the_declared_name() {
    let output = common::compile_and_run_files_with_stdlib(&[
        (
            "Declaration",
            "package helpers\n\
             fun declaredTop(): String = \"OK\"\n",
        ),
        (
            "Use",
            "package use\n\
             import helpers.declaredTop as visibleTop\n\
             fun box(): String = visibleTop()\n",
        ),
    ]);
    assert_eq!(output.as_deref(), Some("OK"));
}

#[test]
fn extension_callable_import_alias_resolves_the_declared_name() {
    let diagnostics = common::front_end_diagnostics_files(
        &[
            "package helpers\nfun String.declaredExtension(): String = this",
            "package use\n\
             import helpers.declaredExtension as visibleExtension\n\
             fun call(value: String): String = value.visibleExtension()",
        ],
        &[],
        None,
    );
    assert!(
        diagnostics.is_empty(),
        "extension alias should resolve its declared callable: {diagnostics:?}"
    );
}

#[test]
fn own_package_extension_outranks_a_star_import() {
    let output = common::compile_and_run_files_with_stdlib(&[
        (
            "Imported",
            "package imported\n\
             fun String.preferred(): String = \"imported\"\n",
        ),
        (
            "Own",
            "package own\n\
             import imported.*\n\
             fun String.preferred(): String = \"own\"\n\
             fun box(): String = if (\"\".preferred() == \"own\") \"OK\" else \"FAIL\"\n",
        ),
    ]);
    assert_eq!(output.as_deref(), Some("OK"));
}

#[test]
fn equally_applicable_extensions_from_two_star_imports_are_ambiguous() {
    let outputs = common::compile_in_process_files(
        &[
            ("A", "package a\nfun String.starCollision(): Int = 1\n"),
            ("B", "package b\nfun String.starCollision(): Int = 2\n"),
            (
                "Use",
                "package use\n\
                 import a.*\n\
                 import b.*\n\
                 fun invalid(): Int = \"\".starCollision()\n",
            ),
        ],
        &[],
        None,
    );
    assert!(
        outputs.is_none(),
        "two equally applicable star-imported extensions resolved by insertion order"
    );
}

#[test]
fn indexed_get_from_two_star_imports_is_ambiguous() {
    let outputs = common::compile_in_process_files(
        &[
            ("Model", "package model\nclass Box\n"),
            (
                "A",
                "package a\n\
                 import model.Box\n\
                 operator fun Box.get(index: Int): String = \"A\"\n",
            ),
            (
                "B",
                "package b\n\
                 import model.Box\n\
                 operator fun Box.get(index: Int): String = \"B\"\n",
            ),
            (
                "Use",
                "package use\n\
                 import model.Box\n\
                 import a.*\n\
                 import b.*\n\
                 fun invalid(): String = Box()[0]\n",
            ),
        ],
        &[],
        None,
    );
    assert!(
        outputs.is_none(),
        "indexed-get convention selected the first star-imported source extension"
    );
}

#[test]
fn binary_operator_from_two_star_imports_is_ambiguous() {
    let outputs = common::compile_in_process_files(
        &[
            ("Model", "package model\nclass Box\n"),
            (
                "A",
                "package a\n\
                 import model.Box\n\
                 operator fun Box.plus(value: Int): String = \"A\"\n",
            ),
            (
                "B",
                "package b\n\
                 import model.Box\n\
                 operator fun Box.plus(value: Int): String = \"B\"\n",
            ),
            (
                "Use",
                "package use\n\
                 import model.Box\n\
                 import a.*\n\
                 import b.*\n\
                 fun invalid(): String = Box() + 1\n",
            ),
        ],
        &[],
        None,
    );
    assert!(
        outputs.is_none(),
        "binary convention selected the first star-imported source extension"
    );
}

#[test]
fn widened_extensions_from_two_star_imports_are_ambiguous() {
    let outputs = common::compile_in_process_files(
        &[
            (
                "A",
                "package a\nfun String.widenedCollision(value: Any): Int = 1\n",
            ),
            (
                "B",
                "package b\nfun String.widenedCollision(value: Any): Int = 2\n",
            ),
            (
                "Use",
                "package use\n\
                 import a.*\n\
                 import b.*\n\
                 fun invalid(value: String): Int = \"\".widenedCollision(value)\n",
            ),
        ],
        &[],
        None,
    );
    assert!(
        outputs.is_none(),
        "widened applicability selected the first star-imported source extension"
    );
}

#[test]
fn defaulted_extensions_from_two_star_imports_are_ambiguous() {
    let outputs = common::compile_in_process_files(
        &[
            (
                "A",
                "package a\nfun String.defaultCollision(value: Int = 1): Int = value\n",
            ),
            (
                "B",
                "package b\nfun String.defaultCollision(value: Int = 2): Int = value\n",
            ),
            (
                "Use",
                "package use\n\
                 import a.*\n\
                 import b.*\n\
                 fun invalid(): Int = \"\".defaultCollision()\n",
            ),
        ],
        &[],
        None,
    );
    assert!(
        outputs.is_none(),
        "default omission selected the first star-imported source extension"
    );
}

#[test]
fn vararg_extensions_from_two_star_imports_are_ambiguous() {
    let outputs = common::compile_in_process_files(
        &[
            (
                "A",
                "package a\nfun String.varargCollision(vararg values: Int): Int = values.size\n",
            ),
            (
                "B",
                "package b\nfun String.varargCollision(vararg values: Int): Int = values.size\n",
            ),
            (
                "Use",
                "package use\n\
                 import a.*\n\
                 import b.*\n\
                 fun invalid(): Int = \"\".varargCollision(1, 2)\n",
            ),
        ],
        &[],
        None,
    );
    assert!(
        outputs.is_none(),
        "vararg spreading selected the first star-imported source extension"
    );
}

#[test]
fn explicit_classpath_get_outranks_same_package_source_get() {
    let output = common::run_box_against(
        "explicit_classpath_get_precedence",
        "package lib\n\
         class Box\n\
         operator fun Box.get(index: Int): String = \"LIB\"\n",
        "package use\n\
         import lib.Box\n\
         import lib.get\n\
         operator fun Box.get(index: Int): String = \"SOURCE\"\n\
         fun box(): String = if (Box()[0] == \"LIB\") \"OK\" else \"FAIL\"\n",
    );
    let Some(output) = output else {
        eprintln!("skipping: provisioned kotlinc/JVM toolchain unavailable");
        return;
    };
    assert_eq!(output, "OK");
}
