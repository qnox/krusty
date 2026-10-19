//! Target-stdlib intrinsic declarations authenticated at the KLIB provider boundary.
//!
//! Annotation spelling is evidence only inside the exact archive compilation input assembly marked
//! as its platform stdlib. The provider validates the complete semantic shape and publishes an
//! exact module-qualified declaration identity. Body lowering consumes that identity; it never
//! searches for an annotation, package, or callable name.

use std::collections::HashMap;

use crate::libraries::{
    CompilerIntrinsic, ExternalCallableKind, KlibDeclarationSignature, KlibDependencyCallable,
    ScalarClass, ScalarComparison, ScalarInstruction,
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
        // operations used while closing a serialized wrapper body.
        if !matches!(
            shape.operation,
            CompilerIntrinsic::ReinterpretBits | CompilerIntrinsic::ScalarInstruction(_)
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

    pub fn len(&self) -> usize {
        self.callables.len()
    }

    pub fn is_empty(&self) -> bool {
        self.callables.is_empty()
    }
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
    let KlibIrType::Simple {
        classifier,
        nullability: KlibIrNullability::NotSpecified,
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
    if declaration.context_count != 0
        || !declaration.context_kinds.is_empty()
        || !declaration.formals.is_empty()
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
    match platform? {
        TargetIntrinsicPlatform::Native => native(placement, declaration),
        TargetIntrinsicPlatform::Wasm => wasm(placement, declaration),
    }
}

fn native(
    placement: crate::libraries::CallablePlacement,
    declaration: &KotlinFunction,
) -> Option<KlibIntrinsicShape> {
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
}
