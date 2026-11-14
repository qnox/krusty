//! `+AllowSmartCastsOnValueClassUnderlyingProperties`: a component of a value class declared in
//! another module is a stable smart-cast subject, for the single underlying property of an inline
//! value class and for each field of a multi-field one. Any other property of another module stays
//! unstable, and so does every property while the feature is off.

use super::common;

const FEATURES: &str = "+FullValueClasses, +AllowSmartCastsOnValueClassUnderlyingProperties";

fn library(features: &str) -> String {
    format!(
        "\
// LANGUAGE: {features}
package lib
value class MultiField(val first: String?, val second: Int?)
@JvmInline value class SingleField(val only: String?)
class Plain(val name: String?)
"
    )
}

#[test]
fn value_class_components_of_another_module_smart_cast_under_the_feature() {
    common::expect_box_ok_against(
        "value class components across modules",
        &library(FEATURES),
        &format!(
            "\
// LANGUAGE: {FEATURES}
import lib.MultiField
import lib.SingleField

fun first(v: MultiField): String = if (v.first != null) v.first else \"fail: first\"
fun second(v: MultiField): Int = if (v.second != null) v.second else -1
fun only(v: SingleField): String = if (v.only != null) v.only else \"fail: only\"

fun box(): String {{
    if (second(MultiField(\"x\", 7)) != 7) return \"fail: second\"
    return first(MultiField(\"O\", 1)) + only(SingleField(\"K\"))
}}
"
        ),
    );
}

const CONSUMER: &str = "\
import lib.MultiField
import lib.Plain
import lib.SingleField

fun first(v: MultiField): String = if (v.first != null) v.first else \"none\"
fun second(v: MultiField): Int = if (v.second != null) v.second else 0
fun only(v: SingleField): String = if (v.only != null) v.only else \"none\"
fun name(p: Plain): String = if (p.name != null) p.name else \"none\"
";

const MISMATCH: &str = "return type mismatch: expected 'String', actual 'String?'.";

/// With the feature on only the ordinary class's property is still unstable; with it off every read is.
#[test]
fn another_modules_properties_stay_unstable_without_the_feature_or_outside_a_value_class() {
    let consumer = |features: &str| format!("// LANGUAGE: {features}\n{CONSUMER}");
    let enabled = common::diagnostics_against(
        "components enabled",
        &library(FEATURES),
        &consumer(FEATURES),
    );
    assert_eq!(enabled, Some(vec![MISMATCH.to_string()]));

    let disabled = common::diagnostics_against(
        "components disabled",
        &library("+FullValueClasses"),
        &consumer("+FullValueClasses"),
    );
    assert_eq!(
        disabled,
        Some(vec![
            MISMATCH.to_string(),
            "return type mismatch: expected 'Int', actual 'Int?'.".to_string(),
            MISMATCH.to_string(),
            MISMATCH.to_string(),
        ])
    );
}
