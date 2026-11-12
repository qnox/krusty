//! Checked common IR to WebAssembly instructions, one file at a time.
//!
//! The IR is a tree with structured control flow, and so is wasm: a `when` is an `if`/`else`
//! chain, a loop is a `block` around a `loop`, and `break`/`continue` are branches to them by
//! depth. Nothing here builds a control-flow graph.
//!
//! **Values.** A Kotlin value is carried as the wasm value its type maps to ([`carrier`]): the
//! integral types up to `Int` (and `Boolean`, `Char`) as `i32`, `Long` as `i64`, `Float`/`Double`
//! as `f32`/`f64`, and a `String` as a NULLABLE reference to the runtime's string struct, whether
//! or not the Kotlin type admits null. A non-null reference type is not defaultable, so a local of
//! one would have to be proved assigned before every read; carrying every reference as nullable
//! makes every local defaultable, and a non-null Kotlin type is still checked by the frontend.
//! A class, interface, `Any` or type-parameter value is carried as `(ref null eq)`, which both an
//! object and a `String` are; reading one as a narrower reference casts it (`objects`).
//!
//! **Declines.** A construct this generator has not been taught declines the whole file with a
//! diagnostic naming it, as the native generator does. Nothing is emitted on a guess.

use krusty::fx_hash::{HashMap, HashSet};

mod absent_arguments;
pub(super) mod any_members;
mod arrays;
pub(super) mod box_members;
mod boxing;
mod callable_reference_identity;
mod class_literals;
mod dependency_exports;
mod enums;
mod exceptions;
mod function_values;
mod generated_members;
mod intrinsic_operations;
mod objects;
mod scalar_instructions;
mod scalar_value_classes;
mod shared_cells;
mod string_conversion;
mod type_of;

#[cfg(test)]
use tests::lower_alone;

pub(super) use scalar_instructions::is_scalar_operation;
pub(super) use shared_cells::parameter_carrier;

pub(super) fn is_direct_operation(operation: krusty::backend::BackendCompilerIntrinsic) -> bool {
    is_scalar_operation(operation)
        || IrBinOp::of_primitive_operator(operation).is_some()
        || operation == krusty::backend::BackendCompilerIntrinsic::ThrowableMessage
        || operation == krusty::backend::BackendCompilerIntrinsic::ThrowableCause
        || operation == krusty::backend::BackendCompilerIntrinsic::ArrayGet
        || operation == krusty::backend::BackendCompilerIntrinsic::ArraySet
        || operation == krusty::backend::BackendCompilerIntrinsic::ArraySize
        || operation == krusty::backend::BackendCompilerIntrinsic::PrimitiveHashCode
        || matches!(
            operation,
            krusty::backend::BackendCompilerIntrinsic::ClassName(_)
        )
}

use krusty::backend::class_tables::module_interfaces::ModuleInterfaces;
use krusty::backend::class_tables::module_slots::ModuleSlotPlan;
use krusty::backend::class_tables::AnyMember;
use krusty::backend::{CheckedBackendCallables, DependencyExportSignatures};
use krusty::ir::{Callee, ClassId, FunId, IrBinOp, IrConst, IrExpr, IrFile, IrIntrinsic, IrTypeOp};
use krusty::types::{EqualityMode, Ty, TyKind};

use super::super::encode::{BlockType, Code, Function, HeapType, Module, ValType};
use super::super::numeric_conversions;
use super::super::objects::REFERENCE;
use super::super::runtime::*;
use super::any_member_reach::FileReach;
use super::callable_registry::{CallableRegistry, FileCallTargets};
use super::classes::{FileClasses, RealizedProperties};
use super::laid_out_classes::LaidOutClasses;
use super::property_registry::PropertyRegistry;

pub(super) type Unsupported = String;

/// The wasm functions one file defines, by the file's own function ids.
pub(super) struct LoweredFile {
    /// The Wasm function index of each emitted function, by its `FunId`; an inline-only template
    /// has no physical function.
    pub(super) functions: Vec<Option<u32>>,
    /// The function running this file's top-level property initializers in declaration order,
    /// when it has any.
    pub(super) initializer: Option<u32>,
    /// The file's calls through `kotlin.Any`'s slots that can reach other files' objects, and its
    /// own slots with no body.
    pub(super) reach: FileReach,
}

/// One file's checked IR and the facts frozen beside it.
pub(super) struct FileInput<'a> {
    pub(super) array_range_checks: bool,
    pub(super) classifiers: &'a dyn krusty::backend::BackendClassifierSource,
    pub(super) ir: &'a IrFile,
    pub(super) properties: &'a RealizedProperties,
    pub(super) callables: &'a CheckedBackendCallables,
    pub(super) dependency_exports: &'a DependencyExportSignatures,
    /// The source module the file belongs to, which numbers its interfaces' members.
    pub(super) module_name: &'a str,
    pub(super) plan: &'a ModuleSlotPlan,
}

/// The program a file is lowered into.
pub(super) struct ModuleState<'a> {
    pub(super) module: &'a mut Module,
    pub(super) runtime: &'a mut Runtime,
    pub(super) callables: &'a mut CallableRegistry,
    pub(super) properties: &'a mut PropertyRegistry,
    /// The classes earlier files of the source module laid out; this file's join them.
    pub(super) classes: &'a mut LaidOutClasses,
}

