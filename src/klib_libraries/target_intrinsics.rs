//! Target-stdlib intrinsic declarations authenticated at the KLIB provider boundary.
//!
//! Annotation spelling is evidence only inside the exact archive compilation input assembly marked
//! as its platform stdlib. The provider validates the complete semantic shape and publishes an
//! exact module-qualified declaration identity. Body lowering consumes that identity; it never
//! searches for an annotation, package, or callable name.

use std::collections::HashMap;

use crate::libraries::{
    AtomicOperation, CompilerIntrinsic, ExternalCallableKind, KlibDeclarationSignature,
    KlibDependencyCallable, ScalarClass, ScalarComparison, ScalarInstruction,
};
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrArguments, KlibIrExprKind, KlibIrFunction, KlibIrNullability, KlibIrType,
};
use crate::metadata::klib_ir::{KlibIrConstant, KlibIrSignature, KlibIrSymbolKind};
use crate::metadata::semantic::{
    AnnotationApplication, AnnotationArgument, KotlinFunction, KotlinFunctionTypeShape, KotlinType,
};
use crate::types::{type_name, Ty, TypeName};

use super::declaration_signatures::SignedFunction;

mod atomic_array_elements;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TargetIntrinsicPlatform {
    Native,
    Wasm,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct KlibIntrinsicShape {
    pub(crate) operation: CompilerIntrinsic,
    pub(crate) kind: ExternalCallableKind,
    pub(crate) receiver: Option<Ty>,
    pub(crate) params: Vec<Ty>,
    pub(crate) ret: Ty,
}

/// One authenticated intrinsic declaration in the selected target stdlib.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KlibIntrinsicCallable {
    identity: KlibDependencyCallable,
    operation: CompilerIntrinsic,
    kind: ExternalCallableKind,
    receiver: Option<Ty>,
    params: Vec<Ty>,
    ret: Ty,
}

impl KlibIntrinsicCallable {
    pub fn identity(&self) -> &KlibDependencyCallable {
        &self.identity
    }

    pub(crate) fn operation(&self) -> CompilerIntrinsic {
        self.operation
    }

    pub(crate) fn kind(&self) -> ExternalCallableKind {
        self.kind
    }

    pub(crate) fn receiver(&self) -> Option<Ty> {
        self.receiver
    }

    pub(crate) fn params(&self) -> &[Ty] {
        &self.params
    }

    pub(crate) fn ret(&self) -> Ty {
        self.ret
    }
}

/// Authenticated intrinsic declarations keyed only by exact KLIB module plus serialized identity.
#[derive(Clone, Debug, Default)]
pub struct KlibIntrinsicFacts {
    callables: HashMap<KlibDependencyCallable, KlibIntrinsicCallable>,
    /// Native's atomic accesses through a property reference to a backing field. The reference is
    /// not a value, so these never become ordinary calls: body lowering realizes the field access.
    atomic_fields: HashMap<KlibDependencyCallable, AtomicOperation>,
    /// Native's atomic accesses to one element of an array, which body lowering realizes as that
    /// element access.
    atomic_array_elements: HashMap<KlibDependencyCallable, AtomicOperation>,
}

impl KlibIntrinsicFacts {
    #[cfg(test)]
    pub(crate) fn empty() -> &'static Self {
        static EMPTY: std::sync::OnceLock<KlibIntrinsicFacts> = std::sync::OnceLock::new();
        EMPTY.get_or_init(Self::default)
    }

    pub(super) fn insert(&mut self, signed: &SignedFunction) {
        let (Some(module), Some(shape)) = (&signed.module, &signed.target_intrinsic) else {
            return;
        };
        // `GCUnsafeCall` predicates are exact Native realizations, not operations shared by KLIB
        // body lowering. Keep them on the published callable fact so Native consumes them at the
        // ordinary external-call boundary. Reinterpretation and scalar instructions are the common
        // operations used while closing a serialized wrapper body, and so are the assertion
        // switches the stdlib's bodies read.
        if !matches!(
            shape.operation,
            CompilerIntrinsic::ReinterpretBits
                | CompilerIntrinsic::ScalarInstruction(_)
                | CompilerIntrinsic::AssertionSwitch
        ) {
            return;
        }
        let identity = KlibDependencyCallable {
            module: module.clone(),
            declaration: KlibDeclarationSignature::Public(signed.signature.clone()),
        };
        self.callables.insert(
            identity.clone(),
            KlibIntrinsicCallable {
                identity,
                operation: shape.operation,
                kind: shape.kind,
                receiver: shape.receiver,
                params: shape.params.clone(),
                ret: shape.ret,
            },
        );
    }

    /// Authenticate compiler-owned operations from the exact target-stdlib IR declaration. An
    /// `internal` declaration can carry a linkable public signature. A private declaration's
    /// file-local identity cannot prove the package that owns the operation, so it is never
    /// authenticated or published into body lowering here.
    pub(super) fn insert_ir(
        &mut self,
        module: &str,
        platform: Option<TargetIntrinsicPlatform>,
        arena: &KlibIrArena,
        function: &KlibIrFunction,
    ) {
        for (authenticate, accesses) in [
            (
                authenticate_atomic_field as fn(_, _, _) -> _,
                &mut self.atomic_fields,
            ),
            (
                atomic_array_elements::authenticate,
                &mut self.atomic_array_elements,
            ),
        ] {
            if let Some(operation) = authenticate(platform, arena, function) {
                let KlibIrSignature::Public(signature) = &function.base.symbol.signature else {
                    unreachable!("an authenticated atomic access has a public signature");
                };
                accesses.insert(
                    KlibDependencyCallable {
                        module: module.to_owned(),
                        declaration: KlibDeclarationSignature::Public(signature.clone()),
                    },
                    operation,
                );
                return;
            }
        }
        let Some(shape) = authenticate_ir(platform, arena, function) else {
            return;
        };
        let KlibIrSignature::Public(signature) = &function.base.symbol.signature else {
            return;
        };
        let identity = KlibDependencyCallable {
            module: module.to_owned(),
            declaration: KlibDeclarationSignature::Public(signature.clone()),
        };
        self.callables.insert(
            identity.clone(),
            KlibIntrinsicCallable {
                identity,
                operation: shape.operation,
                kind: shape.kind,
                receiver: shape.receiver,
                params: shape.params,
                ret: shape.ret,
            },
        );
    }

    pub(crate) fn get(&self, identity: &KlibDependencyCallable) -> Option<&KlibIntrinsicCallable> {
        self.callables.get(identity)
    }

    #[cfg(test)]
    pub(crate) fn atomic_field_declarations(
        &self,
    ) -> impl Iterator<Item = (&KlibDependencyCallable, AtomicOperation)> {
        self.atomic_fields
            .iter()
            .map(|(identity, operation)| (identity, *operation))
    }

    #[cfg(test)]
    pub(crate) fn atomic_array_element_declarations(
        &self,
    ) -> impl Iterator<Item = (&KlibDependencyCallable, AtomicOperation)> {
        self.atomic_array_elements
            .iter()
            .map(|(identity, operation)| (identity, *operation))
    }

    /// The atomic array element access `identity` declares, when it is an authenticated Native
    /// one.
    pub(crate) fn atomic_array_element(
        &self,
        identity: &KlibDependencyCallable,
    ) -> Option<AtomicOperation> {
        self.atomic_array_elements.get(identity).copied()
    }

    /// The atomic field access `identity` declares, when it is an authenticated Native one.
    pub(crate) fn atomic_field(
        &self,
        identity: &KlibDependencyCallable,
    ) -> Option<AtomicOperation> {
        self.atomic_fields.get(identity).copied()
    }

    pub fn len(&self) -> usize {
        self.callables.len()
    }

    pub fn is_empty(&self) -> bool {
        self.callables.is_empty()
    }
}

/// Native's `kotlin.concurrent` atomic field accesses: an external `TypedIntrinsic` extension of
/// `KMutableProperty0<V>` whose operands are `V` and whose result the operation fixes. `V` is the
/// declaration's one type parameter, or a scalar for an addition specialized to it.
fn authenticate_atomic_field(
    platform: Option<TargetIntrinsicPlatform>,
    arena: &KlibIrArena,
    function: &KlibIrFunction,
) -> Option<AtomicOperation> {
    let KlibIrSignature::Public(signature) = &function.base.symbol.signature else {
        return None;
    };
    if platform != Some(TargetIntrinsicPlatform::Native)
        || signature.package().segments() != ["kotlin", "concurrent"]
        || signature.declaration().segments().len() != 1
        || function.base.symbol.kind != KlibIrSymbolKind::Function
        || function.constructor
        || function.body.is_some()
        || function.type_parameters.len() > 1
        || function.dispatch_receiver.is_some()
        || !function.context_parameters.is_empty()
    {
        return None;
    }
    let operation = match ir_string_annotation(
        arena,
        &function.base.annotations,
        &["kotlin", "native", "internal"],
        &["TypedIntrinsic", "<init>"],
    )? {
        "ATOMIC_GET_FIELD" => AtomicOperation::Load,
        "ATOMIC_SET_FIELD" => AtomicOperation::Store,
        "GET_AND_SET_FIELD" => AtomicOperation::Exchange,
        "COMPARE_AND_SET_FIELD" => AtomicOperation::CompareAndSet,
        "COMPARE_AND_EXCHANGE_FIELD" => AtomicOperation::CompareAndExchange,
        "GET_AND_ADD_FIELD" => AtomicOperation::FetchAndAdd,
        _ => return None,
    };
    // The receiver is the property reference itself, which the serializer records as definitely
    // not null.
    let KlibIrType::Simple {
        classifier,
        nullability: KlibIrNullability::NotSpecified | KlibIrNullability::DefinitelyNotNull,
        arguments,
        annotations,
        abbreviation: None,
    } = arena.ty(function.extension_receiver.as_ref()?.ty)
    else {
        return None;
    };
    let KlibIrSignature::Public(reference) = &classifier.signature else {
        return None;
    };
    let [crate::metadata::klib_ir::tree::KlibIrTypeArgument::Type { ty: value, .. }] =
        arguments.as_slice()
    else {
        return None;
    };
    let scalar = ir_semantic_type(arena, *value);
    let value = arena.ty(*value);
    // A scalar is compared by what it denotes; the serializer spells one either way.
    let is_value = |id| match scalar {
        Some(scalar) => ir_semantic_type(arena, id) == Some(scalar),
        None => arena.ty(id) == value,
    };
    // The value is the declaration's one type parameter, or the scalar an addition is specialized
    // to when it declares none.
    let admitted = match function.type_parameters.as_slice() {
        [parameter] => matches!(
            value,
            KlibIrType::Simple {
                classifier,
                nullability: KlibIrNullability::NotSpecified,
                arguments,
                annotations,
                abbreviation: None,
            } if classifier == &parameter.base.symbol && arguments.is_empty() && annotations.is_empty()
        ),
        _ => operation == AtomicOperation::FetchAndAdd && scalar.is_some(),
    };
    if !admitted
        || classifier.kind != KlibIrSymbolKind::Class
        || !annotations.is_empty()
        || reference.member_id().is_some()
        || reference.package().segments() != ["kotlin", "reflect"]
        || reference.declaration().segments() != ["KMutableProperty0"]
        || function.regular_parameters.len() != operation.operands()
        || function
            .regular_parameters
            .iter()
            .any(|parameter| !is_value(parameter.ty))
    {
        return None;
    }
    let answers = match operation {
        AtomicOperation::Store => ir_unit(arena, function.return_type),
        AtomicOperation::CompareAndSet => {
            ir_semantic_type(arena, function.return_type) == Some(Ty::Boolean)
        }
        AtomicOperation::Load
        | AtomicOperation::Exchange
        | AtomicOperation::CompareAndExchange
        | AtomicOperation::FetchAndAdd => is_value(function.return_type),
    };
    answers.then_some(operation)
}

