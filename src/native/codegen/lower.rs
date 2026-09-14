//! Common IR to Cranelift IR, one Kotlin file at a time.
//!
//! The shape mirrors what the retired C emitter did, minus the text: a `statement` walk for the
//! constructs that only sequence (blocks, locals, `return`), and an `expression` walk that yields a
//! Cranelift `Value`. Every runtime boundary is a call to a symbol the prebuilt runtime defines, by
//! the same names `super::super::intrinsics` maps Kotlin declarations onto — the mapping did not
//! change when the emitter did, because the symbols are the runtime's, not the emitter's.
//!
//! Kotlin scalars map onto Cranelift types by width: `Boolean`/`Byte` → `i8`, `Short`/`Char` →
//! `i16`, `Int` → `i32`, `Long` → `i64`, `Float`/`Double` → `f32`/`f64`. Every reference —
//! including a boxed `Int?` — is an `i64` pointer, exactly the `KRef` the runtime traces.

use std::collections::HashMap;
use std::rc::Rc;

mod arrays;
mod defaults;
mod enums;
mod functions;
mod objects;
mod scope;
mod statics;

use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{
    types, AbiParam, Block, BlockArg, InstBuilder, Signature, StackSlotData, StackSlotKind,
    TrapCode,
};
use cranelift_codegen::ir::{FuncRef, Type, Value};
use cranelift_codegen::isa::CallConv;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};

use crate::ir::{
    Callee, ClassId, IrBinOp, IrCheckedOperation, IrConst, IrExpr, IrFile, IrIntrinsic,
    IrLocalPropertyLayout, IrTypeOp,
};
use crate::jvm::classpath::Classpath;
use crate::types::Ty;

use super::super::classes::{self as model, ClassModel, Slot, Symbols, ValueMember};
use super::super::target::NativeTarget;
use super::{Entry, PROGRAM_ENTRY};

/// The construct a lowering declined, phrased for a diagnostic.
pub type Unsupported = String;

/// One lowered Kotlin file.
pub struct Lowered {
    /// A relocatable ELF object.
    pub object: Vec<u8>,
    /// Whether this file declared `main` and therefore defines [`PROGRAM_ENTRY`].
    pub defines_entry: bool,
}

/// How a Kotlin type is carried in machine code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Carrier {
    /// `Unit` in return position: no value.
    Void,
    /// A machine scalar of the given Cranelift type; the flag says whether an argument of this type
    /// is sign- (true) or zero-extended when passed to the runtime's C ABI.
    Scalar(Type, bool),
    /// A `KRef`: an `i64` pointer into the collected heap, or null.
    Ref,
}

impl Carrier {
    fn clif(self) -> Option<Type> {
        match self {
            Self::Void => None,
            Self::Scalar(ty, _) => Some(ty),
            Self::Ref => Some(types::I64),
        }
    }

    fn abi_param(self) -> Option<AbiParam> {
        match self {
            Self::Void => None,
            Self::Scalar(ty, signed) if ty.bits() < 32 => Some(if signed {
                AbiParam::new(ty).sext()
            } else {
                AbiParam::new(ty).uext()
            }),
            Self::Scalar(ty, _) => Some(AbiParam::new(ty)),
            Self::Ref => Some(AbiParam::new(types::I64)),
        }
    }
}

/// Types the generator carries at all. The unsigned integers are declined: Kotlin defines them as
/// value classes over the signed primitives, and carrying `UInt` as the `Int` it wraps would make
/// `1u as? Int` succeed and `4294967295u.toString()` print `-1` — silently wrong, which is the one
/// thing this backend never is.
fn check_carried(ty: Ty) -> Result<(), Unsupported> {
    if ty.non_null().is_unsigned() {
        return Err(format!("an unsigned integer type (`{ty:?}`)"));
    }
    Ok(())
}

/// A nullable primitive is a reference: `Int?` has to represent `null`, so it boxes, exactly as it
/// does on the JVM and as the C runtime already expects.
fn carrier(ty: Ty) -> Carrier {
    match ty {
        Ty::Unit => Carrier::Void,
        Ty::Boolean => Carrier::Scalar(types::I8, false),
        Ty::Byte => Carrier::Scalar(types::I8, true),
        Ty::Short => Carrier::Scalar(types::I16, true),
        Ty::Char => Carrier::Scalar(types::I16, false),
        Ty::Int => Carrier::Scalar(types::I32, true),
        Ty::Long => Carrier::Scalar(types::I64, true),
        Ty::Float => Carrier::Scalar(types::F32, true),
        Ty::Double => Carrier::Scalar(types::F64, true),
        _ => Carrier::Ref,
    }
}

/// Build the ISA for `target`. Non-PIC, because the output is a static executable at a fixed
/// address linked by krusty's own linker; the verifier stays on while the lowering is young.
fn isa_for(target: NativeTarget) -> Result<cranelift_codegen::isa::OwnedTargetIsa, Unsupported> {
    let triple: target_lexicon::Triple = target
        .triple()
        .parse()
        .map_err(|error| format!("target triple `{}` ({error})", target.triple()))?;
    let mut flags = settings::builder();
    for (name, value) in [
        ("is_pic", "false"),
        ("opt_level", "none"),
        ("enable_verifier", "true"),
        ("use_colocated_libcalls", "false"),
    ] {
        flags
            .set(name, value)
            .map_err(|error| format!("Cranelift setting {name}={value} ({error})"))?;
    }
    cranelift_codegen::isa::lookup(triple)
        .map_err(|error| format!("target {target} ({error})"))?
        .finish(settings::Flags::new(flags))
        .map_err(|error| format!("target {target} ({error})"))
}

pub fn lower_file(
    ir: &IrFile,
    classpath: &Rc<Classpath>,
    target: NativeTarget,
    stem: &str,
    entry: Entry,
) -> Result<Lowered, Unsupported> {
    let class_model = model::build(ir)?;

    let isa = isa_for(target)?;
    let builder = ObjectBuilder::new(
        isa,
        format!("{stem}.o"),
        cranelift_module::default_libcall_names(),
    )
    .map_err(|error| format!("object builder ({error})"))?;
    let mut module = ObjectModule::new(builder);

    let mut lowering = FileLowering {
        ir,
        classpath,
        module: &mut module,
        symbols: model::symbols(ir, super::super::linker::runtime_symbols(target.arch)),
        model: class_model,
        functions: Vec::new(),
        imports: HashMap::new(),
        data_imports: HashMap::new(),
        strings: HashMap::new(),
        classes: Vec::new(),
        accessors: HashMap::new(),
        statics: Vec::new(),
        lambdas: HashMap::new(),
        default_wrappers: HashMap::new(),
        default_constructors: HashMap::new(),
        enum_entries: HashMap::new(),
        holders: HashMap::new(),
    };
    lowering.declare_functions()?;
    lowering.declare_classes()?;
    lowering.declare_statics()?;
    lowering.declare_lambdas()?;
    lowering.declare_default_wrappers()?;
    lowering.declare_default_constructors()?;
    lowering.declare_enum_entries()?;
    lowering.define_classes()?;
    lowering.define_default_wrappers()?;
    lowering.define_default_constructors()?;
    lowering.define_enum_entries()?;
    let statics_init = lowering.define_statics_init()?;
    let mut defines_entry = false;
    for index in 0..ir.functions.len() {
        lowering.define_function(index)?;
        let function = &ir.functions[index];
        let is_entry = function.params.is_empty()
            && function.is_static
            && function.dispatch_receiver.is_none()
            && match entry {
                Entry::Main => function.name == "main",
                Entry::Box => function.name == "box" && carrier(function.ret) == Carrier::Ref,
            };
        if is_entry {
            lowering.define_program_entry(index, statics_init)?;
            defines_entry = true;
        }
    }

    let object = module
        .finish()
        .emit()
        .map_err(|error| format!("object emission ({error})"))?;
    Ok(Lowered {
        object,
        defines_entry,
    })
}

struct FileLowering<'a> {
    ir: &'a IrFile,
    classpath: &'a Rc<Classpath>,
    module: &'a mut ObjectModule,
    /// Symbols of this file's classes and functions.
    symbols: Symbols,
    /// Layouts and vtables of this file's classes.
    model: ClassModel,
    /// Declared Cranelift function per IR function index; `None` for an abstract method.
    functions: Vec<Option<FuncId>>,
    /// Runtime functions this file imports, by symbol.
    imports: HashMap<String, FuncId>,
    /// Runtime data this file imports (the built-in type descriptors), by symbol.
    data_imports: HashMap<String, DataId>,
    /// String literal data, deduplicated by content.
    strings: HashMap<Vec<u8>, DataId>,
    /// Per-class emitted items, parallel to `ir.classes`.
    classes: Vec<objects::ClassItems>,
    /// Synthesized field accessors the vtables reference, by slot.
    accessors: HashMap<Slot, FuncId>,
    /// The global slot of each top-level property, parallel to `ir.statics`.
    statics: Vec<DataId>,
    /// The emitted pieces of each lambda, by the expression that creates it.
    lambdas: HashMap<u32, functions::LambdaItems>,
    /// One wrapper per omission shape a call in this file uses.
    default_wrappers: HashMap<defaults::Omission, FuncId>,
    /// The same, for a CONSTRUCTION that leaves arguments out.
    default_constructors: HashMap<defaults::CtorOmission, FuncId>,
    /// Per enum class, its constants' static slots and getters, in declaration order.
    enum_entries: HashMap<ClassId, enums::EnumItems>,
    /// The holder type for a captured `var` of each carrier, by the carrier's spelling.
    holders: HashMap<String, DataId>,
}

impl<'a> FileLowering<'a> {
    fn signature_of(&self, params: &[Ty], ret: Ty) -> Result<Signature, Unsupported> {
        let mut signature = Signature::new(CallConv::SystemV);
        for param in params.iter().chain(std::iter::once(&ret)) {
            check_carried(*param)?;
        }
        for param in params {
            match carrier(*param).abi_param() {
                Some(abi) => signature.params.push(abi),
                None => return Err("a `Unit` parameter".to_string()),
            }
        }
        if let Some(abi) = carrier(ret).abi_param() {
            signature.returns.push(abi);
        }
        Ok(signature)
    }

    /// The signature of an IR function: a method takes its receiver first.
    fn function_signature(
        &self,
        function: &crate::ir::IrFunction,
    ) -> Result<Signature, Unsupported> {
        let mut params = Vec::with_capacity(function.params.len() + 1);
        if let Some(owner) = function.dispatch_receiver {
            if self.ir.class_id_by_name(owner).is_none() {
                return Err(format!(
                    "a method of `{}`, which is not declared in this file",
                    owner.render()
                ));
            }
            params.push(any());
        }
        params.extend(functions::carried_parameters(self.ir, function));
        self.signature_of(&params, function.ret)
    }

