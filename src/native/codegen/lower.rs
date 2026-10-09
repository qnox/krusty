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

mod arithmetic;
mod arrays;
mod boxed;
mod c_abi;
mod calls;
mod carrier;
mod classes_literal;
mod classifier_shapes;
mod compiler_intrinsics;
mod declared_capabilities;
mod defaults;
mod entry;
mod enums;
mod exceptions;
mod intrinsic_operations;
use arithmetic::{arithmetic_result, scalar_bound};
use carrier::{box_suffix, machine_carrier, scalar_suffix, Carrier};
use declared_capabilities::{
    declares_its_own_comparable, implemented_collections, implemented_dependencies,
    overrides_a_throwable_accessor,
};
mod functions;
mod implementor_dispatch;
mod lists;
mod local_delegates;
mod maps;
mod object_entries;
mod objects;
mod ranges;
mod references;
mod representation;
mod scope;
mod statics;
mod strings;
mod type_checks;
mod unsigned;

use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::MemFlagsData;
use cranelift_codegen::ir::{
    types, Block, BlockArg, InstBuilder, Signature, StackSlotData, StackSlotKind, TrapCode,
};
use cranelift_codegen::ir::{FuncRef, Inst, Type, Value};
use cranelift_codegen::isa::CallConv;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};

use crate::ir::{
    Callee, ClassId, FunId, IrBinOp, IrCheckedOperation, IrConst, IrExpr, IrFile, IrIntrinsic,
    IrLocalPropertyLayout, IrStatic, IrTypeOp, MainEntryParameters,
};
use crate::types::{Ty, TypeName};

use super::super::classes::{self as model, AnyMember, ClassModel, Slot};
use super::super::symbols::Symbols;
use super::super::target::NativeTarget;
use super::{Entry, BOX_RESULT_FRAME, PROGRAM_ENTRY};

/// A construct the lowering declined, and the checked node it was declined at.
///
/// The phrase is presentation; the node is identity. The innermost expression or statement a
/// decline leaves through records its common-IR origin, and an outer one never replaces it, so the
/// diagnostic lands on the node that could not be lowered rather than on whatever encloses it.
#[derive(Debug)]
pub struct Unsupported {
    construct: String,
    origin: Option<crate::fir::OriginId>,
}

impl Unsupported {
    /// The construct, phrased for a diagnostic.
    pub fn construct(&self) -> &str {
        &self.construct
    }

    /// The checked origin of the node the construct was declined at, when one recorded it.
    pub fn origin(&self) -> Option<crate::fir::OriginId> {
        self.origin
    }

    /// Attribute this decline to `origin` unless a more deeply nested node already claimed it.
    fn at(mut self, origin: Option<crate::fir::OriginId>) -> Self {
        if self.origin.is_none() {
            self.origin = origin;
        }
        self
    }
}

impl From<String> for Unsupported {
    fn from(construct: String) -> Self {
        Self {
            construct,
            origin: None,
        }
    }
}

impl From<&str> for Unsupported {
    fn from(construct: &str) -> Self {
        construct.to_string().into()
    }
}

impl std::fmt::Display for Unsupported {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.construct)
    }
}

/// `format!` for a decline: the phrase of an [`Unsupported`] with no origin yet.
macro_rules! declined {
    ($($argument:tt)*) => {
        $crate::native::codegen::lower::Unsupported::from(format!($($argument)*))
    };
}
pub(crate) use declined;

/// One lowered Kotlin file.
pub struct Lowered {
    /// A relocatable ELF object.
    pub object: Vec<u8>,
    /// Whether this file declared `main` and therefore defines [`PROGRAM_ENTRY`].
    pub defines_entry: bool,
    /// This file's contribution to the module's C header, in source order.
    pub abi: Vec<super::super::c_abi::Record>,
}

/// Build the ISA for `target`. Non-PIC, because the output is a static executable at a fixed
/// address linked by krusty's own linker; the verifier stays on while the lowering is young.
/// Flags for a load or store through a reference the program already holds: aligned by
/// construction, and non-trapping because null receivers are checked before any access.
fn trusted() -> MemFlagsData {
    MemFlagsData::trusted()
}

fn isa_for(
    target: NativeTarget,
    verify: bool,
) -> Result<cranelift_codegen::isa::OwnedTargetIsa, Unsupported> {
    let triple: target_lexicon::Triple = target
        .triple()
        .parse()
        .map_err(|error| format!("target triple `{}` ({error})", target.triple()))?;
    let mut flags = settings::builder();
    for (name, value) in [
        ("is_pic", "false"),
        ("opt_level", "none"),
        ("enable_verifier", if verify { "true" } else { "false" }),
        ("use_colocated_libcalls", "false"),
    ] {
        flags
            .set(name, value)
            .map_err(|error| format!("Cranelift setting {name}={value} ({error})"))?;
    }
    cranelift_codegen::isa::lookup(triple)
        .map_err(|error| format!("target {target} ({error})"))?
        .finish(settings::Flags::new(flags))
        .map_err(|error| declined!("target {target} ({error})"))
}

/// What the generator is given beyond the file: the pieces a native IR pass prepared for it.
///
/// One struct rather than a growing parameter list, and BORROWED rather than owned, because the
/// pass that made them owns them for the whole lowering.
pub struct FileInput<'a> {
    pub ir: &'a IrFile,
    /// Provider-normalized facts for classifiers whose representation this file needs.
    pub classifiers: &'a dyn crate::backend::BackendClassifierSource,
    /// Provider-normalized facts for exactly the dependency identities this checked file names.
    pub callables: &'a crate::backend::CheckedBackendCallables,
    /// Every symbol the prebuilt runtime defines, which the program's own names must avoid. Read
    /// once per build by the backend rather than once per file.
    pub runtime_symbols: &'a std::collections::HashSet<String>,
    /// C export symbols already claimed by earlier files in this module.
    pub abi_symbols: &'a mut std::collections::HashSet<String>,
    /// This file's identity in the module. Its initializer is exported under it.
    pub source: crate::fir::SourceFileId,
    /// The accessors synthesized for each reference to a dependency property, by site; see
    /// [`crate::native::dependency_references`].
    pub dependency_properties:
        &'a std::collections::HashMap<u32, super::super::dependency_references::DependencyProperty>,
    /// Which classifiers are value classes, and how this target carries each occurrence of one;
    /// see [`crate::native::value_classes`].
    pub value_classes: &'a super::super::value_classes::NativeValueClasses,
}

