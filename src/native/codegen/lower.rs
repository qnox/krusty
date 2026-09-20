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

mod arithmetic;
mod arrays;
mod boxed;
mod classes_literal;
mod defaults;
mod enums;
mod exceptions;
use arithmetic::{arithmetic_result, scalar_bound};
mod functions;
mod lists;
mod objects;
mod ranges;
mod references;
mod scope;
mod statics;
mod strings;
mod unsigned;

use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{
    types, AbiParam, Block, BlockArg, InstBuilder, Signature, StackSlotData, StackSlotKind,
    TrapCode,
};
use cranelift_codegen::ir::{FuncRef, Inst, Type, Value};
use cranelift_codegen::isa::CallConv;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};

use crate::ir::{
    Callee, ClassId, FunId, IrBinOp, IrCheckedOperation, IrConst, IrExpr, IrFile, IrIntrinsic,
    IrLocalPropertyLayout, IrStatic, IrTypeOp,
};
use crate::libraries::SemanticPlatform;
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
        // Kotlin's unsigned integers are value classes, and common lowering erases each to the
        // signed machine integer it wraps — the right machine shape, and the wrong one to reason
        // about: `4294967295u` is that `Int`'s bits and not its value. The carrier keeps both
        // facts, so every widening zero-extends and every comparison and division asks the unsigned
        // question. What a carrier cannot keep is the TYPE, which is why a boxed one gets a
        // descriptor of its own: `1u as? Int` must fail, and a boxed `UInt` must render unsigned.
        Ty::UByte => Carrier::Scalar(types::I8, false),
        Ty::UShort => Carrier::Scalar(types::I16, false),
        Ty::UInt => Carrier::Scalar(types::I32, false),
        Ty::ULong => Carrier::Scalar(types::I64, false),
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
    provider: &Rc<dyn SemanticPlatform>,
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
        provider,
        module: &mut module,
        symbols: model::symbols(ir, super::super::linker::runtime_symbols(target.arch)),
        model: class_model,
        functions: Vec::new(),
        imports: HashMap::new(),
        data_imports: HashMap::new(),
        strings: HashMap::new(),
        string_objects: HashMap::new(),
        classes: Vec::new(),
        accessors: HashMap::new(),
        statics: Vec::new(),
        lambdas: HashMap::new(),
        default_wrappers: HashMap::new(),
        default_constructors: HashMap::new(),
        enum_entries: HashMap::new(),
        reference_identities: HashMap::new(),
        holders: HashMap::new(),
        references: HashMap::new(),
    };
    lowering.declare_functions()?;
    lowering.declare_classes()?;
    lowering.declare_statics()?;
    lowering.declare_lambdas()?;
    lowering.declare_default_wrappers()?;
    lowering.declare_default_constructors()?;
    lowering.declare_enum_entries()?;
    // Before any body is DEFINED, not after: a constructor is a body too, and a property
    // reference written in a class's initializer (`class A { val r = C::z }`) is lowered while
    // `define_classes` runs. Declaring these afterwards left exactly those sites unrealized.
    lowering.declare_property_references()?;
    lowering.declare_local_property_references()?;
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
    provider: &'a Rc<dyn SemanticPlatform>,
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
    /// The interned string OBJECT for each distinct literal text — one slot per text, filled on
    /// first use. Kotlin promises equal literals are the same object, so the text's bytes being
    /// shared (above) is not enough: the string built from them has to be shared too.
    string_objects: HashMap<Vec<u8>, DataId>,
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
    /// The emitted pieces of each property reference, by the expression that creates it.
    references: HashMap<u32, references::ReferenceItems>,
    /// One marker per (referenced declaration, bound-ness) this file mentions — the identity two
    /// callable references compare. Deduplicated here because two sites naming the same
    /// declaration must reach the SAME marker; that is the whole point of it.
    reference_identities: HashMap<String, DataId>,
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

    /// The signature of an IR function: a method takes its receiver first.
    fn function_signature(&self, id: crate::ir::FunId) -> Result<Signature, Unsupported> {
        let function = &self.ir.functions[id as usize];
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
        params.extend(functions::carried_parameters(self.ir, id));
        self.signature_of(&params, function.ret)
    }

    fn declare_functions(&mut self) -> Result<(), Unsupported> {
        for (index, function) in self.ir.functions.iter().enumerate() {
            // A lambda whose body returns NON-LOCALLY is valid only spliced into the caller it
            // returns from. Emitted as a standalone function its `return` returns from itself, and
            // the enclosing call then carries on — `with(1) { return … }` ran the block, ignored
            // the return and fell through, which is a wrong answer rather than a decline. Common
            // lowering marks these; leaving one undeclared is how a call site that names it comes
            // to decline, which is the same mechanism the branch below relies on.
            if self
                .ir
                .inline_only_fns
                .contains(&(index as crate::ir::FunId))
                || function.body.is_none()
            {
                // Nothing to emit, and therefore nothing to DECLARE: an exported symbol that is
                // never defined fails the object's own consistency check, not the link. Two shapes
                // reach here — an abstract method, whose vtable entry is the runtime's loud
                // failure, and a lambda the checked lowering spliced into an `inline` caller and
                // then cleared. Neither is called by name; a call site that somehow named one
                // finds no id and declines.
                self.functions.push(None);
                continue;
            }
            let signature = self.function_signature(index as crate::ir::FunId)?;
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
    /// The slot holding the interned object for one literal text; see [`BodyLowering::string_literal`].
    fn string_object_slot(&mut self, bytes: &[u8]) -> Result<DataId, Unsupported> {
        if let Some(id) = self.string_objects.get(bytes) {
            return Ok(*id);
        }
        let name = format!("kt_str_obj_{}", self.string_objects.len());
        let id = self
            .module
            .declare_data(&name, Linkage::Local, true, false)
            .map_err(|error| format!("declaring an interned string slot ({error})"))?;
        let mut description = DataDescription::new();
        description.define(vec![0u8; 8].into_boxed_slice());
        description.set_align(8);
        self.module
            .define_data(id, &description)
            .map_err(|error| format!("defining an interned string slot ({error})"))?;
        self.string_objects.insert(bytes.to_vec(), id);
        Ok(id)
    }

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
                    handlers: Vec::new(),
                    propagate: None,
                    finallys: Vec::new(),
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
                // After the body, because only the body knows whether any call was made: a frame
                // with nothing to propagate through never creates the block at all.
                body.seal_propagation();
            }
            builder.seal_all_blocks();
            builder.finalize(frontend_config);
        }
        // The emitted function, for when a compiled program answers wrongly and the IR that made it
        // reads correctly — the difference is then in this, and nothing else shows it.
        crate::trace_compiler!("native", "{name}\n{}", context.func.display());
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
        let signature = self.function_signature(index as crate::ir::FunId)?;
        // `this`, when there is one, is value slot 0 and the parameters follow it.
        let mut slots: Vec<Ty> = Vec::with_capacity(function.params.len() + 1);
        if let Some(owner) = function.dispatch_receiver {
            slots.push(Ty::Obj(owner, &[]));
        }
        slots.extend(functions::carried_parameters(
            self.ir,
            index as crate::ir::FunId,
        ));
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
        let uncaught = self.import("kt_check_uncaught", &[], Ty::Unit)?;
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
            // A `throw` nothing caught has left the exception pending and returned a zero value
            // all the way to here. Kotlin ends the program reporting it, which is what this does —
            // and it must happen BEFORE the answer is printed, because that zero is not an answer.
            let uncaught_ref = self.module.declare_func_in_func(uncaught, builder.func);
            builder.ins().call(uncaught_ref, &[]);
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
    /// Whether a `break` inside the body took the exit. A loop whose condition is never false and
    /// whose exit nothing jumps to is not left, which is what makes `while (true) { }` a `Nothing`.
    broken: bool,
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
    /// The `try` blocks the current position is inside, innermost last: each is the dispatch block
    /// that decides which of that `try`'s clauses, if any, matches the exception in flight.
    handlers: Vec<Block>,
    /// The block that leaves this frame with the exception still pending — created on first use,
    /// because a function whose body makes no call cannot observe one.
    propagate: Option<Block>,
    /// The `finally` blocks the current position sits inside, innermost last. Every way out of a
    /// `try` runs them, so `return`, `break` and `continue` consult this before jumping.
    finallys: Vec<PendingFinally>,
}