    fn declare_functions(&mut self) -> Result<(), Unsupported> {
        for (index, function) in self.ir.functions.iter().enumerate() {
            if function.body.is_none() {
                // Nothing to emit, and therefore nothing to DECLARE: an exported symbol that is
                // never defined fails the object's own consistency check, not the link. Two shapes
                // reach here — an abstract method, whose vtable entry is the runtime's loud
                // failure, and a lambda the checked lowering spliced into an `inline` caller and
                // then cleared. Neither is called by name; a call site that somehow named one
                // finds no id and declines.
                self.functions.push(None);
                continue;
            }
            let signature = self.function_signature(function)?;
            let id = self
                .module
                .declare_function(&self.symbols.functions[index], Linkage::Export, &signature)
                .map_err(|error| format!("declaring `{}` ({error})", function.name))?;
            self.functions.push(Some(id));
        }
        Ok(())
    }

    /// A runtime function by symbol, declared as an import on first use.
    fn import(&mut self, symbol: &str, params: &[Ty], ret: Ty) -> Result<FuncId, Unsupported> {
        if let Some(id) = self.imports.get(symbol) {
            return Ok(*id);
        }
        let signature = self.signature_of(params, ret)?;
        let id = self
            .module
            .declare_function(symbol, Linkage::Import, &signature)
            .map_err(|error| format!("importing `{symbol}` ({error})"))?;
        self.imports.insert(symbol.to_string(), id);
        Ok(id)
    }

    /// A string literal's UTF-8 bytes as read-only data, shared between identical literals.
    fn string_data(&mut self, bytes: &[u8]) -> Result<DataId, Unsupported> {
        if let Some(id) = self.strings.get(bytes) {
            return Ok(*id);
        }
        let name = format!("kt_str_{}", self.strings.len());
        let id = self
            .module
            .declare_data(&name, Linkage::Local, false, false)
            .map_err(|error| format!("declaring string data ({error})"))?;
        let mut description = DataDescription::new();
        // NUL-terminated so the bytes are also a C string, should the runtime ever want one.
        let mut contents = bytes.to_vec();
        contents.push(0);
        description.define(contents.into_boxed_slice());
        self.module
            .define_data(id, &description)
            .map_err(|error| format!("defining string data ({error})"))?;
        self.strings.insert(bytes.to_vec(), id);
        Ok(id)
    }

    /// Compile one function body. `fill` receives the body lowering positioned in the entry block
    /// with the function's parameters, and lowers whatever the function is; the tail is shared —
    /// a `Unit` function returns when it falls off its end, and a body that already left
    /// (its last statement was a `return`) closes its unreachable continuation with a trap.
    fn emit_function(
        &mut self,
        id: FuncId,
        signature: Signature,
        result: Carrier,
        name: &str,
        fill: &mut Fill<'_>,
    ) -> Result<(), Unsupported> {
        let frontend_config = self.module.target_config();
        let mut context = self.module.make_context();
        context.func.signature = signature;
        let mut builder_context = FunctionBuilderContext::new();
        {
            let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
            let entry = builder.create_block();
            builder.append_block_params_for_function_params(entry);
            builder.switch_to_block(entry);
            builder.seal_block(entry);
            {
                let mut body = BodyLowering {
                    file: self,
                    builder: &mut builder,
                    values: HashMap::new(),
                    result,
                    loops: Vec::new(),
                    terminated: false,
                };
                let params = body.builder.block_params(entry).to_vec();
                fill(&mut body, &params)?;
                if body.terminated {
                    body.builder.ins().trap(TrapCode::unwrap_user(1));
                } else {
                    if result != Carrier::Void {
                        return Err(format!(
                            "a non-`Unit` function `{name}` that falls off its end"
                        ));
                    }
                    body.builder.ins().return_(&[]);
                }
            }
            builder.seal_all_blocks();
            builder.finalize(frontend_config);
        }
        self.module
            .define_function(id, &mut context)
            .map_err(|error| format!("compiling `{name}` ({error})"))?;
        self.module.clear_context(&mut context);
        Ok(())
    }

    fn define_function(&mut self, index: usize) -> Result<(), Unsupported> {
        let function = &self.ir.functions[index];
        let Some(id) = self.functions[index] else {
            return Ok(());
        };
        let Some(body) = function.body else {
            // Declared as nothing above, so there is nothing to define either.
            return Ok(());
        };
        // A `tailrec` the checked lowering could not rewrite into a loop still recurses, and this
        // generator emits an ordinary call for that recursion. The source wrote `tailrec` because
        // it recurses to a depth no stack survives, so accepting the function means emitting a
        // program that segfaults where it should print its answer. Decline instead: how deep a
        // native stack goes is the machine's business, and a gate must not depend on it.
        if self.ir.unlooped_tailrec.contains(&(index as u32)) {
            return Err(format!(
                "a `tailrec` function `{}` common lowering leaves recursive",
                function.name
            ));
        }
        let signature = self.function_signature(function)?;
        // `this`, when there is one, is value slot 0 and the parameters follow it.
        let mut slots: Vec<Ty> = Vec::with_capacity(function.params.len() + 1);
        if let Some(owner) = function.dispatch_receiver {
            slots.push(Ty::Obj(owner, &[]));
        }
        slots.extend(functions::carried_parameters(self.ir, function));
        let name = function.name.clone();
        let ret = function.ret;
        self.emit_function(
            id,
            signature,
            carrier(ret),
            &name,
            &mut |lowering, params| {
                for (slot, (value, ty)) in params.iter().zip(&slots).enumerate() {
                    let variable = lowering.declare_value(slot as u32, *ty)?;
                    lowering.builder.def_var(variable, *value);
                }
                lowering.statement(body)
            },
        )
    }

    /// `kt_program_entry`: what the runtime's `_start` calls. Records the stack bottom for the
    /// collector, runs the entry function — printing its result when it has one, which is how a
    /// `box()` case reports its verdict — and exits through the kernel; it never returns to
    /// `_start`.
    fn define_program_entry(
        &mut self,
        main_index: usize,
        statics_init: Option<FuncId>,
    ) -> Result<(), Unsupported> {
        let void = Signature::new(CallConv::SystemV);
        let entry_id = self
            .module
            .declare_function(PROGRAM_ENTRY, Linkage::Export, &void)
            .map_err(|error| format!("declaring `{PROGRAM_ENTRY}` ({error})"))?;
        let init = self.import("kt_runtime_init", &[Ty::obj("kotlin/Any")], Ty::Unit)?;
        let exit = self.import("kt_exit", &[Ty::Int], Ty::Unit)?;
        let prints_result = carrier(self.ir.functions[main_index].ret) == Carrier::Ref;
        let println = if prints_result {
            Some(self.import("kt_println_any", &[any()], Ty::Unit)?)
        } else {
            None
        };
        let main = self.functions[main_index].expect("the entry function has a body");
        let frontend_config = self.module.target_config();

        let mut context = self.module.make_context();
        context.func.signature = void;
        let mut builder_context = FunctionBuilderContext::new();
        {
            let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
            let block = builder.create_block();
            builder.switch_to_block(block);
            builder.seal_block(block);
            // The address of this frame's own slot is as good a stack bottom as any: everything
            // the program does happens in frames below it.
            let slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                8,
                3,
            ));
            let bottom = builder.ins().stack_addr(types::I64, slot, 0);
            let init_ref = self.module.declare_func_in_func(init, builder.func);
            builder.ins().call(init_ref, &[bottom]);
            // Top-level properties are initialized before the entry function runs, which is when
            // the JVM would have touched the facade and run its `<clinit>`.
            if let Some(statics_init) = statics_init {
                let statics_ref = self.module.declare_func_in_func(statics_init, builder.func);
                builder.ins().call(statics_ref, &[]);
            }
            let main_ref = self.module.declare_func_in_func(main, builder.func);
            let call = builder.ins().call(main_ref, &[]);
            if let Some(println) = println {
                let result = builder.inst_results(call)[0];
                let println_ref = self.module.declare_func_in_func(println, builder.func);
                builder.ins().call(println_ref, &[result]);
            }
            let zero = builder.ins().iconst(types::I32, 0);
            let exit_ref = self.module.declare_func_in_func(exit, builder.func);
            builder.ins().call(exit_ref, &[zero]);
            builder.ins().return_(&[]);
            builder.finalize(frontend_config);
        }
        self.module
            .define_function(entry_id, &mut context)
            .map_err(|error| format!("compiling `{PROGRAM_ENTRY}` ({error})"))?;
        self.module.clear_context(&mut context);
        Ok(())
    }
}

/// What fills a function body: given the body lowering in the entry block and the function's
/// parameter values, lowers whatever the function is.
type Fill<'f> = dyn FnMut(&mut BodyLowering<'_, '_, '_>, &[Value]) -> Result<(), Unsupported> + 'f;

/// One enclosing loop, for `break`/`continue` to target.
struct LoopFrame {
    label: Option<String>,
    break_block: Block,
    continue_block: Block,
}

struct BodyLowering<'a, 'b, 'c> {
    file: &'b mut FileLowering<'a>,
    builder: &'b mut FunctionBuilder<'c>,
    /// Kotlin value slot → Cranelift variable and its declared type.
    values: HashMap<u32, (Variable, Ty)>,
    result: Carrier,
    /// Loops the current position is inside, innermost last.
    loops: Vec<LoopFrame>,
    /// Whether control has left the current block for good — a `return`, `break` or `continue`
    /// was emitted and the builder now sits in a fresh block nothing jumps to. Statement walkers
    /// stop at the first terminated statement; whatever a dead block does receive is harmless.
    terminated: bool,
}

impl<'a, 'b, 'c> BodyLowering<'a, 'b, 'c> {
    fn declare_value(&mut self, slot: u32, ty: Ty) -> Result<Variable, Unsupported> {
        check_carried(ty)?;
        let Some(clif) = carrier(ty).clif() else {
            return Err("a `Unit`-typed local".to_string());
        };
        let variable = self.builder.declare_var(clif);
        self.values.insert(slot, (variable, ty));
        Ok(variable)
    }

    fn func_ref(&mut self, id: FuncId) -> FuncRef {
        self.file.module.declare_func_in_func(id, self.builder.func)
    }

    /// Call a runtime function by symbol.
    fn runtime_call(
        &mut self,
        symbol: &str,
        params: &[Ty],
        ret: Ty,
        arguments: &[Value],
    ) -> Result<Option<Value>, Unsupported> {
        let id = self.file.import(symbol, params, ret)?;
        let func_ref = self.func_ref(id);
        let call = self.builder.ins().call(func_ref, arguments);
        Ok(self.builder.inst_results(call).first().copied())
    }

    /// The current block has been terminated: move to a block nothing reaches, so that anything
    /// the IR still puts after the terminator has somewhere to go without tripping the builder.
    fn terminate(&mut self) {
        self.terminated = true;
        let dead = self.builder.create_block();
        self.builder.switch_to_block(dead);
    }

