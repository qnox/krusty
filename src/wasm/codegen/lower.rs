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

use crate::fx_hash::{HashMap, HashSet};

mod default_arguments;
mod dependency_exports;
mod exceptions;
mod intrinsic_operations;
mod objects;
mod scalar_instructions;
mod shared_cells;

#[cfg(test)]
use tests::lower_alone;

pub(super) use scalar_instructions::is_scalar_operation;

pub(super) fn is_direct_operation(operation: crate::backend::BackendCompilerIntrinsic) -> bool {
    is_scalar_operation(operation)
        || operation == crate::backend::BackendCompilerIntrinsic::ThrowableMessage
}

use crate::backend::class_tables::module_interfaces::ModuleInterfaces;
use crate::backend::class_tables::module_slots::ModuleSlotPlan;
use crate::backend::{CheckedBackendCallables, DependencyExportSignatures};
use crate::ir::{
    module_override_callable, Callee, FunId, IrBinOp, IrConst, IrExpr, IrFile, IrIntrinsic,
    IrTypeOp,
};
use crate::types::{EqualityMode, Ty, TyKind};

use super::super::encode::{BlockType, Code, Function, HeapType, Module, ValType};
use super::super::objects::REFERENCE;
use super::super::runtime::*;
use super::callable_registry::{CallableRegistry, FileCallTargets};
use super::classes::{FileClasses, RealizedProperties};
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
}

/// One file's checked IR and the facts frozen beside it.
pub(super) struct FileInput<'a> {
    pub(super) classifiers: &'a dyn crate::backend::BackendClassifierSource,
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
}

/// Lower every function and class of `file` into the program.
pub(super) fn lower_file(
    file: FileInput<'_>,
    state: ModuleState<'_>,
) -> Result<LoweredFile, Unsupported> {
    let FileInput {
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
            .map(|param| carrier(*param)?.ok_or_else(|| "a `Unit` parameter".to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        let results = carrier(function.ret)?.into_iter().collect::<Vec<_>>();
        signatures.push(Some(module.func_type(params, results)));
    }
    let indices = callable_registry.declare_file(ir, module)?;
    let default_indices = callable_registry.declare_file_defaults(ir, module)?;
    let call_targets = callable_registry.file_calls(ir, callables, module)?;
    let inert_singleton_bindings = crate::backend::inert_staged_singleton_bindings(
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
        let mut classes =
            super::classes::declare(ir, module, &mut runtime.objects, &indices, properties, plan)?;
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
    let context = FileContext {
        classifiers,
        module_name,
        numbers: plan.interfaces(),
        ir,
        functions: &indices,
        default_functions: &default_indices,
        globals: &globals,
        classes: classes.as_ref(),
        calls: &call_targets,
        callables,
        inert_singleton_bindings: &inert_singleton_bindings,
        properties: property_registry,
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
                        .type_params
                        .iter()
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
                crate::trace_compiler!(
                    "lower",
                    "Wasm function {id} {:?} declined: {reason}",
                    function.name
                );
                return Err(reason);
            }
        }
    }
    default_arguments::define(&context, module, runtime)?;
    if let Some(classes) = &classes {
        objects::define_class_functions(&context, classes, module, runtime)?;
        objects::define_module_property_functions(
            &context,
            classes,
            &member_property_definitions,
            module,
            runtime,
        )?;
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
    Ok(LoweredFile {
        functions: indices,
        initializer,
    })
}

/// What every body of one file reads: the file, and where its functions, properties and classes
/// went in the module.
struct FileContext<'a> {
    classifiers: &'a dyn crate::backend::BackendClassifierSource,
    module_name: &'a str,
    /// The module-wide numbers of the module's interface members.
    numbers: &'a ModuleInterfaces,
    ir: &'a IrFile,
    /// The Wasm function of each emitted function, by `FunId`.
    functions: &'a [Option<u32>],
    /// Default-argument mask entry for each emitted source function that declares defaults.
    default_functions: &'a [Option<u32>],
    /// The global holding each of the file's top-level properties.
    globals: &'a [u32],
    classes: Option<&'a FileClasses>,
    calls: &'a FileCallTargets,
    /// Provider facts frozen for exactly the dependency identities this file calls.
    callables: &'a CheckedBackendCallables,
    /// Staged bound singleton accesses proven inert by the exact checked call and export effect.
    inert_singleton_bindings: &'a HashSet<crate::ir::ExprId>,
    /// Top-level property storage reserved or published by files of this source module.
    properties: &'a PropertyRegistry,
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
        // Every object, interface and `Any` value, and a type parameter's: a reference the
        // frontend checked, cast to its class where one is read (see `super::super::objects`).
        TyKind::Obj(..) | TyKind::TyParam(..) => REFERENCE,
        TyKind::DefinitelyNotNull(inner) if inner.is_ty_param() => REFERENCE,
        TyKind::Nullable(inner)
            if matches!(inner.kind(), TyKind::Obj(..) | TyKind::TyParam(..)) =>
        {
            REFERENCE
        }
        _ => return Err(format!("the type `{}`", describe(ty))),
    }))
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
                crate::ir::IrCheckedOperation::PropertyWrite { .. }
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
        IrExpr::GetValue(slot) => match ir.logical_types.get(&id) {
            Some(ty) => *ty,
            None => *declared
                .get(slot)
                .ok_or_else(|| format!("a read of value {slot} before its declaration"))?,
        },
        // A member access common lowering produced after checking has its declaration's type,
        // which is what the instruction leaves on the stack; a use at a narrower type casts.
        IrExpr::GetField { class, index, .. } if ir.checked_type(id).is_none() => {
            ir.classes[*class as usize].fields[*index as usize].ty
        }
        IrExpr::MethodCall { class, index, .. } if ir.checked_type(id).is_none() => {
            let function = ir.classes[*class as usize].methods[*index as usize];
            ir.functions[function as usize].ret
        }
        IrExpr::PropertyRead { ty, .. } if ir.checked_type(id).is_none() => *ty,
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