/// Whether `id` is exactly `kotlin.Unit`.
fn ir_unit(arena: &KlibIrArena, id: crate::metadata::klib_ir::tree::KlibIrTypeId) -> bool {
    let KlibIrType::Simple {
        classifier,
        nullability: KlibIrNullability::NotSpecified | KlibIrNullability::DefinitelyNotNull,
        arguments,
        annotations,
        abbreviation: None,
    } = arena.ty(id)
    else {
        return false;
    };
    let KlibIrSignature::Public(signature) = &classifier.signature else {
        return false;
    };
    classifier.kind == KlibIrSymbolKind::Class
        && arguments.is_empty()
        && annotations.is_empty()
        && signature.member_id().is_none()
        && signature.package().segments() == ["kotlin"]
        && signature.declaration().segments() == ["Unit"]
}

fn authenticate_ir(
    platform: Option<TargetIntrinsicPlatform>,
    arena: &KlibIrArena,
    function: &KlibIrFunction,
) -> Option<KlibIntrinsicShape> {
    // The Wasm stdlib spells these declarations as ordinary bodyful functions whose body returns
    // `implementedAsIntrinsic`; the compiler contract is the trusted WasmOp annotation plus the
    // complete semantic shape below, not the absence of serialized IR.
    let KlibIrSignature::Public(signature) = &function.base.symbol.signature else {
        // A file-local signature carries no package identity, so it cannot authenticate a
        // compiler operation whose declaring package is part of the contract.
        return None;
    };
    if platform != Some(TargetIntrinsicPlatform::Wasm)
        || !WASM_OPERATION_PACKAGES
            .iter()
            .any(|package| signature.package().segments().iter().eq(package.iter()))
        || signature.declaration().segments().len() != 1
        || signature.member_id().is_none()
        || function.base.symbol.kind != KlibIrSymbolKind::Function
        || function.constructor
        || !function.type_parameters.is_empty()
        || function.dispatch_receiver.is_some()
        || !function.context_parameters.is_empty()
    {
        return None;
    }
    let operation = ir_string_annotation(
        arena,
        &function.base.annotations,
        &["kotlin", "wasm", "internal"],
        &["WasmOp", "<init>"],
    )?;
    let operands = function
        .extension_receiver
        .iter()
        .chain(&function.regular_parameters)
        .map(|parameter| ir_semantic_type(arena, parameter.ty))
        .collect::<Option<Vec<_>>>()?;
    wasm_shape(
        operation,
        function.extension_receiver.is_some(),
        operands,
        ir_semantic_type(arena, function.return_type)?,
    )
}

/// The packages whose `WasmOp` declarations the Wasm stdlib realizes as one instruction: the
/// instruction wrappers of `kotlin.wasm.internal` and the public and `@PublishedApi` declarations
/// of `kotlin` (bit counts, unsigned arithmetic and conversions). Primitive members are the
/// shared builtin operators and are realized there.
const WASM_OPERATION_PACKAGES: [&[&str]; 2] = [&["kotlin"], &["kotlin", "wasm", "internal"]];

/// The compiler operation a `WasmOp` names, if `operands` (an extension receiver first) and
/// `result` are exactly the operation's shape.
fn wasm_shape(
    operation: &str,
    extension: bool,
    operands: Vec<Ty>,
    result: Ty,
) -> Option<KlibIntrinsicShape> {
    let intrinsic = match operation {
        "I32_REINTERPRET_F32"
        | "I64_REINTERPRET_F64"
        | "F32_REINTERPRET_I32"
        | "F64_REINTERPRET_I64" => {
            let (parameter, expected) = match operation {
                "I32_REINTERPRET_F32" => (Ty::Float, Ty::Int),
                "I64_REINTERPRET_F64" => (Ty::Double, Ty::Long),
                "F32_REINTERPRET_I32" => (Ty::Int, Ty::Float),
                _ => (Ty::Long, Ty::Double),
            };
            if extension || operands != [parameter] || result != expected {
                return None;
            }
            CompilerIntrinsic::ReinterpretBits
        }
        _ => {
            let (instruction, class) = wasm_scalar_instruction(operation)?;
            let operand = *operands.first()?;
            if operands.len() != instruction.arity()
                || operands.iter().any(|ty| *ty != operand)
                || ScalarClass::of(operand) != Some(class)
                || !instruction.admits(operand, result)
            {
                return None;
            }
            CompilerIntrinsic::ScalarInstruction(instruction)
        }
    };
    Some(KlibIntrinsicShape {
        operation: intrinsic,
        kind: if extension {
            ExternalCallableKind::Extension
        } else {
            ExternalCallableKind::TopLevel
        },
        receiver: None,
        params: operands,
        ret: result,
    })
}

/// The scalar instruction a `WasmOp` entry names, with the class of its operands. The entry
/// spells the result's class first, as the Wasm instruction does.
fn wasm_scalar_instruction(operation: &str) -> Option<(ScalarInstruction, ScalarClass)> {
    use ScalarClass::{Float32, Float64, Integral32, Integral64};
    use ScalarInstruction::*;
    Some(match operation {
        "I32_CLZ" => (CountLeadingZeros, Integral32),
        "I32_CTZ" => (CountTrailingZeros, Integral32),
        "I32_POPCNT" => (PopulationCount, Integral32),
        "I32_ADD" => (Add, Integral32),
        "I32_SUB" => (Subtract, Integral32),
        "I32_MUL" => (Multiply, Integral32),
        "I32_DIV_S" => (Divide, Integral32),
        "I32_DIV_U" => (DivideUnsigned, Integral32),
        "I32_REM_S" => (Remainder, Integral32),
        "I32_REM_U" => (RemainderUnsigned, Integral32),
        "I32_AND" => (And, Integral32),
        "I32_OR" => (Or, Integral32),
        "I32_XOR" => (Xor, Integral32),
        "I32_SHL" => (ShiftLeft, Integral32),
        "I32_SHR_S" => (ShiftRight, Integral32),
        "I32_SHR_U" => (ShiftRightUnsigned, Integral32),
        "I32_ROTL" => (RotateLeft, Integral32),
        "I32_ROTR" => (RotateRight, Integral32),
        "I32_EQ" => (Compare(ScalarComparison::Equal), Integral32),
        "I32_NE" => (Compare(ScalarComparison::NotEqual), Integral32),
        "I32_LT_S" => (Compare(ScalarComparison::Less), Integral32),
        "I32_LT_U" => (Compare(ScalarComparison::LessUnsigned), Integral32),
        "I32_GT_S" => (Compare(ScalarComparison::Greater), Integral32),
        "I32_GT_U" => (Compare(ScalarComparison::GreaterUnsigned), Integral32),
        "I32_LE_S" => (Compare(ScalarComparison::LessOrEqual), Integral32),
        "I32_LE_U" => (Compare(ScalarComparison::LessOrEqualUnsigned), Integral32),
        "I32_GE_S" => (Compare(ScalarComparison::GreaterOrEqual), Integral32),
        "I32_GE_U" => (
            Compare(ScalarComparison::GreaterOrEqualUnsigned),
            Integral32,
        ),
        "I64_CLZ" => (CountLeadingZeros, Integral64),
        "I64_CTZ" => (CountTrailingZeros, Integral64),
        "I64_POPCNT" => (PopulationCount, Integral64),
        "I64_ADD" => (Add, Integral64),
        "I64_SUB" => (Subtract, Integral64),
        "I64_MUL" => (Multiply, Integral64),
        "I64_DIV_S" => (Divide, Integral64),
        "I64_DIV_U" => (DivideUnsigned, Integral64),
        "I64_REM_S" => (Remainder, Integral64),
        "I64_REM_U" => (RemainderUnsigned, Integral64),
        "I64_AND" => (And, Integral64),
        "I64_OR" => (Or, Integral64),
        "I64_XOR" => (Xor, Integral64),
        "I64_SHL" => (ShiftLeft, Integral64),
        "I64_SHR_S" => (ShiftRight, Integral64),
        "I64_SHR_U" => (ShiftRightUnsigned, Integral64),
        "I64_ROTL" => (RotateLeft, Integral64),
        "I64_ROTR" => (RotateRight, Integral64),
        "I64_EQ" => (Compare(ScalarComparison::Equal), Integral64),
        "I64_NE" => (Compare(ScalarComparison::NotEqual), Integral64),
        "I64_LT_S" => (Compare(ScalarComparison::Less), Integral64),
        "I64_LT_U" => (Compare(ScalarComparison::LessUnsigned), Integral64),
        "I64_GT_S" => (Compare(ScalarComparison::Greater), Integral64),
        "I64_GT_U" => (Compare(ScalarComparison::GreaterUnsigned), Integral64),
        "I64_LE_S" => (Compare(ScalarComparison::LessOrEqual), Integral64),
        "I64_LE_U" => (Compare(ScalarComparison::LessOrEqualUnsigned), Integral64),
        "I64_GE_S" => (Compare(ScalarComparison::GreaterOrEqual), Integral64),
        "I64_GE_U" => (
            Compare(ScalarComparison::GreaterOrEqualUnsigned),
            Integral64,
        ),
        "F32_ABS" => (Absolute, Float32),
        "F32_NEG" => (Negate, Float32),
        "F32_CEIL" => (Ceiling, Float32),
        "F32_FLOOR" => (Floor, Float32),
        "F32_TRUNC" => (Truncate, Float32),
        "F32_NEAREST" => (Nearest, Float32),
        "F32_SQRT" => (SquareRoot, Float32),
        "F32_ADD" => (Add, Float32),
        "F32_SUB" => (Subtract, Float32),
        "F32_MUL" => (Multiply, Float32),
        "F32_DIV" => (Divide, Float32),
        "F32_MIN" => (Minimum, Float32),
        "F32_MAX" => (Maximum, Float32),
        "F32_COPYSIGN" => (CopySign, Float32),
        "F32_EQ" => (Compare(ScalarComparison::Equal), Float32),
        "F32_NE" => (Compare(ScalarComparison::NotEqual), Float32),
        "F32_LT" => (Compare(ScalarComparison::Less), Float32),
        "F32_GT" => (Compare(ScalarComparison::Greater), Float32),
        "F32_LE" => (Compare(ScalarComparison::LessOrEqual), Float32),
        "F32_GE" => (Compare(ScalarComparison::GreaterOrEqual), Float32),
        "F64_ABS" => (Absolute, Float64),
        "F64_NEG" => (Negate, Float64),
        "F64_CEIL" => (Ceiling, Float64),
        "F64_FLOOR" => (Floor, Float64),
        "F64_TRUNC" => (Truncate, Float64),
        "F64_NEAREST" => (Nearest, Float64),
        "F64_SQRT" => (SquareRoot, Float64),
        "F64_ADD" => (Add, Float64),
        "F64_SUB" => (Subtract, Float64),
        "F64_MUL" => (Multiply, Float64),
        "F64_DIV" => (Divide, Float64),
        "F64_MIN" => (Minimum, Float64),
        "F64_MAX" => (Maximum, Float64),
        "F64_COPYSIGN" => (CopySign, Float64),
        "F64_EQ" => (Compare(ScalarComparison::Equal), Float64),
        "F64_NE" => (Compare(ScalarComparison::NotEqual), Float64),
        "F64_LT" => (Compare(ScalarComparison::Less), Float64),
        "F64_GT" => (Compare(ScalarComparison::Greater), Float64),
        "F64_LE" => (Compare(ScalarComparison::LessOrEqual), Float64),
        "F64_GE" => (Compare(ScalarComparison::GreaterOrEqual), Float64),
        "I32_WRAP_I64" => (Wrap, Integral64),
        "I64_EXTEND_I32_S" => (Extend, Integral32),
        "I64_EXTEND_I32_U" => (ExtendUnsigned, Integral32),
        "F32_CONVERT_I32_S" => (ConvertToFloating, Integral32),
        "F32_CONVERT_I32_U" => (ConvertUnsignedToFloating, Integral32),
        "F32_CONVERT_I64_S" => (ConvertToFloating, Integral64),
        "F32_CONVERT_I64_U" => (ConvertUnsignedToFloating, Integral64),
        "F64_CONVERT_I32_S" => (ConvertToFloating, Integral32),
        "F64_CONVERT_I32_U" => (ConvertUnsignedToFloating, Integral32),
        "F64_CONVERT_I64_S" => (ConvertToFloating, Integral64),
        "F64_CONVERT_I64_U" => (ConvertUnsignedToFloating, Integral64),
        "I32_TRUNC_SAT_F32_S" => (TruncateSaturating, Float32),
        "I32_TRUNC_SAT_F32_U" => (TruncateSaturatingUnsigned, Float32),
        "I32_TRUNC_SAT_F64_S" => (TruncateSaturating, Float64),
        "I32_TRUNC_SAT_F64_U" => (TruncateSaturatingUnsigned, Float64),
        "I64_TRUNC_SAT_F32_S" => (TruncateSaturating, Float32),
        "I64_TRUNC_SAT_F32_U" => (TruncateSaturatingUnsigned, Float32),
        "I64_TRUNC_SAT_F64_S" => (TruncateSaturating, Float64),
        "I64_TRUNC_SAT_F64_U" => (TruncateSaturatingUnsigned, Float64),
        "F32_DEMOTE_F64" => (Demote, Float64),
        "F64_PROMOTE_F32" => (Promote, Float32),
        _ => return None,
    })
}

