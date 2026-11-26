//! Positive common-IR capability of provider-normalized primitive declarations.
//!
//! Both dependency closure planning and body lowering consume this authenticated call shape.
//! A known operation with a malformed header is an error, never permission to walk its private
//! implementation or to call a target helper instead.

use super::decline::KlibBodyDeclineReason;
use super::function_lowering::{raw_function_header_with_class, FunctionHeader, HeaderPurpose};
use crate::klib_libraries::KlibDeclarationBodies;
use crate::libraries::{
    CompilerIntrinsic, KlibBodyCallable, KlibDeclarationSignature, PrimitiveUnaryIntrinsic,
};
use crate::types::{ArrayFactoryKind, Ty};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CommonPrimitive {
    Unary(PrimitiveUnaryIntrinsic),
    /// A read of a builtin property whose getter is this provider operation (`Array.size`,
    /// `String.length`). It lowers as the ordinary read of that property; the target realizes it.
    PropertyRead(CompilerIntrinsic),
    /// A builtin scalar's `rangeTo`/`rangeUntil`, the source range operator over its receiver
    /// and its one argument.
    RangeConstruction(crate::fir::FirRangeOperation),
    /// A builtin scalar's `compareTo` over its receiver and its one argument, compared as
    /// `operand` (`Char` as `Int`, mixed numbers at their promoted type).
    Compare {
        operand: Ty,
    },
    /// A builtin scalar's `toInt()` … `toChar()`, Kotlin's numeric conversion of its receiver.
    Conversion,
    /// `String.get`, the character of its receiver at its one `Int` index, as checked FIR lowers
    /// a source call of it.
    StringGet,
    /// A primitive array factory (`intArrayOf` …) over its one vararg parameter: the array its
    /// arguments are packed into is the array it returns, as checked FIR lowers a source call of
    /// it to an array literal.
    ArrayLiteral,
    /// `emptyArray<T>()`: a new reference array of no elements, of the array type the call has.
    EmptyArray,
}

pub(crate) struct CommonPrimitiveCall {
    pub(super) operation: CommonPrimitive,
    /// The dispatch receiver of a member operation; `None` for an array literal.
    pub(super) receiver: Option<Ty>,
    /// The declared value parameters: none, except a range construction's end bound.
    pub(super) params: Vec<Ty>,
    pub(super) ret: Ty,
    pub(super) class_type_parameters: Vec<String>,
    /// The property a [`CommonPrimitive::PropertyRead`] reads, as its provider identifies it.
    pub(super) property: Option<crate::fir::ExternalPropertyId>,
}

fn mismatch(detail: &str) -> KlibBodyDeclineReason {
    KlibBodyDeclineReason::SignatureMismatch(detail.to_owned())
}