    /// Enter `block` as a reachable position.
    fn continue_in(&mut self, block: Block) {
        self.builder.switch_to_block(block);
        self.terminated = false;
    }

    fn statement(&mut self, id: u32) -> Result<(), Unsupported> {
        // `var x = 0` in a class body stores NOTHING. The rule is Kotlin's, it is observable
        // rather than an optimization, and the IR is what knows which store is a declaration's —
        // see `IrFile::is_elided_initializer_store`. A fresh object's storage is already zero
        // here, as it is on every target krusty emits for.
        if self.file.ir.is_elided_initializer_store(id) {
            return Ok(());
        }
        match self.file.ir.expr(id).clone() {
            IrExpr::Block { stmts, value } => {
                for statement in stmts {
                    self.statement(statement)?;
                    if self.terminated {
                        return Ok(());
                    }
                }
                if let Some(value) = value {
                    self.statement(value)?;
                }
            }
            IrExpr::Return(value) => {
                match (value, self.result) {
                    (Some(value), Carrier::Void) => {
                        self.expression(value)?;
                        if self.terminated {
                            return Ok(());
                        }
                        self.builder.ins().return_(&[]);
                    }
                    (Some(value), Carrier::Ref) => {
                        // A function whose result is a reference can still be handed `Unit` — a
                        // `Unit`-returning lambda's body returns the `Unit` OBJECT, because
                        // `FunctionN.invoke` answers with a reference whatever the lambda does.
                        // `reference` materializes the runtime's singleton for exactly that.
                        let value = self.reference(value)?;
                        if self.terminated {
                            return Ok(());
                        }
                        self.builder.ins().return_(&[value]);
                    }
                    (Some(value), _) => {
                        let value = self.expression(value)?;
                        if self.terminated {
                            return Ok(());
                        }
                        let Some(value) = value else {
                            return Err(
                                "a `return` of no value from a non-`Unit` function".to_string()
                            );
                        };
                        self.builder.ins().return_(&[value]);
                    }
                    (None, _) => {
                        self.builder.ins().return_(&[]);
                    }
                }
                self.terminate();
            }
            IrExpr::Variable {
                index, ty, init, ..
            } => {
                // A `var` a closure captures is replaced by a HOLDER: the slot holds the cell, not
                // the value the source declared, and the declaration still says `Int` because that
                // is what the programmer wrote. The initializer is what says which — and believing
                // the declaration instead truncates a pointer into a 32-bit slot, which is a
                // miscompile with no symptom at the point it happens.
                let ty = match init.map(|init| self.file.ir.expr(init)) {
                    Some(IrExpr::RefNew { .. }) => any(),
                    _ => ty,
                };
                if carrier(ty) == Carrier::Void {
                    if let Some(init) = init {
                        self.expression(init)?;
                    }
                    return Ok(());
                }
                let variable = self.declare_value(index, ty)?;
                if let Some(init) = init {
                    let value = self.coerce(init, ty)?;
                    if self.terminated {
                        return Ok(());
                    }
                    let Some(value) = value else {
                        return Err("a local initialized from a `Unit` value".to_string());
                    };
                    self.builder.def_var(variable, value);
                } else {
                    // Kotlin forbids reading an unassigned local, so any definition will do; zero
                    // keeps the SSA construction total.
                    let zero = self.zero_of(ty);
                    self.builder.def_var(variable, zero);
                }
            }
            IrExpr::SetValue { var, value } => {
                let Some(&(variable, ty)) = self.values.get(&var) else {
                    return Err("an assignment to an undeclared local".to_string());
                };
                let value = self.coerce(value, ty)?;
                if self.terminated {
                    return Ok(());
                }
                let Some(value) = value else {
                    return Err("an assignment of a `Unit` value".to_string());
                };
                self.builder.def_var(variable, value);
            }
            IrExpr::When { branches } => {
                self.when(&branches, None)?;
            }
            IrExpr::While {
                cond,
                body,
                update,
                post_test,
                label,
            } => self.loop_statement(cond, body, update, post_test, label)?,
            IrExpr::Break { label } => {
                let target = self.loop_frame(label.as_deref(), "break")?.break_block;
                self.builder.ins().jump(target, &[]);
                self.terminate();
            }
            IrExpr::Continue { label } => {
                let target = self
                    .loop_frame(label.as_deref(), "continue")?
                    .continue_block;
                self.builder.ins().jump(target, &[]);
                self.terminate();
            }
            IrExpr::SetField {
                receiver,
                class,
                index,
                value,
            } => self.field_write(receiver, class, index, value)?,
            IrExpr::SetStatic { index, value } => self.static_write(index, value)?,
            IrExpr::Checked(IrCheckedOperation::PropertyWrite {
                target,
                dispatch_receiver,
                extension_receiver,
                context_arguments,
                value,
                ..
            }) => {
                if extension_receiver.is_some() || !context_arguments.is_empty() {
                    self.receiver_property(
                        &target,
                        extension_receiver,
                        &context_arguments,
                        Some(value),
                    )?;
                    return Ok(());
                }
                match self.checked_property(&target) {
                    Ok((class, index)) => {
                        self.property_write(class, index, dispatch_receiver, value)?
                    }
                    Err(reason) if reason == objects::TOP_LEVEL => {
                        let name = self.checked_property_name(&target)?;
                        self.top_level_write(&name, value)?;
                    }
                    Err(reason) => return Err(reason),
                }
            }
            _ => {
                self.expression(id)?;
            }
        }
        Ok(())
    }

    /// The loop a `break`/`continue` names: the labeled one, or the innermost.
    fn loop_frame(&self, label: Option<&str>, keyword: &str) -> Result<&LoopFrame, Unsupported> {
        let frame = match label {
            Some(label) => self
                .loops
                .iter()
                .rev()
                .find(|frame| frame.label.as_deref() == Some(label)),
            None => self.loops.last(),
        };
        frame.ok_or_else(|| format!("a `{keyword}` outside the loop it names"))
    }

    /// `while`, `do…while`, and the shape a lowered `for` takes: a loop whose `update` runs after
    /// the body at the `continue` target. Every jump is a real edge here — the `goto` scaffolding
    /// the C emitter needed for labels and updates is just what a control-flow graph is.
    fn loop_statement(
        &mut self,
        cond: u32,
        body: u32,
        update: Option<u32>,
        post_test: bool,
        label: Option<String>,
    ) -> Result<(), Unsupported> {
        let header = self.builder.create_block();
        let body_block = self.builder.create_block();
        let exit = self.builder.create_block();
        let update_block = update.map(|_| self.builder.create_block());

        self.builder
            .ins()
            .jump(if post_test { body_block } else { header }, &[]);

        self.continue_in(header);
        let condition = self.expression(cond)?;
        if !self.terminated {
            let Some(condition) = condition else {
                return Err("a loop condition of no value".to_string());
            };
            self.builder
                .ins()
                .brif(condition, body_block, &[], exit, &[]);
        }

        self.loops.push(LoopFrame {
            label,
            break_block: exit,
            continue_block: update_block.unwrap_or(header),
        });
        self.continue_in(body_block);
        self.statement(body)?;
        if !self.terminated {
            self.builder.ins().jump(update_block.unwrap_or(header), &[]);
        }
        // The frame stays for the update: a lowered `for` puts its overflow guard's `break` there.
        if let (Some(update), Some(update_block)) = (update, update_block) {
            self.continue_in(update_block);
            self.statement(update)?;
            if !self.terminated {
                self.builder.ins().jump(header, &[]);
            }
        }
        self.loops.pop();

        self.continue_in(exit);
        Ok(())
    }

    /// `if`/`when`: a chain of conditional branches into one merge block. With `result` the merge
    /// block carries the value as a block parameter and every arm passes its own; without, arm
    /// values are discarded. An arm that leaves (a `return`, a `break`) simply contributes no edge.
    fn when(
        &mut self,
        branches: &[(Option<u32>, u32)],
        result: Option<Ty>,
    ) -> Result<Option<Value>, Unsupported> {
        let merge = self.builder.create_block();
        let result = result.filter(|ty| carrier(*ty) != Carrier::Void);
        if let Some(ty) = result {
            let clif = carrier(ty).clif().expect("non-void carrier");
            self.builder.append_block_param(merge, clif);
        }

        let mut reaches_merge = false;
        let mut has_else = false;
        for (condition, body) in branches {
            match condition {
                Some(condition) => {
                    let condition = self.expression(*condition)?;
                    if self.terminated {
                        break;
                    }
                    let Some(condition) = condition else {
                        return Err("a condition of no value".to_string());
                    };
                    let then_block = self.builder.create_block();
                    let else_block = self.builder.create_block();
                    self.builder
                        .ins()
                        .brif(condition, then_block, &[], else_block, &[]);
                    self.continue_in(then_block);
                    self.arm(*body, result, merge, &mut reaches_merge)?;
                    self.continue_in(else_block);
                }
                None => {
                    has_else = true;
                    self.arm(*body, result, merge, &mut reaches_merge)?;
                    break;
                }
            }
        }
        if !has_else && !self.terminated {
            match result {
                // An exhaustive `when` over an enum or a sealed hierarchy has no `else`: the
                // frontend proved one is unreachable. The proof is not repeated here — this
                // generator does not know the hierarchy — so the fall-through is the runtime's
                // loud failure, which is what Kotlin puts there too (`NoWhenBranchMatched`). It
                // costs a few unreachable instructions and never a wrong answer.
                Some(ty) => {
                    self.runtime_call("kt_no_when_branch_matched", &[], Ty::Unit, &[])?;
                    let clif = carrier(ty).clif().expect("non-void carrier");
                    let unreachable = match clif {
                        types::F32 => self.builder.ins().f32const(0.0),
                        types::F64 => self.builder.ins().f64const(0.0),
                        integer => self.builder.ins().iconst(integer, 0),
                    };
                    self.builder
                        .ins()
                        .jump(merge, &[BlockArg::Value(unreachable)]);
                }
                None => {
                    self.builder.ins().jump(merge, &[]);
                }
            }
            reaches_merge = true;
        }

        if !reaches_merge {
            // Every arm left. The merge block stays unused, and so does whatever follows.
            self.terminated = true;
            return Ok(None);
        }
        self.continue_in(merge);
        Ok(result.map(|_| self.builder.block_params(merge)[0]))
    }

    /// One arm of a `when`: its body, then the edge into `merge` unless the body left.
    fn arm(
        &mut self,
        body: u32,
        result: Option<Ty>,
        merge: Block,
        reaches_merge: &mut bool,
    ) -> Result<(), Unsupported> {
        let value = match result {
            Some(ty) => self.coerce(body, ty)?,
            None => {
                self.statement(body)?;
                None
            }
        };
        if self.terminated {
            return Ok(());
        }
        match (result, value) {
            (Some(_), Some(value)) => {
                self.builder.ins().jump(merge, &[BlockArg::Value(value)]);
            }
            (Some(_), None) => {
                return Err("a `when` arm of no value where one is needed".to_string())
            }
            (None, _) => {
                self.builder.ins().jump(merge, &[]);
            }
        }
        *reaches_merge = true;
        Ok(())
    }