fn ir_string_annotation<'a>(
    arena: &'a KlibIrArena,
    annotations: &'a [crate::metadata::klib_ir::tree::KlibIrAnnotation],
    package: &[&str],
    declaration: &[&str],
) -> Option<&'a str> {
    let mut matches = annotations.iter().filter(|annotation| {
        if annotation.access.symbol.kind != KlibIrSymbolKind::Constructor
            || annotation.constructor_type_arguments != 0
            || !annotation.access.type_arguments.is_empty()
        {
            return false;
        }
        let KlibIrSignature::Public(signature) = &annotation.access.symbol.signature else {
            return false;
        };
        signature
            .package()
            .segments()
            .iter()
            .map(String::as_str)
            .eq(package.iter().copied())
            && signature
                .declaration()
                .segments()
                .iter()
                .map(String::as_str)
                .eq(declaration.iter().copied())
    });
    let annotation = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    let arguments = match &annotation.access.arguments {
        KlibIrArguments::Flat(arguments) => arguments.as_slice(),
        KlibIrArguments::Split {
            dispatch_receiver: None,
            extension_receiver: None,
            values,
        } => values.as_slice(),
        KlibIrArguments::Split { .. } => return None,
    };
    let &[Some(argument)] = arguments else {
        return None;
    };
    match &arena.expr(argument).kind {
        KlibIrExprKind::Const(KlibIrConstant::String(value)) => Some(value),
        _ => None,
    }
}

fn ir_semantic_type(
    arena: &KlibIrArena,
    id: crate::metadata::klib_ir::tree::KlibIrTypeId,
) -> Option<Ty> {
    // A class type is not null whether the serializer leaves its nullability unspecified or
    // records it as definitely not null.
    let KlibIrType::Simple {
        classifier,
        nullability: KlibIrNullability::NotSpecified | KlibIrNullability::DefinitelyNotNull,
        arguments,
        annotations,
        abbreviation: None,
    } = arena.ty(id)
    else {
        return None;
    };
    if classifier.kind != KlibIrSymbolKind::Class
        || !arguments.is_empty()
        || !annotations.is_empty()
    {
        return None;
    }
    let KlibIrSignature::Public(signature) = &classifier.signature else {
        return None;
    };
    if signature.member_id().is_some()
        || signature.package().segments() != ["kotlin"]
        || signature.declaration().segments().len() != 1
    {
        return None;
    }
    match signature.declaration().segments()[0].as_str() {
        "Boolean" => Some(Ty::Boolean),
        "Byte" => Some(Ty::Byte),
        "Short" => Some(Ty::Short),
        "Int" => Some(Ty::Int),
        "UInt" => Some(Ty::UInt),
        "Long" => Some(Ty::Long),
        "ULong" => Some(Ty::ULong),
        "Float" => Some(Ty::Float),
        "Double" => Some(Ty::Double),
        _ => None,
    }
}

pub(super) fn authenticate(
    platform: Option<TargetIntrinsicPlatform>,
    placement: crate::libraries::CallablePlacement,
    declaration: &KotlinFunction,
) -> Option<KlibIntrinsicShape> {
    if platform == Some(TargetIntrinsicPlatform::Native) {
        if let Some(shape) = native_continuation(placement, declaration) {
            return Some(shape);
        }
    }
    if declaration.context_count != 0
        || !declaration.context_kinds.is_empty()
        || declaration.is_suspend
        || declaration.is_inline
        || declaration.is_expect
        || declaration.vararg.is_some()
        || declaration
            .param_defaults
            .iter()
            .any(|defaulted| *defaulted)
    {
        return None;
    }
    // Past the continuation intrinsics above, only `wasm_array_copy` and `interceptedIntrinsic`
    // are generic: the one type parameter is the GC array type both operands share, or the
    // continuation's result type.
    match (platform?, declaration.formals.is_empty()) {
        (TargetIntrinsicPlatform::Native, true) => native(placement, declaration),
        (TargetIntrinsicPlatform::Wasm, true) => wasm(placement, declaration),
        (TargetIntrinsicPlatform::Wasm, false) => wasm_array_copy(placement, declaration)
            .or_else(|| wasm_intercepted_continuation(placement, declaration)),
        (TargetIntrinsicPlatform::Native, false) => None,
    }
}

/// Native's two continuation intrinsics in `kotlin.native.internal`, which kotlinc's
/// pre-serialization inliner leaves in a KLIB body that expanded
/// `suspendCoroutineUninterceptedOrReturn<T>`: the external `getContinuation<T>(): Continuation<T>`
/// and the external suspend `returnIfSuspended<T>(argument: Any?): T`. Each has exactly one type
/// parameter, which its result states.
fn native_continuation(
    placement: crate::libraries::CallablePlacement,
    declaration: &KotlinFunction,
) -> Option<KlibIntrinsicShape> {
    if placement
        != crate::libraries::CallablePlacement::Package(type_name("kotlin/native/internal"))
        || !declaration.is_external
        || declaration.is_inline
        || declaration.is_expect
        || declaration.receiver.is_some()
        || declaration.context_count != 0
        || !declaration.context_kinds.is_empty()
        || declaration.vararg.is_some()
        || declaration
            .param_defaults
            .iter()
            .any(|defaulted| *defaulted)
    {
        return None;
    }
    let [formal] = declaration.formals.as_slice() else {
        return None;
    };
    if formal.reified {
        return None;
    }
    let names_formal = |ty: &KotlinType| matches!(ty, KotlinType::Param { id, nullable: false, .. } if *id == formal.id);
    let entry = string_annotation(
        &declaration.annotation_applications,
        type_name("kotlin/native/internal/TypedIntrinsic"),
        "kind",
    )?;
    let any = Ty::nullable(Ty::obj_name(crate::types::wk::any()));
    let (operation, params) = match entry {
        "GET_CONTINUATION"
            if !declaration.is_suspend
                && declaration.params.is_empty()
                && matches!(
                    &declaration.ret,
                    KotlinType::Class {
                        internal,
                        args,
                        nullable: false,
                        shape: KotlinFunctionTypeShape {
                            receiver: false,
                            context_count: 0,
                            suspend: false,
                        },
                    } if internal == "kotlin/coroutines/Continuation"
                        && matches!(args.as_slice(), [argument] if names_formal(argument))
                ) =>
        {
            (CompilerIntrinsic::CurrentContinuation, Vec::new())
        }
        "RETURN_IF_SUSPENDED"
            if declaration.is_suspend
                && matches!(
                    declaration.params.as_slice(),
                    [KotlinType::Class { internal, args, nullable: true, .. }]
                        if internal == "kotlin/Any" && args.is_empty()
                )
                && names_formal(&declaration.ret) =>
        {
            (CompilerIntrinsic::ReturnIfSuspended, vec![any])
        }
        _ => return None,
    };
    // The result names the declaration's own type parameter, which each call binds: the shape
    // records it as the erased reference a call's type argument substitutes.
    let ret = match operation {
        CompilerIntrinsic::CurrentContinuation => {
            Ty::obj_args("kotlin/coroutines/Continuation", &[any])
        }
        _ => any,
    };
    Some(KlibIntrinsicShape {
        operation,
        kind: ExternalCallableKind::TopLevel,
        receiver: None,
        params,
        ret,
    })
}

fn is_nullable_any(ty: &KotlinType) -> bool {
    matches!(
        ty,
        KotlinType::Class {
            internal,
            args,
            nullable: true,
            shape: KotlinFunctionTypeShape {
                receiver: false,
                context_count: 0,
                suspend: false,
            },
        } if internal == "kotlin/Any" && args.is_empty()
    )
}

