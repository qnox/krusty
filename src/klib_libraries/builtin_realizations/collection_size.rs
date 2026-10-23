//! The complete selected stdlib declaration behind collection sizing in prepared inline bodies.

use crate::libraries::{CompilerIntrinsic, FnKind, FunctionInfo, GenericReturnPolicy, InlineKind};
use crate::types::{type_name, Ty, TypeName};

pub(super) fn realization(
    package: TypeName,
    compiler_builtin_owner: bool,
    function: &FunctionInfo,
) -> Option<CompilerIntrinsic> {
    if !compiler_builtin_owner
        || package != type_name("kotlin/collections")
        || function.callable.name != "collectionSizeOrDefault"
        || function.kind != FnKind::Extension
        || function.context_count != 0
        || function.call_sig.vararg_index.is_some()
        || function.call_sig.param_defaults.contains(&true)
        || function.flags.inline != InlineKind::None
        || function.flags.reified
        || function.flags.suspend
        || function.flags.operator
        || function.flags.infix
        || function.flags.is_abstract
        || function.callable.ret != Ty::Int
    {
        return None;
    }
    let signature = function.generic_sig.as_ref()?;
    let ([formal], [bounds]) = (
        signature.formals.as_slice(),
        signature.formal_bounds.as_slice(),
    ) else {
        return None;
    };
    let bound = Ty::nullable(Ty::obj_name(crate::types::wk::any()));
    if !bounds.is_empty() && bounds.as_slice() != [bound] {
        return None;
    }
    let receiver = Ty::obj_args(
        "kotlin/collections/Iterable",
        &[Ty::ty_param(formal, bound)],
    );
    (signature.receiver == Some(receiver)
        && function.semantic_receiver() == Some(receiver)
        && signature.params == [Ty::Int]
        && function.semantic_params().as_ref() == [Ty::Int]
        && signature.ret == Ty::Int
        && signature.return_policy == GenericReturnPolicy::Exact)
        .then_some(CompilerIntrinsic::CollectionSizeOrDefault)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::{GenericSig, LibraryCallable};

    fn declaration(formal: &str) -> FunctionInfo {
        let bound = Ty::nullable(Ty::obj_name(crate::types::wk::any()));
        let receiver = Ty::obj_args(
            "kotlin/collections/Iterable",
            &[Ty::ty_param(formal, bound)],
        );
        let callable = LibraryCallable::library(
            type_name("kotlin/collections"),
            "collectionSizeOrDefault",
            vec![receiver, Ty::Int],
            Ty::Int,
            Ty::Int,
            "",
        );
        let mut function = FunctionInfo::plain(FnKind::Extension, Some(receiver), callable);
        function.generic_sig = Some(GenericSig {
            formals: vec![formal.into()],
            formal_bounds: vec![vec![bound]],
            receiver: Some(receiver),
            params: vec![Ty::Int],
            ret: Ty::Int,
            return_policy: GenericReturnPolicy::Exact,
        });
        function
    }

    #[test]
    fn sizing_requires_stdlib_authority_and_the_declaration_owned_generic_parameter() {
        let package = type_name("kotlin/collections");
        for formal in ["Element", "UnrelatedSpelling"] {
            let mut selected = declaration(formal);
            assert_eq!(
                realization(package, true, &selected),
                Some(CompilerIntrinsic::CollectionSizeOrDefault)
            );
            assert_eq!(realization(package, false, &selected), None);
            assert_eq!(
                realization(type_name("user/collections"), true, &selected),
                None
            );
            selected.generic_sig.as_mut().unwrap().formal_bounds[0].clear();
            assert_eq!(
                realization(package, true, &selected),
                Some(CompilerIntrinsic::CollectionSizeOrDefault)
            );
        }
        let original = declaration("Element");
        let element = original.receiver.unwrap().type_args()[0];
        for receiver in [
            None,
            Some(Ty::nullable(original.receiver.unwrap())),
            Some(Ty::obj_args("kotlin/collections/Collection", &[Ty::String])),
            Some(Ty::obj_args("kotlin/collections/Iterable", &[Ty::String])),
            Some(Ty::obj("kotlin/collections/Iterable")),
            Some(Ty::obj_args(
                "kotlin/collections/Iterable",
                &[element, element],
            )),
            Some(Ty::obj_args(
                "kotlin/collections/Iterable",
                &[Ty::nullable(element)],
            )),
            Some(Ty::obj_args(
                "kotlin/collections/Iterable",
                &[Ty::star_projection(element)],
            )),
            Some(Ty::obj_args(
                "kotlin/collections/Iterable",
                &[Ty::in_projection(element)],
            )),
            Some(Ty::obj_args(
                "kotlin/collections/Iterable",
                &[Ty::out_projection(element)],
            )),
        ] {
            let mut wrong = original.clone();
            wrong.generic_sig.as_mut().unwrap().receiver = receiver;
            assert_eq!(realization(package, true, &wrong), None);
        }
        let mut wrong_formal = original.clone();
        wrong_formal.generic_sig.as_mut().unwrap().formals = vec!["Another".into()];
        let mut extra_bound = original.clone();
        extra_bound.generic_sig.as_mut().unwrap().formal_bounds[0].push(Ty::String);
        let mut wrong_bound = original.clone();
        wrong_bound.generic_sig.as_mut().unwrap().formal_bounds[0] = vec![Ty::String];
        let mut wrong_parameter = original.clone();
        wrong_parameter.generic_sig.as_mut().unwrap().params = vec![Ty::Long];
        let mut wrong_result = original.clone();
        wrong_result.generic_sig.as_mut().unwrap().ret = Ty::Long;
        let mut wrong_kind = original.clone();
        wrong_kind.kind = FnKind::TopLevel;
        let mut contextual = original.clone();
        contextual.context_count = 1;
        let mut wrong_name = original.clone();
        wrong_name.callable.name = "other".into();
        for wrong in [
            wrong_formal,
            extra_bound,
            wrong_bound,
            wrong_parameter,
            wrong_result,
            wrong_kind,
            contextual,
            wrong_name,
        ] {
            assert_eq!(realization(package, true, &wrong), None);
        }
        for modifier in 0..6 {
            let mut wrong = original.clone();
            wrong.flags.suspend = modifier == 0;
            wrong.flags.operator = modifier == 1;
            wrong.flags.infix = modifier == 2;
            wrong.flags.reified = modifier == 3;
            wrong.flags.is_abstract = modifier == 4;
            if modifier == 5 {
                wrong.flags.inline = InlineKind::CanInline;
            }
            assert_eq!(realization(package, true, &wrong), None);
        }
        for formals in [vec![], vec!["Element".into(), "Extra".into()]] {
            let mut wrong = original.clone();
            wrong.generic_sig.as_mut().unwrap().formals = formals;
            assert_eq!(realization(package, true, &wrong), None);
        }
        for parameters in [vec![], vec![Ty::Int, Ty::Int]] {
            let mut wrong = original.clone();
            wrong.generic_sig.as_mut().unwrap().params = parameters;
            assert_eq!(realization(package, true, &wrong), None);
        }
        let mut vararg = original.clone();
        vararg.call_sig.vararg_index = Some(0);
        assert_eq!(realization(package, true, &vararg), None);
        let mut defaulted = original.clone();
        defaulted.call_sig.param_defaults.push(true);
        assert_eq!(realization(package, true, &defaulted), None);
    }
}
