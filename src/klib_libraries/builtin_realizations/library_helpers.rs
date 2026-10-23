//! Exact helper declarations reached from prepared-inline stdlib and kotlin-test bodies.

use crate::libraries::builtin_declaration::BuiltinFunctionDeclaration;
use crate::libraries::{CompilerIntrinsic, FnKind, ResultFailureIntrinsic};
use crate::types::{type_name, Ty, TyKind};

pub(super) fn realization(
    declaration: BuiltinFunctionDeclaration<'_>,
    compiler_builtin_owner: bool,
) -> Option<CompilerIntrinsic> {
    let facts = declaration;
    if compiler_builtin_owner
        && facts.package == type_name("kotlin/text")
        && facts.name == "trim"
        && facts.kind == FnKind::Extension
        && facts.receiver == Some(Ty::obj("kotlin/CharSequence"))
        && facts.params.is_empty()
        && facts.ret == Ty::obj("kotlin/CharSequence")
        && facts.context_count == 0
        && facts.type_parameter_count == 0
        && facts.vararg.is_none()
        && !facts.is_suspend
        && !facts.is_operator
        && !facts.is_infix
    {
        return Some(CompilerIntrinsic::TextTrim);
    }
    if !compiler_builtin_owner
        || facts.kind != FnKind::TopLevel
        || facts.receiver.is_some()
        || facts.context_count != 0
        || facts.vararg.is_some()
        || facts.is_suspend
        || facts.is_operator
        || facts.is_infix
    {
        return None;
    }
    if facts.package == type_name("kotlin") {
        let unsigned = match facts.name {
            "uintDivide" => Some((Ty::UInt, false)),
            "uintRemainder" => Some((Ty::UInt, true)),
            "ulongDivide" => Some((Ty::ULong, false)),
            "ulongRemainder" => Some((Ty::ULong, true)),
            _ => None,
        };
        if let Some((operand, remainder)) = unsigned {
            let operation = if remainder {
                CompilerIntrinsic::UnsignedRemainder { operand }
            } else {
                CompilerIntrinsic::UnsignedDivide { operand }
            };
            return (facts.type_parameter_count == 0
                && facts.params == [operand, operand]
                && facts.ret == operand)
                .then_some(operation);
        }
        if facts.name == "createFailure" {
            return (facts.type_parameter_count == 0
                && facts.params == [Ty::obj("kotlin/Throwable")]
                && facts.ret == Ty::obj("kotlin/Any"))
            .then_some(CompilerIntrinsic::ResultFailure(
                ResultFailureIntrinsic::Create,
            ));
        }
    }
    if facts.package != type_name("kotlin/test") || facts.name != "checkResultIsFailure" {
        return None;
    }
    let result = Ty::obj_args("kotlin/Result", &[Ty::nullable(Ty::obj("kotlin/Any"))]);
    let message = Ty::nullable(Ty::String);
    let throwable = Ty::obj("kotlin/Throwable");
    match facts.params {
        [given_message, given_result]
            if facts.type_parameter_count == 0
                && *given_message == message
                && *given_result == result
                && facts.ret == throwable =>
        {
            Some(CompilerIntrinsic::ResultFailure(
                ResultFailureIntrinsic::Check,
            ))
        }
        [class, given_message, given_result]
            if facts.type_parameter_count == 1
                && *given_message == message
                && *given_result == result
                && matches!(facts.ret.kind(), TyKind::TyParam(_, bound) if bound == throwable)
                && *class == Ty::obj_args("kotlin/reflect/KClass", &[facts.ret]) =>
        {
            Some(CompilerIntrinsic::ResultFailure(
                ResultFailureIntrinsic::CheckOfClass,
            ))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn helper<'a>(
        package: &str,
        name: &'a str,
        params: &'a [Ty],
        ret: Ty,
    ) -> BuiltinFunctionDeclaration<'a> {
        BuiltinFunctionDeclaration {
            package: type_name(package),
            name,
            kind: FnKind::TopLevel,
            receiver: None,
            params,
            ret,
            context_count: 0,
            type_parameter_count: 0,
            vararg: None,
            is_suspend: false,
            is_operator: false,
            is_infix: false,
        }
    }

    #[test]
    fn text_trim_requires_the_stdlib_and_the_complete_extension_signature() {
        fn trim<'a>() -> BuiltinFunctionDeclaration<'a> {
            let mut declaration =
                helper("kotlin/text", "trim", &[], Ty::obj("kotlin/CharSequence"));
            declaration.kind = FnKind::Extension;
            declaration.receiver = Some(Ty::obj("kotlin/CharSequence"));
            declaration
        }

        assert_eq!(realization(trim(), true), Some(CompilerIntrinsic::TextTrim));
        assert_eq!(realization(trim(), false), None);
        let mut wrong_package = trim();
        wrong_package.package = type_name("example/text");
        assert_eq!(realization(wrong_package, true), None);
        let mut wrong_receiver = trim();
        wrong_receiver.receiver = Some(Ty::String);
        assert_eq!(realization(wrong_receiver, true), None);
        let mut nullable_receiver = trim();
        nullable_receiver.receiver = Some(Ty::nullable(Ty::obj("kotlin/CharSequence")));
        assert_eq!(realization(nullable_receiver, true), None);
        let mut wrong_result = trim();
        wrong_result.ret = Ty::String;
        assert_eq!(realization(wrong_result, true), None);
        let mut wrong_params = trim();
        wrong_params.params = &[Ty::Char];
        assert_eq!(realization(wrong_params, true), None);
        let mut wrong_kind = trim();
        wrong_kind.kind = FnKind::TopLevel;
        assert_eq!(realization(wrong_kind, true), None);
        let mut generic = trim();
        generic.type_parameter_count = 1;
        assert_eq!(realization(generic, true), None);
        let mut contextual = trim();
        contextual.context_count = 1;
        assert_eq!(realization(contextual, true), None);
        let mut vararg = trim();
        vararg.vararg = Some(0);
        assert_eq!(realization(vararg, true), None);
        for modifier in [0, 1, 2] {
            let mut declaration = trim();
            declaration.is_suspend = modifier == 0;
            declaration.is_operator = modifier == 1;
            declaration.is_infix = modifier == 2;
            assert_eq!(realization(declaration, true), None);
        }
    }

    #[test]
    fn unsigned_helpers_require_the_stdlib_and_the_complete_unsigned_signature() {
        for (name, operand, remainder) in [
            ("uintDivide", Ty::UInt, false),
            ("uintRemainder", Ty::UInt, true),
            ("ulongDivide", Ty::ULong, false),
            ("ulongRemainder", Ty::ULong, true),
        ] {
            let params = [operand, operand];
            let operation = if remainder {
                CompilerIntrinsic::UnsignedRemainder { operand }
            } else {
                CompilerIntrinsic::UnsignedDivide { operand }
            };
            assert_eq!(
                realization(helper("kotlin", name, &params, operand), true),
                Some(operation)
            );
            assert_eq!(
                realization(helper("kotlin", name, &params, operand), false),
                None
            );
            assert_eq!(
                realization(helper("other", name, &params, operand), true),
                None
            );
            assert_eq!(
                realization(helper("kotlin", name, &[Ty::Int, Ty::Int], operand), true),
                None
            );
            assert_eq!(
                realization(helper("kotlin", name, &params, Ty::Int), true),
                None
            );
        }
    }

    #[test]
    fn result_helpers_require_the_complete_signature() {
        let throwable = Ty::obj("kotlin/Throwable");
        let any = Ty::obj("kotlin/Any");
        assert_eq!(
            realization(helper("kotlin", "createFailure", &[throwable], any), true),
            Some(CompilerIntrinsic::ResultFailure(
                ResultFailureIntrinsic::Create
            ))
        );
        assert_eq!(
            realization(helper("kotlin", "createFailure", &[any], any), true),
            None
        );
        assert_eq!(
            realization(helper("kotlin", "createFailure", &[throwable], any), false),
            None
        );
        let message = Ty::nullable(Ty::String);
        let result = Ty::obj_args("kotlin/Result", &[Ty::nullable(any)]);
        assert_eq!(
            realization(
                helper(
                    "kotlin/test",
                    "checkResultIsFailure",
                    &[message, result],
                    throwable
                ),
                true
            ),
            Some(CompilerIntrinsic::ResultFailure(
                ResultFailureIntrinsic::Check
            ))
        );
        assert_eq!(
            realization(
                helper(
                    "kotlin/test",
                    "checkResultIsFailure",
                    &[message, result],
                    throwable
                ),
                false
            ),
            None,
            "an identical declaration from an ordinary dependency has no compiler operation"
        );
        assert_eq!(
            realization(
                helper(
                    "kotlin/test",
                    "checkResultIsFailure",
                    &[Ty::String, result],
                    throwable
                ),
                true
            ),
            None
        );
        let t = Ty::ty_param("T", throwable);
        let params = [Ty::obj_args("kotlin/reflect/KClass", &[t]), message, result];
        let mut generic = helper("kotlin/test", "checkResultIsFailure", &params, t);
        generic.type_parameter_count = 1;
        assert_eq!(
            realization(generic, true),
            Some(CompilerIntrinsic::ResultFailure(
                ResultFailureIntrinsic::CheckOfClass
            ))
        );
        let mut untrusted = helper("kotlin/test", "checkResultIsFailure", &params, t);
        untrusted.type_parameter_count = 1;
        assert_eq!(realization(untrusted, false), None);
        let mut wrong_result = helper("kotlin/test", "checkResultIsFailure", &params, throwable);
        wrong_result.type_parameter_count = 1;
        assert_eq!(realization(wrong_result, true), None);
    }
}
