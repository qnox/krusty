//! A setter parameter uses the ordinary Kotlin value-parameter grammar, including annotations. If
//! parsing stops at that annotation, later members of the enclosing class disappear from dependants.

use super::common;

#[test]
fn annotated_setter_parameter_keeps_the_rest_of_the_class() {
    const SRC: &str = r#"
annotation class MagicConstant(val flagsFromClass: String)
annotation class SetterMarker

class FindModel {
    var flags: Int = 0
        set(
            @MagicConstant(flagsFromClass = "Pattern")
            @SetterMarker
            value,
        ) {
            field = value
        }

    enum class SearchContext { ANY }
}

fun box(): String {
    val model = FindModel()
    model.flags = 7
    return if (model.flags == 7 && FindModel.SearchContext.ANY.name == "ANY") "OK" else "fail"
}
"#;

    common::expect_box_ok_with_stdlib(SRC, "AnnotatedSetterParameter");
}