/// `wasm_array_copy<T>(destination: T, destinationIndex: Int, source: T, sourceIndex: Int,
/// length: Int)` in `kotlin.wasm.internal`. It is `@ExcludedFromCodegen`: kotlinc binds the
/// declaration by this name and lowers its call to `array.copy` over the two GC arrays, whose
/// type is the call's type argument.
fn wasm_array_copy(
    placement: crate::libraries::CallablePlacement,
    declaration: &KotlinFunction,
) -> Option<KlibIntrinsicShape> {
    let [element] = declaration.formals.as_slice() else {
        return None;
    };
    let [destination, destination_index, source, source_index, length] =
        declaration.params.as_slice()
    else {
        return None;
    };
    let is_element = |ty: &KotlinType| matches!(ty, KotlinType::Param { id, nullable: false, .. } if *id == element.id);
    let excluded = declaration
        .annotation_applications
        .iter()
        .filter(|annotation| {
            annotation.identity == type_name("kotlin/wasm/internal/ExcludedFromCodegen")
        })
        .count();
    (placement == crate::libraries::CallablePlacement::Package(type_name("kotlin/wasm/internal"))
        && declaration.name == "wasm_array_copy"
        && excluded == 1
        && declaration.receiver.is_none()
        && !element.reified
        && element.bounds.iter().all(is_nullable_any)
        && is_element(destination)
        && is_element(source)
        && [destination_index, source_index, length]
            .into_iter()
            .all(|index| semantic_type(index) == Some(Ty::Int))
        && exact_type(&declaration.ret, "kotlin/Unit"))
    .then(|| {
        let array = Ty::ty_param(
            &element.name,
            Ty::nullable(Ty::obj_name(crate::types::wk::any())),
        );
        KlibIntrinsicShape {
            operation: CompilerIntrinsic::ArrayStorageCopy,
            kind: ExternalCallableKind::TopLevel,
            receiver: None,
            params: vec![array, Ty::Int, array, Ty::Int, Ty::Int],
            ret: Ty::Unit,
        }
    })
}

/// `interceptedIntrinsic<T>(cont: Continuation<T>): Continuation<T>` in `kotlin.wasm.internal`,
/// the `@ExcludedFromCodegen` body of Kotlin/Wasm's `Continuation<T>.intercepted()`. kotlinc binds
/// the declaration by this name and expands its call to the stdlib's
/// `if (cont is CoroutineImpl) cont.intercepted() else cont`.
fn wasm_intercepted_continuation(
    placement: crate::libraries::CallablePlacement,
    declaration: &KotlinFunction,
) -> Option<KlibIntrinsicShape> {
    let [result] = declaration.formals.as_slice() else {
        return None;
    };
    let is_continuation = |ty: &KotlinType| {
        matches!(
            ty,
            KotlinType::Class {
                internal,
                args,
                nullable: false,
                shape: KotlinFunctionTypeShape {
                    receiver: false,
                    context_count: 0,
                    suspend: false,
                },
            } if internal == "kotlin/coroutines/Continuation"
                && matches!(
                    args.as_slice(),
                    [KotlinType::Param { id, nullable: false, .. }] if *id == result.id
                )
        )
    };
    let excluded = declaration
        .annotation_applications
        .iter()
        .filter(|annotation| {
            annotation.identity == type_name("kotlin/wasm/internal/ExcludedFromCodegen")
        })
        .count();
    (placement == crate::libraries::CallablePlacement::Package(type_name("kotlin/wasm/internal"))
        && declaration.name == "interceptedIntrinsic"
        && excluded == 1
        && declaration.receiver.is_none()
        && !result.reified
        && result.bounds.iter().all(is_nullable_any)
        && matches!(declaration.params.as_slice(), [continuation] if is_continuation(continuation))
        && is_continuation(&declaration.ret))
    .then(|| {
        let continuation = Ty::obj_args(
            "kotlin/coroutines/Continuation",
            &[Ty::ty_param(
                &result.name,
                Ty::nullable(Ty::obj_name(crate::types::wk::any())),
            )],
        );
        KlibIntrinsicShape {
            operation: CompilerIntrinsic::InterceptedContinuation,
            kind: ExternalCallableKind::TopLevel,
            receiver: None,
            params: vec![continuation],
            ret: continuation,
        }
    })
}

/// `intToString(value: Int, radix: Int): String` and its `Long` counterpart in `kotlin.text`.
fn radix_to_string(
    placement: crate::libraries::CallablePlacement,
    declaration: &KotlinFunction,
    operand: Ty,
) -> Option<KlibIntrinsicShape> {
    let [value, radix] = declaration.params.as_slice() else {
        return None;
    };
    (placement == crate::libraries::CallablePlacement::Package(type_name("kotlin/text"))
        && declaration.receiver.is_none()
        && semantic_type(value) == Some(operand)
        && semantic_type(radix) == Some(Ty::Int)
        && exact_type(&declaration.ret, "kotlin/String"))
    .then(|| KlibIntrinsicShape {
        operation: CompilerIntrinsic::RadixToString { operand },
        kind: ExternalCallableKind::TopLevel,
        receiver: None,
        params: vec![operand, Ty::Int],
        ret: Ty::obj("kotlin/String"),
    })
}

/// `String.unsafeRangeEquals(thisOffset: Int, other: String, otherOffset: Int, length: Int):
/// Boolean` in `kotlin.text`.
fn string_range_equals(
    placement: crate::libraries::CallablePlacement,
    declaration: &KotlinFunction,
) -> Option<KlibIntrinsicShape> {
    let string = Ty::obj("kotlin/String");
    let [this_offset, other, other_offset, length] = declaration.params.as_slice() else {
        return None;
    };
    (placement == crate::libraries::CallablePlacement::Package(type_name("kotlin/text"))
        && declaration
            .receiver
            .as_ref()
            .is_some_and(|receiver| exact_type(receiver, "kotlin/String"))
        && semantic_type(this_offset) == Some(Ty::Int)
        && exact_type(other, "kotlin/String")
        && semantic_type(other_offset) == Some(Ty::Int)
        && semantic_type(length) == Some(Ty::Int)
        && exact_type(&declaration.ret, "kotlin/Boolean"))
    .then(|| KlibIntrinsicShape {
        operation: CompilerIntrinsic::StringRangeEquals,
        kind: ExternalCallableKind::Extension,
        receiver: Some(string),
        params: vec![string, Ty::Int, string, Ty::Int, Ty::Int],
        ret: Ty::Boolean,
    })
}

/// `String.nativeIndexOf(sought, fromIndex: Int): Int` and `String.nativeLastIndexOf(sought,
/// fromIndex: Int): Int` in `kotlin.text`, where `sought` is a `String` or a `Char`.
fn string_index_of(
    placement: crate::libraries::CallablePlacement,
    declaration: &KotlinFunction,
    sought: Ty,
    sought_name: &str,
    last: bool,
) -> Option<KlibIntrinsicShape> {
    let string = Ty::obj("kotlin/String");
    let [value, from] = declaration.params.as_slice() else {
        return None;
    };
    (placement == crate::libraries::CallablePlacement::Package(type_name("kotlin/text"))
        && declaration
            .receiver
            .as_ref()
            .is_some_and(|receiver| exact_type(receiver, "kotlin/String"))
        && exact_type(value, sought_name)
        && semantic_type(from) == Some(Ty::Int)
        && exact_type(&declaration.ret, "kotlin/Int"))
    .then(|| KlibIntrinsicShape {
        operation: CompilerIntrinsic::StringIndexOf { sought, last },
        kind: ExternalCallableKind::Extension,
        receiver: Some(string),
        params: vec![string, sought, Ty::Int],
        ret: Ty::Int,
    })
}

/// `arrayCopy(array, fromIndex: Int, destination, toIndex: Int, count: Int)` in
/// `kotlin.collections`, where both arrays are `declared`, the array of `element`: a specialized
/// primitive array, or `Array<Any?>`.
fn array_copy(
    placement: crate::libraries::CallablePlacement,
    declaration: &KotlinFunction,
    element: Ty,
    declared: KotlinType,
) -> Option<KlibIntrinsicShape> {
    let array = Ty::array(element);
    if declared.internal().map(type_name) != array.obj_internal() {
        return None;
    }
    let is_array = |ty: &KotlinType| *ty == declared;
    let [source, from, destination, to, count] = declaration.params.as_slice() else {
        return None;
    };
    (placement == crate::libraries::CallablePlacement::Package(type_name("kotlin/collections"))
        && declaration.receiver.is_none()
        && is_array(source)
        && is_array(destination)
        && [from, to, count]
            .into_iter()
            .all(|index| semantic_type(index) == Some(Ty::Int))
        && exact_type(&declaration.ret, "kotlin/Unit"))
    .then(|| KlibIntrinsicShape {
        operation: CompilerIntrinsic::ArrayCopy { array },
        kind: ExternalCallableKind::TopLevel,
        receiver: None,
        params: vec![array, Ty::Int, array, Ty::Int, Ty::Int],
        ret: Ty::Unit,
    })
}