    fn zero_of(&mut self, ty: Ty) -> Value {
        match carrier(ty) {
            Carrier::Scalar(t, _) if t == types::F32 => self.builder.ins().f32const(0.0),
            Carrier::Scalar(t, _) if t == types::F64 => self.builder.ins().f64const(0.0),
            Carrier::Scalar(t, _) => self.builder.ins().iconst(t, 0),
            _ => self.builder.ins().iconst(types::I64, 0),
        }
    }

    /// Lower an expression; `None` is Kotlin `Unit`.
    fn expression(&mut self, id: u32) -> Result<Option<Value>, Unsupported> {
        match self.file.ir.expr(id).clone() {
            IrExpr::Const(constant) => self.constant(&constant).map(Some),
            IrExpr::UnitInstance => Ok(None),
            IrExpr::GetValue(slot) => {
                let Some(&(variable, _)) = self.values.get(&slot) else {
                    return Err("a read of an undeclared local".to_string());
                };
                Ok(Some(self.builder.use_var(variable)))
            }
            IrExpr::Block { stmts, value } => {
                for statement in stmts {
                    self.statement(statement)?;
                    if self.terminated {
                        return Ok(None);
                    }
                }
                match value {
                    Some(value) => self.expression(value),
                    None => Ok(None),
                }
            }
            IrExpr::When { branches } => {
                let result = self.type_of(id);
                self.when(&branches, result)
            }
            IrExpr::Call {
                callee,
                dispatch_receiver,
                args,
            } => self.call(&callee, dispatch_receiver, &args),
            IrExpr::TypeOp {
                op,
                arg,
                type_operand,
            } => self.type_operation(op, arg, type_operand),
            IrExpr::PrimitiveBinOp { op, lhs, rhs } => self.binary(op, lhs, rhs),
            IrExpr::PrimitiveNeg { operand, ty } => self.negate(operand, ty),
            IrExpr::StringConcat(parts) => self.concat(&parts),
            IrExpr::NotNullAssert { operand, .. } => {
                // `x!!` yields `x` or fails. The check is the runtime's so the failure reads the
                // same whatever produced the null, and so the generator emits no control flow for
                // what is, on every path that matters, a value passing straight through.
                let ty = self.type_of(operand);
                let value = self.reference(operand)?;
                if self.terminated {
                    return Ok(None);
                }
                let checked = self
                    .runtime_call("kt_not_null", &[any()], any(), &[value])?
                    .expect("`kt_not_null` returns its argument");
                // `x!!` on a nullable primitive is the unboxing Kotlin means by it.
                match ty.map(|ty| ty.non_null()) {
                    Some(ty) if carrier(ty) != Carrier::Ref => {
                        self.convert(checked, Some(any()), ty)
                    }
                    _ => Ok(Some(checked)),
                }
            }
            IrExpr::New {
                internal,
                args,
                ctor_params,
                defaults,
                default_prefix_count,
                ..
            } => {
                // A construction's `defaults` name SOURCE-value ordinals, which begin after the
                // compiler's leading operands; the generator works in the constructor's physical
                // frame, so they are shifted once, here, rather than at each use.
                let omitted: Vec<u32> = defaults
                    .iter()
                    .map(|ordinal| ordinal + default_prefix_count)
                    .collect();
                self.construction(
                    internal,
                    &args,
                    ctor_params.as_deref(),
                    (!omitted.is_empty()).then_some(omitted.as_slice()),
                )
            }
            IrExpr::MethodCall {
                class,
                index,
                receiver,
                args,
            } => self.method_call(class, index, receiver, &args),
            IrExpr::GetField {
                receiver,
                class,
                index,
            } => self.field_read(receiver, class, index),
            IrExpr::SingletonValue { classifier } => self.singleton(classifier),
            IrExpr::GetStatic(index) => self.static_read(index),
            IrExpr::NewArray { array_type, size } => self.new_array(array_type, size),
            IrExpr::Vararg {
                array_type,
                spreads,
                elements,
            } => self.vararg(array_type, &spreads, &elements),
            IrExpr::Lambda { .. } | IrExpr::CallableReference(_) => self.lambda(id),
            IrExpr::InvokeFunction {
                func,
                args,
                params,
                ret,
            } => self.invoke_function(func, &args, &params, ret),
            IrExpr::RefNew { elem, init } => self.ref_new(elem, init),
            IrExpr::RefGet { holder, elem } => self.ref_get(holder, elem),
            IrExpr::RefSet {
                holder,
                elem,
                value,
            } => self.ref_set(holder, elem, value),
            IrExpr::EnumEntry { classifier, name } => self.enum_entry(classifier, &name),
            IrExpr::EnumValues { classifier } => self.enum_values(classifier),
            IrExpr::EnumValueOf { classifier, arg } => self.enum_value_of(classifier, arg),
            IrExpr::EnclosingInstance {
                receiver, inner, ..
            } => self.enclosing_instance(receiver, inner),
            // `name` and `ordinal` are `kotlin.Enum`'s, and an enum constant answers both from the
            // storage that base contributes.
            IrExpr::Checked(IrCheckedOperation::ExternalPropertyRead {
                target,
                receiver: Some(receiver),
                ..
            }) if self.enum_member_name(target).is_some() => {
                let member = self.enum_member_name(target).expect("checked by the guard");
                self.enum_member(member, receiver)
            }
            IrExpr::Checked(IrCheckedOperation::PropertyRead {
                target,
                dispatch_receiver,
                extension_receiver,
                context_arguments,
                ..
            }) => {
                if extension_receiver.is_some() || !context_arguments.is_empty() {
                    return self.receiver_property(
                        &target,
                        extension_receiver,
                        &context_arguments,
                        None,
                    );
                }
                match self.checked_property(&target) {
                    Ok((class, index)) => self.property_read(class, index, dispatch_receiver),
                    Err(reason) if reason == objects::TOP_LEVEL => {
                        let name = self.checked_property_name(&target)?;
                        self.top_level_read(&name)
                    }
                    Err(reason) => Err(reason),
                }
            }
            IrExpr::Return(_)
            | IrExpr::Variable { .. }
            | IrExpr::SetValue { .. }
            | IrExpr::While { .. }
            | IrExpr::Break { .. }
            | IrExpr::Continue { .. }
            | IrExpr::SetField { .. }
            | IrExpr::SetStatic { .. }
            | IrExpr::Checked(IrCheckedOperation::PropertyWrite { .. }) => {
                self.statement(id)?;
                Ok(None)
            }
            other => Err(describe(&other)),
        }
    }

    fn constant(&mut self, constant: &IrConst) -> Result<Value, Unsupported> {
        Ok(match constant {
            IrConst::Boolean(value) => self.builder.ins().iconst(types::I8, i64::from(*value)),
            IrConst::Byte(value) => self.builder.ins().iconst(types::I8, i64::from(*value)),
            IrConst::Short(value) => self.builder.ins().iconst(types::I16, i64::from(*value)),
            IrConst::Char(value) => self.builder.ins().iconst(types::I16, i64::from(*value)),
            IrConst::Int(value) => self.builder.ins().iconst(types::I32, i64::from(*value)),
            IrConst::Long(value) => self.builder.ins().iconst(types::I64, *value),
            IrConst::Float(value) => self.builder.ins().f32const(*value),
            IrConst::Double(value) => self.builder.ins().f64const(*value),
            IrConst::Null => self.builder.ins().iconst(types::I64, 0),
            IrConst::String(value) => {
                // Kotlin admits unpaired surrogates; UTF-8 does not encode them and the runtime is
                // UTF-8, so such a literal is declined rather than mangled.
                let Some(text) = value.as_str() else {
                    return Err("a string constant containing an unpaired surrogate".to_string());
                };
                self.string_literal(text.as_bytes())?
            }
        })
    }

    /// A string object for a literal's bytes.
    fn string_literal(&mut self, bytes: &[u8]) -> Result<Value, Unsupported> {
        let data = self.file.string_data(bytes)?;
        let global = self
            .file
            .module
            .declare_data_in_func(data, self.builder.func);
        let pointer = self.builder.ins().symbol_value(types::I64, global);
        let length = self.builder.ins().iconst(types::I32, bytes.len() as i64);
        let string = self.runtime_call(
            "kt_string_utf8",
            &[Ty::obj("kotlin/Any"), Ty::Int],
            Ty::String,
            &[pointer, length],
        )?;
        Ok(string.expect("`kt_string_utf8` returns a string"))
    }

    /// Whether an expression produces a function value: a lambda, a `::f`, or anything typed as
    /// one. Every site that would realize equality asks here first, because Kotlin's answer for a
    /// callable reference is structural and this generator's is identity.
    fn is_function_expression(&self, id: u32) -> bool {
        matches!(
            self.file.ir.expr(id),
            IrExpr::Lambda { .. } | IrExpr::CallableReference(_)
        ) || self.type_of(id).is_some_and(is_function_value)
    }