pub(super) fn authenticate(
    selected: KlibBodyCallable<'_>,
    bodies: &KlibDeclarationBodies,
) -> Result<Option<CommonPrimitiveCall>, KlibBodyDeclineReason> {
    if let Some(CompilerIntrinsic::ArrayFactory(ArrayFactoryKind::PrimitiveVararg(element))) =
        selected.compiler_intrinsic()
    {
        return array_literal(selected, element).map(Some);
    }
    if let Some(CompilerIntrinsic::ArrayFactory(ArrayFactoryKind::EmptyReference)) =
        selected.compiler_intrinsic()
    {
        return empty_array(selected).map(Some);
    }
    let operation = match selected.compiler_intrinsic() {
        Some(CompilerIntrinsic::PrimitiveUnary(operation)) => CommonPrimitive::Unary(operation),
        Some(operation @ (CompilerIntrinsic::ArraySize | CompilerIntrinsic::StringLength)) => {
            CommonPrimitive::PropertyRead(operation)
        }
        // An unsigned class's conversion to its carrier is a value-class operation, not this one.
        Some(CompilerIntrinsic::NumericConversion)
            if selected.ret().is_numeric_or_char()
                && selected
                    .class_contract()
                    .map(super::class_headers::declared_receiver)
                    .is_some_and(Ty::is_numeric_or_char) =>
        {
            CommonPrimitive::Conversion
        }
        Some(CompilerIntrinsic::StringGet) => CommonPrimitive::StringGet,
        Some(CompilerIntrinsic::PrimitiveCompare) => {
            let &[parameter] = selected.params() else {
                return Err(mismatch(
                    "a common primitive has a different selected call shape",
                ));
            };
            let receiver = selected
                .class_contract()
                .map(super::class_headers::declared_receiver)
                .ok_or_else(|| mismatch("a primitive comparison has no class contract"))?;
            let operand = crate::libraries::builtin_member_realization::primitive_compare_operand(
                receiver, parameter,
            )
            .ok_or_else(|| {
                mismatch("a primitive comparison has incompatible declared operand types")
            })?;
            CommonPrimitive::Compare { operand }
        }
        None => match selected.range_construction() {
            Some(operation) => CommonPrimitive::RangeConstruction(operation),
            None => return Ok(None),
        },
        // Library algorithms and target instructions are deliberately not common primitives.
        _ => return Ok(None),
    };
    let arity = match operation {
        CommonPrimitive::RangeConstruction(_)
        | CommonPrimitive::Compare { .. }
        | CommonPrimitive::StringGet
        | CommonPrimitive::ArrayLiteral => 1,
        CommonPrimitive::Unary(_)
        | CommonPrimitive::EmptyArray
        | CommonPrimitive::PropertyRead(_)
        | CommonPrimitive::Conversion => 0,
    };
    if !selected.is_member()
        || selected.params().len() != arity
        || selected.context_count() != 0
        || !selected.type_parameters().is_empty()
    {
        return Err(mismatch(
            "a common primitive has a different selected call shape",
        ));
    }
    let signature_kind_matches = match operation {
        CommonPrimitive::Unary(_)
        | CommonPrimitive::RangeConstruction(_)
        | CommonPrimitive::Compare { .. }
        | CommonPrimitive::Conversion
        | CommonPrimitive::StringGet
        | CommonPrimitive::ArrayLiteral
        | CommonPrimitive::EmptyArray => {
            matches!(selected.signature(), KlibDeclarationSignature::Public(_))
        }
        CommonPrimitive::PropertyRead(_) => {
            matches!(selected.signature(), KlibDeclarationSignature::Accessor(_))
        }
    };
    if !signature_kind_matches {
        return Err(mismatch(
            "a common primitive has a different declaration kind",
        ));
    }
    let (header, formals) = authenticated_header(selected, bodies, arity)?;
    let (params, ret, _, _, receiver) = header.linked_signature();
    let params = params.to_vec();
    let receiver = receiver.expect("the authenticated member has dispatch");
    let call_receiver = Some(receiver);
    let valid = match operation {
        CommonPrimitive::Unary(_) => match receiver {
            Ty::Byte | Ty::Short | Ty::Int => ret == Ty::Int,
            Ty::Long => ret == Ty::Long,
            Ty::Float => ret == Ty::Float,
            Ty::Double => ret == Ty::Double,
            _ => false,
        },
        CommonPrimitive::PropertyRead(CompilerIntrinsic::ArraySize) => {
            receiver.is_array() && ret == Ty::Int
        }
        CommonPrimitive::PropertyRead(CompilerIntrinsic::StringLength) => {
            receiver == Ty::String && ret == Ty::Int
        }
        // The provider proved the exact builtin declaration and its range result; the serialized
        // header must state that same scalar operand pair and result.
        CommonPrimitive::RangeConstruction(_) => {
            let range_operand = |ty: Ty| {
                matches!(
                    ty,
                    Ty::Byte | Ty::Short | Ty::Int | Ty::Long | Ty::Char | Ty::UInt | Ty::ULong
                )
            };
            range_operand(receiver)
                && matches!(params.as_slice(), &[param] if range_operand(param))
                && params == selected.params()
                && ret == selected.ret()
                && ret.is_reference()
        }
        // The serialized header states the same scalar pair the provider compared, and `Int`.
        CommonPrimitive::Compare { operand } => {
            ret == Ty::Int
                && params == selected.params()
                && matches!(params.as_slice(), &[param]
                    if crate::libraries::builtin_member_realization::primitive_compare_operand(
                        receiver, param,
                    ) == Some(operand))
        }
        CommonPrimitive::Conversion => {
            receiver.is_numeric_or_char() && ret.is_numeric_or_char() && ret == selected.ret()
        }
        CommonPrimitive::StringGet => {
            receiver == Ty::String
                && ret == Ty::Char
                && params.as_slice() == [Ty::Int]
                && params == selected.params()
        }
        _ => false,
    };
    if !valid {
        return Err(mismatch(
            "a common primitive has incompatible declared operand and result types",
        ));
    }
    let property = match operation {
        CommonPrimitive::PropertyRead(_) => Some(
            selected
                .accessor()
                .ok_or_else(|| mismatch("a common property read has no accessor contract"))?
                .property,
        ),
        CommonPrimitive::Unary(_)
        | CommonPrimitive::RangeConstruction(_)
        | CommonPrimitive::Compare { .. }
        | CommonPrimitive::Conversion
        | CommonPrimitive::StringGet
        | CommonPrimitive::ArrayLiteral
        | CommonPrimitive::EmptyArray => None,
    };
    Ok(Some(CommonPrimitiveCall {
        operation,
        receiver: call_receiver,
        params,
        ret,
        class_type_parameters: formals,
        property,
    }))
}

