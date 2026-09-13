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

use cranelift_codegen::ir::{
    types, AbiParam, InstBuilder, Signature, StackSlotData, StackSlotKind,
};
use cranelift_codegen::ir::{FuncRef, Type, Value};
use cranelift_codegen::isa::CallConv;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};

use crate::ir::{Callee, IrConst, IrExpr, IrFile, IrTypeOp};
use crate::jvm::classpath::Classpath;
use crate::types::Ty;

use super::super::target::NativeTarget;
use super::PROGRAM_ENTRY;

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
) -> Result<Lowered, Unsupported> {
    if !ir.classes.is_empty() {
        return Err("a class declaration".to_string());
    }
    if !ir.statics.is_empty() {
        return Err("a top-level property".to_string());
    }

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
        symbols: function_symbols(ir),
        functions: Vec::new(),
        imports: HashMap::new(),
        strings: HashMap::new(),
    };
    lowering.declare_functions()?;
    let mut defines_entry = false;
    for index in 0..ir.functions.len() {
        lowering.define_function(index)?;
        let function = &ir.functions[index];
        if function.name == "main" && function.params.is_empty() {
            lowering.define_program_entry(index)?;
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
    /// Symbol per IR function index.
    symbols: Vec<String>,
    /// Declared Cranelift function per IR function index.
    functions: Vec<FuncId>,
    /// Runtime functions this file imports, by symbol.
    imports: HashMap<String, FuncId>,
    /// String literal data, deduplicated by content.
    strings: HashMap<Vec<u8>, DataId>,
}

impl<'a> FileLowering<'a> {
    fn signature_of(&self, params: &[Ty], ret: Ty) -> Result<Signature, Unsupported> {
        let mut signature = Signature::new(CallConv::SystemV);
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

    fn declare_functions(&mut self) -> Result<(), Unsupported> {
        for (index, function) in self.ir.functions.iter().enumerate() {
            if function.dispatch_receiver.is_some() {
                return Err(format!("an instance method `{}`", function.name));
            }
            let signature = self.signature_of(&function.params, function.ret)?;
            let id = self
                .module
                .declare_function(&self.symbols[index], Linkage::Export, &signature)
                .map_err(|error| format!("declaring `{}` ({error})", function.name))?;
            self.functions.push(id);
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

    fn define_function(&mut self, index: usize) -> Result<(), Unsupported> {
        let function = &self.ir.functions[index];
        let Some(body) = function.body else {
            return Err(format!("a body-less function `{}`", function.name));
        };
        let signature = self.signature_of(&function.params, function.ret)?;
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
                let mut body_lowering = BodyLowering {
                    file: self,
                    builder: &mut builder,
                    values: HashMap::new(),
                    result: carrier(function.ret),
                    returned: false,
                };
                // Parameters occupy the leading value slots.
                let params = body_lowering.builder.block_params(entry).to_vec();
                for (slot, (value, ty)) in params.iter().zip(function.params.iter()).enumerate() {
                    let variable = body_lowering.declare_value(slot as u32, *ty)?;
                    body_lowering.builder.def_var(variable, *value);
                }
                body_lowering.statement(body)?;
                if !body_lowering.returned {
                    // Falling off the end of a `Unit` function is a `return`.
                    if body_lowering.result != Carrier::Void {
                        return Err(format!(
                            "a non-`Unit` function `{}` that falls off its end",
                            function.name
                        ));
                    }
                    body_lowering.builder.ins().return_(&[]);
                }
            }
            builder.seal_all_blocks();
            builder.finalize(frontend_config);
        }
        self.module
            .define_function(self.functions[index], &mut context)
            .map_err(|error| format!("compiling `{}` ({error})", function.name))?;
        self.module.clear_context(&mut context);
        Ok(())
    }

    /// `kt_program_entry`: what the runtime's `_start` calls. Records the stack bottom for the
    /// collector, runs `main`, exits through the kernel — it never returns to `_start`.
    fn define_program_entry(&mut self, main_index: usize) -> Result<(), Unsupported> {
        let void = Signature::new(CallConv::SystemV);
        let entry_id = self
            .module
            .declare_function(PROGRAM_ENTRY, Linkage::Export, &void)
            .map_err(|error| format!("declaring `{PROGRAM_ENTRY}` ({error})"))?;
        let init = self.import("kt_runtime_init", &[Ty::obj("kotlin/Any")], Ty::Unit)?;
        let exit = self.import("kt_exit", &[Ty::Int], Ty::Unit)?;
        let main = self.functions[main_index];
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
            let main_ref = self.module.declare_func_in_func(main, builder.func);
            builder.ins().call(main_ref, &[]);
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

struct BodyLowering<'a, 'b, 'c> {
    file: &'b mut FileLowering<'a>,
    builder: &'b mut FunctionBuilder<'c>,
    /// Kotlin value slot → Cranelift variable and its declared type.
    values: HashMap<u32, (Variable, Ty)>,
    result: Carrier,
    /// Whether the current block has already been terminated by a `return`.
    returned: bool,
}

impl<'a, 'b, 'c> BodyLowering<'a, 'b, 'c> {
    fn declare_value(&mut self, slot: u32, ty: Ty) -> Result<Variable, Unsupported> {
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

    fn statement(&mut self, id: u32) -> Result<(), Unsupported> {
        match self.file.ir.expr(id).clone() {
            IrExpr::Block { stmts, value } => {
                for statement in stmts {
                    self.statement(statement)?;
                    if self.returned {
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
                        self.builder.ins().return_(&[]);
                    }
                    (Some(value), _) => {
                        let Some(value) = self.expression(value)? else {
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
                self.returned = true;
            }
            IrExpr::Variable {
                index, ty, init, ..
            } => {
                if carrier(ty) == Carrier::Void {
                    if let Some(init) = init {
                        self.expression(init)?;
                    }
                    return Ok(());
                }
                let variable = self.declare_value(index, ty)?;
                if let Some(init) = init {
                    let Some(value) = self.expression(init)? else {
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
                let Some(&(variable, _)) = self.values.get(&var) else {
                    return Err("an assignment to an undeclared local".to_string());
                };
                let Some(value) = self.expression(value)? else {
                    return Err("an assignment of a `Unit` value".to_string());
                };
                self.builder.def_var(variable, value);
            }
            _ => {
                self.expression(id)?;
            }
        }
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
                }
                match value {
                    Some(value) => self.expression(value),
                    None => Ok(None),
                }
            }
            IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion | IrTypeOp::Cast | IrTypeOp::CastNonNull,
                arg,
                type_operand,
            } => self.coerce(arg, type_operand),
            IrExpr::Call {
                callee,
                dispatch_receiver,
                args,
            } => self.call(&callee, dispatch_receiver, &args),
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
                let data = self.file.string_data(text.as_bytes())?;
                let global = self
                    .file
                    .module
                    .declare_data_in_func(data, self.builder.func);
                let pointer = self.builder.ins().symbol_value(types::I64, global);
                let length = self.builder.ins().iconst(types::I32, text.len() as i64);
                let make = self.file.import(
                    "kt_string_utf8",
                    &[Ty::obj("kotlin/Any"), Ty::Int],
                    Ty::String,
                )?;
                let make_ref = self.func_ref(make);
                let call = self.builder.ins().call(make_ref, &[pointer, length]);
                self.builder.inst_results(call)[0]
            }
        })
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
            IrExpr::TypeOp { type_operand, .. } => *type_operand,
            IrExpr::Block {
                value: Some(value), ..
            } => self.type_of(*value)?,
            IrExpr::Call { callee, .. } => match callee {
                Callee::Local(function) => self.file.ir.functions[*function as usize].ret,
                Callee::External { ret, .. } | Callee::Intrinsic { ret, .. } => *ret,
                _ => return None,
            },
            _ => return None,
        })
    }

    /// A representation change between carriers: boxing a scalar into a reference, unboxing one
    /// out, or widening/narrowing between scalars.
    fn coerce(&mut self, arg: u32, target: Ty) -> Result<Option<Value>, Unsupported> {
        let Some(value) = self.expression(arg)? else {
            return Ok(None);
        };
        let target = carrier(target);
        let source = self.type_of(arg).map(carrier);
        match (source, target) {
            (None, _) | (Some(Carrier::Ref), Carrier::Ref) => Ok(Some(value)),
            (Some(source), target) if source == target => Ok(Some(value)),
            (Some(Carrier::Scalar(_, _)), Carrier::Ref) => {
                let ty = self.type_of(arg).expect("known scalar");
                let Some(suffix) = box_suffix(ty) else {
                    return Err(format!(
                        "a `{}` in a position that requires a reference (the runtime cannot render \
                         a floating-point value)",
                        if ty == Ty::Float { "Float" } else { "Double" }
                    ));
                };
                let boxer =
                    self.file
                        .import(&format!("kt_box_{suffix}"), &[ty], Ty::obj("kotlin/Any"))?;
                let boxer_ref = self.func_ref(boxer);
                let call = self.builder.ins().call(boxer_ref, &[value]);
                Ok(Some(self.builder.inst_results(call)[0]))
            }
            (Some(Carrier::Ref), Carrier::Scalar(_, _)) => {
                let ty = target_ty_of(target).expect("scalar carrier");
                let Some(suffix) = box_suffix(ty) else {
                    return Err("an unboxing to a floating-point value".to_string());
                };
                let unboxer = self.file.import(
                    &format!("kt_unbox_{suffix}"),
                    &[Ty::obj("kotlin/Any")],
                    ty,
                )?;
                let unboxer_ref = self.func_ref(unboxer);
                let call = self.builder.ins().call(unboxer_ref, &[value]);
                Ok(Some(self.builder.inst_results(call)[0]))
            }
            (Some(Carrier::Scalar(from, signed)), Carrier::Scalar(to, _)) => {
                Ok(Some(if from.bits() < to.bits() {
                    if signed {
                        self.builder.ins().sextend(to, value)
                    } else {
                        self.builder.ins().uextend(to, value)
                    }
                } else {
                    self.builder.ins().ireduce(to, value)
                }))
            }
            (Some(_), Carrier::Void) => Ok(None),
            (Some(Carrier::Void), _) => Err("a coercion from `Unit`".to_string()),
        }
    }

    fn call(
        &mut self,
        callee: &Callee,
        dispatch_receiver: Option<u32>,
        args: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        match callee {
            Callee::Local(function) => {
                if dispatch_receiver.is_some() {
                    return Err("a local call with a receiver".to_string());
                }
                let arguments = self.arguments(args)?;
                let id = self.file.functions[*function as usize];
                let func_ref = self.func_ref(id);
                let call = self.builder.ins().call(func_ref, &arguments);
                Ok(self.builder.inst_results(call).first().copied())
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
                if dispatch_receiver.is_some() {
                    return Err(format!("the member `{}.{name}`", owner.replace('/', ".")));
                }
                let Some(symbol) =
                    super::super::intrinsics::runtime_function(&owner, &name, params)
                else {
                    return Err(format!(
                        "the declaration `{}.{name}`",
                        owner.replace('/', ".")
                    ));
                };
                let arguments = self.arguments(args)?;
                let id = self.file.import(&symbol, params, *ret)?;
                let func_ref = self.func_ref(id);
                let call = self.builder.ins().call(func_ref, &arguments);
                Ok(self.builder.inst_results(call).first().copied())
            }
            other => Err(format!("a {} call", callee_kind(other))),
        }
    }

    fn arguments(&mut self, args: &[u32]) -> Result<Vec<Value>, Unsupported> {
        let mut values = Vec::with_capacity(args.len());
        for argument in args {
            let Some(value) = self.expression(*argument)? else {
                return Err("a `Unit` argument".to_string());
            };
            values.push(value);
        }
        Ok(values)
    }
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

/// The runtime's box/unbox suffix. `None` for floating point: the runtime has no box for them on
/// purpose, because it cannot render one (see `src/native/runtime/krusty_rt.c`).
fn box_suffix(ty: Ty) -> Option<&'static str> {
    Some(match ty {
        Ty::Boolean => "boolean",
        Ty::Byte => "byte",
        Ty::Short => "short",
        Ty::Char => "char",
        Ty::Int => "int",
        Ty::Long => "long",
        _ => return None,
    })
}

/// One symbol per IR function, unique within the file — the scheme the rest of the native track
/// already uses (`kt_<package>_<name>`, with a suffix on a repeat).
pub(super) fn function_symbols(ir: &IrFile) -> Vec<String> {
    let package = ir
        .package
        .as_deref()
        .map(c_identifier)
        .filter(|package| !package.is_empty());
    let mut taken = std::collections::HashSet::new();
    ir.functions
        .iter()
        .map(|function| {
            let base = match &package {
                Some(package) => format!("kt_{package}_{}", c_identifier(&function.name)),
                None => format!("kt_{}", c_identifier(&function.name)),
            };
            let mut candidate = base.clone();
            let mut ordinal = 0;
            while !taken.insert(candidate.clone()) {
                ordinal += 1;
                candidate = format!("{base}__{ordinal}");
            }
            candidate
        })
        .collect()
}

fn c_identifier(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
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
    let debug = format!("{node:?}");
    let head = debug
        .split(|c: char| !c.is_ascii_alphanumeric())
        .find(|piece| !piece.is_empty())
        .unwrap_or("expression");
    format!("`{head}`")
}
