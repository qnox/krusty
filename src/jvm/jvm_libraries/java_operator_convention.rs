//! Which Java methods Kotlin calls as operators.

/// Kotlin operator capability of a Java instance method. Java declarations cannot encode
/// Kotlin's `operator` modifier, so the interoperability rule is defined entirely by the declared
/// method name and value-parameter arity.
pub(super) fn java_method_has_operator_convention(name: &str, arity: usize) -> bool {
    match name {
        "invoke" => true,
        "unaryPlus" | "unaryMinus" | "not" | "inc" | "dec" | "iterator" | "hasNext" | "next" => {
            arity == 0
        }
        "plus" | "minus" | "times" | "div" | "rem" | "rangeTo" | "rangeUntil" | "contains"
        | "compareTo" | "plusAssign" | "minusAssign" | "timesAssign" | "divAssign"
        | "remAssign" => arity == 1,
        "get" => arity >= 1,
        "set" => arity >= 2,
        "getValue" | "provideDelegate" => arity == 2,
        "setValue" => arity == 3,
        _ => name.strip_prefix("component").is_some_and(|suffix| {
            arity == 0
                && !suffix.is_empty()
                && suffix.bytes().all(|byte| byte.is_ascii_digit())
                && suffix.bytes().any(|byte| byte != b'0')
        }),
    }
}