fn native(
    placement: crate::libraries::CallablePlacement,
    declaration: &KotlinFunction,
) -> Option<KlibIntrinsicShape> {
    if let Some(shape) = assertion_switch(placement, declaration) {
        return Some(shape);
    }
    // Native's typed intrinsics are declarations without a Kotlin implementation. Wasm's
    // instruction wrappers deliberately have an `implementedAsIntrinsic` body instead, so this
    // representation check belongs to Native rather than the shared authentication gate.
    if !declaration.is_external {
        return None;
    }
    if let Some(entry) = string_annotation(
        &declaration.annotation_applications,
        type_name("kotlin/native/internal/GCUnsafeCall"),
        "callee",
    ) {
        let (operation, operand) = match entry {
            "Kotlin_Float_isNaN" => (
                CompilerIntrinsic::FloatingIsNaN { operand: Ty::Float },
                Ty::Float,
            ),
            "Kotlin_Double_isNaN" => (
                CompilerIntrinsic::FloatingIsNaN {
                    operand: Ty::Double,
                },
                Ty::Double,
            ),
            "Kotlin_Float_isInfinite" => (
                CompilerIntrinsic::FloatingIsInfinite { operand: Ty::Float },
                Ty::Float,
            ),
            "Kotlin_Double_isInfinite" => (
                CompilerIntrinsic::FloatingIsInfinite {
                    operand: Ty::Double,
                },
                Ty::Double,
            ),
            "Kotlin_Array_copyImpl" => {
                let any = Ty::nullable(Ty::obj_name(crate::types::wk::any()));
                return array_copy(
                    placement,
                    declaration,
                    any,
                    KotlinType::Class {
                        internal: "kotlin/Array".to_owned(),
                        args: vec![KotlinType::Class {
                            internal: "kotlin/Any".to_owned(),
                            args: Vec::new(),
                            nullable: true,
                            shape: KotlinFunctionTypeShape::default(),
                        }],
                        nullable: false,
                        shape: KotlinFunctionTypeShape::default(),
                    },
                );
            }
            "Kotlin_BooleanArray_copyImpl" => {
                return array_copy(
                    placement,
                    declaration,
                    Ty::Boolean,
                    KotlinType::class("kotlin/BooleanArray"),
                )
            }
            "Kotlin_ByteArray_copyImpl" => {
                return array_copy(
                    placement,
                    declaration,
                    Ty::Byte,
                    KotlinType::class("kotlin/ByteArray"),
                )
            }
            "Kotlin_CharArray_copyImpl" => {
                return array_copy(
                    placement,
                    declaration,
                    Ty::Char,
                    KotlinType::class("kotlin/CharArray"),
                )
            }
            "Kotlin_ShortArray_copyImpl" => {
                return array_copy(
                    placement,
                    declaration,
                    Ty::Short,
                    KotlinType::class("kotlin/ShortArray"),
                )
            }
            "Kotlin_IntArray_copyImpl" => {
                return array_copy(
                    placement,
                    declaration,
                    Ty::Int,
                    KotlinType::class("kotlin/IntArray"),
                )
            }
            "Kotlin_LongArray_copyImpl" => {
                return array_copy(
                    placement,
                    declaration,
                    Ty::Long,
                    KotlinType::class("kotlin/LongArray"),
                )
            }
            "Kotlin_FloatArray_copyImpl" => {
                return array_copy(
                    placement,
                    declaration,
                    Ty::Float,
                    KotlinType::class("kotlin/FloatArray"),
                )
            }
            "Kotlin_DoubleArray_copyImpl" => {
                return array_copy(
                    placement,
                    declaration,
                    Ty::Double,
                    KotlinType::class("kotlin/DoubleArray"),
                )
            }
            "Kotlin_Int_toStringRadix" => return radix_to_string(placement, declaration, Ty::Int),
            "Kotlin_Long_toStringRadix" => {
                return radix_to_string(placement, declaration, Ty::Long)
            }
            "Kotlin_String_unsafeRangeEquals" => {
                return string_range_equals(placement, declaration)
            }
            "Kotlin_String_indexOfString" => {
                return string_index_of(
                    placement,
                    declaration,
                    Ty::obj("kotlin/String"),
                    "kotlin/String",
                    false,
                )
            }
            "Kotlin_String_indexOfChar" => {
                return string_index_of(placement, declaration, Ty::Char, "kotlin/Char", false)
            }
            "Kotlin_String_lastIndexOfChar" => {
                return string_index_of(placement, declaration, Ty::Char, "kotlin/Char", true)
            }
            _ => return None,
        };
        if placement != crate::libraries::CallablePlacement::Package(type_name("kotlin"))
            || !declaration.params.is_empty()
            || declaration.receiver.as_ref().and_then(semantic_type) != Some(operand)
            || !exact_type(&declaration.ret, "kotlin/Boolean")
        {
            return None;
        }
        return Some(KlibIntrinsicShape {
            operation,
            kind: ExternalCallableKind::Extension,
            receiver: Some(operand),
            params: vec![operand],
            ret: Ty::Boolean,
        });
    }
    if declaration.receiver.is_some() {
        return None;
    }
    let entry = string_annotation(
        &declaration.annotation_applications,
        type_name("kotlin/native/internal/TypedIntrinsic"),
        "kind",
    )?;
    let shape = match (entry, placement) {
        ("REINTERPRET", crate::libraries::CallablePlacement::Member { owner, .. })
            if owner == type_name("kotlin/Float")
                && declaration.params.is_empty()
                && exact_type(&declaration.ret, "kotlin/Int") =>
        {
            (
                ExternalCallableKind::Member,
                Some(Ty::Float),
                vec![],
                Ty::Int,
            )
        }
        ("REINTERPRET", crate::libraries::CallablePlacement::Member { owner, .. })
            if owner == type_name("kotlin/Double")
                && declaration.params.is_empty()
                && exact_type(&declaration.ret, "kotlin/Long") =>
        {
            (
                ExternalCallableKind::Member,
                Some(Ty::Double),
                vec![],
                Ty::Long,
            )
        }
        ("REINTERPRET", crate::libraries::CallablePlacement::Package(package))
            if package == type_name("kotlin")
                && exact_signature(declaration, "kotlin/Long", "kotlin/Double") =>
        {
            (
                ExternalCallableKind::TopLevel,
                None,
                vec![Ty::Long],
                Ty::Double,
            )
        }
        ("REINTERPRET", crate::libraries::CallablePlacement::Package(package))
            if package == type_name("kotlin")
                && exact_signature(declaration, "kotlin/Int", "kotlin/Float") =>
        {
            (
                ExternalCallableKind::TopLevel,
                None,
                vec![Ty::Int],
                Ty::Float,
            )
        }
        ("INTEROP_BITS_TO_FLOAT", crate::libraries::CallablePlacement::Package(package))
            if package == type_name("kotlinx/cinterop")
                && exact_signature(declaration, "kotlin/Int", "kotlin/Float") =>
        {
            (
                ExternalCallableKind::TopLevel,
                None,
                vec![Ty::Int],
                Ty::Float,
            )
        }
        ("INTEROP_BITS_TO_DOUBLE", crate::libraries::CallablePlacement::Package(package))
            if package == type_name("kotlinx/cinterop")
                && exact_signature(declaration, "kotlin/Long", "kotlin/Double") =>
        {
            (
                ExternalCallableKind::TopLevel,
                None,
                vec![Ty::Long],
                Ty::Double,
            )
        }
        _ => return None,
    };
    Some(KlibIntrinsicShape {
        operation: CompilerIntrinsic::ReinterpretBits,
        kind: shape.0,
        receiver: shape.1,
        params: shape.2,
        ret: shape.3,
    })
}

/// The two assertion switches of the Native stdlib's `kotlin` package: `internal fun
/// isAssertionThrowingErrorEnabled(): Boolean` and `isAssertionArgumentEvaluationEnabled()`. Their
/// bodies throw `NotImplementedError("Implemented as intrinsic")`; the compiler answers them for
/// the build, so a call never enters the body.
fn assertion_switch(
    placement: crate::libraries::CallablePlacement,
    declaration: &KotlinFunction,
) -> Option<KlibIntrinsicShape> {
    if placement != crate::libraries::CallablePlacement::Package(type_name("kotlin"))
        || !matches!(
            declaration.name.as_str(),
            "isAssertionThrowingErrorEnabled" | "isAssertionArgumentEvaluationEnabled"
        )
        || declaration.visibility != crate::types::Visibility::Internal
        || declaration.is_external
        || declaration.receiver.is_some()
        || !declaration.params.is_empty()
        || !exact_type(&declaration.ret, "kotlin/Boolean")
    {
        return None;
    }
    Some(KlibIntrinsicShape {
        operation: CompilerIntrinsic::AssertionSwitch,
        kind: ExternalCallableKind::TopLevel,
        receiver: None,
        params: Vec::new(),
        ret: Ty::Boolean,
    })
}

fn wasm(
    placement: crate::libraries::CallablePlacement,
    declaration: &KotlinFunction,
) -> Option<KlibIntrinsicShape> {
    let crate::libraries::CallablePlacement::Package(package) = placement else {
        return None;
    };
    // The metadata spelling of `WASM_OPERATION_PACKAGES`.
    if package != type_name("kotlin") && package != type_name("kotlin/wasm/internal") {
        return None;
    }
    let operation = string_annotation(
        &declaration.annotation_applications,
        type_name("kotlin/wasm/internal/WasmOp"),
        "name",
    )?;
    let operands = declaration
        .receiver
        .iter()
        .chain(&declaration.params)
        .map(semantic_type)
        .collect::<Option<Vec<_>>>()?;
    wasm_shape(
        operation,
        declaration.receiver.is_some(),
        operands,
        semantic_type(&declaration.ret)?,
    )
}

fn string_annotation<'a>(
    annotations: &'a [AnnotationApplication],
    identity: TypeName,
    parameter: &str,
) -> Option<&'a str> {
    let annotation = exact_annotation(annotations, identity, parameter)?;
    match &annotation.arguments[0].1 {
        AnnotationArgument::String(value) => Some(value),
        _ => None,
    }
}

fn exact_annotation<'a>(
    annotations: &'a [AnnotationApplication],
    identity: TypeName,
    parameter: &str,
) -> Option<&'a AnnotationApplication> {
    let mut matches = annotations
        .iter()
        .filter(|annotation| annotation.identity == identity);
    let annotation = matches.next()?;
    if matches.next().is_some()
        || annotation.arguments.len() != 1
        || annotation.arguments[0].0 != parameter
    {
        return None;
    }
    Some(annotation)
}

fn exact_signature(declaration: &KotlinFunction, parameter: &str, result: &str) -> bool {
    declaration.params.len() == 1
        && exact_type(&declaration.params[0], parameter)
        && exact_type(&declaration.ret, result)
}

fn exact_type(ty: &KotlinType, classifier: &str) -> bool {
    matches!(
        ty,
        KotlinType::Class {
            internal,
            args,
            nullable: false,
            shape: KotlinFunctionTypeShape {
                receiver: false,
                context_count: 0,
                suspend: false,
            },
        } if internal == classifier && args.is_empty()
    )
}