/// Lower every function and class of `file` into the program.
pub(super) fn lower_file(
    file: FileInput<'_>,
    state: ModuleState<'_>,
) -> Result<LoweredFile, Unsupported> {
    let FileInput {
        array_range_checks,
        classifiers,
        ir,
        properties,
        callables,
        dependency_exports,
        module_name,
        plan,
    } = file;
    let ModuleState {
        module,
        runtime,
        callables: callable_registry,
        properties: property_registry,
        classes: laid_out_classes,
    } = state;
    for property in &ir.statics {
        if property.owner.is_some() {
            return Err("a static property of a class".to_string());
        }
        if property.is_lateinit {
            return Err("a `lateinit` top-level property".to_string());
        }
    }
    let globals = property_registry.prepare_file(ir, |ty| {
        let Some(value) = carrier(ty)? else {
            return Err("a `Unit` top-level property".to_string());
        };
        Ok(module.global(value))
    })?;
    let member_property_definitions = property_registry.prepare_member_properties(ir, module)?;
    let mut signatures = Vec::with_capacity(ir.functions.len());
    for (id, function) in ir.functions.iter().enumerate() {
        if ir.inline_only_fns.contains(&(id as FunId)) {
            signatures.push(None);
            continue;
        }
        if function.is_static && function.dispatch_receiver.is_some() {
            return Err(format!("the static member function `{}`", function.name));
        }
        if function.dispatch_receiver.is_some() {
            signatures.push(Some(super::classes::method_type(module, ir, id as FunId)?));
            continue;
        }
        if function.body.is_none() {
            return Err(format!("the bodiless function `{}`", function.name));
        }
        let params = function
            .params
            .iter()
            .enumerate()
            .map(|(parameter, _)| {
                parameter_carrier(module, ir, id as FunId, parameter as u32)?
                    .ok_or_else(|| "a `Unit` parameter".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let results = carrier(function.ret)?.into_iter().collect::<Vec<_>>();
        signatures.push(Some(module.func_type(params, results)));
    }
    let indices = callable_registry.declare_file(ir, module)?;
    let reference_file =
        runtime
            .callable_references
            .begin_file(module_name, ir, dependency_exports)?;
    let call_targets = callable_registry.file_calls(ir, callables, module)?;
    let inert_singleton_bindings = krusty::backend::inert_staged_singleton_bindings(
        ir,
        |expression| {
            callables
                .singleton_dispatch(expression)
                .map(|dispatch| dispatch.classifier)
        },
        |target, classifier| {
            call_targets
                .singleton_member_receiver(target)
                .is_some_and(|effect| {
                    effect.classifier() == classifier && !effect.evaluate_source()
                })
                || call_targets
                    .singleton_extension_unread_receiver(target)
                    .is_some_and(|effect| {
                        effect.semantic_type() == Ty::obj_name(classifier)
                            && !effect.evaluate_source()
                    })
        },
    );
    let classes = if ir.classes.is_empty() {
        None
    } else {
        let mut classes = super::classes::declare(
            ir,
            module,
            runtime,
            &indices,
            properties,
            super::classes::BeyondFile {
                classifiers,
                plan,
                laid_out: laid_out_classes,
            },
        )?;
        super::interface_members::record(
            &super::interface_members::Recording {
                ir,
                module_name,
                numbers: plan.interfaces(),
                functions: &indices,
                callables,
                exports: dependency_exports,
            },
            &mut classes,
            &mut super::interface_members::Destination {
                module: &mut *module,
                objects: &mut runtime.objects,
                callables: &mut *callable_registry,
            },
        )?;
        Some(classes)
    };
    let reach = std::cell::RefCell::new(FileReach::default());
    let context = FileContext {
        array_range_checks,
        classifiers,
        module_name,
        reference_file,
        numbers: plan.interfaces(),
        ir,
        functions: &indices,
        globals: &globals,
        classes: classes.as_ref(),
        calls: &call_targets,
        callables,
        inert_singleton_bindings: &inert_singleton_bindings,
        properties: property_registry,
        reach: &reach,
    };
    for (id, function) in ir.functions.iter().enumerate() {
        let (Some(index), Some(signature)) = (indices[id], signatures[id]) else {
            if !ir.inline_only_fns.contains(&(id as FunId)) {
                return Err("an emitted function without a Wasm declaration".to_string());
            }
            continue;
        };
        let Some(root) = function.body else {
            // An abstract member: its slot holds the trap, and nothing calls it directly.
            module.define(
                index,
                Function {
                    ty: signature,
                    locals: Vec::new(),
                    code: {
                        let mut code = Code::default();
                        code.unreachable();
                        code
                    },
                },
            );
            continue;
        };
        let mut parameters = Vec::new();
        if let Some(owner) = function.dispatch_receiver {
            parameters.push(Ty::obj_name(owner));
        }
        parameters.extend(function.params.iter().copied());
        let mut body = Body::new(
            &context,
            module,
            runtime,
            &parameters,
            ir.declared_value_slot_types(root),
            function.ret,
        );
        body.scope = krusty::ir::IrEnclosure::Function(id as FunId);
        body.shared_cell_parameters(id as FunId)?;
        match body.function_body(root, function.ret) {
            Ok(()) => {
                let Body { code, locals, .. } = body;
                module.define(
                    index,
                    Function {
                        ty: signature,
                        locals,
                        code,
                    },
                );
            }
            Err(_reason)
                if ir.signatures.get(&(id as FunId)).is_some_and(|signature| {
                    signature
                        .declared_type_parameters()
                        .any(|parameter| parameter.reified)
                }) =>
            {
                // Match Native's boundary: keep a reified declaration that lowers normally, but
                // replace only an unlowerable standalone template body by a loud trap. Checked
                // call sites must have specialized the body before either backend sees it.
                module.define(
                    index,
                    Function {
                        ty: signature,
                        locals: Vec::new(),
                        code: {
                            let mut code = Code::default();
                            code.unreachable();
                            code
                        },
                    },
                );
            }
            Err(reason) => {
                krusty::trace_compiler!(
                    "lower",
                    "Wasm function {id} {:?} declined: {reason}",
                    function.name
                );
                return Err(reason);
            }
        }
    }
    if let Some(classes) = &classes {
        objects::define_class_functions(&context, classes, module, runtime)?;
        objects::define_module_property_functions(
            &context,
            classes,
            &member_property_definitions,
            module,
            runtime,
        )?;
        for (name, record) in classes.records(ir, &indices, &signatures) {
            laid_out_classes.insert(name, record);
        }
    } else if !member_property_definitions.is_empty() {
        return Err("a member property definition without its class layout".to_string());
    }
    let initializer = if ir.statics.iter().any(|property| property.init.is_some()) {
        let index = module.declare();
        let ty = module.func_type(Vec::new(), Vec::new());
        let mut body = Body::new(
            &context,
            module,
            runtime,
            &[],
            ir.statics
                .iter()
                .filter_map(|property| property.init)
                .flat_map(|init| ir.declared_value_slot_types(init))
                .collect(),
            Ty::Unit,
        );
        for (property, global) in ir.statics.iter().zip(&globals) {
            if let Some(init) = property.init {
                body.value(init, property.ty)?;
                body.code.global_set(*global);
            }
        }
        let Body { code, locals, .. } = body;
        module.define(index, Function { ty, locals, code });
        Some(index)
    } else {
        None
    };
    callable_registry.publish(ir, dependency_exports, &indices)?;
    let mut reach = reach.into_inner();
    if let Some(classes) = &classes {
        for class in 0..ir.classes.len() {
            for member in AnyMember::ALL {
                if !classes.realizes(class as ClassId, member) {
                    reach.bodiless_class(member);
                }
            }
        }
    }
    if objects::holds_unprinted_values(ir) {
        reach.bodiless_value(AnyMember::ToString);
    }
    class_literals::name_type_infos(module, runtime, classifiers);
    Ok(LoweredFile {
        functions: indices,
        initializer,
        reach,
    })
}

/// What every body of one file reads: the file, and where its functions, properties and classes
/// went in the module.
struct FileContext<'a> {
    array_range_checks: bool,
    classifiers: &'a dyn krusty::backend::BackendClassifierSource,
    module_name: &'a str,
    /// Backend namespace of this file's local callable declarations.
    reference_file: u32,
    /// The module-wide numbers of the module's interface members.
    numbers: &'a ModuleInterfaces,
    ir: &'a IrFile,
    /// The Wasm function of each emitted function, by `FunId`.
    functions: &'a [Option<u32>],
    /// The global holding each of the file's top-level properties.
    globals: &'a [u32],
    classes: Option<&'a FileClasses>,
    calls: &'a FileCallTargets,
    /// Provider facts frozen for exactly the dependency identities this file calls.
    callables: &'a CheckedBackendCallables,
    /// Staged bound singleton accesses proven inert by the exact checked call and export effect.
    inert_singleton_bindings: &'a HashSet<krusty::ir::ExprId>,
    /// Top-level property storage reserved or published by files of this source module.
    properties: &'a PropertyRegistry,
    /// What the file's bodies record for the module about calls through `kotlin.Any`'s slots.
    reach: &'a std::cell::RefCell<FileReach>,
}

/// The wasm value a Kotlin type is carried as; `None` for a type with no value (`Unit`,
/// `Nothing`).
pub(super) fn carrier(ty: Ty) -> Result<Option<ValType>, Unsupported> {
    Ok(Some(match ty.kind() {
        TyKind::Unit | TyKind::Nothing => return Ok(None),
        TyKind::BOOLEAN
        | TyKind::BYTE
        | TyKind::SHORT
        | TyKind::CHAR
        | TyKind::INT
        | TyKind::UBYTE
        | TyKind::USHORT
        | TyKind::UINT => ValType::I32,
        TyKind::LONG | TyKind::ULONG => ValType::I64,
        TyKind::FLOAT => ValType::F32,
        TyKind::DOUBLE => ValType::F64,
        TyKind::STRING | TyKind::Null => STRING,
        TyKind::Nullable(Ty::String) => STRING,
        // Nullable scalars keep their semantic type until this representation boundary. They
        // use the same boxes as erased values; nullable Unit can hold its singleton or null.
        TyKind::Nullable(Ty::Unit | Ty::Nothing) => REFERENCE,
        TyKind::Nullable(inner)
            if matches!(
                carrier(inner),
                Ok(Some(
                    ValType::I32 | ValType::I64 | ValType::F32 | ValType::F64
                ))
            ) =>
        {
            REFERENCE
        }
        // Every object, interface and `Any` value, and a type parameter's: a reference the
        // frontend checked, cast to its class where one is read (see `super::super::objects`).
        TyKind::Obj(..) | TyKind::TyParam(..) => REFERENCE,
        TyKind::Intersection(..) if is_reference_intersection(ty) => REFERENCE,
        TyKind::DefinitelyNotNull(inner) if inner.is_ty_param() => REFERENCE,
        // A function value is an object (see `super::super::function_values`); a suspend one is
        // invoked with its caller's continuation as one more argument.
        TyKind::Fun(_) => REFERENCE,
        TyKind::Nullable(inner) if matches!(inner.kind(), TyKind::Fun(_)) => REFERENCE,
        TyKind::Nullable(inner)
            if matches!(inner.kind(), TyKind::Obj(..) | TyKind::TyParam(..))
                || is_reference_intersection(inner) =>
        {
            REFERENCE
        }
        _ => return Err(format!("the type `{}`", describe(ty))),
    }))
}

/// Whether `ty` is an intersection whose every component is carried as a reference: its value is
/// each of those types at once, so it is carried as one too.
fn is_reference_intersection(ty: Ty) -> bool {
    match ty.kind() {
        TyKind::Intersection(parts) => parts
            .iter()
            .all(|&part| matches!(carrier(part), Ok(Some(REFERENCE)))),
        _ => false,
    }
}

/// Carrier admitted to the ordinary signed numeric instruction table. Unsigned values are carried
/// only so a checked [`IrIntrinsic::UnsignedCompare`] can consume them explicitly.
pub(super) fn ordinary_numeric_carrier(ty: Ty) -> Result<Option<ValType>, Unsupported> {
    if ty.is_unsigned() {
        return Err(format!("an ordinary operator on `{}`", describe(ty)));
    }
    carrier(ty)
}

/// The string reference type. The runtime declares `$String` as type 1, after `$chars`; see
/// [`Runtime::start`].
pub(super) const STRING: ValType = ValType::Ref {
    nullable: true,
    heap: super::super::encode::HeapType::Concrete(1),
};

fn describe(ty: Ty) -> String {
    match ty.kind() {
        // Boundary conversion for a diagnostic.
        TyKind::Obj(name, _) => name.render().replace('/', "."),
        TyKind::Nullable(inner) => format!("{}?", describe(inner)),
        _ => {
            let other = ty;
            format!("{other:?}")
        }
    }
}

/// The checked type carried into Wasm emission. A generated value block is a transparent
/// sequencing node: when it has no checked type of its own, its final child retains the exact
/// frontend type. This is the same common-IR contract used for physical block representation; it
/// does not derive a type from a callable name or operation shape.
fn expression_type(
    ir: &IrFile,
    declared: &HashMap<u32, Ty>,
    id: u32,
) -> Result<Option<Ty>, Unsupported> {
    let ty = match ir.expr(id) {
        IrExpr::Return(_)
        | IrExpr::Break { .. }
        | IrExpr::Continue { .. }
        | IrExpr::Throw { .. } => return Ok(None),
        IrExpr::Checked(checked_op)
            if matches!(
                &**checked_op,
                krusty::ir::IrCheckedOperation::PropertyWrite { .. }
            ) =>
        {
            Ty::Unit
        }
        IrExpr::Variable { .. }
        | IrExpr::SetValue { .. }
        | IrExpr::SetStatic { .. }
        | IrExpr::SetField { .. }
        | IrExpr::PropertyWrite { .. }
        | IrExpr::While { .. }
        | IrExpr::InlineFrameMarker
        | IrExpr::Block { value: None, .. } => Ty::Unit,
        IrExpr::Block {
            value: Some(value), ..
        } if ir.checked_type(id).is_none() => return expression_type(ir, declared, *value),
        IrExpr::GetValue(slot) => match ir.logical_type(id) {
            Some(ty) => ty,
            None => *declared
                .get(slot)
                .ok_or_else(|| format!("a read of value {slot} before its declaration"))?,
        },
        // A member access common lowering produced after checking has its declaration's type,
        // which is what the instruction leaves on the stack; a use at a narrower type casts.
        IrExpr::EnclosingInstance { inner, outer, .. } if ir.checked_type(id).is_none() => {
            let (class, field) = super::classes::enclosing_field(ir, *inner, *outer)
                .ok_or_else(|| "an enclosing instance without authenticated storage".to_string())?;
            ir.classes[class as usize].fields[field as usize].ty
        }
        IrExpr::GetField { class, index, .. } if ir.checked_type(id).is_none() => {
            ir.classes[*class as usize].fields[*index as usize].ty
        }
        IrExpr::MethodCall { class, index, .. } if ir.checked_type(id).is_none() => {
            let function = ir.classes[*class as usize].methods[*index as usize];
            ir.functions[function as usize].ret
        }
        IrExpr::PropertyRead { ty, .. } if ir.checked_type(id).is_none() => *ty,
        // A top-level property read has its storage's declared type.
        IrExpr::GetStatic(index) if ir.checked_type(id).is_none() => ir.statics[*index as usize].ty,
        // A shared cell's element is read, and a write evaluates to what it stored, at the
        // element's declared type.
        IrExpr::RefGet { elem, .. } | IrExpr::RefSet { elem, .. }
            if ir.checked_type(id).is_none() =>
        {
            *elem
        }
        // Common lowering leaves the inner call at its declaration's result, and records the
        // selected result on a separate coercion. Consume that exact provenance, not a type
        // reconstructed from the callee, operands or an enclosing use.
        IrExpr::Call { .. }
            if ir.checked_type(id).is_none()
                && let Some(&result) = ir.call_declared_ret.get(&id) =>
        {
            result
        }
        // A function-value invocation common lowering synthesized (such as the body of a
        // SAM-converted callable reference) yields its recorded semantic result.
        IrExpr::InvokeFunction { ret, .. } if ir.checked_type(id).is_none() => *ret,
        // A SAM-converted lambda may have no checked type to ask about; it is an instance of its
        // interface.
        IrExpr::Lambda {
            sam: Some(target), ..
        } if ir.checked_type(id).is_none() => Ty::obj_name(target.classifier),
        node => ir
            .checked_type(id)
            .ok_or_else(|| format!("{} without a checked type", describe_node(node)))?,
    };
    Ok((ty != Ty::Nothing).then_some(ty))
}

/// The numeric carriers, ordered so that Kotlin's mixed arithmetic widens to the greater one:
/// `Int + Long` is a `Long`, `Long + Float` a `Float`, `Float + Double` a `Double`.
fn rank(value: ValType) -> Option<u8> {
    match value {
        ValType::I32 => Some(0),
        ValType::I64 => Some(1),
        ValType::F32 => Some(2),
        ValType::F64 => Some(3),
        ValType::Ref { .. } => None,
    }
}

/// The Kotlin type a numeric carrier stands for when arithmetic widens to it.
fn widened(value: ValType) -> Ty {
    match value {
        ValType::I32 => Ty::Int,
        ValType::I64 => Ty::Long,
        ValType::F32 => Ty::Float,
        _ => Ty::Double,
    }
}

/// A loop the body is inside: the absolute depths `break` and `continue` branch to.
struct Loop {
    label: Option<String>,
    exit: u32,
    next: u32,
}

struct Body<'a> {
    array_range_checks: bool,
    classifiers: &'a dyn krusty::backend::BackendClassifierSource,
    module_name: &'a str,
    reference_file: u32,
    numbers: &'a ModuleInterfaces,
    ir: &'a IrFile,
    module: &'a mut Module,
    runtime: &'a mut Runtime,
    functions: &'a [Option<u32>],
    /// The global holding each of the file's top-level properties.
    globals: &'a [u32],
    classes: Option<&'a FileClasses>,
    /// Exact source/dependency call targets registered before this body is emitted.
    calls: &'a FileCallTargets,
    /// Provider facts frozen for exactly the dependency identities this file calls.
    callables: &'a CheckedBackendCallables,
    inert_singleton_bindings: &'a HashSet<krusty::ir::ExprId>,
    properties: &'a PropertyRegistry,
    /// What this file's bodies record for the module about calls through `kotlin.Any`'s slots.
    reach: &'a std::cell::RefCell<FileReach>,
    code: Code,
    /// Locals beyond the parameters, in index order.
    locals: Vec<ValType>,
    next_local: u32,
    /// IR value slot → (wasm local, Kotlin type).
    values: HashMap<u32, (u32, Ty)>,
    /// Slots of `Unit`-typed variables, which have no local.
    unit_values: HashSet<u32>,
    /// Slots whose local holds a shared cell rather than the value: slot → the cell's struct type.
    cells: HashMap<u32, u32>,
    /// The declared type of every variable in the body, read before the code that declares it
    /// is emitted: a block's type is its value's, and that value can be a variable the block
    /// itself declares (`n++` is `{ val old = n; n = old + 1; old }`).
    declared: HashMap<u32, Ty>,
    result: Ty,
    /// How many structured instructions enclose the current position.
    depth: u32,
    loops: Vec<Loop>,
    /// The `finally` regions whose protected code encloses the current position, innermost last.
    cleanups: Vec<exceptions::CleanupFrame>,
    /// The executable scope whose code this body emits: the function, the class's constructor or
    /// singleton, or the file's static initializer. Only the template ownership of a reified
    /// type operation reads it.
    scope: krusty::ir::IrEnclosure,
    /// `kotlin.Enum`'s `name` and `ordinal` for the enum constant whose construction is being
    /// lowered, taken by that construction's allocation.
    enum_base: Option<(Box<str>, u32)>,
}

impl<'a> Body<'a> {
    /// A body whose values `0..parameters.len()` are its parameters, in its first locals.
    fn new(
        context: &FileContext<'a>,
        module: &'a mut Module,
        runtime: &'a mut Runtime,
        parameters: &[Ty],
        mut declared: HashMap<u32, Ty>,
        result: Ty,
    ) -> Self {
        // A dispatch receiver is the first entry in `parameters`, hence slot zero; ordinary
        // parameters follow it. They have no `Variable` node, so add their semantic declarations
        // to the same inventory explicitly.
        declared.extend(
            parameters
                .iter()
                .enumerate()
                .map(|(slot, ty)| (slot as u32, *ty)),
        );
        Self {
            module_name: context.module_name,
            reference_file: context.reference_file,
            array_range_checks: context.array_range_checks,
            classifiers: context.classifiers,
            numbers: context.numbers,
            ir: context.ir,
            module,
            runtime,
            functions: context.functions,
            globals: context.globals,
            classes: context.classes,
            calls: context.calls,
            callables: context.callables,
            inert_singleton_bindings: context.inert_singleton_bindings,
            properties: context.properties,
            reach: context.reach,
            code: Code::default(),
            locals: Vec::new(),
            next_local: parameters.len() as u32,
            values: parameters
                .iter()
                .enumerate()
                .map(|(slot, ty)| (slot as u32, (slot as u32, *ty)))
                .collect(),
            unit_values: HashSet::default(),
            cells: HashMap::default(),
            declared,
            result,
            depth: 0,
            loops: Vec::new(),
            cleanups: Vec::new(),
            scope: krusty::ir::IrEnclosure::File,
            enum_base: None,
        }
    }

    /// Emit a function's body, `root`, returning `ret`.
    fn function_body(&mut self, root: u32, ret: Ty) -> Result<(), Unsupported> {
        // A body whose value completes returns it by leaving it on the stack; one whose every
        // path returns leaves the end unreachable, which `unreachable` states for the validator.
        let falls_through = match self.type_of(root)? {
            Some(ty) => carrier(ty)?.is_some(),
            None => false,
        };
        match carrier(ret)? {
            Some(_) if falls_through => self.value(root, ret),
            Some(_) => {
                self.statement(root)?;
                self.code.unreachable();
                Ok(())
            }
            None => self.statement(root),
        }
    }

    fn open(&mut self) -> u32 {
        self.depth += 1;
        self.depth - 1
    }

    fn close(&mut self) {
        self.depth -= 1;
        self.code.end();
    }

    /// The relative depth of the structured instruction opened at absolute depth `target`.
    fn relative(&self, target: u32) -> u32 {
        self.depth - 1 - target
    }

    fn local(&mut self, ty: ValType) -> u32 {
        self.locals.push(ty);
        self.next_local += 1;
        self.next_local - 1
    }

    /// The checked Kotlin type of an expression, or `None` when it does not complete (`return`,
    /// `break`, a call answering `Nothing`).
    ///
    /// Every value comes with the type the frontend checked it at (`IrFile::logical_types`); the
    /// generator only picks the wasm carrier for it. A node that is a statement by its shape
    /// (a declaration, an assignment, a loop, a block with no value) yields `Unit`, and a jump
    /// yields nothing. A read of a value slot has its declaration's type. Anything else without
    /// a checked type declines rather than have this generator infer one.
    fn type_of(&self, id: u32) -> Result<Option<Ty>, Unsupported> {
        expression_type(self.ir, &self.declared, id)
    }

    /// Kotlin's narrowing numeric conversion of `arg` to `target_ty` (`Long.toInt()`,
    /// `Double.toLong()`, `Double.toFloat()`, `Long.toByte()`): an integral value keeps its low
    /// bits, a floating value rounds towards zero saturating at the result's bounds with NaN as
    /// zero, and a `Double` becomes the nearest `Float`. Widening is the operand coercion above.
    fn numeric_narrowing(&mut self, arg: u32, target_ty: Ty) -> Result<(), Unsupported> {
        let signed = |ty| {
            matches!(
                ty,
                Ty::Byte | Ty::Short | Ty::Char | Ty::Int | Ty::Long | Ty::Float | Ty::Double
            )
        };
        let unsupported = || format!("a coercion to `{}`", describe(target_ty));
        let source_ty = self
            .type_of(arg)?
            .filter(|ty| signed(*ty))
            .ok_or_else(unsupported)?;
        if !signed(target_ty) {
            return Err(unsupported());
        }
        self.expression(arg)?;
        numeric_conversions::convert(&mut self.code, source_ty, target_ty)
    }

    /// Whether a cast of `arg` to `target` checks a reference against a scalar's box: the operand
    /// is carried as a reference and the target is a scalar-carried type or its nullable form.
    /// Every such scalar's box is keyed by its own classifier, so `UInt?` checks against the
    /// `UInt` box, never the `Int` one.
    fn is_primitive_cast(&self, arg: u32, target: Ty) -> bool {
        let scalar =
            |ty: Ty| matches!(carrier(ty), Ok(Some(ty)) if !matches!(ty, ValType::Ref { .. }));
        matches!(self.carrier_of(arg), Ok(Some(ValType::Ref { .. })))
            && match target.kind() {
                TyKind::Nullable(inner) => scalar(inner),
                _ => scalar(target),
            }
    }

    fn carrier_of(&self, id: u32) -> Result<Option<ValType>, Unsupported> {
        match self.type_of(id)? {
            Some(ty) => carrier(ty),
            None => Ok(None),
        }
    }

    /// Evaluate `id` for its effect only.
    fn statement(&mut self, id: u32) -> Result<(), Unsupported> {
        let carried = self.carrier_of(id)?;
        self.expression(id)?;
        if carried.is_some() {
            self.code.drop_();
        }
        Ok(())
    }

    /// Evaluate `id` and convert its value to the carrier of `target`.
    fn value(&mut self, id: u32, target: Ty) -> Result<(), Unsupported> {
        let source_ty = self.type_of(id)?;
        self.expression(id)?;
        // A statement-shaped operand may return/throw before its enclosing value use resumes.
        // Its Unit annotation must stay Unit; only the common completion proof removes the
        // impossible conversion. Live Unit operands still fail below rather than invent a value.
        if source_ty == Some(Ty::Unit) && carrier(target)?.is_some() && self.diverges(id) {
            self.code.unreachable();
            return Ok(());
        }
        let Some(source) = source_ty else {
            // Restore stack polymorphism after structured control only when completion proves
            // divergence: an inlined generic result may retain Nothing but complete normally.
            if self.diverges(id) {
                self.code.unreachable();
            }
            return Ok(());
        };
        if source == Ty::Unit && carrier(target)?.is_some() {
            krusty::trace_compiler!(
                "lower",
                "Wasm value {id} {:?} is Unit but is requested at {target:?}",
                self.ir.expr(id)
            );
        }
        self.coerce_emitted_value(source, target)
    }

    /// Whether evaluating `id` as a value never completes: it ends in a jump or a `throw`, or in an
    /// inline-return frame that no reachable exit leaves.
    fn diverges(&self, id: u32) -> bool {
        self.ir
            .expr_diverges_by(id, &|leaf, _| self.ir.loop_never_exits(leaf))
    }

    /// Convert the value already on the Wasm stack from its emitted declaration type to the type
    /// the checked use selected. Dependency bodies in particular retain a declaration type
    /// parameter while an independently analyzed caller sees its concrete substitution.
    fn coerce_emitted_value(&mut self, source_ty: Ty, target_ty: Ty) -> Result<(), Unsupported> {
        let source = source_ty;
        let target = target_ty;
        let Some(target) = carrier(target)? else {
            if carrier(source)?.is_some() {
                self.code.drop_();
            }
            return Ok(());
        };
        let Some(source) = carrier(source)? else {
            if source == Ty::Unit && target == REFERENCE {
                let unit = self.runtime.unit_object(self.module);
                self.code.global_get(unit);
                return Ok(());
            }
            return Err("a `Unit` value where a value is expected".to_string());
        };
        if source == target {
            // An `Int` stored where a `Byte`, `Short` or `Char` is declared (`b++` on a `Byte`)
            // wraps to that width. `UByte` and `UShort` keep their low bits on the same carrier.
            numeric_conversions::fit(&mut self.code, source_ty, target_ty);
            return Ok(());
        }
        if let (ValType::Ref { .. }, ValType::Ref { heap, .. }) = (source, target) {
            // The frontend checked the value is of the target type: a `String` read as `Any`
            // needs nothing, and one read back from an `Any` (a smart cast, a type parameter's
            // value) is cast to the runtime's string.
            if target != REFERENCE {
                self.code.ref_cast(true, heap);
            }
            return Ok(());
        }
        match (source, target) {
            (ValType::Ref { .. }, _) => return self.unbox(target_ty),
            (_, ValType::Ref { .. }) if target == REFERENCE => return self.box_value(source_ty),
            _ => {}
        }
        match (rank(source), rank(target)) {
            (Some(from), Some(to)) if from < to => {
                numeric_conversions::widen(&mut self.code, source, target);
                Ok(())
            }
            _ => Err(format!(
                "a value carried as {source:?} where {target:?} is expected"
            )),
        }
    }

    /// Emit `id`, leaving its value on the stack when it has one.
    fn expression(&mut self, id: u32) -> Result<(), Unsupported> {
        let node = self.ir.expr(id).clone();
        if let Some(result) = self.box_member_expression(id, &node) {
            return result;
        }
        if let Some(result) = self.emitted_dependency_expression(id, &node) {
            return result;
        }
        match node {
            IrExpr::Const(constant) => self.constant(&constant)?,
            IrExpr::UnitInstance | IrExpr::InlineFrameMarker => {}
            IrExpr::AbsentArgument { ty } => self.absent_argument(ty)?,
            IrExpr::GetValue(slot) if self.cells.contains_key(&slot) => {
                return Err("a shared cell read as a value".to_string());
            }
            IrExpr::GetValue(slot) => {
                if let Some(&(local, declared)) = self.values.get(&slot) {
                    self.code.local_get(local);
                    // A smart cast reads the slot at a narrower type: `Int` out of an `Int?`
                    // local holding a box.
                    if let Some(read) = self.type_of(id)? {
                        if carrier(read)? != carrier(declared)? {
                            self.coerce_emitted_value(declared, read)?;
                        }
                    }
                } else if !self.unit_values.contains(&slot) {
                    return Err(format!("a read of value {slot} before its declaration"));
                }
            }
            IrExpr::Variable {
                index,
                init: Some(init),
                ..
            } if matches!(self.ir.expr(init), IrExpr::RefNew { .. }) => {
                self.shared_cell_variable(index, init)?
            }
            IrExpr::Variable {
                index, ty, init, ..
            } => match carrier(ty)? {
                None => {
                    self.unit_values.insert(index);
                    if let Some(init) = init {
                        self.statement(init)?;
                    }
                }
                Some(value) => {
                    let local = self.local(value);
                    self.values.insert(index, (local, ty));
                    if self.inert_singleton_bindings.contains(&id) {
                        // The nullable reference local's Wasm default is sufficient: every read
                        // is erased at an authenticated dependency call, and the exact singleton
                        // access was proved to have no initialization effect.
                    } else if let Some(init) = init {
                        self.value(init, ty)?;
                        self.code.local_set(local);
                    }
                }
            },
            IrExpr::SetValue { var, .. } if self.cells.contains_key(&var) => {
                return Err("an assignment replacing a shared cell".to_string());
            }
            IrExpr::SetValue { var, value } => match self.values.get(&var).copied() {
                Some((local, ty)) => {
                    self.value(value, ty)?;
                    self.code.local_set(local);
                }
                None if self.unit_values.contains(&var) => self.statement(value)?,
                None => return Err(format!("an assignment to undeclared value {var}")),
            },
            IrExpr::GetStatic(index) => {
                self.code.global_get(self.globals[index as usize]);
            }
            IrExpr::SetStatic { index, value } => {
                let ty = self.ir.statics[index as usize].ty;
                self.value(value, ty)?;
                self.code.global_set(self.globals[index as usize]);
            }
            IrExpr::Block { stmts, value } => {
                for statement in stmts {
                    self.statement(statement)?;
                }
                if let Some(value) = value {
                    self.expression(value)?;
                }
            }
            IrExpr::Return(value) => {
                match (value, carrier(self.result)?) {
                    (Some(value), Some(_)) => self.value(value, self.result)?,
                    (Some(value), None) => self.statement(value)?,
                    (None, Some(_)) => {
                        return Err("a `return` of no value from a non-`Unit` function".into());
                    }
                    (None, None) => {}
                }
                self.control_transfer(exceptions::Transfer::Return, None)?;
            }
            IrExpr::When { branches } => self.when(id, &branches)?,
            IrExpr::While {
                cond,
                body,
                update,
                post_test,
                label,
            } => self.while_(cond, body, update, post_test, label)?,
            IrExpr::Break { label } => {
                let target = self.find_loop(label.as_deref())?.exit;
                self.control_transfer(exceptions::Transfer::Branch(target), None)?;
            }
            IrExpr::Continue { label } => {
                let target = self.find_loop(label.as_deref())?.next;
                self.control_transfer(exceptions::Transfer::Branch(target), None)?;
            }
            IrExpr::PrimitiveBinOp { op, lhs, rhs } => self.binary(id, op, lhs, rhs)?,
            IrExpr::Equality { op, mode, lhs, rhs } => self.equality(op, mode, lhs, rhs)?,
            IrExpr::RefGet { holder, elem } => self.shared_cell_read(holder, elem)?,
            IrExpr::RefSet {
                holder,
                elem,
                value,
            } => self.shared_cell_write(id, holder, elem, value)?,
            IrExpr::PrimitiveNeg { operand, ty } => match carrier(ty)? {
                Some(ValType::I32) => {
                    self.code.i32_const(0);
                    self.value(operand, ty)?;
                    self.code.op(SUB32);
                }
                Some(ValType::I64) => {
                    self.code.i64_const(0);
                    self.value(operand, ty)?;
                    self.code.op(SUB64);
                }
                Some(ValType::F32) => {
                    self.value(operand, ty)?;
                    self.code.op(0x8c);
                }
                Some(ValType::F64) => {
                    self.value(operand, ty)?;
                    self.code.op(0x9a);
                }
                _ => return Err(format!("negation of `{}`", describe(ty))),
            },
            IrExpr::StringConcat(parts) => {
                for (position, part) in parts.iter().enumerate() {
                    self.string_of(*part)?;
                    if position > 0 {
                        self.code.call(self.runtime.string_concat);
                    }
                }
                if parts.is_empty() {
                    self.runtime
                        .string_constant(self.module, &mut self.code, &[]);
                }
            }
            IrExpr::Call {
                callee: ref call_callee,
                dispatch_receiver: None,
                args,
            } if let Callee::Local(function) = &**call_callee => {
                let params = self.ir.functions[*function as usize].params.clone();
                if params.len() != args.len() {
                    return Err("a call whose arguments do not match its parameters".into());
                }
                for (parameter, &arg) in args.iter().enumerate() {
                    self.function_argument(*function, parameter as u32, arg)?;
                }
                let function = self.emitted_function(*function)?;
                self.code.call(function);
            }
            IrExpr::Call {
                callee: ref call_callee,
                dispatch_receiver: None,
                args,
            } if let Callee::Module { target, params, .. }
            | Callee::CrossFile {
                module_target: Some(target),
                params,
                ..
            } = &**call_callee =>
            {
                let function = self
                    .calls
                    .module(*target)
                    .ok_or_else(|| "an unregistered source callable identity".to_string())?;
                self.direct_call(function, params, &args)?;
            }
            IrExpr::Call {
                callee: ref call_callee,
                dispatch_receiver: None,
                args,
            } if let Callee::ModuleDefaultEntry {
                provider, params, ..
            } = &**call_callee =>
            {
                let function = self
                    .calls
                    .module_default(*provider)
                    .ok_or_else(|| "an unregistered source default entry".to_string())?;
                let mut frame = params.clone();
                frame.extend(std::iter::repeat_n(
                    Ty::Int,
                    krusty::backend::default_arguments::mask_words(params.len()),
                ));
                self.direct_call(function, &frame, &args)?;
            }
            // The target stdlib's own `typeOf` body only says it is a compiler intrinsic.
            IrExpr::Call {
                callee: ref call_callee,
                dispatch_receiver: None,
                args,
            } if let Callee::External {
                target,
                substitutions,
                ..
            } = &**call_callee
                && args.is_empty()
                && self.callables.callable(*target).is_some_and(|fact| {
                    fact.compiler_operation()
                        == Some(krusty::backend::BackendCompilerIntrinsic::TypeOf)
                }) =>
            {
                self.type_of_call(*target, substitutions)?;
            }
            IrExpr::Call {
                callee: ref call_callee,
                dispatch_receiver,
                args,
            } if let Callee::External {
                target,
                params,
                defaults,
                extension_receiver_parameter: None,
                ..
            } = &**call_callee
                && defaults.is_empty()
                && params.as_slice() == [Ty::Int]
                && self.callables.callable(*target).is_some_and(|fact| {
                    fact.compiler_operation()
                        == Some(krusty::backend::BackendCompilerIntrinsic::ArrayGet)
                }) =>
            {
                let (Some(receiver), [index]) = (dispatch_receiver, args.as_slice()) else {
                    return Err("an authenticated array read with malformed operands".into());
                };
                self.array_get(receiver, *index)?;
            }
            IrExpr::Call {
                callee: ref call_callee,
                dispatch_receiver,
                args,
            } if let Callee::External {
                target,
                defaults,
                extension_receiver_parameter: None,
                ..
            } = &**call_callee
                && defaults.is_empty()
                && self.callables.callable(*target).is_some_and(|fact| {
                    fact.compiler_operation()
                        == Some(krusty::backend::BackendCompilerIntrinsic::ArraySet)
                }) =>
            {
                let (Some(receiver), [index, value]) = (dispatch_receiver, args.as_slice()) else {
                    return Err("an authenticated array store with malformed operands".into());
                };
                self.array_set(receiver, *index, *value)?;
            }
            IrExpr::Call {
                callee: ref call_callee,
                dispatch_receiver,
                args,
            } if let Callee::External {
                target,
                ret,
                defaults,
                ..
            } = &**call_callee
                && defaults.is_empty()
                && let Some(field) = self.throwable_field(*target) =>
            {
                self.throwable_field_read(field, dispatch_receiver, &args, *ret)?;
            }
            IrExpr::Call {
                dispatch_receiver: Some(receiver),
                args,
                ..
            } if let Some(member) = any_members::call_member(self.ir, self.callables, id) => {
                self.dependency_any_member_call(member, receiver, &args)?;
            }
            IrExpr::Call {
                callee: ref call_callee,
                dispatch_receiver,
                args,
            } if let Callee::External {
                target,
                params,
                ret,
                defaults,
                extension_receiver_parameter: None,
                ..
            } = &**call_callee
                && defaults.is_empty()
                && self.scalar_operation_of(*target).is_some() =>
            {
                self.scalar_operation_call(id, *target, dispatch_receiver, params, *ret, &args)?;
            }
            IrExpr::Call {
                callee: ref call_callee,
                dispatch_receiver,
                args,
            } if let Callee::Intrinsic { operation, .. } = &**call_callee => {
                self.intrinsic(*operation, dispatch_receiver, &args)?
            }
            IrExpr::TypeOp {
                op: op @ (IrTypeOp::Cast | IrTypeOp::CastNonNull),
                arg,
                type_operand,
            } if self.class_of_type(type_operand).is_some()
                || self.interface_of_type(type_operand).is_some() =>
            {
                self.cast(arg, type_operand, op == IrTypeOp::CastNonNull)?
            }
            // `as Int` and `as Int?` on a reference check it against the primitive's box.
            IrExpr::TypeOp {
                op: IrTypeOp::Cast | IrTypeOp::CastNonNull,
                arg,
                type_operand,
            } if self.is_primitive_cast(arg, type_operand) => {
                self.checked_primitive_cast(arg, type_operand)?
            }
            IrExpr::TypeOp {
                op: op @ (IrTypeOp::Cast | IrTypeOp::CastNonNull),
                arg,
                type_operand,
            } if matches!(type_operand.non_null().kind(), TyKind::Fun(_)) => {
                self.function_cast(arg, type_operand, op == IrTypeOp::CastNonNull)?
            }
            IrExpr::TypeOp {
                op: op @ (IrTypeOp::InstanceOf | IrTypeOp::NotInstanceOf | IrTypeOp::SafeCast),
                arg,
                type_operand,
            } => self.type_test(id, op, arg, type_operand)?,
            // A cast to a non-null type is the cast to its nullable form, then a null check: `as
            // String` on an `Any` reads the reference as the runtime's string and traps on `null`,
            // as a cast to a class of the file does.
            IrExpr::TypeOp {
                op: op @ (IrTypeOp::ImplicitCoercion | IrTypeOp::Cast | IrTypeOp::CastNonNull),
                arg,
                type_operand,
            } => {
                let source = self.carrier_of(arg)?;
                // The checker widens a mixed operator's operand to the type the selected operator
                // takes (`Int.plus(Long)` reads its receiver as a `Long`); only the instruction is
                // this generator's. A source `as` never converts a number.
                let widens = |target: ValType| {
                    op == IrTypeOp::ImplicitCoercion
                        && source
                            .and_then(rank)
                            .zip(rank(target))
                            .is_some_and(|(from, to)| from < to)
                };
                match carrier(type_operand)? {
                    // A coercion to `Unit` discards the value.
                    None => self.statement(arg)?,
                    Some(target) if widens(target) => self.value(arg, type_operand)?,
                    Some(target)
                        if source.is_some_and(|source| {
                            source != target
                                && !matches!(
                                    (source, target),
                                    (ValType::Ref { .. }, ValType::Ref { .. })
                                )
                                && !(op == IrTypeOp::ImplicitCoercion
                                    && (rank(source).is_none() || rank(target).is_none()))
                        }) =>
                    {
                        if op != IrTypeOp::ImplicitCoercion {
                            return Err(format!("a coercion to `{}`", describe(type_operand)));
                        }
                        self.numeric_narrowing(arg, type_operand)?;
                    }
                    Some(target) => {
                        self.value(arg, type_operand)?;
                        if op == IrTypeOp::CastNonNull && matches!(target, ValType::Ref { .. }) {
                            self.code.ref_as_non_null();
                        }
                    }
                }
            }
            IrExpr::NotNullAssert { operand, .. } => {
                let operand_carrier = self.carrier_of(operand)?;
                // `!!` on a nullable scalar reads the box back to its carrier. The box is keyed
                // by the scalar's classifier, so a signed or an unsigned value reads through the
                // same `unbox`, which declines a scalar with no box form.
                let scalar_result = matches!(
                    self.carrier_of(id)?,
                    Some(ValType::I32 | ValType::I64 | ValType::F32 | ValType::F64)
                );
                if let (Some(reference @ ValType::Ref { .. }), true) =
                    (operand_carrier, scalar_result)
                {
                    let result_ty = self
                        .type_of(id)?
                        .ok_or("a not-null assertion without a result type")?;
                    self.assert_nonnull_reference(operand, reference)?;
                    self.unbox(result_ty.non_null())?;
                } else if let Some(reference @ ValType::Ref { .. }) = operand_carrier {
                    self.assert_nonnull_reference(operand, reference)?;
                } else {
                    self.expression(operand)?;
                }
            }
            IrExpr::New {
                internal,
                args,
                ctor_params,
                external_target,
                defaults,
                ..
            } => {
                self.new_object(
                    id,
                    internal,
                    &args,
                    ctor_params.as_deref(),
                    external_target,
                    &defaults,
                )?;
            }
            IrExpr::EnumConstant {
                name,
                ordinal,
                construction,
                published: None,
                ..
            } => self.enum_constant(&name, ordinal, construction)?,
            IrExpr::NoSuchEnumConstant { classifier, name } => {
                self.no_such_enum_constant(classifier, name)?
            }
            IrExpr::Throw { operand } => self.throw_value(operand)?,
            IrExpr::Vararg {
                array_type,
                spreads,
                elements,
            } => {
                self.array_literal(array_type, &spreads, &elements)?;
            }
            IrExpr::NewArray { array_type, size } => self.new_array(array_type, size)?,
            IrExpr::Try {
                body,
                catches,
                finally,
                result,
            } => {
                self.try_catch(body, &catches, finally, result)?;
            }
            IrExpr::EnclosingInstance {
                receiver,
                inner,
                outer,
            } => {
                self.enclosing_instance(receiver, inner, outer)?;
            }
            IrExpr::GetField {
                receiver,
                class,
                index,
            } => self.field_read(receiver, class, index)?,
            IrExpr::SetField {
                receiver,
                class,
                index,
                value,
            } => self.field_write(receiver, class, index, value)?,
            IrExpr::MethodCall {
                class,
                index,
                receiver,
                args,
            } => self.method_call(class, index, receiver, &args)?,
            // `Unit` has no carrier: its one value is the absence of one.
            IrExpr::SingletonValue { .. } if self.carrier_of(id)?.is_none() => {}
            IrExpr::SingletonValue { classifier } => self.singleton(classifier)?,
            // The shared enum lowering realized every enum this file declares; a constant of
            // another file's enum is read through the entry that file exports.
            IrExpr::EnumEntry { classifier, name } => {
                let entry = self
                    .calls
                    .dependency_enum_constant(classifier, &name)
                    .ok_or_else(|| {
                        // Boundary conversion for a diagnostic.
                        format!(
                            "an enum another file declares (`{}`)",
                            classifier.render().replace('/', ".")
                        )
                    })?;
                self.code.call(entry);
            }
            IrExpr::PropertyRead {
                receiver: Some(receiver),
                ty,
                ..
            } => {
                self.realized_property_read(id, receiver)?;
                // A property stored as `kotlin.Unit`'s instance and read where the checked use is
                // `Unit` has no carrier there: the stored reference is discarded, as any value
                // converted to a type without a carrier is.
                if carrier(ty)?.is_some() && self.carrier_of(id)?.is_none() {
                    self.code.drop_();
                }
            }
            IrExpr::PropertyWrite {
                receiver: Some(receiver),
                value,
                ..
            } => self.realized_property_write(id, receiver, value)?,
            IrExpr::Checked(ref checked_op)
                if let krusty::ir::IrCheckedOperation::PropertyRead {
                    target,
                    dispatch_receiver,
                    extension_receiver: None,
                    context_arguments,
                    ..
                } = &**checked_op
                    && context_arguments.is_empty() =>
            {
                self.property_read(*target, *dispatch_receiver)?
            }
            IrExpr::Checked(ref checked_op)
                if let krusty::ir::IrCheckedOperation::ExternalPropertyRead { target, .. } =
                    &**checked_op
                    && self.is_array_size_property(*target) =>
            {
                self.checked_array_size(checked_op)?;
            }
            IrExpr::Checked(ref checked_op)
                if let krusty::ir::IrCheckedOperation::ExternalPropertyRead {
                    target,
                    dispatch: krusty::ir::IrPropertyDispatch::Ordinary,
                    receiver: Some(receiver),
                    arguments,
                    parameters,
                    ..
                } = &**checked_op
                    && parameters.is_empty()
                    && arguments.is_empty()
                    && self.is_scalar_value_storage(*target) =>
            {
                self.scalar_value_storage_read(*target, *receiver)?;
            }
            // `name` and `ordinal` are `kotlin.Enum`'s, and an enum constant answers both from
            // the `$Enum` fields its struct leads with.
            IrExpr::Checked(ref checked_op)
                if let krusty::ir::IrCheckedOperation::ExternalPropertyRead {
                    target,
                    receiver: Some(receiver),
                    ..
                } = &**checked_op
                    && let Some(member) =
                        krusty::backend::enums::base_member(self.callables, *target) =>
            {
                self.enum_member(member, *receiver)?
            }
            IrExpr::Checked(ref checked_op)
                if let krusty::ir::IrCheckedOperation::ExternalPropertyRead {
                    target,
                    dispatch: krusty::ir::IrPropertyDispatch::Ordinary,
                    receiver,
                    arguments,
                    parameters,
                    result,
                    source_receiver: Some(_),
                } = &**checked_op
                    && parameters.is_empty()
                    && let Some(field) = self.throwable_field_property(*target) =>
            {
                self.throwable_field_read(field, *receiver, arguments, *result)?;
            }
            IrExpr::Checked(ref checked_op)
                if let krusty::ir::IrCheckedOperation::ExternalPropertyRead {
                    target,
                    dispatch: krusty::ir::IrPropertyDispatch::Ordinary,
                    receiver: Some(receiver),
                    arguments,
                    parameters,
                    ..
                } = &**checked_op
                    && parameters.is_empty()
                    && arguments.is_empty()
                    && let Some(kind) = self.class_name_property(*target) =>
            {
                self.class_name(kind, *receiver)?;
            }
            IrExpr::Checked(ref checked_op)
                if let krusty::ir::IrCheckedOperation::ExternalPropertyRead {
                    target,
                    dispatch: krusty::ir::IrPropertyDispatch::Ordinary,
                    receiver: Some(receiver),
                    arguments,
                    parameters,
                    ..
                } = &**checked_op
                    && parameters.is_empty()
                    && arguments.is_empty()
                    && self.is_string_length_property(*target) =>
            {
                self.intrinsic(IrIntrinsic::StringLength, Some(*receiver), &[])?;
            }
            IrExpr::Checked(ref checked_op)
                if let krusty::ir::IrCheckedOperation::PropertyWrite {
                    target,
                    dispatch_receiver,
                    extension_receiver: None,
                    context_arguments,
                    value,
                    ..
                } = &**checked_op
                    && context_arguments.is_empty() =>
            {
                self.property_write(*target, *dispatch_receiver, *value)?
            }
            IrExpr::Call {
                callee: ref call_callee,
                dispatch_receiver: Some(receiver),
                args,
            } if let Callee::Local(function) = &**call_callee => {
                self.direct_member_call(*function, receiver, &args)?
            }
            IrExpr::Call {
                callee: ref call_callee,
                dispatch_receiver: Some(receiver),
                args,
            } if matches!(
                &**call_callee,
                Callee::Virtual { .. } | Callee::Super { .. }
            ) =>
            {
                self.member_call(call_callee, receiver, &args)?
            }
            IrExpr::BottomValue {
                producer,
                completion,
            } => {
                self.expression(producer)?;
                if completion.diverges_when_discarded() {
                    self.code.unreachable();
                }
            }
            IrExpr::Lambda {
                impl_fn,
                captures,
                sam,
                ..
            } => self.function_value(
                impl_fn,
                &captures,
                function_values::FunctionEquality::Identity,
                sam.as_deref(),
            )?,
            IrExpr::CallableReference(reference) if !reference.declaration_suspend => {
                self.callable_reference(&reference)?
            }
            IrExpr::InvokeFunction {
                func,
                args,
                params,
                ret,
            } => self.invoke_function(func, &args, &params, ret)?,
            IrExpr::KClassLiteral {
                classifier, value, ..
            } => self.class_literal(id, classifier, value)?,
            IrExpr::Call { callee, .. } => {
                return Err(format!("a call to {}", callee_kind(&callee)));
            }
            IrExpr::Checked(operation) => {
                return Err(format!(
                    "the expression `Checked` ({})",
                    checked_operation_name(&operation)
                ));
            }
            other => return Err(describe_node(&other)),
        }
        Ok(())
    }

    fn direct_call(
        &mut self,
        function: u32,
        params: &[Ty],
        args: &[u32],
    ) -> Result<(), Unsupported> {
        if params.len() != args.len() {
            return Err("a call whose arguments do not match its parameters".into());
        }
        for (&arg, &param) in args.iter().zip(params) {
            self.value(arg, param)?;
        }
        self.code.call(function);
        Ok(())
    }

    fn emitted_function(&self, function: FunId) -> Result<u32, Unsupported> {
        let emitted = self
            .functions
            .get(function as usize)
            .copied()
            .ok_or_else(|| "a call with an invalid local function identity".to_string())?;
        emitted.ok_or_else(|| "a call to an inline-only function that was not spliced".to_string())
    }

    fn constant(&mut self, constant: &IrConst) -> Result<(), Unsupported> {
        match constant {
            IrConst::Boolean(value) => self.code.i32_const(i32::from(*value)),
            IrConst::Byte(value) => self.code.i32_const(i32::from(*value)),
            IrConst::Short(value) => self.code.i32_const(i32::from(*value)),
            IrConst::Int(value) => self.code.i32_const(*value),
            IrConst::Char(value) => self.code.i32_const(i32::from(*value)),
            IrConst::Long(value) => self.code.i64_const(*value),
            IrConst::Float(value) => self.code.f32_const(*value),
            IrConst::Double(value) => self.code.f64_const(*value),
            IrConst::String(value) => {
                let units = value.units().collect::<Vec<_>>();
                self.runtime
                    .string_constant(self.module, &mut self.code, &units);
                &mut self.code
            }
            IrConst::Null => self.code.ref_null(NULL),
            IrConst::UByte(value) => self.code.i32_const(i32::from(*value)),
            IrConst::UShort(value) => self.code.i32_const(i32::from(*value)),
            IrConst::UInt(value) => self.code.i32_const(*value as i32),
            IrConst::ULong(value) => self.code.i64_const(*value as i64),
        };
        Ok(())
    }

    fn find_loop(&self, label: Option<&str>) -> Result<&Loop, Unsupported> {
        self.loops
            .iter()
            .rev()
            .find(|candidate| label.is_none() || candidate.label.as_deref() == label)
            .ok_or_else(|| "a `break` or `continue` outside a loop".to_string())
    }

    fn when(&mut self, id: u32, branches: &[(Option<u32>, u32)]) -> Result<(), Unsupported> {
        let result = self.type_of(id)?;
        let carried = match result {
            Some(ty) => carrier(ty)?,
            None => None,
        };
        let block = carried.map_or(BlockType::Empty, BlockType::Value);
        let mut open = 0;
        let mut exhaustive = false;
        for (condition, body) in branches {
            match condition {
                Some(condition) => {
                    self.value(*condition, Ty::Boolean)?;
                    self.code.if_(block);
                    self.open();
                    open += 1;
                    self.branch(*body, result, carried)?;
                    self.code.else_();
                }
                None => {
                    self.branch(*body, result, carried)?;
                    exhaustive = true;
                    break;
                }
            }
        }
        if !exhaustive && carried.is_some() {
            // A `when` used as a value covers every case; the frontend proved the last condition
            // holds whenever the earlier ones do not.
            self.code.unreachable();
        }
        for _ in 0..open {
            self.close();
        }
        Ok(())
    }

    fn branch(
        &mut self,
        body: u32,
        result: Option<Ty>,
        carried: Option<ValType>,
    ) -> Result<(), Unsupported> {
        match (result, carried) {
            (Some(ty), Some(_)) => self.value(body, ty),
            _ => self.statement(body),
        }
    }

    fn while_(
        &mut self,
        cond: u32,
        body: u32,
        update: Option<u32>,
        post_test: bool,
        label: Option<String>,
    ) -> Result<(), Unsupported> {
        self.code.block(BlockType::Empty);
        let exit = self.open();
        self.code.loop_(BlockType::Empty);
        let top = self.open();
        if !post_test {
            self.value(cond, Ty::Boolean)?;
            let depth = self.relative(exit);
            self.code.op(EQZ32).br_if(depth);
        }
        self.code.block(BlockType::Empty);
        let next = self.open();
        self.loops.push(Loop { label, exit, next });
        self.statement(body)?;
        self.loops.pop();
        self.close();
        if let Some(update) = update {
            self.statement(update)?;
        }
        if post_test {
            self.value(cond, Ty::Boolean)?;
            let depth = self.relative(top);
            self.code.br_if(depth);
        } else {
            let depth = self.relative(top);
            self.code.br(depth);
        }
        self.close();
        self.close();
        Ok(())
    }

    fn binary(&mut self, id: u32, op: IrBinOp, lhs: u32, rhs: u32) -> Result<(), Unsupported> {
        let (Some(left), Some(right)) = (self.type_of(lhs)?, self.type_of(rhs)?) else {
            // An operand that does not complete: emit up to it and stop.
            self.expression(lhs)?;
            if self.type_of(lhs)?.is_some() {
                self.expression(rhs)?;
            }
            return Ok(());
        };
        let Some(result) = self.type_of(id)? else {
            return Err(format!("the operator {op:?} answering `Nothing`"));
        };
        // Generated Elvis and safe-call guards compare an actual null operand by identity;
        // invoking user equals(null) here would change whether the guarded value is present.
        let null_guard = matches!(op, IrBinOp::Eq | IrBinOp::Ne)
            && (matches!(self.ir.expr(lhs), IrExpr::Const(IrConst::Null))
                || matches!(self.ir.expr(rhs), IrExpr::Const(IrConst::Null)));
        if (matches!(op, IrBinOp::RefEq | IrBinOp::RefNe) || null_guard)
            && matches!(carrier(left)?, Some(ValType::Ref { .. }))
            && matches!(carrier(right)?, Some(ValType::Ref { .. }))
        {
            self.expression(lhs)?;
            self.expression(rhs)?;
            self.code.ref_eq();
            if matches!(op, IrBinOp::Ne | IrBinOp::RefNe) {
                self.code.op(EQZ32);
            }
            return Ok(());
        }
        let shift = matches!(op, IrBinOp::Shl | IrBinOp::Shr | IrBinOp::Ushr);
        let unsigned = if shift {
            left.is_unsigned() || result.is_unsigned()
        } else {
            left.is_unsigned() || right.is_unsigned()
        };
        // Equality, shifts and the bitwise connectives are sign-agnostic on the carrier (a shift
        // takes its right-shift kind from `unsigned` below). Addition, subtraction and
        // multiplication have the same bits for a two's-complement unsigned value, so they use
        // those instructions on purpose. Division, remainder and the signed comparisons still
        // need an unsigned instruction this lowering does not select yet.
        if unsigned
            && !matches!(
                op,
                IrBinOp::Eq
                    | IrBinOp::Ne
                    | IrBinOp::Add
                    | IrBinOp::Sub
                    | IrBinOp::Mul
                    | IrBinOp::Shl
                    | IrBinOp::Shr
                    | IrBinOp::Ushr
                    | IrBinOp::BitAnd
                    | IrBinOp::BitOr
                    | IrBinOp::BitXor
            )
        {
            return Err(format!("an ordinary operator on `{}`", describe(left)));
        }
        // The operator's checked result fixes the instruction's operand width; a comparison
        // compares at the wider of its operands' carriers.
        let operand = if is_comparison(op) {
            let (Some(l), Some(r)) = (carrier(left)?, carrier(right)?) else {
                return Err("an operator on `Unit`".into());
            };
            match (rank(l), rank(r)) {
                (Some(a), Some(b)) => Some(if a >= b { l } else { r }),
                _ => return Err(format!("the operator {op:?} on references")),
            }
        } else {
            carrier(result)?
        };
        let Some(operand) = operand else {
            return Err("an operator on `Unit`".into());
        };
        let operand_ty = if result == Ty::Boolean && !is_comparison(op) {
            Ty::Boolean
        } else {
            widened(operand)
        };
        self.value(lhs, operand_ty)?;
        if shift {
            self.value(rhs, Ty::Int)?;
            if operand == ValType::I64 {
                self.code.op(EXTEND_I32_S);
            }
        } else {
            self.value(rhs, operand_ty)?;
        }
        // Unsignedness is a checked semantic fact; the I32/I64 carrier itself deliberately does
        // not encode it. Shifts take an ordinary Int count, so the value (left operand) decides.
        let opcode = opcode(op, operand, unsigned)
            .ok_or_else(|| format!("the operator {op:?} on `{}`", describe(left)))?;
        match opcode {
            Opcode::Plain(opcode) => {
                self.code.op(opcode);
            }
            Opcode::Call(Division::I32) => {
                self.code.call(self.runtime.i32_div);
            }
            Opcode::Call(Division::I64) => {
                self.code.call(self.runtime.i64_div);
            }
        }
        // `Char`, `UByte` and `UShort` arithmetic stays inside the value's own width.
        self.mask_narrow_bits(result);
        Ok(())
    }

    /// Keep a narrow `i32` result in its own bits. `UInt` and `ULong` already fill their carrier.
    fn mask_narrow_bits(&mut self, result: Ty) {
        let mask = match result {
            Ty::Char | Ty::UShort => Some(0xffff),
            Ty::UByte => Some(0xff),
            _ => None,
        };
        if let Some(mask) = mask {
            self.code.i32_const(mask).op(AND32);
        }
    }

    fn equality(
        &mut self,
        op: IrBinOp,
        mode: EqualityMode,
        lhs: u32,
        rhs: u32,
    ) -> Result<(), Unsupported> {
        let left = self.carrier_of(lhs)?;
        let right = self.carrier_of(rhs)?;
        let negate = matches!(op, IrBinOp::Ne | IrBinOp::RefNe);
        match (left, right) {
            (Some(STRING), Some(STRING)) => {
                self.expression(lhs)?;
                self.expression(rhs)?;
                if matches!(op, IrBinOp::RefEq | IrBinOp::RefNe) {
                    self.code.ref_eq();
                } else {
                    self.code.call(self.runtime.string_equals);
                }
            }
            (Some(l), Some(r)) if rank(l).is_some() && rank(r).is_some() => {
                let wide = if rank(l) >= rank(r) { l } else { r };
                if matches!(wide, ValType::F32 | ValType::F64) && mode != EqualityMode::Ieee754 {
                    return Err("structural floating-point equality".into());
                }
                let ty = widened(wide);
                self.value(lhs, ty)?;
                self.value(rhs, ty)?;
                let opcode = match wide {
                    ValType::I32 => EQ32,
                    ValType::I64 => EQ64,
                    ValType::F32 => 0x5b,
                    _ => 0x61,
                };
                self.code.op(opcode);
            }
            (Some(ValType::Ref { .. }), Some(ValType::Ref { .. }))
                if matches!(op, IrBinOp::RefEq | IrBinOp::RefNe) =>
            {
                self.expression(lhs)?;
                self.expression(rhs)?;
                self.code.ref_eq();
            }
            (Some(ValType::Ref { .. }), Some(ValType::Ref { .. })) => {
                self.object_equality(lhs, rhs)?
            }
            // `Unit` has one value, so two `Unit` operands are equal, by identity as well.
            (None, None) => {
                self.statement(lhs)?;
                self.statement(rhs)?;
                self.code.i32_const(1);
            }
            // `Unit` against a reference: its instance, compared as any object is.
            (None, Some(ValType::Ref { .. })) | (Some(ValType::Ref { .. }), None)
                if matches!(op, IrBinOp::RefEq | IrBinOp::RefNe) =>
            {
                self.value(lhs, objects::any())?;
                self.value(rhs, objects::any())?;
                self.code.ref_eq();
            }
            (None, Some(ValType::Ref { .. })) | (Some(ValType::Ref { .. }), None) => {
                self.object_equality(lhs, rhs)?
            }
            // `any == 3`: the primitive side is boxed and the left value's `equals` decides, as
            // kotlinc's `Intrinsics.areEqual` does. IEEE-754 equality of a nullable floating
            // value is a different relation and stays unsupported.
            (Some(ValType::Ref { .. }), Some(scalar))
            | (Some(scalar), Some(ValType::Ref { .. }))
                if rank(scalar).is_some()
                    && matches!(op, IrBinOp::Eq | IrBinOp::Ne)
                    && mode != EqualityMode::Ieee754 =>
            {
                self.object_equality(lhs, rhs)?
            }
            _ => {
                return Err("an equality between values of these types".into());
            }
        }
        if negate {
            self.code.op(EQZ32);
        }
        Ok(())
    }
}

fn is_comparison(op: IrBinOp) -> bool {
    matches!(
        op,
        IrBinOp::Lt
            | IrBinOp::Le
            | IrBinOp::Gt
            | IrBinOp::Ge
            | IrBinOp::Eq
            | IrBinOp::Ne
            | IrBinOp::RefEq
            | IrBinOp::RefNe
    )
}

enum Division {
    I32,
    I64,
}

enum Opcode {
    Plain(u8),
    Call(Division),
}

/// The instruction realizing `op` over operands carried as `operand`.
fn opcode(op: IrBinOp, operand: ValType, unsigned: bool) -> Option<Opcode> {
    use IrBinOp::*;
    let plain = match (operand, op) {
        (ValType::I32, Add) => ADD32,
        (ValType::I32, Sub) => SUB32,
        (ValType::I32, Mul) => MUL32,
        (ValType::I32, Div) => return Some(Opcode::Call(Division::I32)),
        (ValType::I32, Rem) => REM_S32,
        (ValType::I32, Lt) => LT_S32,
        (ValType::I32, Le) => LE_S32,
        (ValType::I32, Gt) => GT_S32,
        (ValType::I32, Ge) => GE_S32,
        (ValType::I32, Eq | RefEq) => EQ32,
        (ValType::I32, Ne | RefNe) => NE32,
        (ValType::I32, And | BitAnd) => AND32,
        (ValType::I32, Or | BitOr) => OR32,
        (ValType::I32, BitXor) => XOR32,
        (ValType::I32, Shl) => SHL32,
        (ValType::I32, Shr) if unsigned => SHR_U32,
        (ValType::I32, Shr) => SHR_S32,
        (ValType::I32, Ushr) => SHR_U32,
        (ValType::I64, Add) => ADD64,
        (ValType::I64, Sub) => SUB64,
        (ValType::I64, Mul) => MUL64,
        (ValType::I64, Div) => return Some(Opcode::Call(Division::I64)),
        (ValType::I64, Rem) => REM_S64,
        (ValType::I64, Lt) => LT_S64,
        (ValType::I64, Le) => LE_S64,
        (ValType::I64, Gt) => GT_S64,
        (ValType::I64, Ge) => GE_S64,
        (ValType::I64, Eq | RefEq) => EQ64,
        (ValType::I64, Ne | RefNe) => NE64,
        (ValType::I64, BitAnd) => AND64,
        (ValType::I64, BitOr) => OR64,
        (ValType::I64, BitXor) => XOR64,
        (ValType::I64, Shl) => SHL64,
        (ValType::I64, Shr) if unsigned => SHR_U64,
        (ValType::I64, Shr) => SHR_S64,
        (ValType::I64, Ushr) => SHR_U64,
        (ValType::F32, Add) => 0x92,
        (ValType::F32, Sub) => 0x93,
        (ValType::F32, Mul) => 0x94,
        (ValType::F32, Div) => 0x95,
        (ValType::F32, Eq) => 0x5b,
        (ValType::F32, Ne) => 0x5c,
        (ValType::F32, Lt) => 0x5d,
        (ValType::F32, Gt) => 0x5e,
        (ValType::F32, Le) => 0x5f,
        (ValType::F32, Ge) => 0x60,
        (ValType::F64, Add) => 0xa0,
        (ValType::F64, Sub) => 0xa1,
        (ValType::F64, Mul) => 0xa2,
        (ValType::F64, Div) => 0xa3,
        (ValType::F64, Eq) => 0x61,
        (ValType::F64, Ne) => 0x62,
        (ValType::F64, Lt) => 0x63,
        (ValType::F64, Gt) => 0x64,
        (ValType::F64, Le) => 0x65,
        (ValType::F64, Ge) => 0x66,
        _ => return None,
    };
    Some(Opcode::Plain(plain))
}

fn callee_kind(callee: &Callee) -> String {
    match callee {
        Callee::Local(_) => "a local function with a receiver".to_string(),
        Callee::External { .. } => "a library function".to_string(),
        Callee::Intrinsic { operation, .. } => format!("the intrinsic {operation:?}"),
        Callee::CrossFile { .. } | Callee::Module { .. } => {
            "a function of another file".to_string()
        }
        _ => "a function through this calling convention".to_string(),
    }
}

fn checked_operation_name(operation: &krusty::ir::IrCheckedOperation) -> &'static str {
    use krusty::ir::IrCheckedOperation;
    match operation {
        IrCheckedOperation::Call { .. } => "Call",
        IrCheckedOperation::ConstructorDelegation { .. } => "ConstructorDelegation",
        IrCheckedOperation::PropertyRead { .. } => "PropertyRead",
        IrCheckedOperation::PropertyWrite { .. } => "PropertyWrite",
        IrCheckedOperation::ExternalPropertyRead { .. } => "ExternalPropertyRead",
        IrCheckedOperation::ExternalPropertyWrite { .. } => "ExternalPropertyWrite",
        IrCheckedOperation::LateinitFieldRead { .. } => "LateinitFieldRead",
        IrCheckedOperation::BackingFieldRead { .. } => "BackingFieldRead",
        IrCheckedOperation::BackingFieldWrite { .. } => "BackingFieldWrite",
        IrCheckedOperation::RangeConstruction { .. } => "RangeConstruction",
        IrCheckedOperation::RangeContains { .. } => "RangeContains",
        IrCheckedOperation::ComparableRangeContains { .. } => "ComparableRangeContains",
        IrCheckedOperation::IllegalProgressionStep { .. } => "IllegalProgressionStep",
        IrCheckedOperation::RangeLoop { .. } => "RangeLoop",
        IrCheckedOperation::PropertyReference { .. } => "PropertyReference",
    }
}