    /// The Kotlin type of an expression, as far as the lowering needs it: enough to decide the
    /// carrier. `None` means undetermined, and callers that cannot proceed decline.
    fn type_of(&self, id: u32) -> Option<Ty> {
        Some(match self.file.ir.expr(id) {
            IrExpr::Const(constant) => match constant {
                IrConst::Boolean(_) => Ty::Boolean,
                IrConst::Byte(_) => Ty::Byte,
                IrConst::Short(_) => Ty::Short,
                IrConst::Int(_) => Ty::Int,
                IrConst::Long(_) => Ty::Long,
                IrConst::Float(_) => Ty::Float,
                IrConst::Double(_) => Ty::Double,
                IrConst::Char(_) => Ty::Char,
                IrConst::String(_) => Ty::String,
                IrConst::Null => Ty::Null,
            },
            IrExpr::UnitInstance => Ty::Unit,
            IrExpr::GetValue(slot) => self.values.get(slot)?.1,
            IrExpr::TypeOp {
                op, type_operand, ..
            } => match op {
                IrTypeOp::InstanceOf | IrTypeOp::NotInstanceOf => Ty::Boolean,
                IrTypeOp::SafeCast => Ty::nullable(*type_operand),
                _ => *type_operand,
            },
            IrExpr::Block {
                value: Some(value), ..
            } => self.type_of(*value)?,
            IrExpr::Block { value: None, .. } => Ty::Unit,
            IrExpr::When { branches } => {
                // NOT the first arm's type: `when (s) { "a" -> 1; else -> null }` is `Int?`, and
                // carrying it as the `Int` the first arm produces would make the `null` arm store
                // a pointer into a 32-bit slot. Arms whose carriers disagree — a `null` arm being
                // the common case — make the whole `when` a reference, which is what nullability
                // means here. An arm that leaves (a `return`) has no type and does not vote.
                let mut result: Option<Ty> = None;
                for (_, body) in branches {
                    let Some(ty) = self.type_of(*body) else {
                        continue;
                    };
                    result = Some(match result {
                        None => ty,
                        Some(previous) if carrier(previous) == carrier(ty) => previous,
                        Some(_) => any(),
                    });
                }
                result?
            }
            IrExpr::StringConcat(_) => Ty::String,
            IrExpr::PrimitiveNeg { ty, .. } => *ty,
            IrExpr::PrimitiveBinOp { op, lhs, .. } => match op {
                IrBinOp::Lt
                | IrBinOp::Le
                | IrBinOp::Gt
                | IrBinOp::Ge
                | IrBinOp::Eq
                | IrBinOp::Ne
                | IrBinOp::RefEq
                | IrBinOp::RefNe
                | IrBinOp::And
                | IrBinOp::Or => Ty::Boolean,
                // Kotlin has no `Byte.plus(Byte): Byte`: arithmetic on the narrow integer types
                // produces `Int`, and a result typed `Byte` here would pick the wrong carrier. An
                // operand of a bounded type parameter is read through its bound for the same
                // reason: the operator unboxes it, so the RESULT is that primitive and not the
                // reference the declaration spells — a caller told otherwise would skip the boxing
                // the next parameter needs, and the mismatch reaches the verifier, or worse.
                _ => match scalar_bound(self.type_of(*lhs)?)? {
                    Ty::Byte | Ty::Short | Ty::Char => Ty::Int,
                    other => other,
                },
            },
            IrExpr::Call { callee, .. } => match callee {
                Callee::Local(function)
                | Callee::LocalWithDefaults { function, .. }
                | Callee::ClassStaticWithDefaults { function, .. } => {
                    self.file.ir.functions[*function as usize].ret
                }
                Callee::External { ret, .. }
                | Callee::Intrinsic { ret, .. }
                | Callee::Super { ret, .. } => *ret,
                Callee::Special { source, .. } => {
                    let function = self.file.ir.checked_callable_functions.get(&(*source)?)?;
                    self.file.ir.functions[*function as usize].ret
                }
                _ => return None,
            },
            IrExpr::New { internal, .. }
            | IrExpr::SingletonValue {
                classifier: internal,
            }
            | IrExpr::EnumEntry {
                classifier: internal,
                ..
            }
            | IrExpr::EnumValueOf {
                classifier: internal,
                ..
            } => Ty::Obj(*internal, &[]),
            IrExpr::EnumValues { classifier } => {
                Ty::obj_args("kotlin/Array", &[Ty::Obj(*classifier, &[])])
            }
            IrExpr::MethodCall { class, index, .. } => {
                let fid = self.file.ir.classes[*class as usize].methods[*index as usize];
                self.file.ir.functions[fid as usize].ret
            }
            IrExpr::GetField { class, index, .. } => super::super::captures::physical_ty(
                self.file.ir,
                *class,
                *index,
                self.file.ir.classes[*class as usize].fields[*index as usize].ty,
            ),
            IrExpr::GetStatic(index) => self.file.ir.statics[*index as usize].ty,
            IrExpr::NewArray { array_type, .. } | IrExpr::Vararg { array_type, .. } => *array_type,
            IrExpr::InvokeFunction { ret, .. } => *ret,
            IrExpr::RefGet { elem, .. } | IrExpr::RefSet { elem, .. } => *elem,
            // A function value and a captured-variable holder are both objects.
            // A reference knows the function type it stands for, which is what makes an `equals`
            // on it recognisable as one between function values.
            IrExpr::CallableReference(reference) => reference.function_type,
            IrExpr::Lambda { .. } | IrExpr::RefNew { .. } => any(),
            // `name` and `ordinal` belong to `kotlin.Enum`, a class no file declares, so the
            // checked property table has nothing to say about them; their types are the language's
            // and are stated where the read itself is recognized.
            IrExpr::Checked(IrCheckedOperation::ExternalPropertyRead { target, .. }) => {
                match self.enum_member_name(*target)? {
                    "name" => Ty::String,
                    _ => Ty::Int,
                }
            }
            IrExpr::Checked(IrCheckedOperation::PropertyRead { target, .. }) => {
                self.file.ir.checked_properties.get(target)?.ty
            }
            _ => return None,
        })
    }

    /// An expression coerced to the carrier of `target`.
    fn coerce(&mut self, arg: u32, target: Ty) -> Result<Option<Value>, Unsupported> {
        let Some(value) = self.expression(arg)? else {
            // `Unit` is a value in Kotlin, and a position that wants a reference wants that value:
            // `val u: Any = Unit`, an argument of type `Any?`, a `Unit`-returning lambda's result.
            // The runtime owns the singleton, so there is one of it program-wide.
            if !self.terminated && carrier(target) == Carrier::Ref {
                return self.runtime_call("kt_unit", &[], any(), &[]);
            }
            return Ok(None);
        };
        if self.terminated {
            return Ok(Some(value));
        }
        let source = self.type_of(arg);
        self.convert(value, source, target)
    }

    /// An expression in a position that requires a reference, boxing a scalar if necessary.
    fn reference(&mut self, id: u32) -> Result<Value, Unsupported> {
        match self.coerce(id, any())? {
            Some(value) => Ok(value),
            // Only a lowering that already left has no value here; anything else, including
            // `Unit`, `coerce` materialized.
            None => Ok(self.builder.ins().iconst(types::I64, 0)),
        }
    }

    /// A representation change between carriers: boxing a scalar into a reference, unboxing one
    /// out, or widening/narrowing between scalars. An undetermined source leaves the value alone.
    fn convert(
        &mut self,
        value: Value,
        source: Option<Ty>,
        target: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        check_carried(target)?;
        if let Some(source) = source {
            check_carried(source)?;
        }
        let target = carrier(target);
        match (source.map(carrier), target) {
            (None, target) => {
                // The lowering could not type the expression. Its machine type is still known,
                // and when that already is the target's carrier nothing needs doing; otherwise a
                // conversion would be a guess, and a guess is declined.
                let actual = self.builder.func.dfg.value_type(value);
                match target.clif() {
                    Some(clif) if clif == actual => Ok(Some(value)),
                    None => Ok(None),
                    Some(clif) => Err(format!(
                        "a value of undetermined type (carried as `{actual}`) where a `{clif}` is \
                         required"
                    )),
                }
            }
            (Some(Carrier::Ref), Carrier::Ref) => Ok(Some(value)),
            (Some(source), target) if source == target => Ok(Some(value)),
            (Some(Carrier::Scalar(_, _)), Carrier::Ref) => {
                let ty = source.expect("known scalar");
                let Some(suffix) = box_suffix(ty) else {
                    return Err(format!(
                        "a `{ty:?}` in a position that requires a reference"
                    ));
                };
                self.runtime_call(
                    &format!("kt_box_{suffix}"),
                    &[ty],
                    Ty::obj("kotlin/Any"),
                    &[value],
                )
            }
            (Some(Carrier::Ref), Carrier::Scalar(_, _)) => {
                let ty = target_ty_of(target).expect("scalar carrier");
                let Some(suffix) = box_suffix(ty) else {
                    return Err(format!("an unboxing to `{ty:?}`"));
                };
                self.runtime_call(
                    &format!("kt_unbox_{suffix}"),
                    &[Ty::obj("kotlin/Any")],
                    ty,
                    &[value],
                )
            }
            (Some(Carrier::Scalar(from, signed)), Carrier::Scalar(to, _)) => {
                Ok(Some(self.resize(value, from, signed, to)))
            }
            (Some(_), Carrier::Void) => Ok(None),
            (Some(Carrier::Void), _) => Err("a coercion from `Unit`".to_string()),
        }
    }

    /// Convert a scalar between carriers, as Kotlin's `toInt()`/`toFloat()`/`toChar()` family and
    /// its widening conversions do: integers extend by the source's signedness or truncate;
    /// integer to float rounds; float to integer SATURATES with `NaN` to zero (`fcvt_to_sint_sat`
    /// is exactly Kotlin's rule, where the machine's plain conversion would trap or produce the
    /// indefinite value); a narrower integer target goes through `Int` first, as `Double.toByte()`
    /// is defined to.
    fn resize(&mut self, value: Value, from: Type, signed: bool, to: Type) -> Value {
        if from == to {
            return value;
        }
        match (from.is_float(), to.is_float()) {
            (false, false) => {
                if from.bits() < to.bits() {
                    if signed {
                        self.builder.ins().sextend(to, value)
                    } else {
                        self.builder.ins().uextend(to, value)
                    }
                } else {
                    self.builder.ins().ireduce(to, value)
                }
            }
            (false, true) => {
                if signed {
                    self.builder.ins().fcvt_from_sint(to, value)
                } else {
                    self.builder.ins().fcvt_from_uint(to, value)
                }
            }
            (true, false) => {
                let wide = if to.bits() > 32 {
                    types::I64
                } else {
                    types::I32
                };
                let integer = self.builder.ins().fcvt_to_sint_sat(wide, value);
                if wide == to {
                    integer
                } else {
                    self.builder.ins().ireduce(to, integer)
                }
            }
            (true, true) => {
                if to.bits() > from.bits() {
                    self.builder.ins().fpromote(to, value)
                } else {
                    self.builder.ins().fdemote(to, value)
                }
            }
        }
    }

    /// An operator's operand as the primitive it is: a reference-carried one is a box, opened to
    /// the type its own bound names.
    fn unboxed_operand(&mut self, value: Value, ty: Option<Ty>) -> Result<Value, Unsupported> {
        let Some(ty) = ty else {
            return Ok(value);
        };
        if carrier(ty) != Carrier::Ref {
            return Ok(value);
        }
        let Some(scalar) = scalar_bound(ty) else {
            return Err("an operator on a reference operand".to_string());
        };
        Ok(self
            .convert(value, Some(any()), scalar)?
            .expect("a scalar target yields a value"))
    }