fn semantic_type(ty: &KotlinType) -> Option<Ty> {
    [
        ("kotlin/Boolean", Ty::Boolean),
        ("kotlin/Int", Ty::Int),
        ("kotlin/UInt", Ty::UInt),
        ("kotlin/Long", Ty::Long),
        ("kotlin/ULong", Ty::ULong),
        ("kotlin/Float", Ty::Float),
        ("kotlin/Double", Ty::Double),
    ]
    .into_iter()
    .find_map(|(classifier, semantic)| exact_type(ty, classifier).then_some(semantic))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::semantic::KotlinModality;
    use crate::types::Visibility;

    fn declaration(
        name: &str,
        params: &[&str],
        ret: &str,
        annotation: AnnotationApplication,
    ) -> KotlinFunction {
        KotlinFunction {
            name: name.to_owned(),
            receiver: None,
            params: params.iter().copied().map(KotlinType::class).collect(),
            ret: KotlinType::class(ret),
            formals: Vec::new(),
            param_names: (0..params.len()).map(|index| format!("p{index}")).collect(),
            param_defaults: vec![false; params.len()],
            inline_modifiers: vec![crate::types::InlineParameterModifier::None; params.len()],
            vararg: None,
            visibility: Visibility::Public,
            modality: KotlinModality::Final,
            is_inline: false,
            has_reified_type_params: false,
            is_suspend: false,
            is_operator: false,
            is_infix: false,
            is_synthesized: false,
            is_expect: false,
            is_external: true,
            is_static: false,
            context_count: 0,
            context_kinds: Vec::new(),
            annotations: vec![annotation.identity],
            annotation_applications: vec![annotation],
            return_value_status: Default::default(),
            contract: None,
        }
    }

    fn wasm_annotation(operation: &str) -> AnnotationApplication {
        AnnotationApplication {
            identity: type_name("kotlin/wasm/internal/WasmOp"),
            arguments: vec![(
                "name".to_owned(),
                AnnotationArgument::String(operation.to_owned()),
            )],
        }
    }

    fn native_annotation(kind: &str) -> AnnotationApplication {
        AnnotationApplication {
            identity: type_name("kotlin/native/internal/TypedIntrinsic"),
            arguments: vec![(
                "kind".to_owned(),
                AnnotationArgument::String(kind.to_owned()),
            )],
        }
    }

    #[test]
    fn wasm_reinterpret_operations_require_authority_package_payload_and_signature() {
        let placement =
            crate::libraries::CallablePlacement::Package(type_name("kotlin/wasm/internal"));
        let exact = declaration(
            "wasm_i32_reinterpret_f32",
            &["kotlin/Float"],
            "kotlin/Int",
            wasm_annotation("I32_REINTERPRET_F32"),
        );
        assert!(authenticate(None, placement, &exact).is_none());
        assert_eq!(
            authenticate(Some(TargetIntrinsicPlatform::Wasm), placement, &exact),
            Some(KlibIntrinsicShape {
                operation: CompilerIntrinsic::ReinterpretBits,
                kind: ExternalCallableKind::TopLevel,
                receiver: None,
                params: vec![Ty::Float],
                ret: Ty::Int,
            })
        );

        let wrong_package = crate::libraries::CallablePlacement::Package(type_name("spoof"));
        assert!(authenticate(Some(TargetIntrinsicPlatform::Wasm), wrong_package, &exact).is_none());
        let wrong_signature = declaration(
            "wasm_i32_reinterpret_f32",
            &["kotlin/Double"],
            "kotlin/Int",
            wasm_annotation("I32_REINTERPRET_F32"),
        );
        assert!(authenticate(
            Some(TargetIntrinsicPlatform::Wasm),
            placement,
            &wrong_signature
        )
        .is_none());
        let wrong_payload = declaration(
            "wasm_i32_reinterpret_f32",
            &["kotlin/Float"],
            "kotlin/Int",
            wasm_annotation("I32_ADD"),
        );
        assert!(authenticate(
            Some(TargetIntrinsicPlatform::Wasm),
            placement,
            &wrong_payload
        )
        .is_none());
        let mut bodyful = declaration(
            "wasm_i32_reinterpret_f32",
            &["kotlin/Float"],
            "kotlin/Int",
            wasm_annotation("I32_REINTERPRET_F32"),
        );
        bodyful.is_external = false;
        assert_eq!(
            authenticate(Some(TargetIntrinsicPlatform::Wasm), placement, &bodyful),
            authenticate(Some(TargetIntrinsicPlatform::Wasm), placement, &exact),
            "the Wasm stdlib serializes instruction wrappers with an implementedAsIntrinsic body",
        );
    }

    #[test]
    fn wasm_scalar_instructions_require_their_operand_class_arity_and_package() {
        let wasm = Some(TargetIntrinsicPlatform::Wasm);
        let internal =
            crate::libraries::CallablePlacement::Package(type_name("kotlin/wasm/internal"));
        let kotlin = crate::libraries::CallablePlacement::Package(type_name("kotlin"));
        let instruction = |name, params: &[&str], ret, operation| {
            declaration(name, params, ret, wasm_annotation(operation))
        };
        let shape = |operation, kind, params: &[Ty], ret| {
            Some(KlibIntrinsicShape {
                operation: CompilerIntrinsic::ScalarInstruction(operation),
                kind,
                receiver: None,
                params: params.to_vec(),
                ret,
            })
        };

        let divide = instruction(
            "wasm_i32_div_u",
            &["kotlin/Int", "kotlin/Int"],
            "kotlin/Int",
            "I32_DIV_U",
        );
        assert_eq!(
            authenticate(wasm, internal, &divide),
            shape(
                ScalarInstruction::DivideUnsigned,
                ExternalCallableKind::TopLevel,
                &[Ty::Int, Ty::Int],
                Ty::Int
            )
        );
        assert!(authenticate(None, internal, &divide).is_none());
        assert!(authenticate(
            wasm,
            crate::libraries::CallablePlacement::Package(type_name("spoof")),
            &divide
        )
        .is_none());
        let unsigned = instruction(
            "uintDivide",
            &["kotlin/UInt", "kotlin/UInt"],
            "kotlin/UInt",
            "I32_DIV_U",
        );
        assert_eq!(
            authenticate(wasm, kotlin, &unsigned),
            shape(
                ScalarInstruction::DivideUnsigned,
                ExternalCallableKind::TopLevel,
                &[Ty::UInt, Ty::UInt],
                Ty::UInt
            )
        );
        let less = instruction(
            "wasm_f64_lt",
            &["kotlin/Double", "kotlin/Double"],
            "kotlin/Boolean",
            "F64_LT",
        );
        assert_eq!(
            authenticate(wasm, internal, &less),
            shape(
                ScalarInstruction::Compare(ScalarComparison::Less),
                ExternalCallableKind::TopLevel,
                &[Ty::Double, Ty::Double],
                Ty::Boolean
            )
        );
        let extend = instruction(
            "uintToULong",
            &["kotlin/Int"],
            "kotlin/ULong",
            "I64_EXTEND_I32_U",
        );
        assert_eq!(
            authenticate(wasm, kotlin, &extend),
            shape(
                ScalarInstruction::ExtendUnsigned,
                ExternalCallableKind::TopLevel,
                &[Ty::Int],
                Ty::ULong
            )
        );

        let mut leading_zeros = instruction("countLeadingZeroBits", &[], "kotlin/Int", "I32_CLZ");
        leading_zeros.receiver = Some(KotlinType::class("kotlin/Int"));
        assert_eq!(
            authenticate(wasm, kotlin, &leading_zeros),
            shape(
                ScalarInstruction::CountLeadingZeros,
                ExternalCallableKind::Extension,
                &[Ty::Int],
                Ty::Int
            ),
            "an extension receiver is the instruction's first operand"
        );
        assert!(
            authenticate(
                wasm,
                crate::libraries::CallablePlacement::Member {
                    owner: type_name("kotlin/Int"),
                    owner_is_interface: false,
                },
                &instruction("countLeadingZeroBits", &[], "kotlin/Int", "I32_CLZ"),
            )
            .is_none(),
            "a primitive member is the shared builtin operator, not a target instruction"
        );

        for (rejected, why) in [
            (
                instruction("wasm_i64_clz", &["kotlin/Int"], "kotlin/Int", "I64_CLZ"),
                "the operand class is the instruction's",
            ),
            (
                instruction("wasm_i32_add", &["kotlin/Int"], "kotlin/Int", "I32_ADD"),
                "the arity is the instruction's",
            ),
            (
                instruction(
                    "wasm_i32_add",
                    &["kotlin/Int", "kotlin/UInt"],
                    "kotlin/Int",
                    "I32_ADD",
                ),
                "every operand has one type",
            ),
            (
                instruction("wasm_i32_clz", &["kotlin/Int"], "kotlin/Long", "I32_CLZ"),
                "the result is the instruction's",
            ),
            (
                instruction("wasm_f32_sqrt", &["kotlin/Int"], "kotlin/Int", "F32_SQRT"),
                "a floating instruction takes floating operands",
            ),
            (
                instruction("wasm_i32_eqz", &["kotlin/Int"], "kotlin/Boolean", "I32_EQZ"),
                "an operation outside the table authenticates nothing",
            ),
        ] {
            assert!(authenticate(wasm, internal, &rejected).is_none(), "{why}");
        }
    }

    #[test]
    fn native_reinterpret_declarations_have_their_exact_receiver_or_parameter_shape() {
        let cases = [
            (
                crate::libraries::CallablePlacement::Member {
                    owner: type_name("kotlin/Float"),
                    owner_is_interface: false,
                },
                declaration("bits", &[], "kotlin/Int", native_annotation("REINTERPRET")),
                Some(Ty::Float),
                vec![],
                Ty::Int,
            ),
            (
                crate::libraries::CallablePlacement::Member {
                    owner: type_name("kotlin/Double"),
                    owner_is_interface: false,
                },
                declaration("bits", &[], "kotlin/Long", native_annotation("REINTERPRET")),
                Some(Ty::Double),
                vec![],
                Ty::Long,
            ),
            (
                crate::libraries::CallablePlacement::Package(type_name("kotlin")),
                declaration(
                    "fromBits",
                    &["kotlin/Int"],
                    "kotlin/Float",
                    native_annotation("REINTERPRET"),
                ),
                None,
                vec![Ty::Int],
                Ty::Float,
            ),
            (
                crate::libraries::CallablePlacement::Package(type_name("kotlin")),
                declaration(
                    "fromBits",
                    &["kotlin/Long"],
                    "kotlin/Double",
                    native_annotation("REINTERPRET"),
                ),
                None,
                vec![Ty::Long],
                Ty::Double,
            ),
            (
                crate::libraries::CallablePlacement::Package(type_name("kotlinx/cinterop")),
                declaration(
                    "bitsToFloat",
                    &["kotlin/Int"],
                    "kotlin/Float",
                    native_annotation("INTEROP_BITS_TO_FLOAT"),
                ),
                None,
                vec![Ty::Int],
                Ty::Float,
            ),
            (
                crate::libraries::CallablePlacement::Package(type_name("kotlinx/cinterop")),
                declaration(
                    "bitsToDouble",
                    &["kotlin/Long"],
                    "kotlin/Double",
                    native_annotation("INTEROP_BITS_TO_DOUBLE"),
                ),
                None,
                vec![Ty::Long],
                Ty::Double,
            ),
        ];
        assert_eq!(cases.len(), 6);
        for (placement, declaration, receiver, params, ret) in cases {
            assert_eq!(
                authenticate(
                    Some(TargetIntrinsicPlatform::Native),
                    placement,
                    &declaration
                ),
                Some(KlibIntrinsicShape {
                    operation: CompilerIntrinsic::ReinterpretBits,
                    kind: if receiver.is_some() {
                        ExternalCallableKind::Member
                    } else {
                        ExternalCallableKind::TopLevel
                    },
                    receiver,
                    params,
                    ret,
                })
            );
        }

        let mut bodyful_native = declaration(
            "fromBits",
            &["kotlin/Int"],
            "kotlin/Float",
            native_annotation("REINTERPRET"),
        );
        bodyful_native.is_external = false;
        assert!(authenticate(
            Some(TargetIntrinsicPlatform::Native),
            crate::libraries::CallablePlacement::Package(type_name("kotlin")),
            &bodyful_native,
        )
        .is_none());

        let enum_payload = AnnotationApplication {
            identity: type_name("kotlin/native/internal/TypedIntrinsic"),
            arguments: vec![(
                "kind".to_owned(),
                AnnotationArgument::Enum {
                    class: type_name("kotlin/native/internal/IntrinsicType"),
                    entry: "REINTERPRET".to_owned(),
                },
            )],
        };
        assert!(authenticate(
            Some(TargetIntrinsicPlatform::Native),
            crate::libraries::CallablePlacement::Package(type_name("kotlin")),
            &declaration("fromBits", &["kotlin/Int"], "kotlin/Float", enum_payload),
        )
        .is_none());

        let wrong_enum_class = AnnotationApplication {
            identity: type_name("kotlin/native/internal/TypedIntrinsic"),
            arguments: vec![(
                "kind".to_owned(),
                AnnotationArgument::Enum {
                    class: type_name("spoof/IntrinsicType"),
                    entry: "REINTERPRET".to_owned(),
                },
            )],
        };
        assert!(authenticate(
            Some(TargetIntrinsicPlatform::Native),
            crate::libraries::CallablePlacement::Package(type_name("kotlin")),
            &declaration(
                "fromBits",
                &["kotlin/Int"],
                "kotlin/Float",
                wrong_enum_class,
            ),
        )
        .is_none());
    }

    #[test]
    fn native_gc_unsafe_floating_predicates_require_the_exact_entry_and_extension_shape() {
        let gc_unsafe = |callee: &str| AnnotationApplication {
            identity: type_name("kotlin/native/internal/GCUnsafeCall"),
            arguments: vec![(
                "callee".to_owned(),
                AnnotationArgument::String(callee.to_owned()),
            )],
        };
        let cases = [
            (
                "isNaN",
                "Kotlin_Float_isNaN",
                "kotlin/Float",
                CompilerIntrinsic::FloatingIsNaN { operand: Ty::Float },
                Ty::Float,
            ),
            (
                "isNaN",
                "Kotlin_Double_isNaN",
                "kotlin/Double",
                CompilerIntrinsic::FloatingIsNaN {
                    operand: Ty::Double,
                },
                Ty::Double,
            ),
            (
                "isInfinite",
                "Kotlin_Float_isInfinite",
                "kotlin/Float",
                CompilerIntrinsic::FloatingIsInfinite { operand: Ty::Float },
                Ty::Float,
            ),
            (
                "isInfinite",
                "Kotlin_Double_isInfinite",
                "kotlin/Double",
                CompilerIntrinsic::FloatingIsInfinite {
                    operand: Ty::Double,
                },
                Ty::Double,
            ),
        ];
        for (name, entry, receiver, operation, operand) in cases {
            let mut exact = declaration(name, &[], "kotlin/Boolean", gc_unsafe(entry));
            exact.receiver = Some(KotlinType::class(receiver));
            assert_eq!(
                authenticate(
                    Some(TargetIntrinsicPlatform::Native),
                    crate::libraries::CallablePlacement::Package(type_name("kotlin")),
                    &exact,
                ),
                Some(KlibIntrinsicShape {
                    operation,
                    kind: ExternalCallableKind::Extension,
                    receiver: Some(operand),
                    params: vec![operand],
                    ret: Ty::Boolean,
                })
            );
        }

        let mut exact = declaration(
            "isNaN",
            &[],
            "kotlin/Boolean",
            gc_unsafe("Kotlin_Float_isNaN"),
        );
        exact.receiver = Some(KotlinType::class("kotlin/Float"));

        let mut spoofed_entry = declaration(
            "isNaN",
            &[],
            "kotlin/Boolean",
            gc_unsafe("User_Float_isNaN"),
        );
        spoofed_entry.receiver = Some(KotlinType::class("kotlin/Float"));
        assert!(authenticate(
            Some(TargetIntrinsicPlatform::Native),
            crate::libraries::CallablePlacement::Package(type_name("kotlin")),
            &spoofed_entry,
        )
        .is_none());

        let mut wrong_receiver = declaration(
            "isNaN",
            &[],
            "kotlin/Boolean",
            gc_unsafe("Kotlin_Float_isNaN"),
        );
        wrong_receiver.receiver = Some(KotlinType::class("kotlin/Double"));
        assert!(authenticate(
            Some(TargetIntrinsicPlatform::Native),
            crate::libraries::CallablePlacement::Package(type_name("kotlin")),
            &wrong_receiver,
        )
        .is_none());

        assert!(authenticate(
            Some(TargetIntrinsicPlatform::Native),
            crate::libraries::CallablePlacement::Package(type_name("spoof")),
            &exact,
        )
        .is_none());
    }

    #[test]
    fn native_assertion_switches_require_the_exact_internal_declaration() {
        let native = Some(TargetIntrinsicPlatform::Native);
        let kotlin = crate::libraries::CallablePlacement::Package(type_name("kotlin"));
        let switch = |name: &str| {
            let mut declaration = declaration(
                name,
                &[],
                "kotlin/Boolean",
                AnnotationApplication {
                    identity: type_name("kotlin/PublishedApi"),
                    arguments: Vec::new(),
                },
            );
            declaration.visibility = Visibility::Internal;
            declaration.is_external = false;
            declaration
        };
        for name in [
            "isAssertionThrowingErrorEnabled",
            "isAssertionArgumentEvaluationEnabled",
        ] {
            assert_eq!(
                authenticate(native, kotlin, &switch(name)),
                Some(KlibIntrinsicShape {
                    operation: CompilerIntrinsic::AssertionSwitch,
                    kind: ExternalCallableKind::TopLevel,
                    receiver: None,
                    params: Vec::new(),
                    ret: Ty::Boolean,
                }),
                "{name}"
            );
        }

        let exact = switch("isAssertionThrowingErrorEnabled");
        assert!(authenticate(
            native,
            crate::libraries::CallablePlacement::Package(type_name("spoof")),
            &exact,
        )
        .is_none());
        assert!(authenticate(Some(TargetIntrinsicPlatform::Wasm), kotlin, &exact).is_none());
        assert!(authenticate(None, kotlin, &exact).is_none());
        let mut public = switch("isAssertionThrowingErrorEnabled");
        public.visibility = Visibility::Public;
        assert!(authenticate(native, kotlin, &public).is_none());
        let mut parameterized = switch("isAssertionThrowingErrorEnabled");
        parameterized.params = vec![KotlinType::class("kotlin/Int")];
        assert!(authenticate(native, kotlin, &parameterized).is_none());
        let mut wrong_result = switch("isAssertionThrowingErrorEnabled");
        wrong_result.ret = KotlinType::class("kotlin/Int");
        assert!(authenticate(native, kotlin, &wrong_result).is_none());
        assert!(authenticate(native, kotlin, &switch("isAssertionEnabled")).is_none());
    }

    #[test]
    fn native_gc_unsafe_radix_rendering_requires_the_exact_entry_package_and_shape() {
        let gc_unsafe = |callee: &str| AnnotationApplication {
            identity: type_name("kotlin/native/internal/GCUnsafeCall"),
            arguments: vec![(
                "callee".to_owned(),
                AnnotationArgument::String(callee.to_owned()),
            )],
        };
        let native = Some(TargetIntrinsicPlatform::Native);
        let text = crate::libraries::CallablePlacement::Package(type_name("kotlin/text"));
        for (name, entry, operand_class, operand) in [
            (
                "intToString",
                "Kotlin_Int_toStringRadix",
                "kotlin/Int",
                Ty::Int,
            ),
            (
                "longToString",
                "Kotlin_Long_toStringRadix",
                "kotlin/Long",
                Ty::Long,
            ),
        ] {
            let exact = declaration(
                name,
                &[operand_class, "kotlin/Int"],
                "kotlin/String",
                gc_unsafe(entry),
            );
            assert_eq!(
                authenticate(native, text, &exact),
                Some(KlibIntrinsicShape {
                    operation: CompilerIntrinsic::RadixToString { operand },
                    kind: ExternalCallableKind::TopLevel,
                    receiver: None,
                    params: vec![operand, Ty::Int],
                    ret: Ty::obj("kotlin/String"),
                }),
                "{entry}"
            );
            assert!(authenticate(None, text, &exact).is_none(), "{entry}");
            assert!(authenticate(
                native,
                crate::libraries::CallablePlacement::Package(type_name("spoof")),
                &exact
            )
            .is_none());
            let mut bodyful = declaration(
                name,
                &[operand_class, "kotlin/Int"],
                "kotlin/String",
                gc_unsafe(entry),
            );
            bodyful.is_external = false;
            assert!(authenticate(native, text, &bodyful).is_none(), "{entry}");
            let wrong_radix = declaration(
                name,
                &[operand_class, "kotlin/Long"],
                "kotlin/String",
                gc_unsafe(entry),
            );
            assert!(
                authenticate(native, text, &wrong_radix).is_none(),
                "{entry}"
            );
            let wrong_result = declaration(
                name,
                &[operand_class, "kotlin/Int"],
                "kotlin/Int",
                gc_unsafe(entry),
            );
            assert!(
                authenticate(native, text, &wrong_result).is_none(),
                "{entry}"
            );
            let mut extension = declaration(
                name,
                &[operand_class, "kotlin/Int"],
                "kotlin/String",
                gc_unsafe(entry),
            );
            extension.receiver = Some(KotlinType::class(operand_class));
            assert!(authenticate(native, text, &extension).is_none(), "{entry}");
        }
        let spoofed = declaration(
            "intToString",
            &["kotlin/Int", "kotlin/Int"],
            "kotlin/String",
            gc_unsafe("User_Int_toStringRadix"),
        );
        assert!(authenticate(native, text, &spoofed).is_none());
        let swapped = declaration(
            "intToString",
            &["kotlin/Long", "kotlin/Int"],
            "kotlin/String",
            gc_unsafe("Kotlin_Int_toStringRadix"),
        );
        assert!(authenticate(native, text, &swapped).is_none());
    }

    #[test]
    fn wasm_array_copy_requires_its_exact_generic_declaration() {
        let wasm = Some(TargetIntrinsicPlatform::Wasm);
        let internal =
            crate::libraries::CallablePlacement::Package(type_name("kotlin/wasm/internal"));
        let excluded = AnnotationApplication {
            identity: type_name("kotlin/wasm/internal/ExcludedFromCodegen"),
            arguments: Vec::new(),
        };
        let element = crate::metadata::semantic::KotlinTypeParameterId(7);
        let element_type = KotlinType::Param {
            name: "T".to_owned(),
            id: element,
            nullable: false,
        };
        let generic = |name: &str, annotation: AnnotationApplication| {
            let mut copy = declaration(
                name,
                &[
                    "kotlin/Int",
                    "kotlin/Int",
                    "kotlin/Int",
                    "kotlin/Int",
                    "kotlin/Int",
                ],
                "kotlin/Unit",
                annotation,
            );
            copy.is_external = false;
            copy.formals = vec![crate::metadata::semantic::KotlinTypeParameter {
                id: element,
                name: "T".to_owned(),
                bounds: Vec::new(),
                variance: crate::types::TypeVariance::Invariant,
                only_input: false,
                reified: false,
            }];
            copy.params[0] = element_type.clone();
            copy.params[2] = element_type.clone();
            copy
        };
        let exact = generic("wasm_array_copy", excluded.clone());
        let storage = Ty::ty_param("T", Ty::nullable(Ty::obj_name(crate::types::wk::any())));
        assert_eq!(
            authenticate(wasm, internal, &exact),
            Some(KlibIntrinsicShape {
                operation: CompilerIntrinsic::ArrayStorageCopy,
                kind: ExternalCallableKind::TopLevel,
                receiver: None,
                params: vec![storage, Ty::Int, storage, Ty::Int, Ty::Int],
                ret: Ty::Unit,
            })
        );
        assert!(authenticate(Some(TargetIntrinsicPlatform::Native), internal, &exact).is_none());
        assert!(authenticate(
            wasm,
            crate::libraries::CallablePlacement::Package(type_name("kotlin")),
            &exact
        )
        .is_none());
        assert!(
            authenticate(wasm, internal, &generic("wasm_array_new", excluded.clone())).is_none()
        );
        assert!(authenticate(
            wasm,
            internal,
            &generic("wasm_array_copy", wasm_annotation("ARRAY_COPY"))
        )
        .is_none());
        let mut nullable = generic("wasm_array_copy", excluded.clone());
        nullable.params[2] = KotlinType::Param {
            name: "T".to_owned(),
            id: element,
            nullable: true,
        };
        assert!(authenticate(wasm, internal, &nullable).is_none());
        let mut bounded = generic("wasm_array_copy", excluded.clone());
        bounded.formals[0].bounds = vec![KotlinType::class("kotlin/Any")];
        assert!(authenticate(wasm, internal, &bounded).is_none());
        let mut reified = generic("wasm_array_copy", excluded.clone());
        reified.formals[0].reified = true;
        assert!(authenticate(wasm, internal, &reified).is_none());
        let mut concrete = generic("wasm_array_copy", excluded);
        concrete.params[2] = KotlinType::class("kotlin/Int");
        assert!(authenticate(wasm, internal, &concrete).is_none());
    }

    #[test]
    fn wasm_intercepted_continuation_requires_its_exact_generic_declaration() {
        let wasm = Some(TargetIntrinsicPlatform::Wasm);
        let internal =
            crate::libraries::CallablePlacement::Package(type_name("kotlin/wasm/internal"));
        let excluded = AnnotationApplication {
            identity: type_name("kotlin/wasm/internal/ExcludedFromCodegen"),
            arguments: Vec::new(),
        };
        let result = crate::metadata::semantic::KotlinTypeParameterId(3);
        let continuation = |argument: KotlinType| KotlinType::Class {
            internal: "kotlin/coroutines/Continuation".to_owned(),
            args: vec![argument],
            nullable: false,
            shape: KotlinFunctionTypeShape {
                receiver: false,
                context_count: 0,
                suspend: false,
            },
        };
        let formal = KotlinType::Param {
            name: "T".to_owned(),
            id: result,
            nullable: false,
        };
        let generic = |name: &str, annotation: AnnotationApplication| {
            let mut intercepted = declaration(name, &["kotlin/Int"], "kotlin/Int", annotation);
            intercepted.is_external = false;
            intercepted.formals = vec![crate::metadata::semantic::KotlinTypeParameter {
                id: result,
                name: "T".to_owned(),
                bounds: Vec::new(),
                variance: crate::types::TypeVariance::Invariant,
                only_input: false,
                reified: false,
            }];
            intercepted.params[0] = continuation(formal.clone());
            intercepted.ret = continuation(formal.clone());
            intercepted
        };
        let exact = generic("interceptedIntrinsic", excluded.clone());
        let operand = Ty::obj_args(
            "kotlin/coroutines/Continuation",
            &[Ty::ty_param(
                "T",
                Ty::nullable(Ty::obj_name(crate::types::wk::any())),
            )],
        );
        assert_eq!(
            authenticate(wasm, internal, &exact),
            Some(KlibIntrinsicShape {
                operation: CompilerIntrinsic::InterceptedContinuation,
                kind: ExternalCallableKind::TopLevel,
                receiver: None,
                params: vec![operand],
                ret: operand,
            })
        );
        assert!(authenticate(Some(TargetIntrinsicPlatform::Native), internal, &exact).is_none());
        assert!(authenticate(
            wasm,
            crate::libraries::CallablePlacement::Package(type_name("kotlin/coroutines")),
            &exact
        )
        .is_none());
        assert!(authenticate(wasm, internal, &generic("intercepted", excluded.clone())).is_none());
        let mut included = generic("interceptedIntrinsic", excluded.clone());
        included.annotation_applications.clear();
        assert!(authenticate(wasm, internal, &included).is_none());
        let mut nullable = generic("interceptedIntrinsic", excluded.clone());
        nullable.ret = KotlinType::Class {
            internal: "kotlin/coroutines/Continuation".to_owned(),
            args: vec![formal.clone()],
            nullable: true,
            shape: KotlinFunctionTypeShape {
                receiver: false,
                context_count: 0,
                suspend: false,
            },
        };
        assert!(authenticate(wasm, internal, &nullable).is_none());
        let mut reified = generic("interceptedIntrinsic", excluded.clone());
        reified.formals[0].reified = true;
        assert!(authenticate(wasm, internal, &reified).is_none());
        let mut concrete = generic("interceptedIntrinsic", excluded);
        concrete.params[0] = continuation(KotlinType::class("kotlin/Int"));
        assert!(authenticate(wasm, internal, &concrete).is_none());
    }

    #[test]
    fn native_gc_unsafe_string_range_equality_requires_the_exact_entry_package_and_shape() {
        let gc_unsafe = |callee: &str| AnnotationApplication {
            identity: type_name("kotlin/native/internal/GCUnsafeCall"),
            arguments: vec![(
                "callee".to_owned(),
                AnnotationArgument::String(callee.to_owned()),
            )],
        };
        let native = Some(TargetIntrinsicPlatform::Native);
        let text = crate::libraries::CallablePlacement::Package(type_name("kotlin/text"));
        let params = ["kotlin/Int", "kotlin/String", "kotlin/Int", "kotlin/Int"];
        let range_equals = |params: &[&str], ret: &str, callee: &str| {
            let mut declaration = declaration("unsafeRangeEquals", params, ret, gc_unsafe(callee));
            declaration.visibility = Visibility::Private;
            declaration.receiver = Some(KotlinType::class("kotlin/String"));
            declaration
        };
        let exact = range_equals(&params, "kotlin/Boolean", "Kotlin_String_unsafeRangeEquals");
        let string = Ty::obj("kotlin/String");
        assert_eq!(
            authenticate(native, text, &exact),
            Some(KlibIntrinsicShape {
                operation: CompilerIntrinsic::StringRangeEquals,
                kind: ExternalCallableKind::Extension,
                receiver: Some(string),
                params: vec![string, Ty::Int, string, Ty::Int, Ty::Int],
                ret: Ty::Boolean,
            })
        );
        assert!(authenticate(None, text, &exact).is_none());
        assert!(authenticate(
            native,
            crate::libraries::CallablePlacement::Package(type_name("spoof")),
            &exact
        )
        .is_none());
        let mut bodyful =
            range_equals(&params, "kotlin/Boolean", "Kotlin_String_unsafeRangeEquals");
        bodyful.is_external = false;
        assert!(authenticate(native, text, &bodyful).is_none());
        let mut top_level =
            range_equals(&params, "kotlin/Boolean", "Kotlin_String_unsafeRangeEquals");
        top_level.receiver = None;
        assert!(authenticate(native, text, &top_level).is_none());
        let mut builder_receiver =
            range_equals(&params, "kotlin/Boolean", "Kotlin_String_unsafeRangeEquals");
        builder_receiver.receiver = Some(KotlinType::class("kotlin/text/StringBuilder"));
        assert!(authenticate(native, text, &builder_receiver).is_none());
        let swapped = range_equals(
            &["kotlin/String", "kotlin/Int", "kotlin/Int", "kotlin/Int"],
            "kotlin/Boolean",
            "Kotlin_String_unsafeRangeEquals",
        );
        assert!(authenticate(native, text, &swapped).is_none());
        let wrong_result = range_equals(&params, "kotlin/Int", "Kotlin_String_unsafeRangeEquals");
        assert!(authenticate(native, text, &wrong_result).is_none());
        let spoofed = range_equals(&params, "kotlin/Boolean", "User_String_unsafeRangeEquals");
        assert!(authenticate(native, text, &spoofed).is_none());
    }

    #[test]
    fn native_gc_unsafe_array_copy_requires_the_exact_entry_package_and_arrays() {
        let gc_unsafe = |callee: &str| AnnotationApplication {
            identity: type_name("kotlin/native/internal/GCUnsafeCall"),
            arguments: vec![(
                "callee".to_owned(),
                AnnotationArgument::String(callee.to_owned()),
            )],
        };
        let native = Some(TargetIntrinsicPlatform::Native);
        let collections =
            crate::libraries::CallablePlacement::Package(type_name("kotlin/collections"));
        let copy = |array: KotlinType, entry: &str| {
            let mut declaration = declaration(
                "arrayCopy",
                &[
                    "kotlin/Int",
                    "kotlin/Int",
                    "kotlin/Int",
                    "kotlin/Int",
                    "kotlin/Int",
                ],
                "kotlin/Unit",
                gc_unsafe(entry),
            );
            declaration.params[0] = array.clone();
            declaration.params[2] = array;
            declaration
        };
        let any_array = KotlinType::Class {
            internal: "kotlin/Array".to_owned(),
            args: vec![KotlinType::Class {
                internal: "kotlin/Any".to_owned(),
                args: Vec::new(),
                nullable: true,
                shape: KotlinFunctionTypeShape::default(),
            }],
            nullable: false,
            shape: KotlinFunctionTypeShape::default(),
        };
        for (entry, declared, array) in [
            (
                "Kotlin_IntArray_copyImpl",
                KotlinType::class("kotlin/IntArray"),
                Ty::array(Ty::Int),
            ),
            (
                "Kotlin_BooleanArray_copyImpl",
                KotlinType::class("kotlin/BooleanArray"),
                Ty::array(Ty::Boolean),
            ),
            (
                "Kotlin_Array_copyImpl",
                any_array.clone(),
                Ty::array(Ty::nullable(Ty::obj("kotlin/Any"))),
            ),
        ] {
            assert_eq!(
                authenticate(native, collections, &copy(declared.clone(), entry)),
                Some(KlibIntrinsicShape {
                    operation: CompilerIntrinsic::ArrayCopy { array },
                    kind: ExternalCallableKind::TopLevel,
                    receiver: None,
                    params: vec![array, Ty::Int, array, Ty::Int, Ty::Int],
                    ret: Ty::Unit,
                }),
                "{entry}"
            );
            assert!(
                authenticate(None, collections, &copy(declared.clone(), entry)).is_none(),
                "{entry}"
            );
            assert!(
                authenticate(
                    native,
                    crate::libraries::CallablePlacement::Package(type_name("kotlin")),
                    &copy(declared.clone(), entry)
                )
                .is_none(),
                "{entry}"
            );
            let mut mixed = copy(declared.clone(), entry);
            mixed.params[2] = KotlinType::class("kotlin/LongArray");
            assert!(
                authenticate(native, collections, &mixed).is_none(),
                "{entry}"
            );
            let mut counted_long = copy(declared, entry);
            counted_long.params[4] = KotlinType::class("kotlin/Long");
            assert!(
                authenticate(native, collections, &counted_long).is_none(),
                "{entry}"
            );
        }
        // An entry names the one array kind it copies.
        assert!(authenticate(
            native,
            collections,
            &copy(
                KotlinType::class("kotlin/LongArray"),
                "Kotlin_IntArray_copyImpl"
            )
        )
        .is_none());
        assert!(authenticate(
            native,
            collections,
            &copy(
                KotlinType::class("kotlin/IntArray"),
                "Kotlin_Array_copyImpl"
            )
        )
        .is_none());
        assert!(
            authenticate(native, collections, &copy(any_array, "User_Array_copyImpl")).is_none()
        );
    }
}