/// One `finally` the current position is inside.
pub(super) struct PendingFinally {
    /// The block's body, re-lowered at each exit rather than shared. That is what kotlinc emits
    /// too: the paths are disjoint, so a copy on each runs exactly once.
    pub(super) body: u32,
    /// How many loops were open when its `try` was entered. A `break` runs the finallys entered
    /// INSIDE the loop it leaves and no others, and this is what tells them apart.
    pub(super) loops_at_entry: usize,
    /// How many handlers were open when its `try` was entered. Running the block restores this
    /// depth first, because an exception raised INSIDE a `finally` leaves the `try` that finally
    /// belongs to: it does not reach that `try`'s own `catch` clauses, and it must not re-enter
    /// the same `finally`.
    pub(super) handlers_at_entry: usize,
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
        let call = self.emit_call(func_ref, arguments)?;
        Ok(self.builder.inst_results(call).first().copied())
    }

    /// The current block has been terminated: move to a block nothing reaches, so that anything
    /// the IR still puts after the terminator has somewhere to go without tripping the builder.
    fn terminate(&mut self) {
        self.terminated = true;
        let dead = self.builder.create_block();
        self.builder.switch_to_block(dead);
    }

    /// A checked bottom value: a producer whose Kotlin type is `Nothing` but which still has a
    /// physical fallthrough, which the target has to say something about.
    ///
    /// Common lowering decided WHAT once, by reading the producer rather than its spelling, and
    /// this realizes that decision in both positions rather than re-deciding by position. A call
    /// that genuinely answers `Nothing` has no honest continuation, so a path that reaches past it
    /// is a callee that lied and fails loudly. A generic result SUBSTITUTED to `Nothing` is a
    /// different thing wearing the same type: the call really did produce a value, kotlinc erases
    /// the result and lets execution carry on, and the value it carries on with is that one — so
    /// it is handed to whatever asked, and to nothing at all in statement position.
    fn bottom_value(&mut self, producer: u32, diverge: bool) -> Result<Option<Value>, Unsupported> {
        let value = self.expression(producer)?;
        if self.terminated || !diverge {
            return Ok(value);
        }
        self.runtime_call("kt_nothing_value_returned", &[], Ty::Unit, &[])?;
        self.builder.ins().trap(TrapCode::unwrap_user(3));
        self.terminate();
        Ok(None)
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
                        self.run_finallys_for_return()?;
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
                        self.run_finallys_for_return()?;
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
                        self.run_finallys_for_return()?;
                        if self.terminated {
                            return Ok(());
                        }
                        self.builder.ins().return_(&[value]);
                    }
                    (None, _) => {
                        self.run_finallys_for_return()?;
                        if self.terminated {
                            return Ok(());
                        }
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
            IrExpr::Checked(IrCheckedOperation::RangeLoop {
                variable,
                counter,
                operation,
                start,
                end,
                body,
                label,
            }) => self.counted_loop(variable, counter, operation, start, end, body, label)?,
            IrExpr::BottomValue {
                producer,
                completion,
            } => {
                self.bottom_value(producer, completion.diverges_when_discarded())?;
            }
            IrExpr::Break { label } => {
                let index = self.loop_index(label.as_deref(), "break")?;
                self.loops[index].broken = true;
                let target = self.loops[index].break_block;
                self.run_finallys_for_jump(index)?;
                if self.terminated {
                    return Ok(());
                }
                self.builder.ins().jump(target, &[]);
                self.terminate();
            }
            IrExpr::Continue { label } => {
                let index = self.loop_index(label.as_deref(), "continue")?;
                let target = self.loops[index].continue_block;
                self.run_finallys_for_jump(index)?;
                if self.terminated {
                    return Ok(());
                }
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
                        dispatch_receiver,
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

    /// Which loop a `break`/`continue` names, as an index. The index is what decides how many
    /// `finally` blocks the jump has to run through: those entered INSIDE this loop, and no more.
    fn loop_index(&self, label: Option<&str>, keyword: &str) -> Result<usize, Unsupported> {
        let found = match label {
            Some(label) => self
                .loops
                .iter()
                .rposition(|frame| frame.label.as_deref() == Some(label)),
            None => self.loops.len().checked_sub(1),
        };
        found.ok_or_else(|| format!("a `{keyword}` outside the loop it names"))
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
        // A condition that is never false takes the exit OUT of the graph rather than leaving it
        // unreachable in it: nothing branches there, so a loop no `break` leaves keeps the exit
        // pristine and it is dropped. Kotlin reads `while (true)` the same way, which is why a
        // body written like this types as `Nothing` and a function may declare it.
        let always = matches!(
            self.file.ir.expr(cond),
            IrExpr::Const(IrConst::Boolean(true))
        );

        self.builder
            .ins()
            .jump(if post_test { body_block } else { header }, &[]);

        self.continue_in(header);
        if always {
            self.builder.ins().jump(body_block, &[]);
        } else {
            let condition = self.expression(cond)?;
            if !self.terminated {
                let Some(condition) = condition else {
                    return Err("a loop condition of no value".to_string());
                };
                self.builder
                    .ins()
                    .brif(condition, body_block, &[], exit, &[]);
            }
        }

        self.loops.push(LoopFrame {
            label,
            break_block: exit,
            continue_block: update_block.unwrap_or(header),
            broken: false,
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
        let broken = self.loops.pop().is_some_and(|frame| frame.broken);

        // Nothing branches to the exit of a loop that is never false and never broken, so there is
        // no position after it to continue in.
        if always && !broken {
            self.terminate();
            return Ok(());
        }
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
    ///
    /// The checked SEMANTIC type sits beside every expression in common IR, and this is the one
    /// point every carried value passes through — so the two things that need saying about a type
    /// rather than about a node are said here, and cover every route in rather than the routes
    /// thought of one at a time.
    fn expression(&mut self, id: u32) -> Result<Option<Value>, Unsupported> {
        let logical = self.file.ir.logical_types.get(&id).copied();
        let value = self.lowered_expression(id)?;
        let (Some(value), Some(logical)) = (value, logical) else {
            return Ok(value);
        };
        // A `UByte` or `UShort` arrives already widened into an `Int` — the erasure's doing — with
        // the checked type saying how narrow it really is. Narrowing it back here is what keeps the
        // value and its type agreeing from this point on, and reading a `UShort` out of an `Int` is
        // exactly what makes `UShort.MAX_VALUE` print `-1`.
        //
        // Only where the value is a SCALAR. A boxed one is a pointer, which is an `I64` like a
        // `Long` and would be truncated by the very narrowing that fixes the unboxed case: `val any:
        // Any = 7u` carries a reference whose checked type is still `UInt`.
        if !logical.is_unsigned() || !self.carried_as_scalar(id) {
            return Ok(Some(value));
        }
        let Some(narrow) = carrier(logical).clif() else {
            return Ok(Some(value));
        };
        let actual = self.builder.func.dfg.value_type(value);
        if actual.is_int() && narrow.is_int() && actual.bits() > narrow.bits() {
            return Ok(Some(self.builder.ins().ireduce(narrow, value)));
        }
        Ok(Some(value))
    }

    /// Does this expression's machine shape hold the value itself rather than a pointer to it?
    fn carried_as_scalar(&self, id: u32) -> bool {
        self.physical_type_of(id)
            .is_some_and(|ty| matches!(carrier(ty), Carrier::Scalar(..)))
    }

    fn lowered_expression(&mut self, id: u32) -> Result<Option<Value>, Unsupported> {
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
            IrExpr::BottomValue {
                producer,
                completion,
            } => self.bottom_value(producer, completion.diverges_when_discarded()),
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
            // `declaration` separates the classifier's own `E.valueOf(name)` from the standard
            // library's INLINE `enumValueOf<E>(name)`. Both name the same lookup by entry name, and
            // the two differ only in what a consumer that records SOURCE POSITIONS attributes an
            // inline expansion to. This generator records none, so the lookup it emits is the same
            // one either way; the distinction is read where it is meaningful, not repeated here.
            IrExpr::EnumValueOf {
                classifier,
                arg,
                declaration: _,
            } => self.enum_value_of(classifier, arg),
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
            // `p.name`: a checked read of a dependency property whose receiver is a property
            // reference, answered out of the reference's own table.
            IrExpr::Checked(IrCheckedOperation::ExternalPropertyRead {
                target,
                receiver: Some(receiver),
                ..
            }) if self.reference_property(target, receiver).is_some() => self
                .reference_property(target, receiver)
                .expect("checked by the guard"),
            // `k.simpleName` / `k.qualifiedName`: the descriptor's own Kotlin name.
            IrExpr::Checked(IrCheckedOperation::ExternalPropertyRead {
                target,
                receiver: Some(receiver),
                ..
            }) if self.class_name_accessor(target).is_some() => {
                let symbol = self
                    .class_name_accessor(target)
                    .expect("checked by the guard");
                self.class_name(symbol, receiver)
            }
            // `e.message`: the one field a runtime `Throwable` carries.
            IrExpr::Checked(IrCheckedOperation::ExternalPropertyRead {
                target,
                receiver: Some(receiver),
                ..
            }) if self.is_throwable_message(target) => self.throwable_message(receiver),
            // `cs.length` where the receiver is typed `CharSequence`: a string, on this target.
            IrExpr::Checked(IrCheckedOperation::ExternalPropertyRead {
                target,
                receiver: Some(receiver),
                ..
            }) if self.is_char_sequence_length(target) => self.char_sequence_length(receiver),
            // `x.indices` is `0..size - 1` of the receiver, so it needs the receiver's own size
            // rather than anything the property declaration says.
            IrExpr::Checked(IrCheckedOperation::ExternalPropertyRead {
                target,
                receiver: Some(receiver),
                ..
            }) if self.external_getter_is_indices(target) => self.indices(receiver),
            // `range.first` and its three siblings: a checked read of a dependency property whose
            // getter the runtime answers, exactly as an explicit call to it would be.
            IrExpr::Checked(IrCheckedOperation::ExternalPropertyRead {
                target,
                receiver: Some(receiver),
                ..
            }) if self.range_getter(target).is_some() => {
                let (owner, name, ret) = self.range_getter(target).expect("checked by the guard");
                self.range_member(&owner, &name, receiver, &[], ret)
                    .expect("a range member, by the guard")
            }
            // `p.first`: the same runtime answer an explicit call to the getter would get.
            IrExpr::Checked(IrCheckedOperation::ExternalPropertyRead {
                target,
                receiver: Some(receiver),
                ..
            }) if self.lazy_getter(target, receiver).is_some()
                || self.pair_getter(target, receiver).is_some() =>
            {
                let name = self
                    .lazy_getter(target, receiver)
                    .or_else(|| self.pair_getter(target, receiver))
                    .expect("checked by the guard");
                match self.list_member(&name, receiver, &[], any()) {
                    Some(realized) => realized,
                    None => Err(format!("`{name}` of a receiver that answers none of it")),
                }
            }
            // `values.size`: the same runtime answer an explicit call to the getter would get,
            // and reached the same way — by the receiver, not by the property's declaration.
            IrExpr::Checked(IrCheckedOperation::ExternalPropertyRead {
                target,
                receiver: Some(receiver),
                ..
            }) if self.indexed_value_getter(target, receiver).is_some() => {
                let name = self
                    .indexed_value_getter(target, receiver)
                    .expect("checked by the guard");
                let answer = lists::indexed_value_getter_ty(&name);
                match self.list_member(&name, receiver, &[], answer) {
                    Some(realized) => realized,
                    None => Err(format!(
                        "`{name}` of a receiver that is not an indexed value"
                    )),
                }
            }
            IrExpr::Checked(IrCheckedOperation::ExternalPropertyRead {
                target,
                receiver: Some(receiver),
                ..
            }) if self.list_getter(target, receiver).is_some() => {
                let name = self
                    .list_getter(target, receiver)
                    .expect("checked by the guard");
                match self.list_member(&name, receiver, &[], Ty::Int) {
                    Some(realized) => realized,
                    None => Err(format!("`{name}` of a receiver that is not a list")),
                }
            }
            // `::foo.name` — `KCallable.name` of a reference written right here, which is the
            // DECLARATION's own name and therefore known already.
            IrExpr::Checked(IrCheckedOperation::ExternalPropertyRead {
                target,
                receiver: Some(receiver),
                ..
            }) if self.callable_reference_name(target, receiver).is_some() => {
                let name = self
                    .callable_reference_name(target, receiver)
                    .expect("checked by the guard");
                self.callable_name(receiver, &name)
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
                        dispatch_receiver,
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
            IrExpr::Checked(IrCheckedOperation::PropertyReference { .. }) => {
                self.property_reference(id)
            }
            IrExpr::LocalPropertyReference { .. } => self.local_property_reference(id),
            IrExpr::KClassLiteral { classifier, value } => self.class_literal(classifier, value),
            IrExpr::Checked(IrCheckedOperation::RangeConstruction {
                operation,
                start,
                start_type,
                end,
                end_type,
                result,
            }) => self.range_construction(operation, start, start_type, end, end_type, result),
            // `x in a..b`: the checker kept the bounds rather than a range, so nothing is built.
            IrExpr::Checked(IrCheckedOperation::RangeContains {
                operation,
                value,
                start,
                end,
                negated,
                counter,
            }) => self.range_contains(operation, value, start, end, negated, counter),
            IrExpr::LateinitCheck { operand, name } => self.lateinit_check(operand, &name),
            IrExpr::Throw { operand } => self.throw(operand),
            IrExpr::Try {
                body,
                catches,
                finally,
                result,
            } => self.try_catch(body, &catches, finally, result),
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
            other => Err(self.describe_declined(&other)),
        }
    }

    /// The declining phrase for a node this generator does not lower.
    ///
    /// `describe` reads the node's shape and nothing else, which is right for most of them. A
    /// dependency PROPERTY is the exception: every one arrives as the same
    /// `Checked(ExternalPropertyRead)`, so the backlog lumped eighteen unrelated properties —
    /// `Double.Companion.MAX_VALUE`, `System.out`, `UIntArray.indices` — under one row and could
    /// not be worked from, which is exactly what `describe`'s own comment warns against. The
    /// provider knows the owner and the name, so they are stated the way a declining CALL already
    /// states its callee.
    fn describe_declined(&self, node: &IrExpr) -> String {
        let named = match node {
            IrExpr::Checked(IrCheckedOperation::ExternalPropertyRead { target, .. }) => self
                .external_property_name(*target)
                .map(|name| ("read", name)),
            IrExpr::Checked(IrCheckedOperation::ExternalPropertyWrite { target, .. }) => self
                .external_property_name(*target)
                .map(|name| ("write", name)),
            _ => None,
        };
        match named {
            Some((access, name)) => format!("a {access} of the property `{name}`"),
            // A target the provider cannot name is a different gap from an unimplemented
            // property, so it keeps the shape-only phrasing rather than borrowing a name it does
            // not have.
            None => describe(node),
        }
    }

    /// `owner.name` for a dependency property, as the provider records them.
    fn external_property_name(&self, target: crate::fir::ExternalPropertyId) -> Option<String> {
        let property = self.file.provider.external_property(target)?;
        let getter = self.file.provider.external_callable(property.getter)?;
        Some(format!(
            "{}.{}",
            getter.callable.owner.render(),
            property.name
        ))
    }

    fn constant(&mut self, constant: &IrConst) -> Result<Value, Unsupported> {
        Ok(match constant {
            IrConst::Boolean(value) => self.builder.ins().iconst(types::I8, i64::from(*value)),
            // Common IR hands over the unsigned VALUE and its checked type; the carrier is this
            // backend's to choose, and it carries an unsigned value as the machine integer its
            // value class wraps. So 200u is the 8-bit pattern that reads as -56 signed, which is
            // what every comparison, widening and `toString` in `unsigned.rs` already expects.
            IrConst::UByte(value) => self
                .builder
                .ins()
                .iconst(types::I8, i64::from(*value as i8)),
            IrConst::UShort(value) => self
                .builder
                .ins()
                .iconst(types::I16, i64::from(*value as i16)),
            IrConst::UInt(value) => self
                .builder
                .ins()
                .iconst(types::I32, i64::from(*value as i32)),
            IrConst::ULong(value) => self.builder.ins().iconst(types::I64, *value as i64),
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
    /// One of `kotlin.test`'s assertions; see `intrinsics::assertion_call`.
    ///
    /// The message is a `String?` the caller may leave out, and the runtime takes `null` for the
    /// form without one — so the shape is fixed and only its last operand varies.
    fn assertion(
        &mut self,
        symbol: &str,
        compared: usize,
        args: &[u32],
        params: &[Ty],
    ) -> Result<Option<Value>, Unsupported> {
        // `assertTrue`/`assertFalse` ask about a `Boolean` and take it as one; only the equality
        // operands are compared structurally and therefore cross boxed.
        let value = match params.first() {
            Some(&Ty::Boolean) => Ty::Boolean,
            _ => Ty::nullable(Ty::obj("kotlin/Any")),
        };
        // Whether a message was WRITTEN, which the declaration cannot say: `message` is defaulted,
        // so a call that leaves it out still names the declaration that has it.
        let has_message = args.len() > compared;
        if args.len() < compared {
            return Err(format!(
                "a `kotlin.test` assertion missing an operand (`{symbol}`)"
            ));
        }
        let mut operands = Vec::with_capacity(compared + 1);
        let mut carried = Vec::with_capacity(compared + 1);
        for argument in &args[..compared] {
            let Some(operand) = self.coerce(*argument, value)? else {
                return Ok(None);
            };
            operands.push(operand);
            carried.push(value);
        }
        let message = if has_message {
            match self.coerce(args[compared], Ty::nullable(Ty::String))? {
                Some(text) => text,
                None => return Ok(None),
            }
        } else {
            // Kotlin's `null` message: the form written without one.
            self.builder.ins().iconst(types::I64, 0)
        };
        if self.terminated {
            return Ok(None);
        }
        operands.push(message);
        carried.push(any());
        self.runtime_call(symbol, &carried, Ty::Unit, &operands)
    }

    /// A string literal, INTERNED: equal literals are one object, which is Kotlin's promise and
    /// observable through `===`. Building one where it is written would answer `false` for
    /// `"a" === "a"`, and would answer it for a function returning a literal too — every call
    /// allocating a new string.
    ///
    /// So the bytes get a slot alongside them, one per distinct text, and the runtime fills it on
    /// first use. The slot is the interning table: there is no lookup, because the generator
    /// already knows which literals are the same text.
    fn string_literal(&mut self, bytes: &[u8]) -> Result<Value, Unsupported> {
        let data = self.file.string_data(bytes)?;
        let slot = self.file.string_object_slot(bytes)?;
        let global = self
            .file
            .module
            .declare_data_in_func(data, self.builder.func);
        let pointer = self.builder.ins().symbol_value(types::I64, global);
        let length = self.builder.ins().iconst(types::I32, bytes.len() as i64);
        let slot = self
            .file
            .module
            .declare_data_in_func(slot, self.builder.func);
        let slot = self.builder.ins().symbol_value(types::I64, slot);
        let string = self.runtime_call(
            "kt_string_literal",
            &[Ty::obj("kotlin/Any"), Ty::Int, Ty::obj("kotlin/Any")],
            Ty::String,
            &[pointer, length, slot],
        )?;
        Ok(string.expect("`kt_string_literal` returns a string"))
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
    /// The type an expression's value has: how wide it is, and how to read the bits in it.
    ///
    /// The node shapes answer the first — a value class has the shape of what it wraps, which is
    /// what common lowering erased it to. The checked type beside the expression answers the second,
    /// and for an UNSIGNED value the difference is not cosmetic: a `UInt` boxed as the `Int` sharing
    /// its bits prints `-1879048193` for `0x8fffffffU`, and one compared as that `Int` answers
    /// `0uL >= ULong.MAX_VALUE` true.
    fn type_of(&self, id: u32) -> Option<Ty> {
        let physical = self.physical_type_of(id)?;
        let logical = self.file.ir.logical_types.get(&id).copied();
        // The checked type wins whenever it is unsigned, the value is a SCALAR rather than a
        // pointer to one, and it is no WIDER than that scalar: `expression` has narrowed the value
        // to it, so the two agree. Wider would be a claim about bits that are not there, and a
        // pointer is not the value at all — `val any: Any = 7u` is still checked as `UInt`.
        let bits = |ty: Ty| carrier(ty).clif().map(|clif| clif.bits());
        match logical {
            Some(logical)
                if logical.is_unsigned()
                    && matches!(carrier(physical), Carrier::Scalar(..))
                    && bits(logical) <= bits(physical) =>
            {
                Some(logical)
            }
            _ => Some(physical),
        }
    }

    /// The type of a block's value when that value is a local the block itself DECLARES.
    ///
    /// `Self::physical_type_of` answers a `GetValue` from the slot map, and that map is a lowering
    /// artifact: a slot is in it once its declaring statement has been emitted. Typing is asked
    /// EARLIER than that — a `when` types itself before lowering any arm, to know whether its
    /// merge block carries a value — so a branch ending in a local it declares had no type, the
    /// whole `when` typed as no-value, and every arm was lowered as a statement. The value went
    /// nowhere and the destination read zero.
    ///
    /// Nothing about that is specific to what the block computes; it is any branch spliced from an
    /// inline function, which is how `Array(n) { … }` arrives. The IR knows the answer — the
    /// declaring `IrExpr::Variable` carries the type — so this asks the IR rather than the map.
    fn declared_in(&self, stmts: &[u32], value: u32) -> Option<Ty> {
        let IrExpr::GetValue(slot) = self.file.ir.expr(value) else {
            return None;
        };
        stmts.iter().rev().find_map(|&statement| {
            match self.file.ir.expr(statement) {
                IrExpr::Variable { index, ty, .. } if index == slot => Some(*ty),
                // A splice may nest another block around the declaration.
                IrExpr::Block { stmts, .. } => self.declared_in(stmts, value),
                _ => None,
            }
        })
    }

    /// The machine shape an expression lowers to, read from the node itself.
    fn physical_type_of(&self, id: u32) -> Option<Ty> {
        Some(match self.file.ir.expr(id) {
            IrExpr::Const(constant) => match constant {
                IrConst::Boolean(_) => Ty::Boolean,
                // The unsigned identity common IR retained. Answering the signed type here would
                // put every such constant back on the signed reading it was separated from.
                IrConst::UByte(_) => Ty::UByte,
                IrConst::UShort(_) => Ty::UShort,
                IrConst::UInt(_) => Ty::UInt,
                IrConst::ULong(_) => Ty::ULong,
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
                stmts,
                value: Some(value),
            } => self
                .type_of(*value)
                .or_else(|| self.declared_in(stmts, *value))?,
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
            IrExpr::PrimitiveBinOp { op, lhs, rhs, .. } => match op {
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
                // An operand of a bounded type parameter is read through its bound: the operator
                // unboxes it, so the RESULT is that primitive and not the reference the
                // declaration spells — a caller told otherwise would skip the boxing the next
                // parameter needs, and the mismatch reaches the verifier, or worse.
                _ => arithmetic_result(
                    *op,
                    scalar_bound(self.type_of(*lhs)?)?,
                    self.type_of(*rhs).and_then(scalar_bound),
                ),
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
            IrExpr::Checked(IrCheckedOperation::ExternalPropertyRead {
                target, receiver, ..
            }) => {
                // `cs.length` is an `Int`, and `::foo.name` a `String`: both are the language's
                // own types, stated here for the same reason `kotlin.Enum`'s two are.
                if self.is_char_sequence_length(*target) {
                    return Some(Ty::Int);
                }
                if self.class_name_accessor(*target).is_some() {
                    return Some(Ty::nullable(Ty::String));
                }
                if let Some(receiver) = receiver {
                    if self.callable_reference_name(*target, *receiver).is_some() {
                        return Some(Ty::String);
                    }
                }
                match self.enum_member_name(*target) {
                    Some("name") => Ty::String,
                    Some(_) => Ty::Int,
                    // A pair's components are references; a list's `size` is an `Int`; a range's
                    // own members answer at their element's width. Which of the three this read is
                    // depends on the receiver as much as on the getter — a range declares `first`
                    // too — so the receiverless read is none of them.
                    None => match receiver {
                        Some(receiver)
                            if self.lazy_getter(*target, *receiver).is_some()
                                || self.pair_getter(*target, *receiver).is_some() =>
                        {
                            any()
                        }
                        Some(receiver)
                            if self.indexed_value_getter(*target, *receiver).is_some() =>
                        {
                            let name = self
                                .indexed_value_getter(*target, *receiver)
                                .expect("checked by the guard");
                            lists::indexed_value_getter_ty(&name)
                        }
                        Some(receiver) if self.list_getter(*target, *receiver).is_some() => Ty::Int,
                        _ => self.range_getter(*target)?.2,
                    },
                }
            }
            // A constructed range is an object of the type the checker gave it, which is what makes
            // `1..3 == r` recognisable as an equality between references rather than a guess.
            IrExpr::Checked(IrCheckedOperation::RangeConstruction { result, .. }) => *result,
            // A property reference is an object of the reflection type the checker gave it. When
            // the node carries none, the interface every one of them wears answers the two
            // questions asked of this — that it is a reference, and that its members are the
            // reference machinery's.
            // A local delegated property's metadata wears the interface Kotlin gives it, which is
            // what sends its `name` through the reference machinery.
            IrExpr::LocalPropertyReference { .. } => Ty::obj("kotlin/reflect/KProperty"),
            // A class literal is an object of the reflection type Kotlin gives it, which is what
            // makes an equality between two of them an equality between references.
            IrExpr::KClassLiteral { .. } => classes_literal::kclass(),
            IrExpr::Checked(IrCheckedOperation::PropertyReference { mutable, .. }) => self
                .file
                .ir
                .logical_types
                .get(&id)
                .copied()
                .unwrap_or_else(|| {
                    Ty::obj(if *mutable {
                        "kotlin/reflect/KMutableProperty"
                    } else {
                        "kotlin/reflect/KProperty"
                    })
                }),
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
        let target_ty = target;
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
                // The TARGET's own type, not one recovered from its carrier: a carrier cannot tell
                // a `UByte` from a `Boolean`, and each unboxes through its own descriptor.
                let ty = target_ty.non_null();
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
                let params = functions::carried_parameters(self.file.ir, *function);
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
                let call = self.emit_call(func_ref, &arguments)?;
                Ok(self.builder.inst_results(call).first().copied())
            }
            Callee::Super {
                owner,
                name,
                kind,
                source,
                params,
                ..
            } => {
                let Some(receiver) = dispatch_receiver else {
                    return Err(format!("a `super` call without a receiver (`{name}`)"));
                };
                self.direct_call(*owner, name, *kind, *source, Some(params), receiver, args)
            }
            Callee::Virtual {
                owner,
                name,
                params,
                ..
            } => {
                let Some(receiver) = dispatch_receiver else {
                    return Err(format!("a virtual call without a receiver (`{name}`)"));
                };
                self.virtual_call(*owner, name, params.as_ref(), receiver, args)
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
                // A diamond `super.f()` to a superinterface's DEFAULT METHOD. `Callee::Special`
                // carries no accessor kind because it never names one.
                self.direct_call(
                    *owner,
                    name,
                    crate::ir::IrSuperCallKind::Function,
                    *source,
                    None,
                    receiver,
                    args,
                )
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
                let Some(realization) = self.file.provider.external_callable(*target) else {
                    return Err("an unresolvable dependency call".to_string());
                };
                let owner = realization.callable.owner.render();
                // The Kotlin name the declaration PUBLISHES, not the spelling it is realized under.
                // A physical name is an emit handle: a JVM realization may RENAME a member, and
                // where the signature mentions a value class kotlinc appends a hash of the erasure
                // (`UInt.compareTo` is realized as `compareTo-WZ4Q5Ns`). Neither is recoverable
                // from the spelling, and neither has to be: the contract carries the Kotlin name
                // beside it. Where it does not, the member declines rather than being guessed at.
                let name = realization
                    .callable
                    .reflection_name
                    .clone()
                    .unwrap_or_else(|| realization.callable.name.clone());
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
                        // `x.isNaN()` and its two siblings are one comparison each. Realizing them
                        // here rather than in the runtime keeps the operand unboxed — the member
                        // path below crosses everything as a reference, which for a `Double` would
                        // mean allocating a box to ask a question about its bits.
                        if let Some(predicate) =
                            super::super::intrinsics::float_predicate(&owner, &name)
                        {
                            return self.float_predicate(predicate, receiver);
                        }
                        // A range object's members are the runtime's, and `contains` is why they
                        // are not table entries below: that path crosses every argument as a
                        // reference, which would box the very `Int` the question is about.
                        if let Some(realized) =
                            self.range_member(&owner, &name, receiver, args, *ret)
                        {
                            return realized;
                        }
                        // The unsigned integers: a value class the erasure made look like the
                        // signed number sharing its bits, so every member where that difference
                        // shows is answered on purpose rather than by the signed instruction.
                        if let Some(realized) =
                            self.unsigned_member(&owner, &name, params, *ret, receiver, args)
                        {
                            return realized;
                        }
                        // A property reference answers its own members through its own table.
                        if let Some(realized) = self.reference_member(&name, receiver, args, *ret) {
                            return realized;
                        }
                        // `val x by ::top`: the stdlib's delegate operators on a reference, which
                        // are `get`/`set` under another name and reach the same table.
                        if let Some(realized) =
                            self.reference_delegate(&owner, &name, receiver, args, *ret)
                        {
                            return realized;
                        }
                        // A `listOf` result and the iterator it answers with. The PHYSICAL
                        // parameters go with it: they are what tells `removeAt` from `remove`,
                        // and the semantic ones cannot — see `lists::list_symbol`.
                        let physical = realization.callable.physical_params.clone();
                        if let Some(realized) =
                            self.list_member_declared(&name, receiver, args, *ret, &physical)
                        {
                            return realized;
                        }
                        // `a to b`: an extension of the tuples facade, so its left operand is the
                        // receiver here rather than an argument.
                        if let Some(realized) =
                            self.pair_construction(&owner, &name, receiver, args)
                        {
                            return realized;
                        }
                        // `x++` where `x` is an `Int?`: the member is the primitive's, and so is
                        // the value, whatever it arrived carried as.
                        if let Some(realized) = self.boxed_step(&owner, &name, params, receiver) {
                            return realized;
                        }
                        // `42.toUInt()`: a SIGNED receiver converted to an unsigned type.
                        // Nothing is called — Kotlin defines the conversion as the ordinary signed
                        // one to the target's width with those bits reinterpreted, and this
                        // backend already carries an unsigned value as the machine integer it
                        // wraps, so the reinterpretation is not an operation at all.
                        //
                        // The receiver is read at ITS OWN type, which is what makes the answer
                        // right: `convert` resizes by the SOURCE's signedness, so a negative
                        // `Int` widening to `ULong` sign-extends (kotlinc answers
                        // 18446744073709551615, not 4294967295) while a wide source narrowing
                        // truncates. Only an integer source is taken; a float one saturates
                        // instead and `unsigned_conversion` says why it is not here.
                        if let Some(target) =
                            super::super::intrinsics::unsigned_conversion(&owner, &name)
                        {
                            let source = self.type_of(receiver).map(Ty::non_null);
                            if let Some(source @ (Ty::Byte | Ty::Short | Ty::Int | Ty::Long)) =
                                source
                            {
                                let Some(value) = self.coerce(receiver, source)? else {
                                    return Ok(None);
                                };
                                if self.terminated {
                                    return Ok(None);
                                }
                                let Some(produced) = self.convert(value, Some(source), target)?
                                else {
                                    return Ok(None);
                                };
                                return self.convert(produced, Some(target), *ret);
                            }
                        }
                        // A member that asks about a NUMBER rather than an object, carried as one:
                        // `s[i]` must not box its index to reach the runtime.
                        if let Some((symbol, carried, answer)) =
                            super::super::intrinsics::scalar_member(&owner, &name, params)
                        {
                            let mut arguments = vec![self.reference(receiver)?];
                            for (argument, ty) in args.iter().zip(&carried[1..]) {
                                let Some(value) = self.coerce(*argument, *ty)? else {
                                    return Ok(None);
                                };
                                arguments.push(value);
                            }
                            if self.terminated {
                                return Ok(None);
                            }
                            let produced =
                                self.runtime_call(symbol, &carried, answer, &arguments)?;
                            // The table says what the runtime function PHYSICALLY answers; the
                            // call node says what the site expects. Reconciling the two is this
                            // boundary's job rather than something to assume: they are different
                            // sources and nothing here made them agree.
                            //
                            // They disagree today for `Number.toByte`/`toShort`, which the
                            // provider provider types `Int` because `desc_to_ty` reads the JVM
                            // descriptors `B` and `S` as `Int` — a core defect with its own fix,
                            // invisible to the JVM backend because a byte and an int share a stack
                            // slot there. Without this, the i8 the runtime answers reaches a box
                            // helper that takes an i32 and Cranelift's verifier rejects the
                            // function. When the provider is fixed the coercion becomes a no-op.
                            let Some(produced) = produced else {
                                return Ok(None);
                            };
                            return self.convert(produced, Some(answer), *ret);
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
                        // A vararg parameter is PHYSICALLY an array however its element type was
                        // substituted, which is the one fact that separates `listOf`'s two
                        // declarations; see `list_construction`.
                        let packs_a_vararg = realization
                            .callable
                            .physical_params
                            .first()
                            .is_some_and(|ty| ty.non_null().is_reference_array());
                        if let Some(realized) =
                            self.list_construction(&owner, &name, packs_a_vararg, args)
                        {
                            return realized;
                        }
                        if let Some(realized) = self.lazy_construction(&owner, &name, args) {
                            return realized;
                        }
                        // `kotlin.test`'s assertions. Their operands cross as REFERENCES rather
                        // than at their own widths: `assertEquals` is generic, so a call with
                        // `Int` arguments arrives typed `Int`, and the comparison Kotlin makes is
                        // `==` on whatever the values are.
                        if let Some((symbol, compared)) =
                            super::super::intrinsics::assertion_call(&owner, &name, params)
                        {
                            return self.assertion(symbol, compared, args, params);
                        }
                        // `assertFailsWith<T> { … }`. Not one of the comparisons above: it runs a
                        // block and answers what that block threw, so it is the `try` machinery
                        // rather than a runtime call.
                        if super::super::intrinsics::is_assert_fails_with(&owner, &name, params) {
                            return self.assert_fails_with(args, params, *ret);
                        }
                        // `run { … }` and `with(x) { … }`: the scope functions with no receiver to
                        // arrive on, so they reach this path rather than the member one.
                        if let Some(realized) =
                            self.top_level_scope_function(&owner, &name, args, params, *ret)
                        {
                            return realized;
                        }
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
            // `relational_operator` records that the call came from a `ComparisonCall`, which
            // lets a backend emit a branch instead of materializing the -1/0/1. This one answers
            // the same VALUE either way and leaves that to Cranelift, so the hint is ignored
            // rather than acted on.
            IrIntrinsic::PrimitiveCompare { operand, .. } => {
                let (Some(receiver), [argument]) = (receiver, args) else {
                    return Err("a malformed `compareTo`".to_string());
                };
                // The operand type named here is the erased one, which for an unsigned value is
                // the signed number sharing its bits — and ordering is exactly the question that
                // reads those bits differently. The receiver's own checked type decides.
                if let Some(element) = self
                    .type_of(receiver)
                    .map(Ty::non_null)
                    .filter(|ty| ty.is_unsigned())
                {
                    return self.unsigned_compare(element, receiver, *argument, ret);
                }
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
            // `"$u"`: the frontend names the conversion rather than letting the template reach for
            // `Any.toString()`, because the value it would reach for is the signed number sharing
            // the bits.
            IrIntrinsic::UnsignedToString { source } => {
                let Some(receiver) = receiver else {
                    return Err("a malformed unsigned `toString`".to_string());
                };
                let Some(realized) = self.unsigned_intrinsic_to_string(source.non_null(), receiver)
                else {
                    return Err(format!("an unsigned `toString` of `{source:?}`"));
                };
                realized
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
            // A floating-point value hashes by its BITS, which is what makes `NaN`'s hash a
            // number at all: `Float` gives its 32 directly, and `Double` folds its 64 exactly as
            // `Long` does. Reading the bits is a reinterpretation, not a conversion — a
            // conversion would round `NaN` to something and lose the very distinction the rule
            // exists for.
            Ty::Float => self.bits_of(operand, types::I32),
            Ty::Double => {
                let bits = self.bits_of(operand, types::I64);
                self.fold_to_int(bits)
            }
            other => return Err(format!("a data-class field of type `{other:?}`")),
        };
        Ok(hash)
    }

    /// A floating-point value's BITS as an integer of the same width.
    ///
    /// A reinterpretation, not a conversion: a conversion would round, and rounding `NaN` loses the
    /// very distinction the rules that ask for these bits exist to preserve. Cranelift's `bitcast`
    /// takes an endianness rather than the ordinary memory flags — the two widths are the same
    /// register here, so `little` names a reinterpretation with no swap on every target krusty
    /// emits for.
    fn bits_of(&mut self, value: Value, clif: Type) -> Value {
        // ONLY the endianness: `bitcast` rejects every other flag, and the alignment and
        // non-trapping bits `trusted` carries are a memory access's business, which this is not.
        let flags = cranelift_codegen::ir::MemFlagsData::new()
            .with_endianness(cranelift_codegen::ir::Endianness::Little);
        self.builder.ins().bitcast(clif, flags, value)
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
    /// field compares by its BITS, which is the opposite of what the comparison instruction
    /// answers in both directions: `NaN` equals `NaN`, and `0.0` does not equal `-0.0`.
    fn field_equals(
        &mut self,
        left: u32,
        right: u32,
        ty: Ty,
    ) -> Result<Option<Value>, Unsupported> {
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
        if carrier(ty) != Carrier::Ref {
            // `Double.equals` is not `==`: it reads the bits, so `NaN` equals itself and the two
            // zeroes are distinct. Comparing the reinterpreted integers is exactly that rule, and
            // it is the rule Kotlin's own `equals` states — `==` on the machine answers the other
            // way round on both of those values.
            if matches!(ty.non_null(), Ty::Float | Ty::Double) {
                let clif = if ty.non_null() == Ty::Float {
                    types::I32
                } else {
                    types::I64
                };
                let left = self.bits_of(left, clif);
                let right = self.bits_of(right, clif);
                return Ok(self.builder.ins().icmp(IntCC::Equal, left, right));
            }
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
/// `Any?`: the type every runtime reference parameter is declared as.
fn any() -> Ty {
    Ty::nullable(Ty::obj("kotlin/Any"))
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

/// The Kotlin type an arithmetic operator produces, which is not always its operands'.
///
/// Arithmetic on the narrow integer types is `Int` arithmetic — Kotlin has no
/// `Byte.plus(Byte): Byte`, and a result typed `Byte` would pick the wrong carrier. `Char` is the
/// exception that makes the others a rule: `Char.plus(Int)` and `Char.minus(Int)` are declared to
/// return `Char`, and only `Char.minus(Char)` returns `Int`. So `val c: Any = 'A' + 1` boxes a
/// `Char` holding `'B'`, not an `Int` holding `66`.
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
        // Each unsigned type has a descriptor of its own, which is what makes `1u as? Int` fail
        // and a boxed one render its value rather than the signed number sharing its bits.
        Ty::UByte => "ubyte",
        Ty::UShort => "ushort",
        Ty::UInt => "uint",
        Ty::ULong => "ulong",
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
