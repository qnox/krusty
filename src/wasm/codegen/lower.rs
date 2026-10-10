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
//!
//! **Declines.** A construct this generator has not been taught declines the whole file with a
//! diagnostic naming it, as the native generator does. Nothing is emitted on a guess.

use std::collections::{HashMap, HashSet};

use crate::ir::{Callee, IrBinOp, IrConst, IrExpr, IrFile, IrIntrinsic, IrTypeOp};
use crate::types::{EqualityMode, Ty};

use super::super::encode::{BlockType, Code, Function, Module, ValType};
use super::super::runtime::*;

pub(super) type Unsupported = String;

/// The wasm functions one file defines, by the file's own function ids.
pub(super) struct LoweredFile {
    /// The wasm function index of each of the file's functions, by its `FunId`.
    pub(super) functions: Vec<u32>,
    /// The function running this file's top-level property initializers in declaration order,
    /// when it has any.
    pub(super) initializer: Option<u32>,
}

/// Lower every function of `ir` into `module`.
pub(super) fn lower_file(
    ir: &IrFile,
    module: &mut Module,
    runtime: &mut Runtime,
) -> Result<LoweredFile, Unsupported> {
    if !ir.classes.is_empty() {
        return Err("a class".to_string());
    }
    let mut globals = Vec::with_capacity(ir.statics.len());
    for property in &ir.statics {
        if property.owner.is_some() {
            return Err("a static property of a class".to_string());
        }
        if property.is_lateinit {
            return Err("a `lateinit` top-level property".to_string());
        }
        let Some(value) = carrier(property.ty)? else {
            return Err("a `Unit` top-level property".to_string());
        };
        globals.push(module.global(value));
    }
    let mut signatures = Vec::with_capacity(ir.functions.len());
    for function in &ir.functions {
        if function.dispatch_receiver.is_some() {
            return Err(format!("the member function `{}`", function.name));
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
        signatures.push(module.func_type(params, results));
    }
    let indices = signatures
        .iter()
        .map(|_| module.declare())
        .collect::<Vec<_>>();
    for (id, function) in ir.functions.iter().enumerate() {
        let mut body = Body {
            ir,
            module: &mut *module,
            runtime: &mut *runtime,
            functions: &indices,
            globals: &globals,
            code: Code::default(),
            locals: Vec::new(),
            next_local: function.params.len() as u32,
            values: function
                .params
                .iter()
                .enumerate()
                .map(|(slot, ty)| (slot as u32, (slot as u32, *ty)))
                .collect(),
            unit_values: HashSet::new(),
            declared: declared_variables(ir, function.body.expect("checked above")),
            result: function.ret,
            depth: 0,
            loops: Vec::new(),
        };
        let root = function.body.expect("checked above");
        // A body whose value completes returns it by leaving it on the stack; one whose every
        // path returns leaves the end unreachable, which `unreachable` states for the validator.
        let falls_through = match body.type_of(root)? {
            Some(ty) => carrier(ty)?.is_some(),
            None => false,
        };
        match carrier(function.ret)? {
            Some(_) if falls_through => body.value(root, function.ret)?,
            Some(_) => {
                body.statement(root)?;
                body.code.unreachable();
            }
            None => body.statement(root)?,
        }
        let Body { code, locals, .. } = body;
        module.define(
            indices[id],
            Function {
                ty: signatures[id],
                locals,
                code,
            },
        );
    }
    let initializer = if ir.statics.iter().any(|property| property.init.is_some()) {
        let index = module.declare();
        let ty = module.func_type(Vec::new(), Vec::new());
        let mut body = Body {
            ir,
            module: &mut *module,
            runtime: &mut *runtime,
            functions: &indices,
            globals: &globals,
            code: Code::default(),
            locals: Vec::new(),
            next_local: 0,
            values: HashMap::new(),
            unit_values: HashSet::new(),
            declared: ir
                .statics
                .iter()
                .filter_map(|property| property.init)
                .flat_map(|init| declared_variables(ir, init))
                .collect(),
            result: Ty::Unit,
            depth: 0,
            loops: Vec::new(),
        };
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
    Ok(LoweredFile {
        functions: indices,
        initializer,
    })
}

/// Every variable `root` declares, with its declared type.
fn declared_variables(ir: &IrFile, root: u32) -> HashMap<u32, Ty> {
    let mut declared = HashMap::new();
    let mut pending = vec![root];
    while let Some(id) = pending.pop() {
        if let IrExpr::Variable { index, ty, .. } = ir.expr(id) {
            declared.insert(*index, *ty);
        }
        crate::ir::for_each_child(&ir.exprs, id, &mut |child| pending.push(child));
    }
    declared
}

/// The wasm value a Kotlin type is carried as; `None` for a type with no value (`Unit`,
/// `Nothing`).
pub(super) fn carrier(ty: Ty) -> Result<Option<ValType>, Unsupported> {
    Ok(Some(match ty {
        Ty::Unit | Ty::Nothing => return Ok(None),
        Ty::Boolean | Ty::Byte | Ty::Short | Ty::Char | Ty::Int => ValType::I32,
        Ty::Long => ValType::I64,
        Ty::Float => ValType::F32,
        Ty::Double => ValType::F64,
        Ty::String | Ty::Null => STRING,
        Ty::Nullable(&Ty::String) => STRING,
        other => return Err(format!("the type `{}`", describe(other))),
    }))
}

/// The string reference type. The runtime declares `$String` as type 1, after `$chars`; see
/// [`Runtime::start`].
const STRING: ValType = ValType::Ref {
    nullable: true,
    heap: super::super::encode::HeapType::Concrete(1),
};

fn describe(ty: Ty) -> String {
    match ty {
        // Boundary conversion for a diagnostic.
        Ty::Obj(name, _) => name.render().replace('/', "."),
        Ty::Nullable(inner) => format!("{}?", describe(*inner)),
        other => format!("{other:?}"),
    }
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
    ir: &'a IrFile,
    module: &'a mut Module,
    runtime: &'a mut Runtime,
    functions: &'a [u32],
    /// The global holding each of the file's top-level properties.
    globals: &'a [u32],
    code: Code,
    /// Locals beyond the parameters, in index order.
    locals: Vec<ValType>,
    next_local: u32,
    /// IR value slot → (wasm local, Kotlin type).
    values: HashMap<u32, (u32, Ty)>,
    /// Slots of `Unit`-typed variables, which have no local.
    unit_values: HashSet<u32>,
    /// The declared type of every variable in the body, read before the code that declares it
    /// is emitted: a block's type is its value's, and that value can be a variable the block
    /// itself declares (`n++` is `{ val old = n; n = old + 1; old }`).
    declared: HashMap<u32, Ty>,
    result: Ty,
    /// How many structured instructions enclose the current position.
    depth: u32,
    loops: Vec<Loop>,
}

impl Body<'_> {
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
        let ty = match self.ir.expr(id) {
            IrExpr::Return(_)
            | IrExpr::Break { .. }
            | IrExpr::Continue { .. }
            | IrExpr::Throw { .. } => return Ok(None),
            IrExpr::Variable { .. }
            | IrExpr::SetValue { .. }
            | IrExpr::SetStatic { .. }
            | IrExpr::While { .. }
            | IrExpr::InlineFrameMarker
            | IrExpr::Block { value: None, .. } => Ty::Unit,
            IrExpr::GetValue(slot) => match self.ir.logical_types.get(&id) {
                Some(ty) => *ty,
                None => match self.values.get(slot) {
                    Some((_, ty)) => *ty,
                    None => *self
                        .declared
                        .get(slot)
                        .ok_or_else(|| format!("a read of value {slot} before its declaration"))?,
                },
            },
            node => self
                .ir
                .checked_type(id)
                .ok_or_else(|| format!("{} without a checked type", describe_node(node)))?,
        };
        Ok((ty != Ty::Nothing).then_some(ty))
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
        let target_ty = target;
        let source_ty = self.type_of(id)?;
        self.expression(id)?;
        let Some(source) = source_ty else {
            return Ok(());
        };
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
                (Some(from), to) if from == to => None,
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
        match node {
            IrExpr::Const(constant) => self.constant(&constant)?,
            IrExpr::UnitInstance | IrExpr::InlineFrameMarker => {}
            IrExpr::GetValue(slot) => {
                if let Some((local, _)) = self.values.get(&slot) {
                    self.code.local_get(*local);
                } else if !self.unit_values.contains(&slot) {
                    return Err(format!("a read of value {slot} before its declaration"));
                }
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
                    if let Some(init) = init {
                        self.value(init, ty)?;
                        self.code.local_set(local);
                    }
                }
            },
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
                        return Err("a `return` of no value from a non-`Unit` function".into())
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
                callee: Callee::Local(function),
                dispatch_receiver: None,
                args,
            } => {
                let params = self.ir.functions[function as usize].params.clone();
                if params.len() != args.len() {
                    return Err("a call whose arguments do not match its parameters".into());
                }
                for (arg, param) in args.iter().zip(params) {
                    self.value(*arg, param)?;
                }
                self.code.call(self.functions[function as usize]);
            }
            IrExpr::Call {
                callee:
                    Callee::Intrinsic {
                        operation: IrIntrinsic::PrimitiveCompare { operand, .. },
                        ..
                    },
                dispatch_receiver,
                args,
            } => match (dispatch_receiver.as_ref(), args.as_slice()) {
                (None, [lhs, rhs]) | (Some(lhs), [rhs]) => self.compare(operand, *lhs, *rhs)?,
                _ => return Err("`compareTo` with an unexpected operand shape".to_string()),
            },
            IrExpr::Call { callee, .. } => {
                return Err(format!("a call to {}", callee_kind(&callee)));
            }
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
                    Some(target) if source.is_some_and(|source| source != target) => {
                        return Err(format!("a coercion to `{}`", describe(type_operand)));
                    }
                    Some(_) => self.value(arg, type_operand)?,
                }
            }
            IrExpr::NotNullAssert { operand, .. } => {
                // A failed assertion traps: there are no exceptions to throw yet.
                self.expression(operand)?;
                if self.carrier_of(operand)? == Some(STRING) {
                    self.code.ref_as_non_null();
                }
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
            other => return Err(describe_node(&other)),
        }
        Ok(())
    }

    /// `a.compareTo(b)` on two integral values: `-1`, `0` or `1`. A floating-point `compareTo` is a
    /// total order (`NaN` greatest, `-0.0` below `0.0`), not this, and declines.
    fn compare(&mut self, operand: Ty, lhs: u32, rhs: u32) -> Result<(), Unsupported> {
        let value = carrier(operand)?;
        let (greater, less) = match value {
            Some(ValType::I32) => (GT_S32, LT_S32),
            Some(ValType::I64) => (GT_S64, LT_S64),
            _ => return Err(format!("`compareTo` on `{}`", describe(operand))),
        };
        let value = value.expect("matched above");
        let left = self.local(value);
        let right = self.local(value);
        self.value(lhs, operand)?;
        self.code.local_set(left);
        self.value(rhs, operand)?;
        self.code.local_set(right);
        self.code
            .local_get(left)
            .local_get(right)
            .op(greater)
            .local_get(left)
            .local_get(right)
            .op(less)
            .op(SUB32);
        Ok(())
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
            IrConst::UByte(_) | IrConst::UShort(_) | IrConst::UInt(_) | IrConst::ULong(_) => {
                return Err("an unsigned constant".to_string())
            }
        };
        Ok(())
    }

    /// Push `id` converted to a non-null `String`, as a string template does.
    fn string_of(&mut self, id: u32) -> Result<(), Unsupported> {
        let ty = self.type_of(id)?;
        self.expression(id)?;
        let function = match ty {
            Some(Ty::String | Ty::Null | Ty::Nullable(&Ty::String)) => self.runtime.string_or_null,
            Some(Ty::Int | Ty::Short | Ty::Byte) => self.runtime.string_of_i32,
            Some(Ty::Long) => self.runtime.string_of_i64,
            Some(Ty::Char) => self.runtime.string_of_char,
            Some(Ty::Boolean) => self.runtime.string_of_boolean,
            Some(other) => {
                return Err(format!(
                    "a string template part of type `{}`",
                    describe(other)
                ))
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
        let shift = matches!(op, IrBinOp::Shl | IrBinOp::Shr | IrBinOp::Ushr);
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
        let opcode = opcode(op, operand)
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
fn opcode(op: IrBinOp, operand: ValType) -> Option<Opcode> {
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