/// Name an IR node for a decline: its variant, without its operands.
fn describe_node(node: &IrExpr) -> String {
    let rendered = format!("{node:?}");
    let variant = rendered
        .split(|c: char| !c.is_alphanumeric())
        .next()
        .unwrap_or("expression");
    format!("the expression `{variant}`")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lower one file with no interface or dependency classifier declarations.
    pub(super) fn lower_alone(
        ir: &IrFile,
        properties: &RealizedProperties,
        started: &mut super::super::Started,
    ) -> Result<LoweredFile, Unsupported> {
        let module_facts = krusty::backend::BackendModuleFacts::from_classifiers([], [])
            .expect("empty classifier facts");
        let classifiers = krusty::backend::CheckedBackendClassifiers::new(
            &module_facts,
            &krusty::libraries::EmptySymbolSource,
        );
        lower_file(
            FileInput {
                array_range_checks: super::super::super::array_access::default_range_checks(),
                classifiers: &classifiers,
                ir,
                properties,
                callables: &CheckedBackendCallables::default(),
                dependency_exports: &DependencyExportSignatures::default(),
                module_name: "main",
                plan: &ModuleSlotPlan::empty(),
            },
            ModuleState {
                module: &mut started.module,
                runtime: &mut started.runtime,
                callables: &mut started.callables,
                properties: &mut started.properties,
                classes: &mut started.classes,
            },
        )
    }

    fn lower_unit_operand(
        target: crate::target::WasmTarget,
        diverges: bool,
    ) -> Result<(), Unsupported> {
        let mut ir = IrFile::default();
        let seven = ir.add_expr(IrExpr::Const(IrConst::Int(7)));
        ir.set_logical_type(seven, Ty::Int);
        let statement = if diverges {
            ir.add_expr(IrExpr::Return(Some(seven)))
        } else {
            ir.add_expr(IrExpr::UnitInstance)
        };
        ir.set_logical_type(statement, if diverges { Ty::Nothing } else { Ty::Unit });
        let operand = ir.add_expr(IrExpr::Block {
            stmts: vec![statement],
            value: None,
        });
        ir.set_logical_type(operand, Ty::Unit);
        let returned = ir.add_expr(IrExpr::Return(Some(operand)));
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![returned],
            value: None,
        });
        ir.add_fun(krusty::ir::IrFunction {
            name: "nestedReturn".to_owned(),
            params: Vec::new(),
            ret: Ty::Int,
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        let mut started = super::super::Started::new(target);
        let properties = krusty::backend::local_properties::realize(&mut ir).unwrap();
        lower_alone(&ir, &properties, &mut started).map(|_| ())
    }

    #[test]
    fn only_a_proven_nonreturning_unit_operand_can_satisfy_a_value_use() {
        for target in crate::target::WasmTarget::ALL {
            assert_eq!(lower_unit_operand(target, true), Ok(()));
            assert_eq!(
                lower_unit_operand(target, false),
                Err("a `Unit` value where a value is expected".to_owned())
            );
        }
    }

    #[test]
    fn an_untyped_call_uses_only_its_published_declared_result() {
        let mut ir = IrFile::default();
        let formal = Ty::ty_param("declaration-owned-T", Ty::nullable(Ty::obj("kotlin/Any")));
        let function = ir.add_fun(krusty::ir::IrFunction {
            name: "sample".to_owned(),
            params: Vec::new(),
            ret: formal,
            param_checks: Vec::new(),
            body: None,
            is_static: true,
            dispatch_receiver: None,
        });
        let call = ir.add_expr(IrExpr::Call {
            callee: Box::new(Callee::Local(function)),
            dispatch_receiver: None,
            args: Vec::new(),
        });
        let coercion = ir.add_expr(IrExpr::TypeOp {
            op: krusty::ir::IrTypeOp::ImplicitCoercion,
            arg: call,
            type_operand: Ty::String,
        });
        ir.call_declared_ret.insert(call, formal);
        ir.set_logical_type(coercion, Ty::String);
        assert_eq!(ir.checked_type(call), None);
        assert_eq!(
            expression_type(&ir, &HashMap::default(), call),
            Ok(Some(formal))
        );
        assert_eq!(
            expression_type(&ir, &HashMap::default(), coercion),
            Ok(Some(Ty::String))
        );

        ir.call_declared_ret.remove(&call);
        assert_eq!(
            expression_type(&ir, &HashMap::default(), call),
            Err("the expression `Call` without a checked type".to_owned())
        );
    }

    #[test]
    fn a_checked_call_keeps_its_checked_type_over_declared_provenance() {
        let mut ir = IrFile::default();
        let declared = Ty::nullable(Ty::obj("kotlin/Any"));
        let function = ir.add_fun(krusty::ir::IrFunction {
            name: "sample".to_owned(),
            params: Vec::new(),
            ret: declared,
            param_checks: Vec::new(),
            body: None,
            is_static: true,
            dispatch_receiver: None,
        });
        let call = ir.add_expr(IrExpr::Call {
            callee: Box::new(Callee::Local(function)),
            dispatch_receiver: None,
            args: Vec::new(),
        });
        ir.call_declared_ret.insert(call, declared);
        ir.set_logical_type(call, Ty::String);
        assert_eq!(
            expression_type(&ir, &HashMap::default(), call),
            Ok(Some(Ty::String))
        );
    }

    #[test]
    fn an_untyped_sequencing_block_retains_its_final_child_type() {
        let mut ir = IrFile::default();
        let value = ir.add_expr(IrExpr::GetValue(7));
        let inner = ir.add_expr(IrExpr::Block {
            stmts: Vec::new(),
            value: Some(value),
        });
        let outer = ir.add_expr(IrExpr::Block {
            stmts: Vec::new(),
            value: Some(inner),
        });
        let declared = HashMap::from_iter([(7, Ty::Float)]);

        assert_eq!(expression_type(&ir, &declared, outer), Ok(Some(Ty::Float)));
    }

    #[test]
    fn a_block_checked_at_a_narrower_type_keeps_that_checked_type() {
        let mut ir = IrFile::default();
        let value = ir.add_expr(IrExpr::GetValue(7));
        let block = ir.add_expr(IrExpr::Block {
            stmts: Vec::new(),
            value: Some(value),
        });
        ir.set_logical_type(block, Ty::Byte);
        let declared = HashMap::from_iter([(7, Ty::Int)]);

        assert_eq!(expression_type(&ir, &declared, block), Ok(Some(Ty::Byte)));
    }
}