    /// Two scalar operands brought to one width, as Kotlin's operator overloads do (`Byte + Int`
    /// is `Int + Int`). Returns the values, their common type, and whether comparisons are signed.
    fn unify(
        &mut self,
        lhs: Value,
        lhs_ty: Option<Ty>,
        rhs: Value,
        rhs_ty: Option<Ty>,
    ) -> Result<(Value, Value, Type, bool), Unsupported> {
        // Kotlin's arithmetic and comparison operators are the PRIMITIVE ones, so an operand whose
        // type carries as a reference here is a boxed primitive: a value of a generic type whose
        // bound is a number, which the JVM boxes for the same reason. It has to be unboxed before
        // either side's machine type means anything — this function unifies by machine type, and a
        // pointer and an `Int` unify into pointer arithmetic that reads like an answer and is not
        // one.
        let lhs = self.unboxed_operand(lhs, lhs_ty)?;
        let rhs = self.unboxed_operand(rhs, rhs_ty)?;
        let left = self.builder.func.dfg.value_type(lhs);
        let right = self.builder.func.dfg.value_type(rhs);
        if left.is_float() != right.is_float() {
            return Err("an operator mixing integer and floating-point operands".to_string());
        }
        let signed_of = |ty: Option<Ty>| !matches!(ty, Some(Ty::Char));
        let width = if left.bits() >= right.bits() {
            left
        } else {
            right
        };
        let lhs = self.resize(lhs, left, signed_of(lhs_ty), width);
        let rhs = self.resize(rhs, right, signed_of(rhs_ty), width);
        // Only `Char` compares unsigned, and only against another `Char`; widened to `Int` it is a
        // non-negative `Int` and signed comparison is the same thing.
        let signed = !(matches!(lhs_ty, Some(Ty::Char)) && matches!(rhs_ty, Some(Ty::Char)));
        Ok((lhs, rhs, width, signed))
    }

    /// A built-in binary operator, with Kotlin's semantics where the machine's differ.
    fn binary(&mut self, op: IrBinOp, lhs: u32, rhs: u32) -> Result<Option<Value>, Unsupported> {
        let lhs_ty = self.type_of(lhs);
        let rhs_ty = self.type_of(rhs);

        if matches!(op, IrBinOp::Eq | IrBinOp::Ne) {
            let against_null = matches!(self.file.ir.expr(lhs), IrExpr::Const(IrConst::Null))
                || matches!(self.file.ir.expr(rhs), IrExpr::Const(IrConst::Null));
            let on_references = lhs_ty.map(carrier) == Some(Carrier::Ref)
                || rhs_ty.map(carrier) == Some(Carrier::Ref);
            if against_null {
                // `x == null` is `x === null` in Kotlin: no `equals` is ever called.
                let left = self.reference(lhs)?;
                let right = self.reference(rhs)?;
                let condition = comparison(op, true).expect("equality");
                return Ok(Some(self.builder.ins().icmp(condition, left, right)));
            }
            if self.is_function_expression(lhs) || self.is_function_expression(rhs) {
                // Kotlin compares two callable references by the DECLARATION they name and the
                // receiver they bind, so `::f == ::f` is true even though each `::f` is its own
                // object. The generator gives one `kotlin.Any`'s identity equality, which answers
                // that `false` — so the comparison is declined rather than answered wrongly. What
                // it needs is one emitted type per referenced declaration, with an `equals` that
                // compares the type and the bound receiver.
                return Err("equality on a function value".to_string());
            }
            if on_references {
                // Kotlin's `==` on references is `equals`, dispatched through the receiver's
                // vtable and null-safe in Kotlin's sense (`null` equals only `null`) — which is
                // exactly what `kt_equals` is. A scalar on either side boxes, because `any == 5`
                // means `any?.equals(5)` there too.
                let left = self.reference(lhs)?;
                let right = self.reference(rhs)?;
                if self.terminated {
                    return Ok(None);
                }
                let equal = self
                    .runtime_call("kt_equals", &[any(), any()], Ty::Boolean, &[left, right])?
                    .expect("`kt_equals` returns a Boolean");
                return Ok(Some(if op == IrBinOp::Ne {
                    let one = self.builder.ins().iconst(types::I8, 1);
                    self.builder.ins().bxor(equal, one)
                } else {
                    equal
                }));
            }
            if lhs_ty.is_none() && rhs_ty.is_none() {
                // Neither a known scalar nor a known reference: either equality would be a guess.
                return Err("an equality on an undetermined operand type".to_string());
            }
        }
        if matches!(op, IrBinOp::RefEq | IrBinOp::RefNe) {
            // `===` between two values of a primitive type compares the VALUES (Kotlin: identity
            // equality on primitives is `==`, with a deprecation warning); boxing each side and
            // comparing the boxes' addresses would say `0L !== 0L`. Floating-point identity has
            // its own rules (`-0.0`, `NaN`) that nothing here implements yet, so it is declined.
            let both_scalars = matches!(lhs_ty.map(carrier), Some(Carrier::Scalar(..)))
                && matches!(rhs_ty.map(carrier), Some(Carrier::Scalar(..)));
            if both_scalars {
                let Some(left) = self.expression(lhs)? else {
                    return Err("a `Unit` operand".to_string());
                };
                let Some(right) = self.expression(rhs)? else {
                    return Err("a `Unit` operand".to_string());
                };
                if self.terminated {
                    return Ok(None);
                }
                let (left, right, ty, _) = self.unify(left, lhs_ty, right, rhs_ty)?;
                if ty.is_float() {
                    return Err("identity equality on floating-point values".to_string());
                }
                let condition = comparison(op, true).expect("identity");
                return Ok(Some(self.builder.ins().icmp(condition, left, right)));
            }
            let left = self.reference(lhs)?;
            let right = self.reference(rhs)?;
            let condition = comparison(op, true).expect("identity");
            return Ok(Some(self.builder.ins().icmp(condition, left, right)));
        }

        let Some(left) = self.expression(lhs)? else {
            return Err("a `Unit` operand".to_string());
        };
        let Some(right) = self.expression(rhs)? else {
            return Err("a `Unit` operand".to_string());
        };
        if self.terminated {
            return Ok(None);
        }

        // Shifts take an `Int` count whatever the left operand's width; Cranelift masks the count
        // to the operand width, which is exactly Kotlin's rule (`1 shl 32 == 1`).
        if matches!(op, IrBinOp::Shl | IrBinOp::Shr | IrBinOp::Ushr) {
            if self.builder.func.dfg.value_type(left).is_float() {
                return Err("a shift of a floating-point operand".to_string());
            }
            let left = self.widen_narrow_integer(left, lhs_ty);
            return Ok(Some(match op {
                IrBinOp::Shl => self.builder.ins().ishl(left, right),
                IrBinOp::Shr => self.builder.ins().sshr(left, right),
                _ => self.builder.ins().ushr(left, right),
            }));
        }

        let (left, right, ty, signed) = self.unify(left, lhs_ty, right, rhs_ty)?;
        if ty.is_float() {
            return Ok(Some(match op {
                IrBinOp::Add => self.builder.ins().fadd(left, right),
                IrBinOp::Sub => self.builder.ins().fsub(left, right),
                IrBinOp::Mul => self.builder.ins().fmul(left, right),
                IrBinOp::Div => self.builder.ins().fdiv(left, right),
                IrBinOp::Rem => return Err("`%` on floating-point operands".to_string()),
                IrBinOp::Lt
                | IrBinOp::Le
                | IrBinOp::Gt
                | IrBinOp::Ge
                | IrBinOp::Eq
                | IrBinOp::Ne => {
                    let condition = float_comparison(op).expect("comparison");
                    self.builder.ins().fcmp(condition, left, right)
                }
                other => return Err(format!("`{other:?}` on floating-point operands")),
            }));
        }

        // Arithmetic on the narrow types is `Int` arithmetic; `Boolean` is not narrow arithmetic.
        let arithmetic = !matches!(
            op,
            IrBinOp::Lt
                | IrBinOp::Le
                | IrBinOp::Gt
                | IrBinOp::Ge
                | IrBinOp::Eq
                | IrBinOp::Ne
                | IrBinOp::And
                | IrBinOp::Or
        );
        let (left, right, ty) = if arithmetic && ty.bits() < 32 && lhs_ty != Some(Ty::Boolean) {
            (
                self.widen_narrow_integer(left, lhs_ty),
                self.widen_narrow_integer(right, rhs_ty),
                types::I32,
            )
        } else {
            (left, right, ty)
        };

        Ok(Some(match op {
            // `iadd`/`isub`/`imul` wrap, which is Kotlin's rule; there is nothing to guard.
            IrBinOp::Add => self.builder.ins().iadd(left, right),
            IrBinOp::Sub => self.builder.ins().isub(left, right),
            IrBinOp::Mul => self.builder.ins().imul(left, right),
            // Division by zero throws and `MIN_VALUE / -1` wraps in Kotlin; the machine traps on
            // both, so the runtime decides.
            IrBinOp::Div | IrBinOp::Rem => {
                let (name, kotlin) = if ty == types::I64 {
                    ("long", Ty::Long)
                } else {
                    ("int", Ty::Int)
                };
                let helper = if op == IrBinOp::Div { "div" } else { "rem" };
                let result = self.runtime_call(
                    &format!("kt_{helper}_{name}"),
                    &[kotlin, kotlin],
                    kotlin,
                    &[left, right],
                )?;
                result.expect("division returns a value")
            }
            IrBinOp::BitAnd | IrBinOp::And => self.builder.ins().band(left, right),
            IrBinOp::BitOr | IrBinOp::Or => self.builder.ins().bor(left, right),
            IrBinOp::BitXor => self.builder.ins().bxor(left, right),
            IrBinOp::Lt | IrBinOp::Le | IrBinOp::Gt | IrBinOp::Ge | IrBinOp::Eq | IrBinOp::Ne => {
                let condition = comparison(op, signed).expect("comparison");
                self.builder.ins().icmp(condition, left, right)
            }
            IrBinOp::RefEq | IrBinOp::RefNe | IrBinOp::Shl | IrBinOp::Shr | IrBinOp::Ushr => {
                unreachable!("handled above")
            }
        }))
    }

    /// `Byte`/`Short`/`Char` operands of arithmetic become `Int`, as Kotlin's operators declare.
    fn widen_narrow_integer(&mut self, value: Value, ty: Option<Ty>) -> Value {
        let from = self.builder.func.dfg.value_type(value);
        if from.is_float() || from.bits() >= 32 {
            return value;
        }
        self.resize(value, from, !matches!(ty, Some(Ty::Char)), types::I32)
    }

    /// Unary minus. `-Int.MIN_VALUE` is `Int.MIN_VALUE` in Kotlin, and `ineg` wraps the same way.
    fn negate(&mut self, operand: u32, ty: Ty) -> Result<Option<Value>, Unsupported> {
        let Some(value) = self.coerce(operand, ty)? else {
            return Err("a negation of `Unit`".to_string());
        };
        if self.terminated {
            return Ok(None);
        }
        Ok(Some(if carrier(ty).clif().is_some_and(Type::is_float) {
            self.builder.ins().fneg(value)
        } else {
            self.builder.ins().ineg(value)
        }))
    }