pub fn lower_file(
    input: FileInput<'_>,
    target: NativeTarget,
    stem: &str,
    entry: Entry,
    verify: bool,
) -> Result<Lowered, Unsupported> {
    let FileInput {
        ir,
        classifiers,
        callables,
        dependency_properties,
        runtime_symbols,
        abi_symbols,
        source,
        value_classes,
    } = input;
    let class_model = model::build(ir, value_classes)?;

    let isa = isa_for(target, verify)?;
    let builder = ObjectBuilder::new(
        isa,
        format!("{stem}.o"),
        cranelift_module::default_libcall_names(),
    )
    .map_err(|error| format!("object builder ({error})"))?;
    let mut module = ObjectModule::new(builder);

    let mut lowering = FileLowering {
        ir,
        classifiers,
        callables,
        values: value_classes,
        module: &mut module,
        symbols: super::super::symbols::symbols(ir, runtime_symbols.clone()),
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
        local_delegate_helpers: HashMap::new(),
        default_wrappers: HashMap::new(),
        module_default_entries: Vec::new(),
        default_constructors: HashMap::new(),
        enum_entries: HashMap::new(),
        reference_identities: HashMap::new(),
        holders: HashMap::new(),
        references: HashMap::new(),
        dependency_properties,
        dependency_property_skips: HashMap::new(),
        implemented_collections: implemented_collections(ir, classifiers),
        overrides_a_throwable_accessor: overrides_a_throwable_accessor(ir),
        // Filled once the class model can be consulted: which classes are walkable is which ones
        // a thunk could be emitted for, and only the model knows that.
        unwalkable_collections: std::collections::HashSet::new(),
        walkable_classes: std::collections::HashMap::new(),
        declares_its_own_comparable: declares_its_own_comparable(ir),
        implemented_dependencies: implemented_dependencies(ir),
    };
    // Before anything reads a shape: the walking members a class answers for decide both which
    // receivers have a runtime-walkable representation and which shapes still decline; both are read while
    // bodies are lowered.
    lowering.resolve_walkable_classes();
    lowering.declare_functions()?;
    lowering.declare_classes()?;
    lowering.declare_statics()?;
    lowering.declare_lambdas()?;
    lowering.declare_local_delegate_helpers()?;
    lowering.declare_default_wrappers()?;
    lowering.declare_module_default_entries()?;
    lowering.declare_default_constructors()?;
    lowering.declare_enum_entries()?;
    // Constructors are bodies too, so property-reference artifacts must exist before classes are
    // defined: a class initializer may itself contain `C::property`.
    lowering.declare_property_references()?;
    lowering.declare_local_property_references()?;
    lowering.define_local_delegate_helpers()?;
    lowering.define_classes()?;
    lowering.define_default_wrappers()?;
    lowering.define_module_default_entries()?;
    lowering.define_default_constructors()?;
    lowering.define_enum_entries()?;
    let statics_init = lowering.define_statics_init()?;
    let file_init = lowering.define_file_init(source, statics_init)?;
    let program_entry = match entry {
        // The frontend selected the file's `main` and its form; the backend only realizes it.
        Entry::Main => ir.entry_point.map(|point| {
            (
                point.function as usize,
                point.parameters == MainEntryParameters::Arguments,
            )
        }),
        // A `box` case answers through a parameterless `box(): String`, the test corpus's own
        // convention rather than a Kotlin entry point.
        Entry::Box => ir
            .functions
            .iter()
            .position(|function| {
                function.params.is_empty()
                    && function.is_static
                    && function.dispatch_receiver.is_none()
                    && function.name == "box"
                    && lowering.carrier(function.ret) == Carrier::Ref
            })
            .map(|index| (index, false)),
    };
    let mut defines_entry = false;
    for index in 0..ir.functions.len() {
        lowering.define_function(index)?;
        if let Some((_, takes_arguments)) = program_entry.filter(|(chosen, _)| *chosen == index) {
            lowering.define_program_entry(
                index,
                entry,
                file_init,
                statics_init.is_some(),
                takes_arguments,
            )?;
            defines_entry = true;
        }
    }
    let abi = super::super::c_abi::file_records(ir, abi_symbols);
    lowering.define_c_exports(file_init, &abi)?;

    let object = module
        .finish()
        .emit()
        .map_err(|error| format!("object emission ({error})"))?;
    Ok(Lowered {
        object,
        defines_entry,
        abi,
    })
}

struct FileLowering<'a> {
    ir: &'a IrFile,
    classifiers: &'a dyn crate::backend::BackendClassifierSource,
    callables: &'a crate::backend::CheckedBackendCallables,
    /// Which classifiers are value classes, and which occurrences travel unboxed.
    values: &'a super::super::value_classes::NativeValueClasses,
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
    /// Native-local accessor helper per checked local-delegate plan and accessor side.
    local_delegate_helpers: HashMap<(u32, bool), FuncId>,
    /// One wrapper per omission shape a call in this file uses.
    default_wrappers: HashMap<defaults::Omission, FuncId>,
    /// The entry each top-level function with defaults exports for callers in other files, by IR
    /// function, in function order.
    module_default_entries: Vec<(u32, FuncId)>,
    /// The same, for a construction that leaves arguments out.
    default_constructors: HashMap<defaults::CtorOmission, FuncId>,
    /// Per enum class, its constants' static slots and getters, in declaration order.
    enum_entries: HashMap<ClassId, enums::EnumItems>,
    /// The holder type for a captured `var` of each carrier, by the carrier's spelling.
    holders: HashMap<String, DataId>,
    /// The emitted pieces of each property reference, by the expression that creates it.
    references: HashMap<u32, references::ReferenceSite>,
    /// One marker per (referenced declaration, bound-ness) this file mentions.
    reference_identities: HashMap<String, DataId>,
    /// The runtime-known types this file puts a class of its OWN behind, by resolved identity.
    ///
    /// A receiver typed by one of these may be an object of the program's rather than one the
    /// runtime made, and the tables that answer a dependency member answer only for the runtime's.
    implemented_dependencies: std::collections::HashSet<crate::types::TypeName>,
    /// The collection SHAPES this file declares a class of its own behind.
    ///
    /// A receiver typed by one of those goes to the runtime's own dispatch, which knows only the
    /// collections this runtime MAKES — a range and a list. An object of the program's own behind
    /// that type would have its vtable read for an entry it does not have, so a receiver of a
    /// shape listed here declines by name instead of being answered wrongly. A receiver of any
    /// OTHER shape is answered as usual; see [`implemented_collections`].
    implemented_collections: std::collections::HashSet<super::super::intrinsics::CollectionShape>,
    /// Accessors synthesized for each dependency-property reference, keyed by expression site.
    dependency_properties:
        &'a std::collections::HashMap<u32, super::super::dependency_references::DependencyProperty>,
    /// Why a dependency-property reference was left without an object, keyed by expression site.
    dependency_property_skips: HashMap<u32, String>,
    /// Whether this file redeclares `Throwable.message` or `Throwable.cause`; see
    /// [`overrides_a_throwable_accessor`].
    overrides_a_throwable_accessor: bool,
    /// The classes of THIS FILE the runtime can walk, by their runtime collection shape.
    ///
    /// A class implementing `kotlin.collections.Iterable` or `Iterator` records where its own
    /// `iterator`/`hasNext`/`next` sit in its descriptor (`objects::WalkSlots`), which is what
    /// lets the runtime's walking entry points reach an object it did not make. This says which
    /// class that is, so a receiver typed by the class rather than by the interface plays the
    /// shape too, so its iteration-protocol members use the same descriptor dispatch as a list.
    walkable_classes: std::collections::HashMap<
        crate::types::TypeName,
        super::super::intrinsics::CollectionShape,
    >,
    /// The shapes among those that the runtime cannot walk an object of this file's behind; see
    /// [`unwalkable_collections`].
    unwalkable_collections: std::collections::HashSet<super::super::intrinsics::CollectionShape>,
    /// Whether this file declares a class an object of which could stand behind a `Comparable<T>`;
    /// see [`declares_its_own_comparable`].
    declares_its_own_comparable: bool,
}

