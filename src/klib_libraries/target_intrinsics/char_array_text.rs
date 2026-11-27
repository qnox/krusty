//! Wasm's `CharArray.concatToString`, authenticated by its exact `kotlin.text` declaration.

use crate::libraries::{CharArrayRange, CompilerIntrinsic, ExternalCallableKind};
use crate::metadata::semantic::KotlinFunction;
use crate::types::{type_name, Ty};

use super::{exact_type, semantic_type, KlibIntrinsicShape};

/// The Wasm stdlib's conversions of a char array to a string, in `kotlin.text`:
/// `CharArray.concatToString()` and `CharArray.concatToString(startIndex, endIndex)`. Their bodies
/// build the string from the stdlib's own string layout; a target that lays strings out itself
/// realizes them by their exact declarations instead. The deprecated `String(chars)` factories
/// are errors to call, so no program reaches them.
pub(super) fn authenticate(
    placement: crate::libraries::CallablePlacement,
    declaration: &KotlinFunction,
) -> Option<KlibIntrinsicShape> {
    let chars = Ty::array(Ty::Char);
    if placement != crate::libraries::CallablePlacement::Package(type_name("kotlin/text"))
        || declaration.name != "concatToString"
        || !declaration
            .receiver
            .as_ref()
            .is_some_and(|receiver| exact_type(receiver, "kotlin/CharArray"))
        || !declaration.formals.is_empty()
        || declaration.context_count != 0
        || !declaration.context_kinds.is_empty()
        || declaration.is_suspend
        || declaration.is_inline
        || declaration.is_expect
        || declaration.vararg.is_some()
        || !exact_type(&declaration.ret, "kotlin/String")
    {
        return None;
    }
    let (range, params) = match declaration.params.as_slice() {
        [] => (CharArrayRange::Whole, vec![chars]),
        [start, end]
            if [start, end]
                .into_iter()
                .all(|index| semantic_type(index) == Some(Ty::Int)) =>
        {
            (CharArrayRange::StartEnd, vec![chars, Ty::Int, Ty::Int])
        }
        _ => return None,
    };
    Some(KlibIntrinsicShape {
        operation: CompilerIntrinsic::CharArrayToString(range),
        kind: ExternalCallableKind::Extension,
        receiver: Some(chars),
        params,
        ret: Ty::obj("kotlin/String"),
    })
}

#[cfg(test)]
mod tests {
    use super::super::tests::declaration;
    use super::super::KlibIntrinsicShape;
    use super::super::{authenticate as authenticate_declaration, TargetIntrinsicPlatform};
    use crate::libraries::{CharArrayRange, CompilerIntrinsic, ExternalCallableKind};
    use crate::metadata::semantic::{AnnotationApplication, KotlinType};
    use crate::types::{type_name, Ty};

    #[test]
    fn wasm_char_array_conversions_are_authenticated_by_their_exact_declarations() {
        let wasm = Some(TargetIntrinsicPlatform::Wasm);
        let text = crate::libraries::CallablePlacement::Package(type_name("kotlin/text"));
        let chars = Ty::array(Ty::Char);
        let string = Ty::obj("kotlin/String");
        let annotation = AnnotationApplication {
            identity: type_name("kotlin/SinceKotlin"),
            arguments: Vec::new(),
        };
        let conversion = |name: &str, extension: bool, params: &[&str]| {
            let mut declaration = declaration(name, params, "kotlin/String", annotation.clone());
            declaration.is_external = false;
            if extension {
                declaration.receiver = Some(KotlinType::class("kotlin/CharArray"));
            }
            declaration
        };
        let shape = |range, kind, receiver, params: &[Ty]| KlibIntrinsicShape {
            operation: CompilerIntrinsic::CharArrayToString(range),
            kind,
            receiver,
            params: params.to_vec(),
            ret: string,
        };
        let concat = conversion("concatToString", true, &[]);
        assert_eq!(
            authenticate_declaration(wasm, text, &concat),
            Some(shape(
                CharArrayRange::Whole,
                ExternalCallableKind::Extension,
                Some(chars),
                &[chars]
            ))
        );
        let mut concat_range = conversion("concatToString", true, &["kotlin/Int", "kotlin/Int"]);
        concat_range.param_defaults = vec![true, true];
        assert_eq!(
            authenticate_declaration(wasm, text, &concat_range),
            Some(shape(
                CharArrayRange::StartEnd,
                ExternalCallableKind::Extension,
                Some(chars),
                &[chars, Ty::Int, Ty::Int]
            ))
        );
        // Only the Wasm stdlib's `kotlin.text`, by the declarations' whole shape.
        assert!(
            authenticate_declaration(Some(TargetIntrinsicPlatform::Native), text, &concat)
                .is_none()
        );
        assert!(authenticate_declaration(None, text, &concat).is_none());
        assert!(authenticate_declaration(
            wasm,
            crate::libraries::CallablePlacement::Package(type_name("spoof")),
            &concat
        )
        .is_none());
        let mut wrong_result = conversion("concatToString", true, &[]);
        wrong_result.ret = KotlinType::class("kotlin/Int");
        assert!(authenticate_declaration(wasm, text, &wrong_result).is_none());
        let mut other_receiver = conversion("concatToString", true, &[]);
        other_receiver.receiver = Some(KotlinType::class("kotlin/ByteArray"));
        assert!(authenticate_declaration(wasm, text, &other_receiver).is_none());
        let wrong_indices = conversion("concatToString", true, &["kotlin/Int", "kotlin/Long"]);
        assert!(authenticate_declaration(wasm, text, &wrong_indices).is_none());
        let one_index = conversion("concatToString", true, &["kotlin/Int"]);
        assert!(authenticate_declaration(wasm, text, &one_index).is_none());
        // The deprecated factories are errors to call, so none is realized.
        for params in [
            &["kotlin/CharArray"][..],
            &["kotlin/CharArray", "kotlin/Int", "kotlin/Int"],
        ] {
            assert!(
                authenticate_declaration(wasm, text, &conversion("String", false, params))
                    .is_none()
            );
        }
        assert!(authenticate_declaration(wasm, text, &conversion("toString", true, &[])).is_none());
    }
}
