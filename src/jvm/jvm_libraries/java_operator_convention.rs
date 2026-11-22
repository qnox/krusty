//! How Kotlin calls a member that a classfile declares without Kotlin metadata: its call shape,
//! the operators a Java method is by convention, and the parameters a Java method declares.

use crate::libraries::{CallSig, LibraryMember};

/// The classfile facts [`publish_classfile_member_shape`] reads for one member.
pub(super) struct ClassfileMemberShape {
    /// The member's normalized value-parameter count.
    pub(super) value_arity: usize,
    /// The method is `ACC_VARARGS`: its last parameter is the `vararg`.
    pub(super) vararg: bool,
    /// The declaring class follows Java's type semantics, which give a method operator conventions.
    pub(super) java_semantics: bool,
    /// The member is a Java declaration: a method of a class without Kotlin metadata, not a
    /// constructor, which publishes its own declaration parameters. A Kotlin class's synthetic
    /// members (`constructor-impl`, `box-impl`) declare nothing of their own.
    pub(super) java_declaration: bool,
}

/// Publish the call shape of `member`, a classfile member with no Kotlin declaration of its own.
pub(super) fn publish_classfile_member_shape(
    member: &mut LibraryMember,
    shape: ClassfileMemberShape,
) {
    // A Java method is declared by its classfile signature, so its normalized parameters are the
    // declared parameters a call's arguments are represented for.
    if shape.java_declaration {
        member.declared_params = Some(member.params.clone().into_boxed_slice());
    }
    let vararg_index = shape
        .vararg
        .then_some(shape.value_arity)
        .and_then(|arity| arity.checked_sub(1));
    member.call_sig =
        CallSig::metadata_member(shape.value_arity, Vec::new(), Vec::new(), vararg_index);
    // Java has no `operator` modifier. Kotlin therefore recognizes a Java method as an operator
    // declaration from the language-defined name/arity convention. Normalize that capability
    // here; the resolver can then require the same semantic flag for Kotlin metadata, Java
    // classfiles, and Java source stubs.
    if shape.java_semantics
        && java_method_has_operator_convention(&member.name, member.params.len())
    {
        member.set_is_operator(true);
    }
}

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