impl<'a> FileLowering<'a> {
    fn signature_of(&self, params: &[Ty], ret: Ty) -> Result<Signature, Unsupported> {
        let mut signature = Signature::new(CallConv::SystemV);
        for param in params {
            match self.carrier(*param).abi_param() {
                Some(abi) => signature.params.push(abi),
                None => return Err("a `Unit` parameter".into()),
            }
        }
        if let Some(abi) = self.carrier(ret).abi_param() {
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
                return Err(declined!(
                    "a method of `{}`, which is not declared in this file",
                    owner.render()
                ));
            }
            // A value class's member takes the VALUE as `this`; its box reaches it through a
            // bridge. Every other member takes the object, as a reference.
            params.push(if self.values.is_value_class(owner) {
                Ty::Obj(owner, &[])
            } else {
                any()
            });
        }
        params.extend(super::super::captures::carried_parameters(self.ir, id));
        self.signature_of(&params, function.ret)
    }

    /// Whether a function declares a REIFIED type parameter.
    ///
    /// Kotlin permits one only on an `inline` function, and splices such a function at every call
    /// site precisely so that `is T` and `T::class` have a type to name. Its own body is therefore
    /// never the one that runs — which is what lets [`Self::define_function`] put a trap where a
    /// body it cannot lower would go, instead of declining the file for a declaration nothing
    /// calls.
    fn declares_a_reified_parameter(&self, id: crate::ir::FunId) -> bool {
        self.ir.signatures.get(&id).is_some_and(|signature| {
            signature
                .type_params
                .iter()
                .any(|parameter| parameter.reified)
        })
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
            // A file-level function is reached across the module by its `kt_mod_` symbol, which
            // stays hidden. The public C name, when there is one, is a separate export.
            let linkage = if super::super::c_abi::is_file_level(self.ir, index) {
                Linkage::Hidden
            } else {
                Linkage::Export
            };
            let id = self
                .module
                .declare_function(&self.symbols.functions[index], linkage, &signature)
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
        result: Ty,
        name: &str,
        fill: &mut Fill<'_>,
    ) -> Result<(), Unsupported> {
        // Both facts about the result travel into the body: the carrier decides the shape of the
        // `return`, and the TYPE decides how a value that arrives in another representation is
        // converted into it — a carrier cannot, because `Boolean` and `UByte` share one.
        let carried = self.carrier(result);
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
                    result: carried,
                    result_type: result,
                    unit_values: std::collections::HashSet::new(),
                    loops: Vec::new(),
                    terminated: false,
                    handlers: Vec::new(),
                    propagate: None,
                    finallys: Vec::new(),
                    dead: Vec::new(),
                    unresolved_reified: Vec::new(),
                };
                let params = body.builder.block_params(entry).to_vec();
                fill(&mut body, &params)?;
                if body.terminated {
                    body.builder.ins().trap(TrapCode::unwrap_user(1));
                } else if carried == Carrier::Void {
                    body.builder.ins().return_(&[]);
                } else {
                    // Kotlin requires a non-`Unit` function to return on every path, and the
                    // frontend has already checked it. The one way a checked program still reaches
                    // here is a `when` the frontend proved EXHAUSTIVE whose subject matched no
                    // branch — `when (a) { A.V -> return "OK" }` over a one-constant enum falls
                    // through this generator's merge edge, because proving exhaustiveness needs a
                    // hierarchy this generator does not have. So the end of such a function is the
                    // runtime's loud failure, which is what kotlinc puts there too
                    // (`NoWhenBranchMatchedException`): a few unreachable instructions, and never
                    // a wrong answer.
                    body.runtime_call("kt_no_when_branch_matched", &[], Ty::Unit, &[])?;
                    body.builder.ins().trap(TrapCode::unwrap_user(1));
                }
                // After the body, because only the body knows whether any call was made: a frame
                // with nothing to propagate through never creates the block at all.
                body.seal_propagation();
                // Last, because the two above may still be emitting into a dead block — the final
                // `trap` just above is emitted into one whenever the body ended terminated.
                body.seal_dead_blocks();
            }
            builder.seal_all_blocks();
            builder.finalize(frontend_config);
        }
        // The emitted function, for when a compiled program answers wrongly and the IR that made it
        // reads correctly — the difference is then in this, and nothing else shows it.
        crate::trace_compiler!("native", "{name}\n{}", context.func.display());
        if let Err(error) = self.module.define_function(id, &mut context) {
            return Err(declined!(
                "compiling `{name}` ({error:?})\n{}",
                context.func.display()
            ));
        }
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
        // A `tailrec` Kotlin loops and the checked lowering does not still recurses, and this
        // generator emits an ordinary call for that recursion. The source wrote `tailrec` because
        // it recurses to a depth no stack survives, so accepting the function means emitting a
        // program that segfaults where it should print its answer. Decline instead: how deep a
        // native stack goes is the machine's business, and a gate must not depend on it.
        //
        // This is NOT every `tailrec` whose body still holds a self-call. A non-tail self-call is
        // one kotlinc leaves recursive too — it reports NON_TAIL_RECURSIVE_CALL and emits the
        // call — so declining those refused programs kotlinc compiles, and compiles the same way.
        if self.ir.unlooped_tailrec.contains(&(index as u32)) {
            return Err(declined!(
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
        slots.extend(super::super::captures::carried_parameters(
            self.ir,
            index as crate::ir::FunId,
        ));
        let name = function.name.clone();
        let ret = function.ret;
        let attempt = self.emit_function(
            id,
            signature.clone(),
            ret,
            &name,
            &mut |lowering, params| {
                for (slot, (value, ty)) in params.iter().zip(&slots).enumerate() {
                    let variable = lowering.declare_value(slot as u32, *ty)?;
                    lowering.builder.def_var(variable, *value);
                }
                lowering.statement(body)
            },
        );
        // A REIFIED declaration whose body this generator cannot lower gets a TRAP for a body.
        //
        // Such a function is `inline` by Kotlin's own rule and is spliced at every call site, so
        // the body emitted here is never the one that runs — and lowering it asks what `T` is
        // where nothing has said, which is how `inline fun <reified T> Any?.isTOrNull() = this is
        // T?` came to decline a whole file from its DECLARATION rather than from any use.
        // Emitting the trap instead keeps the symbol, which a vtable slot and a linker both need,
        // and says so loudly if a call ever did arrive. The body is still lowered FIRST and kept
        // when it lowers: `inline fun <reified T, U> keep(value: U): U = value` touches `T`
        // nowhere, and a call to it is dispatched through the class's own table like any other.
        // Nothing of the failed attempt is committed — `emit_function` defines the function in the
        // module only once the body is whole.
        match attempt {
            Ok(()) => Ok(()),
            Err(_) if self.declares_a_reified_parameter(index as crate::ir::FunId) => self
                .emit_function(id, signature, ret, &name, &mut |lowering, _| {
                    // Nothing but the TRAP `emit_function` puts after a terminated body, in the
                    // entry block — `terminate` would open a dead block for it instead and leave
                    // the entry without a terminator at all. The flag is set directly for that
                    // reason.
                    lowering.terminated = true;
                    Ok(())
                }),
            Err(reason) => Err(reason),
        }
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
    /// Slots of locals whose type is `Unit`. They hold no machine value — there is one `Unit` and
    /// the runtime owns it — so there is no variable to declare, but the local is still READABLE:
    /// `val u = println("x"); u.toString()` is "kotlin.Unit". The slot is remembered so a read
    /// answers the `Unit` value rather than a missing declaration.
    unit_values: std::collections::HashSet<u32>,
    result: Carrier,
    /// The declared result TYPE behind [`Self::result`]. A `return` whose value arrives in another
    /// representation is converted into this, which the carrier alone cannot name.
    result_type: Ty,
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
    /// Every block [`Self::terminate`] opened for the unreachable code after a jump. Most receive
    /// that code and end in a terminator of their own; one whose statement walker stopped instead,
    /// or that a construct switched away from, stays EMPTY — and an empty block is not a block
    /// Cranelift will accept. They are swept at the end of the body, where what is still empty is
    /// finally known.
    dead: Vec<Block>,
    /// Exact semantic identities of reified parameters belonging to an erased helper currently
    /// being emitted. A runtime type operation naming one executes Kotlin's direct-call failure;
    /// ordinary functions and specialized inline copies leave this empty.
    unresolved_reified: Vec<String>,
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
    /// How a semantic type is carried in this file; see [`FileLowering::carrier`].
    fn carrier(&self, ty: Ty) -> Carrier {
        self.file.carrier(ty)
    }

    fn declare_value(&mut self, slot: u32, ty: Ty) -> Result<Variable, Unsupported> {
        let Some(clif) = self.carrier(ty).clif() else {
            return Err("a `Unit`-typed local".into());
        };
        let variable = self.builder.declare_var(clif);
        self.values.insert(slot, (variable, ty));
        Ok(variable)
    }

    /// `value == null`, as a Boolean.
    pub(super) fn is_null(&mut self, value: Value) -> Value {
        let zero = self.builder.ins().iconst(types::I64, 0);
        self.builder.ins().icmp(IntCC::Equal, value, zero)
    }

    /// The address of a data item.
    pub(super) fn data_address(&mut self, data: DataId) -> Value {
        let global = self
            .file
            .module
            .declare_data_in_func(data, self.builder.func);
        self.builder.ins().symbol_value(types::I64, global)
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
        // Sealed at birth. Nothing ever branches here — the block exists only to absorb whatever
        // an abandoned expression is still emitting — and "sealed with no predecessors" is how the
        // builder RECOGNIZES unreachable code. Without it, leaving this block half-built (which is
        // exactly what `x + break` does) trips the builder's own "fill your block before
        // switching" invariant, checked in the one CI run that keeps `debug_assert!` on.
        self.builder.seal_block(dead);
        self.dead.push(dead);
        self.builder.switch_to_block(dead);
    }

    /// Give every block opened for unreachable code a terminator of its own.
    ///
    /// `break` and `continue` inside an EXPRESSION are what leave one without: `x = x + break`
    /// jumps out of the loop while the concatenation is still being built, so the store and the
    /// loop's own back edge are skipped and the block opened to hold them ends in whatever the
    /// abandoned expression had already materialized — a bare constant, in that case. The loop
    /// then switches to its exit and the unfinished one is orphaned.
    ///
    /// What matters is the TERMINATOR, not emptiness: a block nothing reached still holds the
    /// operands evaluated before the jump. Unreachable by construction, so a trap is the honest
    /// end — nothing branches there, and a `return` would need a value this has no way to make.
    fn seal_dead_blocks(&mut self) {
        for block in std::mem::take(&mut self.dead) {
            let ends = self
                .builder
                .func
                .layout
                .last_inst(block)
                .is_some_and(|inst| self.builder.func.dfg.insts[inst].opcode().is_terminator());
            if ends {
                continue;
            }
            self.builder.switch_to_block(block);
            self.builder.ins().trap(TrapCode::unwrap_user(1));
        }
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

    /// Whether this exact declaration-initializer store writes the bits already present in a
    /// freshly allocated native field. The expression identity comes from common lowering; the
    /// all-zero carrier decision belongs to this backend's representation.
    fn is_elided_initializer_store(&self, id: u32) -> bool {
        fn is_null(ir: &IrFile, expression: u32) -> bool {
            match ir.expr(expression) {
                IrExpr::Const(IrConst::Null) => true,
                IrExpr::TypeOp {
                    op: IrTypeOp::ImplicitCoercion,
                    arg,
                    ..
                } => is_null(ir, *arg),
                _ => false,
            }
        }

        fn is_scalar_zero(ir: &IrFile, expression: u32) -> bool {
            match ir.expr(expression) {
                IrExpr::Const(IrConst::Boolean(false))
                | IrExpr::Const(IrConst::Byte(0))
                | IrExpr::Const(IrConst::Short(0))
                | IrExpr::Const(IrConst::Int(0))
                | IrExpr::Const(IrConst::Long(0))
                | IrExpr::Const(IrConst::Char(0))
                | IrExpr::Const(IrConst::UByte(0))
                | IrExpr::Const(IrConst::UShort(0))
                | IrExpr::Const(IrConst::UInt(0))
                | IrExpr::Const(IrConst::ULong(0)) => true,
                IrExpr::Const(IrConst::Float(value)) => value.to_bits() == 0,
                IrExpr::Const(IrConst::Double(value)) => value.to_bits() == 0,
                IrExpr::TypeOp {
                    op: IrTypeOp::ImplicitCoercion,
                    arg,
                    ..
                } => is_scalar_zero(ir, *arg),
                _ => false,
            }
        }

        if !self.file.ir.property_initializer_stores.contains(&id) {
            return false;
        }
        let IrExpr::SetField {
            class,
            index,
            value,
            ..
        } = self.file.ir.expr(id)
        else {
            return false;
        };
        let slot = self.file.ir.classes[*class as usize].fields[*index as usize].ty;
        match self.carrier(slot) {
            Carrier::Scalar(_, _) => is_scalar_zero(self.file.ir, *value),
            Carrier::Ref => is_null(self.file.ir, *value),
            Carrier::Void => false,
        }
    }

    fn statement(&mut self, id: u32) -> Result<(), Unsupported> {
        self.statement_effect(id)
            .map_err(|declined| declined.at(self.file.ir.node_origin(id)))
    }

    fn statement_effect(&mut self, id: u32) -> Result<(), Unsupported> {
        // `var x = 0` in a class body stores NOTHING. The rule is Kotlin's, it is observable
        // rather than an optimization, and the IR is what knows which store is a declaration's —
        // see `property_initializer_stores`. A fresh object's storage is already zero here, as it
        // is on every target krusty emits for.
        if self.is_elided_initializer_store(id) {
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
                        // `coerce` materializes the runtime's singleton for exactly that. It coerces
                        // to the DECLARED result, not to `Any`: a value class carried as its value
                        // is returned as that value, never boxed on the way out.
                        let declared = self.result_type;
                        let value = match self.coerce(value, declared)? {
                            Some(value) => value,
                            None => self.builder.ins().iconst(types::I64, 0),
                        };
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
                        let source = self.type_of(value);
                        let lowered = self.expression(value)?;
                        if self.terminated {
                            return Ok(());
                        }
                        let Some(lowered) = lowered else {
                            return Err("a `return` of no value from a non-`Unit` function".into());
                        };
                        // The expression need not already be in the result's representation: a
                        // body whose value is typed by a type PARAMETER carries a reference, and
                        // `fun <T : Int> foo(x: T): Int = x` returns exactly that where an `Int`
                        // is declared. Unboxing is the conversion, and the declared result type is
                        // what names it.
                        let declared = self.result_type;
                        let Some(value) = self.convert(lowered, source, declared)? else {
                            return Err(
                                "a `return` whose value does not reach the declared result".into(),
                            );
                        };
                        if self.terminated {
                            return Ok(());
                        }
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
                let ty = match init.map(|init| self.file.ir.expr(init)) {
                    Some(IrExpr::RefNew { .. }) => any(),
                    _ => ty,
                };
                if self.carrier(ty) == Carrier::Void {
                    if let Some(init) = init {
                        self.expression(init)?;
                    }
                    // Nothing to declare and something to remember: the local exists, and reading
                    // it must answer `Unit` rather than report a slot that was never declared.
                    self.unit_values.insert(index);
                    return Ok(());
                }
                let variable = self.declare_value(index, ty)?;
                let deferred_zero = self.file.ir.deferred_local_types.contains_key(&id)
                    && init.is_some_and(|initializer| {
                        matches!(self.file.ir.expr(initializer), IrExpr::Const(_))
                    });
                if deferred_zero {
                    // Common IR keeps the parser's synthetic declaration initializer and the
                    // original generic type as separate provenance. JVM needs the original type's
                    // erased zero; Native stores the specialized value directly, so seed the SSA
                    // variable with this target's zero instead of trying to unbox the placeholder
                    // `null` into a specialized primitive.
                    let zero = self.zero_of(ty);
                    self.builder.def_var(variable, zero);
                } else if let Some(init) = init {
                    let value = self.coerce(init, ty)?;
                    if self.terminated {
                        return Ok(());
                    }
                    let Some(value) = value else {
                        return Err("a local initialized from a `Unit` value".into());
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
                if self.unit_values.contains(&var) {
                    // A `Unit`-typed local stores nothing, so the assignment is its right-hand
                    // side's effect and no more.
                    self.expression(value)?;
                    return Ok(());
                }
                let Some(&(variable, ty)) = self.values.get(&var) else {
                    return Err("an assignment to an undeclared local".into());
                };
                let value = self.coerce(value, ty)?;
                if self.terminated {
                    return Ok(());
                }
                let Some(value) = value else {
                    return Err("an assignment of a `Unit` value".into());
                };
                self.builder.def_var(variable, value);
            }
            // A debug-frame boundary has no native machine effect. This backend does not emit a
            // debug-local table yet, so it consumes the marker exactly as the JavaScript backend
            // does instead of rejecting an otherwise ordinary inline expansion.
            IrExpr::InlineFrameMarker => {}
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
            IrExpr::BottomValue {
                producer,
                completion,
            } => {
                self.bottom_value(producer, completion.diverges_when_discarded())?;
            }
            IrExpr::Break { label } => {
                let index = self.loop_index(label.as_deref(), "break")?;
                let target = self.loops[index].break_block;
                self.run_finallys_for_jump(index)?;
                if self.terminated {
                    // A `finally` on the way out diverged — it returned or threw — so this `break`
                    // never arrives. The loop is NOT broken by it: marking it so would make the
                    // exit reachable, and the position after a `while (true)` nothing leaves is
                    // exactly the `Nothing` that lets a function end there.
                    return Ok(());
                }
                self.loops[index].broken = true;
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
                    Err(reason) if reason.construct() == objects::TOP_LEVEL => {
                        self.top_level_write(&target, value)?;
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
        found.ok_or_else(|| declined!("a `{keyword}` outside the loop it names"))
    }

    /// `while`, `do…while`, and the shape a lowered `for` takes: a loop whose `update` runs after
    /// the body at the `continue` target. Every jump is a real edge here — the `goto` scaffolding
    /// the C emitter needed for labels and updates is just what a control-flow graph is.
    /// A loop's test, emitted into the block the current position is in: go round again or leave.
    /// A condition that is never false takes no test at all, which is what keeps the exit of a
    /// `while (true)` out of the graph.
    fn loop_test(
        &mut self,
        cond: u32,
        always: bool,
        body_block: Block,
        exit: Block,
    ) -> Result<(), Unsupported> {
        if always {
            self.builder.ins().jump(body_block, &[]);
            return Ok(());
        }
        let condition = self.expression(cond)?;
        if self.terminated {
            return Ok(());
        }
        let Some(condition) = condition else {
            return Err("a loop condition of no value".into());
        };
        self.builder
            .ins()
            .brif(condition, body_block, &[], exit, &[]);
        Ok(())
    }

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

        // A PRE-test loop asks its condition before the body runs, so the condition is lowered
        // first. A POST-test one asks it after — and Kotlin scopes a `do`-block's locals into the
        // `while`, so the condition may READ what the body declares. Lowering it here would look
        // for a slot the body has not reached yet, which is why it waits until below. The BLOCK is
        // the same either way; only when it is filled differs.
        if !post_test {
            self.continue_in(header);
            self.loop_test(cond, always, body_block, exit)?;
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
        // Popped BEFORE the post-test condition for the same reason the pre-test one is lowered
        // before the push: a jump written in a condition leaves the enclosing loop, not this one.
        let broken = self.loops.pop().is_some_and(|frame| frame.broken);
        if post_test {
            self.continue_in(header);
            self.loop_test(cond, always, body_block, exit)?;
        }

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
        let result = result.filter(|ty| self.carrier(*ty) != Carrier::Void);
        if let Some(ty) = result {
            let clif = self.carrier(ty).clif().expect("non-void carrier");
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
                        return Err("a condition of no value".into());
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
                    let clif = self.carrier(ty).clif().expect("non-void carrier");
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
            (Some(_), None) => return Err("a `when` arm of no value where one is needed".into()),
            (None, _) => {
                self.builder.ins().jump(merge, &[]);
            }
        }
        *reaches_merge = true;
        Ok(())
    }

    fn zero_of(&mut self, ty: Ty) -> Value {
        match self.carrier(ty) {
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
        self.expression_value(id)
            .map_err(|declined| declined.at(self.file.ir.node_origin(id)))
    }

    fn expression_value(&mut self, id: u32) -> Result<Option<Value>, Unsupported> {
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
        let Some(narrow) = self.carrier(logical).clif() else {
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
            .is_some_and(|ty| matches!(self.carrier(ty), Carrier::Scalar(..)))
    }

    fn lowered_expression(&mut self, id: u32) -> Result<Option<Value>, Unsupported> {
        match self.file.ir.expr(id).clone() {
            IrExpr::Const(constant) => self.constant(&constant).map(Some),
            IrExpr::UnitInstance => Ok(None),
            IrExpr::GetValue(slot) => {
                if self.unit_values.contains(&slot) {
                    // `Unit` in value position: no machine value, exactly as a `Unit`-returning
                    // call produces none. A position that wants a reference gets the runtime's
                    // singleton from `coerce`, which is where every other `Unit` value comes from.
                    return Ok(None);
                }
                let Some(&(variable, _)) = self.values.get(&slot) else {
                    return Err("a read of an undeclared local".into());
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
            } => {
                if matches!(
                    op,
                    IrTypeOp::InstanceOf
                        | IrTypeOp::NotInstanceOf
                        | IrTypeOp::Cast
                        | IrTypeOp::CastNonNull
                ) && self.unresolved_reified.iter().any(|parameter| {
                    type_operand
                        .type_parameter_occurrence_bound(parameter)
                        .is_some()
                }) {
                    // The operand precedes kotlinc's reified-operation marker and therefore keeps
                    // any side effect or exception it produces before the marker's own failure.
                    let _ = self.expression(arg)?;
                    if self.terminated {
                        return Ok(None);
                    }
                    let message = self.string_literal(
                        b"This function has a reified type parameter and thus can only be inlined at compilation time, not called directly.",
                    )?;
                    self.raise("kt_type_unsupported_operation_exception", message)
                } else {
                    self.type_operation(
                        op,
                        arg,
                        type_operand,
                        self.file.ir.declaration_result_coercions.contains(&id),
                    )
                }
            }
            IrExpr::PrimitiveBinOp { op, lhs, rhs } => self.binary(op, lhs, rhs),
            IrExpr::Equality { op, mode, lhs, rhs } => self.equality(op, mode, lhs, rhs),
            IrExpr::PrimitiveNeg { operand, ty } => self.negate(operand, ty),
            IrExpr::StringConcat(parts) => self.concat(&parts),
            IrExpr::BottomValue {
                producer,
                completion,
            } => self.bottom_value(producer, completion.diverges_when_discarded()),
            IrExpr::NotNullAssert { operand, .. } => self.not_null_assert(operand),
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
            IrExpr::LateinitInitialized {
                receiver,
                class,
                index,
            } => self.lateinit_initialized(receiver, class, index),
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
            IrExpr::EnumEntries { classifier } => self.enum_entries(classifier),
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
            IrExpr::Checked(IrCheckedOperation::ExternalPropertyRead {
                target,
                receiver: Some(receiver),
                ..
            }) if self.reference_property_role(target).is_some() => self
                .reference_property(target, receiver)
                .expect("the selected getter role was checked by the guard"),
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
            }) if self.throwable_field(target).is_some() => {
                let symbol = self.throwable_field(target).expect("just matched");
                if self.file.overrides_a_throwable_accessor {
                    return Err(declined!(
                        "a read of `Throwable.{}` in a file that overrides one",
                        if symbol == "kt_throwable_cause" {
                            "cause"
                        } else {
                            "message"
                        }
                    ));
                }
                self.throwable_field_read(symbol, receiver)
            }
            // `cs.length` where the receiver is typed `CharSequence`: a string, on this target.
            IrExpr::Checked(IrCheckedOperation::ExternalPropertyRead {
                target,
                receiver: Some(receiver),
                ..
            }) if self.is_text_length(target) => self.text_length(receiver),
            IrExpr::Checked(IrCheckedOperation::ExternalPropertyRead {
                target,
                receiver: Some(receiver),
                ..
            }) if self.external_getter_is_indices(target) => self.indices(receiver),
            IrExpr::Checked(IrCheckedOperation::ExternalPropertyRead {
                target,
                receiver: Some(receiver),
                result,
                ..
            }) if self.range_getter(target, receiver).is_some() => {
                let (owner, name) = self
                    .range_getter(target, receiver)
                    .expect("checked by the guard");
                self.range_property_member(owner, &name, receiver, result)
                    .expect("a range member, by the guard")
            }
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
                match self.list_property_member(&name, receiver, any()) {
                    Some(realized) => realized,
                    None => Err(declined!("`{name}` of a receiver that answers none of it")),
                }
            }
            IrExpr::Checked(IrCheckedOperation::ExternalPropertyRead {
                target,
                receiver: Some(receiver),
                ..
            }) if self.indexed_value_getter(target, receiver).is_some() => {
                let name = self
                    .indexed_value_getter(target, receiver)
                    .expect("checked by the guard");
                let answer = lists::indexed_value_getter_ty(&name);
                match self.list_property_member(&name, receiver, answer) {
                    Some(realized) => realized,
                    None => Err(declined!(
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
                match self.list_property_member(&name, receiver, Ty::Int) {
                    Some(realized) => realized,
                    None => Err(declined!(
                        "`{name}` of a receiver that is not a collection"
                    )),
                }
            }
            IrExpr::Checked(IrCheckedOperation::ExternalPropertyRead {
                target,
                receiver: Some(receiver),
                ..
            }) if self.map_getter(target, receiver).is_some() => {
                let property = self
                    .map_getter(target, receiver)
                    .expect("checked by the guard");
                match self.map_property_member(property, receiver, property.answer()) {
                    Some(realized) => realized,
                    None => Err(declined!(
                        "`{}` of a receiver that is not a map",
                        property.name()
                    )),
                }
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
                    Err(reason) if reason.construct() == objects::TOP_LEVEL => {
                        self.top_level_read(&target)
                    }
                    Err(reason) => Err(reason),
                }
            }
            IrExpr::Checked(IrCheckedOperation::PropertyReference { .. }) => {
                self.property_reference(id)
            }
            IrExpr::LocalDelegateAccess(access) => self.local_delegate_access(access),
            IrExpr::LocalPropertyReference(_) => self.local_property_reference(id),
            IrExpr::KClassLiteral {
                classifier, value, ..
            } => self.class_literal(classifier, value),
            IrExpr::Checked(IrCheckedOperation::RangeConstruction {
                operation,
                start,
                start_type,
                end,
                end_type,
                result,
            }) => self.range_construction(operation, start, start_type, end, end_type, result),
            IrExpr::Checked(IrCheckedOperation::RangeContains {
                operation,
                value,
                start,
                end,
                negated,
                counter,
            }) => self.range_contains(operation, value, start, end, negated, counter),
            IrExpr::Checked(IrCheckedOperation::IllegalProgressionStep { step }) => {
                self.illegal_progression_step(step)
            }
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
            other => Err(self.describe_declined(&other).into()),
        }
    }

    /// The declining phrase for a node this generator does not lower.
    ///
    /// `describe` reads the node's shape and nothing else, which is right for most of them. A
    /// dependency PROPERTY is the exception: every one arrives as the same
    /// `Checked(ExternalPropertyRead)`, so the backlog lumped eighteen unrelated properties —
    /// `Double.Companion.MAX_VALUE`, `System.out`, `UIntArray.indices` — under one row and could
    /// not be worked from, which is exactly what `describe`'s own comment warns against. The
    /// frozen selected-declaration facts know the owner and the name, so they are stated the way a
    /// declining CALL already states its callee.
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
            // A target absent from the checked file's frozen facts is a different gap from an
            // unimplemented property, so it keeps the shape-only phrasing rather than borrowing a
            // name it does not have.
            None => describe(node),
        }
    }

    /// `owner.name` for a dependency property, from the frozen selected-declaration facts.
    fn external_property_name(&self, target: crate::fir::ExternalPropertyId) -> Option<String> {
        let property = self.file.callables.property(target)?;
        let getter = self.file.callables.callable(property.getter)?;
        Some(format!(
            "{}.{}",
            getter.physical_owner.render(),
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
                    return Err("a string constant containing an unpaired surrogate".into());
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
    ) -> Result<Option<Value>, Unsupported> {
        // `assertTrue`/`assertFalse` ask about a `Boolean` and take it as one; every other
        // assertion compares its operands STRUCTURALLY and takes them boxed.
        //
        // The SYMBOL is what decides, not the parameter type. `assertEquals` is generic, so
        // `assertEquals(true, true)` has `Boolean` as its first parameter after substitution —
        // and reading that handed two raw machine values to an entry point that reads them as
        // references, which dereferenced 1 as a pointer.
        let value = if matches!(symbol, "kt_assert_true" | "kt_assert_false") {
            Ty::Boolean
        } else {
            Ty::nullable(Ty::obj("kotlin/Any"))
        };
        // Whether a message was WRITTEN, which the declaration cannot say: `message` is defaulted,
        // so a call that leaves it out still names the declaration that has it.
        let has_message = args.len() > compared;
        if args.len() < compared {
            return Err(declined!(
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
        let Some(physical) = self.physical_type_of(id) else {
            // Every path that leaves an expression untyped comes through here, so this is where a
            // decline naming only the two carriers becomes followable.
            let rendered = format!("{:?}", self.file.ir.expr(id));
            crate::trace_compiler!(
                "native",
                "untyped expression {id}: {}",
                &rendered[..rendered.len().min(200)]
            );
            return None;
        };
        let logical = self.file.ir.logical_types.get(&id).copied();
        // The checked type wins whenever it is unsigned, the value is a SCALAR rather than a
        // pointer to one, and it is no WIDER than that scalar: `expression` has narrowed the value
        // to it, so the two agree. Wider would be a claim about bits that are not there, and a
        // pointer is not the value at all — `val any: Any = 7u` is still checked as `UInt`.
        let bits = |ty: Ty| self.carrier(ty).clif().map(|clif| clif.bits());
        match logical {
            Some(logical)
                if logical.is_unsigned()
                    && matches!(self.carrier(physical), Carrier::Scalar(..))
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
            IrExpr::GetValue(slot) => match self.values.get(slot) {
                Some(&(_, ty)) => ty,
                None if self.unit_values.contains(slot) => Ty::Unit,
                None => return None,
            },
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
                        Some(previous) if self.carrier(previous) == self.carrier(ty) => previous,
                        Some(_) => any(),
                    });
                }
                result?
            }
            // A `try` merges every arm at its checked result's carrier, so that is what it yields.
            IrExpr::Try { result, .. } => *result,
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
            IrExpr::Equality { .. } => Ty::Boolean,
            IrExpr::Call { callee, .. } => match callee {
                Callee::Local(function)
                | Callee::LocalWithDefaults { function, .. }
                | Callee::ClassStaticWithDefaults { function, .. } => {
                    self.file.ir.functions[*function as usize].ret
                }
                Callee::External { ret, .. }
                | Callee::Intrinsic { ret, .. }
                | Callee::Module { ret, .. }
                | Callee::ModuleWithDefaults { ret, .. }
                | Callee::Super { ret, .. } => *ret,
                Callee::Special { source, .. } => {
                    let function = self.file.ir.checked_callable_functions.get(&(*source)?)?;
                    self.file.ir.functions[*function as usize].ret
                }
                // A VIRTUAL call yields what the slot it dispatches through carries, which is the
                // DECLARED return rather than the one this receiver's class narrows it to. For a
                // generic member that is a type parameter, so the value is a reference — and
                // saying so is what lets a site wanting a machine value unbox it. Leaving it
                // undetermined is what made `class A(a: Tr<Int>) : Tr<Int> by a`'s `a.prop`
                // reach an `Int` position as a reference with nothing to convert it by.
                Callee::Virtual {
                    params: Some((_, ret)),
                    ..
                } => *ret,
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
            IrExpr::EnumEntries { classifier } => {
                Ty::obj_args("kotlin/enums/EnumEntries", &[Ty::Obj(*classifier, &[])])
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
            // The RAW field behind `::prop.isInitialized`, which carries what the field holds:
            // the comparison against null is a node of its own around this one.
            IrExpr::LateinitInitialized { class, index, .. } => {
                super::super::captures::physical_ty(
                    self.file.ir,
                    *class,
                    *index,
                    self.file.ir.classes[*class as usize].fields[*index as usize].ty,
                )
            }
            IrExpr::GetStatic(index) => self.file.ir.statics[*index as usize].ty,
            IrExpr::NewArray { array_type, .. } | IrExpr::Vararg { array_type, .. } => *array_type,
            IrExpr::InvokeFunction { ret, .. } => *ret,
            IrExpr::RefGet { elem, .. } | IrExpr::RefSet { elem, .. } => *elem,
            IrExpr::CallableReference(reference) => reference.function_type,
            IrExpr::Lambda { .. } | IrExpr::RefNew { .. } => any(),
            // `name` and `ordinal` belong to `kotlin.Enum`, a class no file declares, so the
            // checked property table has nothing to say about them; their types are the language's
            // and are stated where the read itself is recognized.
            IrExpr::Checked(IrCheckedOperation::ExternalPropertyRead {
                target,
                receiver,
                result,
                ..
            }) => {
                if self.is_text_length(*target) {
                    return Some(Ty::Int);
                }
                if self.class_name_accessor(*target).is_some() {
                    return Some(Ty::nullable(Ty::String));
                }
                if self.reference_property_role(*target).is_some() {
                    return self
                        .file
                        .callables
                        .property(*target)
                        .map(|property| property.result);
                }
                match self.enum_member_name(*target) {
                    Some("name") => Ty::String,
                    Some(_) => Ty::Int,
                    // A pair's components are references; a list's `size` is an `Int`; a range's
                    // own members answer at their element's width. Which of the three this read is
                    // depends on the receiver as much as on the getter — a range declares `first`
                    // too — so the receiverless read is none of them.
                    None => match receiver {
                        // `x.indices` is realized as an `IntRange` and as nothing else, whatever
                        // the receiver is indexable as, so the read IS one. Saying so is what
                        // lets `b in a.indices` recognize its receiver as a range when `b` is not
                        // an `Int`: that comparison is the ranges FACADE's, which reads its
                        // element from the receiver, and a receiver with no type sent the call
                        // to the dependency-member path to be declined by name.
                        Some(_) if self.external_getter_is_indices(*target) => {
                            Ty::obj("kotlin/ranges/IntRange")
                        }
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
                        Some(receiver) if self.map_getter(*target, *receiver).is_some() => {
                            let property = self
                                .map_getter(*target, *receiver)
                                .expect("checked by the guard");
                            let answer = property.answer();
                            // The runtime's answer is only the carrier; the checked result names
                            // the collection the read IS (`m.keys` is a `Set`). A walk over the
                            // read itself, `for (k in m.keys)`, finds its collection shape there.
                            if self.carrier(*result) == self.carrier(answer) {
                                *result
                            } else {
                                answer
                            }
                        }
                        Some(receiver) => {
                            self.range_getter(*target, *receiver)?;
                            *result
                        }
                        None => return None,
                    },
                }
            }
            IrExpr::Checked(IrCheckedOperation::RangeConstruction { result, .. }) => *result,
            IrExpr::LocalPropertyReference(_) => Ty::obj("kotlin/reflect/KProperty"),
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
            // `x!!` yields `x` or fails, so its type is the OPERAND's with the nullability taken
            // off — which is what the lowering already does, unboxing a nullable primitive there.
            // Saying so here is what lets a CONSUMER of `x!!` know what it is holding: without it
            // the value's type is undetermined, and `c!!.toInt()` on a `Char?` reached `convert`
            // with a reference where a machine value was required and declined by that name.
            IrExpr::NotNullAssert { operand, .. } => self.physical_type_of(*operand)?.non_null(),
            // A `lateinit` read yields its operand too; only the guard differs.
            IrExpr::LateinitCheck { operand, .. } => self.physical_type_of(*operand)?,
            IrExpr::Checked(IrCheckedOperation::PropertyRead { target, .. }) => {
                self.file.ir.checked_properties.get(target)?.ty
            }
            _ => return None,
        })
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

    /// One field's contribution to a data class's `hashCode`, which is the field's own `hashCode`.
    ///
    /// Kotlin's answer for each primitive is fixed, and these are those answers rather than
    /// anything this generator is free to choose: a program can print a hash, and two programs
    /// that agree on everything else must agree on it. `Boolean` is 1231 or 1237 — arbitrary, and
    /// arbitrary in the same way everywhere. `Long` folds its halves together so the high word is
    /// not lost in the truncation to `Int`, and `Double` does the same to its bits. A `Float` is
    /// its bits. The smaller integers are themselves, widened.
    fn field_hash(&mut self, value: u32, ty: Ty) -> Result<Option<Value>, Unsupported> {
        if self.carrier(ty) == Carrier::Ref {
            // Including a nullable primitive, which is a box and hashes through its own type.
            let value = self.reference(value)?;
            if self.terminated {
                return Ok(None);
            }
            return self.runtime_call("kt_hash_code", &[any()], Ty::Int, &[value]);
        }
        let Some(operand) = self.coerce(value, ty)? else {
            return Err("a `Unit` data-class field".into());
        };
        if self.terminated {
            return Ok(None);
        }
        self.value_hash(operand, ty).map(Some)
    }

    /// The hash of a value already in hand, by the same rules.
    pub(super) fn value_hash(&mut self, operand: Value, ty: Ty) -> Result<Value, Unsupported> {
        // A value class hashes as the value it holds, by the rule for THAT value's type.
        let ty = self.file.values.project(ty);
        if self.carrier(ty) == Carrier::Ref {
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
            // An unsigned integer hashes as the SIGNED value it wraps, because that is what
            // Kotlin declares: `UByte` and `UShort` answer `data.toInt()` on their `Byte`/`Short`,
            // which sign-extends, `UInt` answers its `Int` unchanged, and `ULong` folds its `Long`
            // exactly as `Long` does. Nothing here may choose differently — a program can print a
            // hash, and two programs that agree on everything else must agree on it.
            Ty::Byte | Ty::Short | Ty::UByte | Ty::UShort => {
                self.builder.ins().sextend(types::I32, operand)
            }
            Ty::Char => self.builder.ins().uextend(types::I32, operand),
            Ty::Int | Ty::UInt => operand,
            Ty::Long | Ty::ULong => self.fold_to_int(operand),
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
            other => return Err(declined!("a data-class field of type `{other:?}`")),
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
        if self.carrier(ty) != Carrier::Ref {
            let left = self.coerce(left, ty)?;
            let right = self.coerce(right, ty)?;
            if self.terminated {
                return Ok(None);
            }
            let (Some(left), Some(right)) = (left, right) else {
                return Err("a `Unit` data-class field".into());
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
    /// Two strings, concatenated.
    pub(super) fn join(&mut self, left: Value, right: Value) -> Result<Value, Unsupported> {
        Ok(self
            .runtime_call("kt_string_plus", &[any(), any()], any(), &[left, right])?
            .expect("`kt_string_plus` returns a string"))
    }

    pub(super) fn values_equal(
        &mut self,
        left: Value,
        right: Value,
        ty: Ty,
    ) -> Result<Value, Unsupported> {
        // A value class compares by the value it holds, by the rule for THAT value's type.
        let ty = self.file.values.project(ty);
        if self.carrier(ty) != Carrier::Ref {
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
                return Err("a `Unit` argument".into());
            };
            values.push(value);
        }
        Ok(values)
    }
}

/// `Any?`: the type every runtime reference parameter is declared as.
fn any() -> Ty {
    Ty::nullable(Ty::obj("kotlin/Any"))
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
    // A CHECKED callable reference is one whose invocation common lowering did not turn into an
    // adapter, so the target is what says which piece of work it is. Reporting the node alone put
    // every one of them under one line of the backlog, which cannot be worked from.
    if let IrExpr::CallableReference(reference) = node {
        use crate::ir::IrCallableReferenceTarget as Target;
        return match &reference.target {
            Target::Module(_) => {
                "a reference to a declaration of this file, kept as a reflection value".to_string()
            }
            Target::Constructor { .. } => "a reference to a CONSTRUCTOR".to_string(),
            Target::External { .. } => "a reference to a DEPENDENCY declaration".to_string(),
            Target::Classifier { .. } => {
                "a reference to a classifier's implicit member".to_string()
            }
            Target::Local { .. } => "a reference to a local declaration".to_string(),
            Target::FunctionValueConversion { .. } => {
                "a reference adapting another function value".to_string()
            }
            Target::FunctionInvoke => {
                "a reference to a function value's `invoke` declaration".to_string()
            }
        };
    }
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
        assert_eq!(machine_carrier(Ty::Int), Carrier::Scalar(types::I32, true));
        assert_eq!(
            machine_carrier(Ty::nullable(Ty::Int)),
            Carrier::Ref,
            "`Int?` must represent `null`, so it boxes exactly as it does on the JVM"
        );
        assert_eq!(machine_carrier(Ty::Unit), Carrier::Void);
        assert_eq!(machine_carrier(Ty::String), Carrier::Ref);
    }

    #[test]
    fn narrow_scalars_extend_to_the_c_abi_by_their_kotlin_signedness() {
        // `Byte` is signed and `Char` is not; passing either to the runtime in a 32-bit register
        // must extend it the way the C prototype's type does, or `kt_println_char('é')` prints a
        // negative code point.
        assert!(
            machine_carrier(Ty::Byte).abi_param().unwrap().extension == ArgumentExtension::Sext
        );
        assert!(
            machine_carrier(Ty::Char).abi_param().unwrap().extension == ArgumentExtension::Uext
        );
        assert!(
            machine_carrier(Ty::Boolean).abi_param().unwrap().extension == ArgumentExtension::Uext
        );
        assert!(machine_carrier(Ty::Int).abi_param().unwrap().extension == ArgumentExtension::None);
    }

    #[test]
    fn only_a_verified_build_runs_the_cranelift_verifier() {
        let target = NativeTarget::host().expect("a supported host");
        assert!(!isa_for(target, false).unwrap().flags().enable_verifier());
        assert!(isa_for(target, true).unwrap().flags().enable_verifier());
    }
}