    /// A string template: each part rendered by the runtime and joined left to right.
    fn concat(&mut self, parts: &[u32]) -> Result<Option<Value>, Unsupported> {
        let Some((first, rest)) = parts.split_first() else {
            return self.string_literal(b"").map(Some);
        };
        let mut joined = self.reference(*first)?;
        if rest.is_empty() {
            // A lone `"$x"` is `x.toString()`.
            return self.runtime_call("kt_to_string", &[any()], Ty::String, &[joined]);
        }
        for part in rest {
            let part = self.reference(*part)?;
            if self.terminated {
                return Ok(None);
            }
            joined = self
                .runtime_call(
                    "kt_string_plus",
                    &[any(), any()],
                    Ty::String,
                    &[joined, part],
                )?
                .expect("`kt_string_plus` returns a string");
        }
        Ok(Some(joined))
    }

    fn call(
        &mut self,
        callee: &Callee,
        dispatch_receiver: Option<u32>,
        args: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        match callee {
            // A call that leaves arguments out goes through the wrapper for its omission shape,
            // which computes them in the callee's own frame — where a default that reads an
            // earlier parameter can find it.
            Callee::LocalWithDefaults { function, defaults } => {
                self.defaulted_call(*function, defaults, dispatch_receiver, args)
            }
            Callee::ClassStaticWithDefaults {
                function, defaults, ..
            } => self.defaulted_call(*function, defaults, dispatch_receiver, args),
            // A static method owned by a class is, to this generator, a function with a symbol —
            // the owner is a JVM placement fact, and there is no flat facade here for it to be
            // placed differently from. A local function declared inside a member is the shape that
            // arrives this way.
            Callee::Local(function) | Callee::ClassStatic { function, .. } => {
                if dispatch_receiver.is_some() {
                    return Err("a static call with a receiver".to_string());
                }
                let params = functions::carried_parameters(
                    self.file.ir,
                    &self.file.ir.functions[*function as usize],
                );
                let arguments = self.arguments(args, &params)?;
                if self.terminated {
                    return Ok(None);
                }
                let Some(id) = self.file.functions[*function as usize] else {
                    return Err(format!(
                        "a call to `{}`, which has no body",
                        self.file.ir.functions[*function as usize].name
                    ));
                };
                let func_ref = self.func_ref(id);
                let call = self.builder.ins().call(func_ref, &arguments);
                Ok(self.builder.inst_results(call).first().copied())
            }
            Callee::Super {
                owner,
                name,
                source,
                params,
                ..
            } => {
                let Some(receiver) = dispatch_receiver else {
                    return Err(format!("a `super` call without a receiver (`{name}`)"));
                };
                self.direct_call(*owner, name, *source, Some(params), receiver, args)
            }
            Callee::Special {
                owner,
                name,
                source,
                ..
            } => {
                let Some(receiver) = dispatch_receiver else {
                    return Err(format!("a `super` call without a receiver (`{name}`)"));
                };
                self.direct_call(*owner, name, *source, None, receiver, args)
            }
            Callee::Intrinsic { operation, ret } => {
                self.intrinsic(*operation, *ret, dispatch_receiver, args)
            }
            Callee::External {
                target,
                params,
                ret,
                ..
            } => {
                let Some(realization) = self.file.classpath.external_callable(*target) else {
                    return Err("an unresolvable dependency call".to_string());
                };
                let owner = realization.callable.owner.render();
                let name = realization.callable.name.clone();
                match dispatch_receiver {
                    // A member: the receiver is the runtime function's first argument, and
                    // everything crosses as a reference.
                    Some(receiver) => {
                        // `f.equals(…)` and `f.hashCode()` on a function value answer by the
                        // declaration named, not by identity — the same reason `==` on one is
                        // declined above, and undecidable for the same reason: the receiver's type
                        // no longer says whether a lambda or a reference produced it.
                        if matches!(name.as_str(), "equals" | "hashCode")
                            && self.is_function_expression(receiver)
                        {
                            return Err("equality on a function value".to_string());
                        }
                        // `x.apply(block)` where `block` is a function VALUE rather than a
                        // lambda written here: nothing was spliced, so the call is realized as
                        // what it means — invoke the block on the receiver.
                        if let Some(realized) =
                            self.scope_function(&owner, &name, receiver, args, *ret)
                        {
                            return realized;
                        }
                        let Some(symbol) =
                            super::super::intrinsics::runtime_member(&owner, &name, params)
                        else {
                            return Err(format!("the member `{}.{name}`", owner.replace('/', ".")));
                        };
                        let mut arguments = vec![self.reference(receiver)?];
                        for argument in args {
                            arguments.push(self.reference(*argument)?);
                        }
                        if self.terminated {
                            return Ok(None);
                        }
                        let signature = vec![any(); arguments.len()];
                        self.runtime_call(symbol, &signature, *ret, &arguments)
                    }
                    None => {
                        let Some(symbol) =
                            super::super::intrinsics::runtime_function(&owner, &name, params)
                        else {
                            return Err(format!(
                                "the declaration `{}.{name}`",
                                owner.replace('/', ".")
                            ));
                        };
                        let arguments = self.arguments(args, params)?;
                        if self.terminated {
                            return Ok(None);
                        }
                        self.runtime_call(&symbol, params, *ret, &arguments)
                    }
                }
            }
            other => Err(format!("a {} call", callee_kind(other))),
        }
    }

    /// A compiler-selected operation on built-in types, realized by the runtime.
    fn intrinsic(
        &mut self,
        operation: IrIntrinsic,
        ret: Ty,
        receiver: Option<u32>,
        args: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        match operation {
            IrIntrinsic::PrimitiveCompare { operand } => {
                let (Some(receiver), [argument]) = (receiver, args) else {
                    return Err("a malformed `compareTo`".to_string());
                };
                let Some(suffix) = scalar_suffix(operand) else {
                    return Err("`compareTo` on a non-scalar operand".to_string());
                };
                let left = self.coerce(receiver, operand)?;
                let right = self.coerce(*argument, operand)?;
                if self.terminated {
                    return Ok(None);
                }
                let (Some(left), Some(right)) = (left, right) else {
                    return Err("`compareTo` on `Unit`".to_string());
                };
                self.runtime_call(
                    &format!("kt_compare_{suffix}"),
                    &[operand, operand],
                    ret,
                    &[left, right],
                )
            }
            IrIntrinsic::StringPlus => {
                let (Some(receiver), [argument]) = (receiver, args) else {
                    return Err("a malformed `String.plus`".to_string());
                };
                let left = self.reference(receiver)?;
                let right = self.reference(*argument)?;
                if self.terminated {
                    return Ok(None);
                }
                self.runtime_call("kt_string_plus", &[any(), any()], ret, &[left, right])
            }
            IrIntrinsic::ArrayGet => {
                let (Some(receiver), [index]) = (receiver, args) else {
                    return Err("a malformed array read".to_string());
                };
                self.array_get(receiver, *index, ret)
            }
            IrIntrinsic::ArraySet => {
                let (Some(receiver), [index, value]) = (receiver, args) else {
                    return Err("a malformed array store".to_string());
                };
                self.array_set(receiver, *index, *value)
            }
            IrIntrinsic::ArraySize => {
                let Some(receiver) = receiver else {
                    return Err("a malformed array size".to_string());
                };
                self.array_size(receiver)
            }
            IrIntrinsic::StringLength => {
                let Some(receiver) = receiver else {
                    return Err("a malformed `String.length`".to_string());
                };
                let value = self.reference(receiver)?;
                if self.terminated {
                    return Ok(None);
                }
                self.runtime_call("kt_string_length", &[any()], ret, &[value])
            }
            IrIntrinsic::NullableAnyToString => {
                let Some(receiver) = receiver else {
                    return Err("a malformed `toString`".to_string());
                };
                let value = self.reference(receiver)?;
                if self.terminated {
                    return Ok(None);
                }
                self.runtime_call("kt_to_string", &[any()], ret, &[value])
            }
            IrIntrinsic::DataClassFieldHash { ty } => {
                let [value] = args else {
                    return Err("a malformed data-class field hash".to_string());
                };
                self.field_hash(*value, ty)
            }
            IrIntrinsic::DataClassFieldEquals { ty } => {
                let [left, right] = args else {
                    return Err("a malformed data-class field comparison".to_string());
                };
                self.field_equals(*left, *right, ty)
            }
            other => Err(format!("the `{other:?}` intrinsic")),
        }
    }

    /// One field's contribution to a data class's `hashCode`, which is the field's own `hashCode`.
    ///
    /// Kotlin's answer for each primitive is fixed, and these are those answers rather than
    /// anything this generator is free to choose: a program can print a hash, and two programs
    /// that agree on everything else must agree on it. `Boolean` is 1231 or 1237 — arbitrary, and
    /// arbitrary in the same way everywhere. `Long` folds its halves together so the high word is
    /// not lost in the truncation to `Int`, and `Double` does the same to its bits. A `Float` is
    /// its bits. The smaller integers are themselves, widened.
    fn field_hash(&mut self, value: u32, ty: Ty) -> Result<Option<Value>, Unsupported> {
        if carrier(ty) == Carrier::Ref {
            // Including a nullable primitive, which is a box and hashes through its own type.
            let value = self.reference(value)?;
            if self.terminated {
                return Ok(None);
            }
            return self.runtime_call("kt_hash_code", &[any()], Ty::Int, &[value]);
        }
        let Some(operand) = self.coerce(value, ty)? else {
            return Err("a `Unit` data-class field".to_string());
        };
        if self.terminated {
            return Ok(None);
        }
        self.value_hash(operand, ty).map(Some)
    }

    /// The hash of a value already in hand, by the same rules.
    pub(super) fn value_hash(&mut self, operand: Value, ty: Ty) -> Result<Value, Unsupported> {
        if carrier(ty) == Carrier::Ref {
            return Ok(self
                .runtime_call("kt_hash_code", &[any()], Ty::Int, &[operand])?
                .expect("`kt_hash_code` returns an Int"));
        }
        let hash = match ty.non_null() {
            Ty::Boolean => {
                let yes = self.builder.ins().iconst(types::I32, 1231);
                let no = self.builder.ins().iconst(types::I32, 1237);
                let zero = self.builder.ins().iconst(types::I8, 0);
                let set = self.builder.ins().icmp(IntCC::NotEqual, operand, zero);
                self.builder.ins().select(set, yes, no)
            }
            Ty::Byte | Ty::Short => self.builder.ins().sextend(types::I32, operand),
            Ty::Char => self.builder.ins().uextend(types::I32, operand),
            Ty::Int => operand,
            Ty::Long => self.fold_to_int(operand),
            // A floating-point field hashes by its BITS, with `Float` giving them directly and
            // `Double` folding them as `Long` does. Not emitted yet, and deliberately: a data
            // class holding one cannot compile at all today, because the `toString` synthesized
            // beside this `hashCode` has to render the value and the runtime cannot. Writing the
            // hash before the rendering would mean shipping a line no test can reach.
            Ty::Float | Ty::Double => {
                return Err("a data-class field holding a floating-point value".to_string())
            }
            other => return Err(format!("a data-class field of type `{other:?}`")),
        };
        Ok(hash)
    }