/// Convert the numeric value on top of the stack from `from` to the wider `to`.
fn widen(code: &mut Code, from: ValType, to: ValType) {
    let opcode = match (from, to) {
        (ValType::I32, ValType::I64) => EXTEND_I32_S,
        (ValType::I32, ValType::F32) => 0xb2,
        (ValType::I64, ValType::F32) => 0xb4,
        (ValType::I32, ValType::F64) => 0xb7,
        (ValType::I64, ValType::F64) => 0xb9,
        (ValType::F32, ValType::F64) => 0xbb,
        _ => return,
    };
    code.op(opcode);
}

/// A loop the body is inside: the absolute depths `break` and `continue` branch to.
struct Loop {
    label: Option<String>,
    exit: u32,
    next: u32,
}

struct Body<'a> {
    classifiers: &'a dyn crate::backend::BackendClassifierSource,
    module_name: &'a str,
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
    inert_singleton_bindings: &'a HashSet<crate::ir::ExprId>,
    properties: &'a PropertyRegistry,
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
}

impl<'a> Body<'a> {
    /// A body whose values `0..parameters.len()` are its parameters, in its first locals.
    fn new(
        context: &FileContext<'a>,
        module: &'a mut Module,
        runtime: &'a mut Runtime,
        parameters: &[Ty],
        declared: HashMap<u32, Ty>,
        result: Ty,
    ) -> Self {
        Self::with_physical_parameters(
            context,
            module,
            runtime,
            parameters,
            parameters.len(),
            declared,
            result,
        )
    }