/// The array literal a primitive array factory of `element`s is: a top-level function of one
/// vararg parameter, packed into the primitive array of `element` it returns.
fn array_literal(
    selected: KlibBodyCallable<'_>,
    element: Ty,
) -> Result<CommonPrimitiveCall, KlibBodyDeclineReason> {
    let ret = selected.ret();
    let packed = ret.is_array() && ret.array_read_elem() == Some(element);
    if selected.is_member()
        || selected.context_count() != 0
        || !selected.type_parameters().is_empty()
        || selected.vararg_index() != Some(0)
        || selected.params().len() != 1
        || !packed
    {
        return Err(mismatch(
            "a primitive array factory has a different selected call shape",
        ));
    }
    Ok(CommonPrimitiveCall {
        operation: CommonPrimitive::ArrayLiteral,
        receiver: None,
        params: vec![ret],
        ret,
        class_type_parameters: Vec::new(),
        property: None,
    })
}

/// The empty reference array `emptyArray<T>()` is: a top-level function of one type parameter and
/// no value parameters, returning a reference array.
fn empty_array(
    selected: KlibBodyCallable<'_>,
) -> Result<CommonPrimitiveCall, KlibBodyDeclineReason> {
    let ret = selected.ret();
    if selected.is_member()
        || selected.context_count() != 0
        || selected.type_parameters().len() != 1
        || selected.vararg_index().is_some()
        || !selected.params().is_empty()
        || !ret.is_reference_array()
    {
        return Err(mismatch(
            "an empty array factory has a different selected call shape",
        ));
    }
    Ok(CommonPrimitiveCall {
        operation: CommonPrimitive::EmptyArray,
        receiver: None,
        params: Vec::new(),
        ret,
        class_type_parameters: Vec::new(),
        property: None,
    })
}

fn authenticated_header(
    selected: KlibBodyCallable<'_>,
    bodies: &KlibDeclarationBodies,
    arity: usize,
) -> Result<(FunctionHeader, Vec<String>), KlibBodyDeclineReason> {
    let super::class_headers::ClassMemberHeader {
        contract: class,
        arena,
        function,
        types,
        formals,
        ..
    } = super::class_headers::authenticate(selected, bodies)?;
    super::class_headers::authenticate_receiver(class, arena, function)?;
    let header = raw_function_header_with_class(
        Some(selected),
        HeaderPurpose::External,
        arena,
        function,
        types,
    )?;
    let (params, _, contexts, extension, receiver) = header.linked_signature();
    let expected_receiver = super::class_headers::declared_receiver(class);
    if receiver != Some(expected_receiver) || params.len() != arity || contexts != 0 || extension {
        return Err(mismatch(
            "the serialized primitive receiver or arguments disagree with its class contract",
        ));
    }
    Ok((header, formals))
}