    /// `(value xor (value ushr 32)).toInt()` — how Kotlin folds 64 bits into a hash.
    fn fold_to_int(&mut self, value: Value) -> Value {
        let shift = self.builder.ins().iconst(types::I64, 32);
        let high = self.builder.ins().ushr(value, shift);
        let folded = self.builder.ins().bxor(value, high);
        self.builder.ins().ireduce(types::I32, folded)
    }

    /// Whether two data-class fields are equal, which is `equals`, not `==` on the machine.
    ///
    /// A reference field is the runtime's null-safe `equals`, which is Kotlin's. A floating-point
    /// field would compare by its BITS — so that `NaN` equals `NaN` and `0.0` does not equal
    /// `-0.0`, the opposite of what the comparison instruction answers — and is declined for the
    /// same reason the hash above is: the `toString` synthesized beside this `equals` cannot
    /// render the value, so no data class holding one compiles today.
    fn field_equals(
        &mut self,
        left: u32,
        right: u32,
        ty: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        if !ty.is_nullable() && matches!(ty.non_null(), Ty::Float | Ty::Double) {
            return Err("a data-class field holding a floating-point value".to_string());
        }
        if carrier(ty) != Carrier::Ref {
            let left = self.coerce(left, ty)?;
            let right = self.coerce(right, ty)?;
            if self.terminated {
                return Ok(None);
            }
            let (Some(left), Some(right)) = (left, right) else {
                return Err("a `Unit` data-class field".to_string());
            };
            return self.values_equal(left, right, ty).map(Some);
        }
        let left = self.reference(left)?;
        let right = self.reference(right)?;
        if self.terminated {
            return Ok(None);
        }
        self.values_equal(left, right, ty).map(Some)
    }

    /// Whether two values already in hand are `equals`, by the same rules.
    pub(super) fn values_equal(
        &mut self,
        left: Value,
        right: Value,
        ty: Ty,
    ) -> Result<Value, Unsupported> {
        if !ty.is_nullable() && matches!(ty.non_null(), Ty::Float | Ty::Double) {
            return Err("a field holding a floating-point value".to_string());
        }
        if carrier(ty) != Carrier::Ref {
            return Ok(self.builder.ins().icmp(IntCC::Equal, left, right));
        }
        Ok(self
            .runtime_call("kt_equals", &[any(), any()], Ty::Boolean, &[left, right])?
            .expect("`kt_equals` returns a Boolean"))
    }

    /// Arguments coerced to the parameter carriers they are passed as.
    fn arguments(&mut self, args: &[u32], parameters: &[Ty]) -> Result<Vec<Value>, Unsupported> {
        let mut values = Vec::with_capacity(args.len());
        for (index, argument) in args.iter().enumerate() {
            let value = match parameters.get(index) {
                Some(ty) => self.coerce(*argument, *ty)?,
                None => self.expression(*argument)?,
            };
            if self.terminated {
                return Ok(values);
            }
            let Some(value) = value else {
                return Err("a `Unit` argument".to_string());
            };
            values.push(value);
        }
        Ok(values)
    }
}

/// Whether a value is a function value — a lambda or a callable reference. Spelled several ways:
/// `(Int) -> Int` is `Ty::Fun`, while a reference stored in a `val` takes the `KFunction1` its
/// declaration names. Equality has to treat all of them alike, and the reason is that by the time
/// a value reaches a comparison its spelling no longer says WHICH it is: `val f: (Int) -> Int =
/// ::double` erases the reference to the same `Function1` a lambda gets. A lambda's `equals` is
/// identity, which this generator would answer correctly; a reference's is structural, which it
/// would answer wrongly — so the one type they share has to be declined for both.
fn is_function_value(ty: Ty) -> bool {
    if matches!(ty.non_null(), Ty::Fun(_)) {
        return true;
    }
    ty.non_null().obj_internal().is_some_and(|name| {
        let name = name.render();
        name.starts_with("kotlin/Function") || name.starts_with("kotlin/reflect/KFunction")
    })
}

/// The primitive a reference-carried type REPRESENTS, or `None` when it represents no primitive.
///
/// A type parameter is the case that matters: `T : Int` is carried as a reference — a boxed `Int`,
/// exactly as the JVM carries it — and its bound is what says which primitive is in the box. The
/// chain is peeled because a bound can name another parameter, and the step count is capped so a
/// cyclic one cannot spin.
fn scalar_bound(ty: Ty) -> Option<Ty> {
    let mut at = ty.non_null();
    for _ in 0..16 {
        if carrier(at) != Carrier::Ref {
            return Some(at);
        }
        match at {
            Ty::TyParam(_, bound) => at = bound.non_null(),
            _ => return None,
        }
    }
    None
}

/// `Any?`: the type every runtime reference parameter is declared as.
fn any() -> Ty {
    Ty::nullable(Ty::obj("kotlin/Any"))
}

/// The integer condition for a Kotlin comparison operator.
fn comparison(op: IrBinOp, signed: bool) -> Option<IntCC> {
    Some(match (op, signed) {
        (IrBinOp::Eq | IrBinOp::RefEq, _) => IntCC::Equal,
        (IrBinOp::Ne | IrBinOp::RefNe, _) => IntCC::NotEqual,
        (IrBinOp::Lt, true) => IntCC::SignedLessThan,
        (IrBinOp::Le, true) => IntCC::SignedLessThanOrEqual,
        (IrBinOp::Gt, true) => IntCC::SignedGreaterThan,
        (IrBinOp::Ge, true) => IntCC::SignedGreaterThanOrEqual,
        (IrBinOp::Lt, false) => IntCC::UnsignedLessThan,
        (IrBinOp::Le, false) => IntCC::UnsignedLessThanOrEqual,
        (IrBinOp::Gt, false) => IntCC::UnsignedGreaterThan,
        (IrBinOp::Ge, false) => IntCC::UnsignedGreaterThanOrEqual,
        _ => return None,
    })
}

/// The floating-point condition for a Kotlin comparison: ordered for `<`/`<=`/`>`/`>=`/`==` (any
/// NaN makes them false) and unordered for `!=` (`NaN != NaN` is true), as IEEE and Kotlin agree.
fn float_comparison(op: IrBinOp) -> Option<FloatCC> {
    Some(match op {
        IrBinOp::Eq => FloatCC::Equal,
        IrBinOp::Ne => FloatCC::NotEqual,
        IrBinOp::Lt => FloatCC::LessThan,
        IrBinOp::Le => FloatCC::LessThanOrEqual,
        IrBinOp::Gt => FloatCC::GreaterThan,
        IrBinOp::Ge => FloatCC::GreaterThanOrEqual,
        _ => return None,
    })
}

/// The runtime's suffix for a scalar type's `kt_compare_*` and console functions.
fn scalar_suffix(ty: Ty) -> Option<&'static str> {
    Some(match ty {
        Ty::Boolean => "boolean",
        Ty::Byte => "byte",
        Ty::Short => "short",
        Ty::Char => "char",
        Ty::Int => "int",
        Ty::Long => "long",
        Ty::Float => "float",
        Ty::Double => "double",
        _ => return None,
    })
}

/// The Kotlin type a scalar carrier stands for, for naming the runtime's unboxers.
fn target_ty_of(carrier: Carrier) -> Option<Ty> {
    match carrier {
        Carrier::Scalar(t, signed) => Some(match (t, signed) {
            (types::I8, false) => Ty::Boolean,
            (types::I8, true) => Ty::Byte,
            (types::I16, true) => Ty::Short,
            (types::I16, false) => Ty::Char,
            (types::I32, _) => Ty::Int,
            (types::I64, _) => Ty::Long,
            (types::F32, _) => Ty::Float,
            (types::F64, _) => Ty::Double,
            _ => return None,
        }),
        _ => None,
    }
}

/// The runtime's box/unbox suffix for a scalar.
fn box_suffix(ty: Ty) -> Option<&'static str> {
    Some(match ty {
        Ty::Boolean => "boolean",
        Ty::Byte => "byte",
        Ty::Short => "short",
        Ty::Char => "char",
        Ty::Int => "int",
        Ty::Long => "long",
        Ty::Float => "float",
        Ty::Double => "double",
        _ => return None,
    })
}

fn callee_kind(callee: &Callee) -> &'static str {
    match callee {
        Callee::Local(_) => "local",
        Callee::ClassStatic { .. }
        | Callee::ClassStaticWithDefaults { .. }
        | Callee::ClassStaticDefault { .. } => "class-static",
        Callee::LocalDefault(_) | Callee::LocalWithDefaults { .. } => "defaulted",
        Callee::Intrinsic { .. } => "intrinsic",
        Callee::CrossFile { .. } => "cross-file",
        Callee::Module { .. } | Callee::ModuleWithDefaults { .. } => "same-module",
        Callee::External { .. } => "dependency",
        Callee::Static { .. } => "static",
        Callee::Virtual { .. } => "virtual",
        Callee::Super { .. } => "super",
        Callee::Special { .. } => "special",
    }
}

/// A short phrase naming the construct, for the declining diagnostic.
fn describe(node: &IrExpr) -> String {
    // The first two identifiers, not one: `Checked(Call { … })` and `Checked(PropertyRead { … })`
    // are different pieces of work, and a backlog that lumps them together cannot be worked from.
    let debug = format!("{node:?}");
    let mut identifiers = debug
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|piece| !piece.is_empty());
    let head = identifiers.next().unwrap_or("expression");
    match (head, identifiers.next()) {
        ("Checked", Some(operation)) => format!("`Checked({operation})`"),
        _ => format!("`{head}`"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cranelift_codegen::ir::ArgumentExtension;

    #[test]
    fn a_nullable_primitive_is_carried_as_a_reference() {
        assert_eq!(carrier(Ty::Int), Carrier::Scalar(types::I32, true));
        assert_eq!(
            carrier(Ty::nullable(Ty::Int)),
            Carrier::Ref,
            "`Int?` must represent `null`, so it boxes exactly as it does on the JVM"
        );
        assert_eq!(carrier(Ty::Unit), Carrier::Void);
        assert_eq!(carrier(Ty::String), Carrier::Ref);
    }

    #[test]
    fn narrow_scalars_extend_to_the_c_abi_by_their_kotlin_signedness() {
        // `Byte` is signed and `Char` is not; passing either to the runtime in a 32-bit register
        // must extend it the way the C prototype's type does, or `kt_println_char('é')` prints a
        // negative code point.
        assert!(carrier(Ty::Byte).abi_param().unwrap().extension == ArgumentExtension::Sext);
        assert!(carrier(Ty::Char).abi_param().unwrap().extension == ArgumentExtension::Uext);
        assert!(carrier(Ty::Boolean).abi_param().unwrap().extension == ArgumentExtension::Uext);
        assert!(carrier(Ty::Int).abi_param().unwrap().extension == ArgumentExtension::None);
    }
}