    /// Build a body whose physical Wasm signature has trailing backend parameters that are not
    /// semantic value slots (the default-argument entry's mask words).
    fn with_physical_parameters(
        context: &FileContext<'a>,
        module: &'a mut Module,
        runtime: &'a mut Runtime,
        parameters: &[Ty],
        physical_parameter_count: usize,
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
            code: Code::default(),
            locals: Vec::new(),
            next_local: physical_parameter_count as u32,
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
        let (Some(source), Some(target)) = (carrier(source_ty)?, carrier(target_ty)?) else {
            return Err(unsupported());
        };
        self.expression(arg)?;
        let carried = match (source, target) {
            (ValType::I64, ValType::I32) => {
                self.code.op(0xa7);
                Ty::Int
            }
            (ValType::F32, ValType::I32) => {
                self.code.trunc_sat(0);
                Ty::Int
            }
            (ValType::F64, ValType::I32) => {
                self.code.trunc_sat(2);
                Ty::Int
            }
            (ValType::F32, ValType::I64) => {
                self.code.trunc_sat(4);
                Ty::Long
            }
            (ValType::F64, ValType::I64) => {
                self.code.trunc_sat(6);
                Ty::Long
            }
            (ValType::F64, ValType::F32) => {
                self.code.op(0xb6);
                Ty::Float
            }
            _ => return Err(unsupported()),
        };
        // A `Byte`, `Short` or `Char` result narrows the `Int` it came through.
        self.coerce_emitted_value(carried, target_ty)
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
        if source_ty == Some(Ty::Unit)
            && carrier(target)?.is_some()
            && self.ir.expr_diverges_by(id, &|_, _| false)
        {
            self.code.unreachable();
            return Ok(());
        }
        let Some(source) = source_ty else {
            return Ok(());
        };
        if source == Ty::Unit && carrier(target)?.is_some() {
            crate::trace_compiler!(
                "lower",
                "Wasm value {id} {:?} is Unit but is requested at {target:?}",
                self.ir.expr(id)
            );
        }
        self.coerce_emitted_value(source, target)
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
            return Err("a `Unit` value where a value is expected".to_string());
        };
        if source == target {
            // An `Int` stored where a `Byte`, `Short` or `Char` is declared (`b++` on a `Byte`)
            // wraps to that width: the 32-bit carrier must hold the narrow value's own bits.
            let narrowing = match (source_ty, target_ty) {
                (from, to) if from == to => None,
                (_, Ty::Byte) => Some(0xc0),
                (_, Ty::Short) => Some(0xc1),
                (_, Ty::Char) => Some(0),
                _ => None,
            };
            match narrowing {
                Some(0) => {
                    self.code.i32_const(0xffff).op(AND32);
                }
                Some(extend) => {
                    self.code.op(extend);
                }
                None => {}
            }
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
        match (rank(source), rank(target)) {
            (Some(from), Some(to)) if from < to => {
                widen(&mut self.code, source, target);
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
        if let Some(result) = self.emitted_dependency_expression(id, &node) {
            return result;
        }
        match node {
            IrExpr::Const(constant) => self.constant(&constant)?,
            IrExpr::UnitInstance | IrExpr::InlineFrameMarker => {}
            IrExpr::GetValue(slot) if self.cells.contains_key(&slot) => {
                return Err("a shared cell read as a value".to_string());
            }
            IrExpr::GetValue(slot) => {
                if let Some((local, _)) = self.values.get(&slot) {
                    self.code.local_get(*local);
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
                self.code.return_();
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
                let depth = self.relative(target);
                self.code.br(depth);
            }
            IrExpr::Continue { label } => {
                let target = self.find_loop(label.as_deref())?.next;
                let depth = self.relative(target);
                self.code.br(depth);
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
                for (arg, param) in args.iter().zip(params) {
                    self.value(*arg, param)?;
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
            } if let Callee::ModuleWithDefaults {
                default_provider,
                params,
                defaults,
                dispatch_receiver_ty: None,
                extension_receiver_parameter: None,
                ..
            } = &**call_callee =>
            {
                let provider = module_override_callable(*default_provider).ok_or_else(|| {
                    "a source default entry whose provider is not in the current module".to_string()
                })?;
                let function = self
                    .calls
                    .module_default(provider)
                    .ok_or_else(|| "an unregistered source default entry".to_string())?;
                self.defaulted_call(function, params, defaults, &args)?;
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
                && self.is_throwable_message(*target) =>
            {
                self.throwable_message(dispatch_receiver, &args, *ret)?;
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
                self.scalar_operation_call(*target, dispatch_receiver, params, *ret, &args)?;
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
            IrExpr::TypeOp {
                op: op @ (IrTypeOp::InstanceOf | IrTypeOp::NotInstanceOf | IrTypeOp::SafeCast),
                arg,
                type_operand,
            } => self.type_test(op, arg, type_operand)?,
            IrExpr::TypeOp {
                op: op @ (IrTypeOp::ImplicitCoercion | IrTypeOp::Cast),
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
                        }) =>
                    {
                        if op != IrTypeOp::ImplicitCoercion {
                            return Err(format!("a coercion to `{}`", describe(type_operand)));
                        }
                        self.numeric_narrowing(arg, type_operand)?;
                    }
                    Some(_) => self.value(arg, type_operand)?,
                }
            }
            IrExpr::NotNullAssert { operand, .. } => {
                let operand_carrier = self.carrier_of(operand)?;
                if matches!(operand_carrier, Some(ValType::Ref { .. }))
                    && matches!(
                        self.carrier_of(id)?,
                        Some(ValType::I32 | ValType::I64 | ValType::F32 | ValType::F64)
                    )
                {
                    return Err(
                        "a not-null assertion requiring a scalar reference conversion".into(),
                    );
                }
                if let Some(reference @ ValType::Ref { .. }) = operand_carrier {
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
                    internal,
                    &args,
                    ctor_params.as_deref(),
                    external_target,
                    &defaults,
                )?;
            }
            IrExpr::Throw { operand } => self.throw_value(operand)?,
            IrExpr::Try {
                body,
                catches,
                finally,
                result,
            } => {
                self.try_catch(body, &catches, finally, result)?;
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
            IrExpr::SingletonValue { classifier } => self.singleton(classifier)?,
            IrExpr::PropertyRead {
                receiver: Some(receiver),
                ..
            } => self.realized_property_read(id, receiver)?,
            IrExpr::PropertyWrite {
                receiver: Some(receiver),
                value,
                ..
            } => self.realized_property_write(id, receiver, value)?,
            IrExpr::Checked(ref checked_op)
                if let crate::ir::IrCheckedOperation::PropertyRead {
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
                if let crate::ir::IrCheckedOperation::ExternalPropertyRead {
                    target,
                    dispatch: crate::ir::IrPropertyDispatch::Ordinary,
                    receiver,
                    arguments,
                    parameters,
                    result,
                    source_receiver: Some(_),
                } = &**checked_op
                    && parameters.is_empty()
                    && self.is_throwable_message_property(*target) =>
            {
                self.throwable_message(*receiver, arguments, *result)?;
            }
            IrExpr::Checked(ref checked_op)
                if let crate::ir::IrCheckedOperation::PropertyWrite {
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
            IrExpr::Call { callee, .. } => {
                return Err(format!("a call to {}", callee_kind(&callee)));
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

    /// Push `id` converted to a non-null `String`, as a string template does.
    fn string_of(&mut self, id: u32) -> Result<(), Unsupported> {
        let ty = self.type_of(id)?;
        self.expression(id)?;
        let function = match ty {
            Some(Ty::String | Ty::Null) => self.runtime.string_or_null,
            Some(ty) if ty == Ty::nullable(Ty::String) => self.runtime.string_or_null,
            Some(Ty::Int | Ty::Short | Ty::Byte) => self.runtime.string_of_i32,
            Some(Ty::Long) => self.runtime.string_of_i64,
            Some(Ty::Char) => self.runtime.string_of_char,
            Some(Ty::Boolean) => self.runtime.string_of_boolean,
            Some(ty) if self.class_of_type(ty).is_some() => {
                return self.object_to_string(ty);
            }
            Some(other) => {
                return Err(format!(
                    "a string template part of type `{}`",
                    describe(other)
                ));
            }
            None => return Ok(()),
        };
        self.code.call(function);
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
        if unsigned
            && !matches!(
                op,
                IrBinOp::Eq | IrBinOp::Ne | IrBinOp::Shl | IrBinOp::Shr | IrBinOp::Ushr
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
        // `Char` arithmetic stays a 16-bit code unit.
        if result == Ty::Char {
            self.code.i32_const(0xffff).op(AND32);
        }
        Ok(())
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
        let module_facts = crate::backend::BackendModuleFacts::from_classifiers([], [])
            .expect("empty classifier facts");
        let classifiers = crate::backend::CheckedBackendClassifiers::new(
            &module_facts,
            &crate::libraries::EmptySymbolSource,
        );
        lower_file(
            FileInput {
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
            },
        )
    }

    fn lower_unit_operand(
        target: crate::wasm::WasmTarget,
        diverges: bool,
    ) -> Result<(), Unsupported> {
        let mut ir = IrFile::default();
        let seven = ir.add_expr(IrExpr::Const(IrConst::Int(7)));
        ir.logical_types.insert(seven, Ty::Int);
        let statement = if diverges {
            ir.add_expr(IrExpr::Return(Some(seven)))
        } else {
            ir.add_expr(IrExpr::UnitInstance)
        };
        ir.logical_types
            .insert(statement, if diverges { Ty::Nothing } else { Ty::Unit });
        let operand = ir.add_expr(IrExpr::Block {
            stmts: vec![statement],
            value: None,
        });
        ir.logical_types.insert(operand, Ty::Unit);
        let returned = ir.add_expr(IrExpr::Return(Some(operand)));
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![returned],
            value: None,
        });
        ir.add_fun(crate::ir::IrFunction {
            name: "nestedReturn".to_owned(),
            params: Vec::new(),
            ret: Ty::Int,
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        let mut started = super::super::Started::new(target);
        let properties = crate::backend::local_properties::realize(&mut ir).unwrap();
        lower_alone(&ir, &properties, &mut started).map(|_| ())
    }

    #[test]
    fn only_a_proven_nonreturning_unit_operand_can_satisfy_a_value_use() {
        for target in crate::wasm::WasmTarget::ALL {
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
        let function = ir.add_fun(crate::ir::IrFunction {
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
            op: crate::ir::IrTypeOp::ImplicitCoercion,
            arg: call,
            type_operand: Ty::String,
        });
        ir.call_declared_ret.insert(call, formal);
        ir.logical_types.insert(coercion, Ty::String);
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
        let function = ir.add_fun(crate::ir::IrFunction {
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
        ir.logical_types.insert(call, Ty::String);
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
        ir.logical_types.insert(block, Ty::Byte);
        let declared = HashMap::from_iter([(7, Ty::Int)]);

        assert_eq!(expression_type(&ir, &declared, block), Ok(Some(Ty::Byte)));
    }
}
