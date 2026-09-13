//! Common IR to C.
//!
//! C is the target because it is the shortest path to a *genuinely native* binary that does not
//! commit the project to a code generator. `docs/BUILD_AND_NATIVE_PLAN.md` keeps the Cranelift
//! question open on purpose, and picking either Cranelift or LLVM to print `Hello, world!` would
//! answer it by accident. Emitting C answers nothing: the same lowering feeds a real backend later,
//! and in the meantime `cc` is already on every machine that builds this compiler.
//!
//! **What this emitter refuses.** Anything it has not been taught produces a diagnostic naming the
//! construct, and the file emits nothing. It never falls back to a guess. The JVM backend can
//! afford a best effort because `kotlinc` decides what is correct; nothing decides that here yet,
//! so a wrong emission would be indistinguishable from a right one until someone ran it.
//!
//! **Classes.** A class becomes a C struct whose first member is the object header, laid out and
//! given a vtable by [`super::classes`]; construction allocates through the collector and runs an
//! emitted constructor function; an instance method is a C function taking the receiver first;
//! a call on an instance loads the implementation from the receiver's type descriptor
//! (`kt_dispatch`). What the IR presents for these is not quite what its declarations suggest, and
//! the emitter follows the IR: a property access arrives as a *checked* operation naming a
//! `PropertyId` (`IrExpr::Checked`), and an override of `kotlin.Any`'s members is not recorded in
//! `function_overrides`, so those three are matched by name and signature.
//!
//! One GNU C extension is used: the statement expression `({ …; value; })`, which is how a Kotlin
//! block with a value renders in expression position, and how a receiver is evaluated exactly once
//! before it is used both to find and to call a method. gcc and clang both support it; a portable
//! spelling would mean hoisting temporaries through a lowering pass, which is real work that
//! belongs with the rest of the native lowering rather than in the first emitter.

use std::collections::{HashMap, HashSet};

use super::classes::{c_kind, CKind, ClassModel, Slot, SlotKey};
use crate::ir::{Callee, ClassId, IrBinOp, IrCheckedOperation, IrConst, IrExpr, IrFile, IrTypeOp};
use crate::jvm::classpath::Classpath;
use crate::types::{Ty, TypeName};

/// The runtime's suffix for a scalar carrier, used to name its `kt_*` helpers.
fn scalar_suffix(kind: CKind) -> Option<&'static str> {
    match kind {
        CKind::Scalar(name) => name.strip_prefix("kt_"),
        _ => None,
    }
}

/// The runtime's boxing/unboxing suffix for a scalar carrier.
///
/// `None` for `Float`/`Double`: the runtime has no box for them ON PURPOSE, because it cannot
/// render one and Kotlin's `Double.toString` is not `%g` (see `super::runtime`). Returning a
/// suffix here would emit a call to a function that does not exist, turning a construct the
/// backend should DECLINE into a link error with no diagnostic.
fn box_suffix(kind: CKind) -> Option<&'static str> {
    match scalar_suffix(kind) {
        Some("float" | "double") | None => None,
        suffix => suffix,
    }
}

/// Render a Kotlin string as a C string literal plus its UTF-8 byte length.
///
/// `None` for a string containing an unpaired surrogate. Kotlin admits those (`"\uD800"`), UTF-8
/// does not encode them, and the runtime is UTF-8 — so this is declined rather than mangled.
fn c_string_literal(value: &crate::kt_string::KtString) -> Option<(String, usize)> {
    let text = value.as_str()?;
    Some((c_literal_of(&text), text.len()))
}

/// A C string literal for UTF-8 `text`.
fn c_literal_of(text: &str) -> String {
    let mut literal = String::with_capacity(text.len() + 2);
    literal.push('"');
    for byte in text.bytes() {
        match byte {
            b'"' => literal.push_str("\\\""),
            b'\\' => literal.push_str("\\\\"),
            b'\n' => literal.push_str("\\n"),
            b'\r' => literal.push_str("\\r"),
            b'\t' => literal.push_str("\\t"),
            // Everything else goes out as an octal escape, including plain ASCII letters' UTF-8
            // continuation bytes. Octal (not hex) because a hex escape in C has no length limit:
            // `"\x41" "1"` is fine but `"\x411"` is one huge character.
            0x20..=0x7E => literal.push(byte as char),
            other => literal.push_str(&format!("\\{other:03o}")),
        }
    }
    literal.push('"');
    literal
}

/// Sanitize a Kotlin name into something C will accept as an identifier.
fn c_identifier(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            out.push(character);
        } else {
            out.push('_');
        }
    }
    out
}

/// A loop the emitter is currently inside.
struct LoopFrame {
    /// The Kotlin label on the loop, if it carried one.
    label: Option<String>,
    /// The unique C label base for this loop's `goto` targets.
    base: String,
    /// Whether `continue` must be a `goto` because the loop has an update to run first.
    continue_is_goto: bool,
}

/// Which way out of a loop a jump takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Exit {
    Break,
    Continue,
}

impl std::fmt::Display for Exit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Break => "break",
            Self::Continue => "continue",
        })
    }
}

/// One emitted C translation unit, plus the entry point it declares (if any).
pub(super) struct CTranslationUnit {
    pub source: String,
    /// The C symbol of this file's Kotlin `main`, when it declares one.
    pub entry: Option<String>,
}

/// Every C symbol the file defines, allocated up front from one pool so no two can collide.
///
/// Kotlin overloads share a name and C has no overloading, so a repeat gets a suffix. The suffixed
/// spelling is itself checked for collisions: a file declaring both `greet` and `greet__1` would
/// otherwise hand two functions the same symbol, and the linker would silently pick one of them.
/// Classes draw from the same pool as functions because their descriptors and constructors are
/// ordinary C objects too: a class `X` and a top-level function `type_X` must not both become
/// `kt_type_X`.
pub(super) struct Symbols {
    /// The sanitized, unique base name of each class, from which `struct kt_class_<base>`,
    /// `kt_type_<base>`, `kt_<base>__init` and the struct members `<base>_f<i>` derive.
    classes: Vec<String>,
    /// C symbol per IR function index.
    functions: Vec<String>,
}

/// The names a class's base reserves. Kept in one place so the reservation and the uses agree.
fn class_symbol_family(base: &str) -> [String; 4] {
    [
        format!("kt_type_{base}"),
        format!("kt_vtable_{base}"),
        format!("kt_refs_{base}"),
        format!("kt_{base}__init"),
    ]
}

pub(super) fn symbols(ir: &IrFile) -> Symbols {
    let mut taken: HashSet<String> = HashSet::new();
    let mut unique = |base: String, family: &dyn Fn(&str) -> Vec<String>| -> String {
        let mut candidate = base.clone();
        let mut ordinal = 0;
        loop {
            let names = family(&candidate);
            if names.iter().all(|name| !taken.contains(name)) {
                taken.extend(names);
                return candidate;
            }
            ordinal += 1;
            candidate = format!("{base}__{ordinal}");
        }
    };

    // Classes first: a class reserves a whole family of names, and a function only one.
    let classes: Vec<String> = ir
        .classes
        .iter()
        .map(|class| {
            unique(c_identifier(&class.fq_name()), &|base| {
                class_symbol_family(base).to_vec()
            })
        })
        .collect();

    let package = ir
        .package
        .as_deref()
        .map(c_identifier)
        .filter(|package| !package.is_empty());
    let mut functions = vec![String::new(); ir.functions.len()];
    // Methods are named by their class, then everything else by the package.
    for (class, base) in ir.classes.iter().zip(&classes) {
        for &fid in &class.methods {
            let function = &ir.functions[fid as usize];
            let method = format!("kt_{base}_{}", c_identifier(&function.name));
            functions[fid as usize] = unique(method, &|name| vec![name.to_string()]);
        }
    }
    for (index, function) in ir.functions.iter().enumerate() {
        if !functions[index].is_empty() {
            continue;
        }
        let base = match &package {
            Some(package) => format!("kt_{package}_{}", c_identifier(&function.name)),
            None => format!("kt_{}", c_identifier(&function.name)),
        };
        functions[index] = unique(base, &|name| vec![name.to_string()]);
    }
    Symbols { classes, functions }
}

/// One C symbol per IR function, unique within the file.
#[cfg(test)]
fn function_symbols(ir: &IrFile) -> Vec<String> {
    symbols(ir).functions
}

pub(super) struct Emitter<'a> {
    ir: &'a IrFile,
    classpath: &'a Classpath,
    symbols: Symbols,
    /// Layouts and vtables of this file's classes; built by `emit_file`, empty until then.
    model: Option<ClassModel>,
    /// Declared type of each value slot in the function being emitted.
    values: HashMap<u32, Ty>,
    /// The C carrier of the enclosing function's result. A `return` has to know it, and a `return`
    /// can appear arbitrarily deep — including inside a block that is itself in EXPRESSION position
    /// (`val x = if (c) return else 1`), where there is no enclosing statement to thread it from.
    result: CKind,
    /// Loops enclosing the statement being emitted, innermost last.
    loops: Vec<LoopFrame>,
    label_count: u32,
    /// Temporaries introduced for receivers evaluated once and used twice.
    temp_count: u32,
    /// Synthesized field accessors the vtables reference, emitted once each.
    synthesized: Vec<Slot>,
    out: String,
}

/// The construct an emission declined, phrased for a diagnostic.
pub(super) type Unsupported = String;

impl<'a> Emitter<'a> {
    pub(super) fn new(ir: &'a IrFile, classpath: &'a Classpath) -> Self {
        Self {
            symbols: symbols(ir),
            ir,
            classpath,
            model: None,
            values: HashMap::new(),
            result: CKind::Void,
            loops: Vec::new(),
            label_count: 0,
            temp_count: 0,
            synthesized: Vec::new(),
            out: String::new(),
        }
    }

    fn model(&self) -> &ClassModel {
        self.model.as_ref().expect("the class model is built first")
    }

    pub(super) fn emit_file(mut self) -> Result<CTranslationUnit, Unsupported> {
        if !self.ir.statics.is_empty() {
            return Err("a top-level property".to_string());
        }
        self.model = Some(super::classes::build(self.ir)?);

        self.out.push_str("/* Generated by krusty. */\n");
        self.out.push_str("#include \"krusty_rt.h\"\n\n");

        // Layouts first: every struct is complete (inherited fields are spelled out), so nothing
        // depends on definition order, and the offsets the collector will trace are asserted
        // against what the C compiler actually laid out.
        for &class in &self.model().order.clone() {
            self.class_struct(class)?;
        }

        // Forward declarations next, so order of definition never decides what resolves.
        for (index, function) in self.ir.functions.iter().enumerate() {
            if function.dispatch_receiver.is_some() && function.body.is_none() {
                continue;
            }
            self.out
                .push_str(&format!("{};\n", self.signature(index, function)?));
        }
        for class in 0..self.ir.classes.len() {
            let class = class as ClassId;
            self.out
                .push_str(&format!("{};\n", self.constructor_signature(class)));
        }
        self.out.push('\n');

        // Descriptors and vtables reference the functions declared above, and a descriptor names
        // its superclass's, so they go out superclass-first; synthesized accessors are discovered
        // while writing the vtables and defined right after.
        for &class in &self.model().order.clone() {
            self.class_descriptor(class)?;
        }
        let synthesized = std::mem::take(&mut self.synthesized);
        for accessor in &synthesized {
            self.synthesized_accessor(accessor);
        }

        for class in 0..self.ir.classes.len() {
            self.constructor(class as ClassId)?;
        }

        let mut entry = None;
        for index in 0..self.ir.functions.len() {
            let function = &self.ir.functions[index];
            let Some(body) = function.body else {
                if function.dispatch_receiver.is_some() {
                    // Abstract: its vtable entry is the runtime's loud failure.
                    continue;
                }
                return Err(format!("a body-less function `{}`", function.name));
            };
            if function.name == "main" && function.params.is_empty() && function.is_static {
                entry = Some(self.symbols.functions[index].clone());
            }

            self.begin_frame();
            let mut first_slot = 0;
            if let Some(owner) = function.dispatch_receiver {
                self.values.insert(0, Ty::Obj(owner, &[]));
                first_slot = 1;
            }
            for (slot, ty) in function.params.iter().enumerate() {
                self.values.insert(slot as u32 + first_slot, *ty);
            }
            self.collect_variable_types(body);

            let signature = self.signature(index, function)?;
            self.out.push_str(&format!("{signature} {{\n"));
            self.result = c_kind(function.ret);
            self.statement(body, 1)?;
            self.out.push_str("}\n\n");
        }

        Ok(CTranslationUnit {
            source: self.out,
            entry,
        })
    }

    /// Reset the per-function state.
    fn begin_frame(&mut self) {
        self.values = HashMap::new();
        self.loops.clear();
        self.temp_count = 0;
        self.result = CKind::Void;
    }

    fn signature(
        &self,
        index: usize,
        function: &crate::ir::IrFunction,
    ) -> Result<String, Unsupported> {
        let mut parameters = Vec::with_capacity(function.params.len() + 1);
        let mut first_slot = 0;
        if let Some(owner) = function.dispatch_receiver {
            if self.ir.class_id_by_name(owner).is_none() {
                return Err(format!(
                    "a method of `{}`, which is not declared in this file",
                    owner.render()
                ));
            }
            parameters.push("KRef v0".to_string());
            first_slot = 1;
        }
        for (slot, ty) in function.params.iter().enumerate() {
            match c_kind(*ty) {
                CKind::Void => return Err(format!("a `Unit` parameter of `{}`", function.name)),
                kind => parameters.push(format!("{} v{}", kind.spelling(), slot + first_slot)),
            }
        }
        let parameters = if parameters.is_empty() {
            "void".to_string()
        } else {
            parameters.join(", ")
        };
        Ok(format!(
            "{} {}({parameters})",
            c_kind(function.ret).spelling(),
            self.symbols.functions[index]
        ))
    }

    /// Record the declared type of every local the function introduces.
    fn collect_variable_types(&mut self, root: u32) {
        let mut pending = vec![root];
        let mut seen = std::collections::HashSet::new();
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                continue;
            }
            if let IrExpr::Variable { index, ty, .. } = self.ir.expr(id) {
                self.values.insert(*index, *ty);
            }
            crate::ir::for_each_child(&self.ir.exprs, id, &mut |child| pending.push(child));
        }
    }

    // ---- classes -----------------------------------------------------------------------------

    fn struct_name(&self, class: ClassId) -> String {
        format!("struct kt_class_{}", self.symbols.classes[class as usize])
    }

    fn type_symbol(&self, class: ClassId) -> String {
        format!("kt_type_{}", self.symbols.classes[class as usize])
    }

    fn constructor_symbol(&self, class: ClassId) -> String {
        format!("kt_{}__init", self.symbols.classes[class as usize])
    }

    /// The struct member holding field `index` of `class` — named by the DECLARING class, so a
    /// subclass field that shadows a superclass field's Kotlin name still gets its own member.
    fn member(&self, class: ClassId, index: u32) -> String {
        format!("{}_f{index}", self.symbols.classes[class as usize])
    }

    fn accessor_symbol(&self, slot: &Slot) -> String {
        match slot {
            Slot::FieldGetter { class, field } => {
                format!("kt_{}__get_f{field}", self.symbols.classes[*class as usize])
            }
            Slot::FieldSetter { class, field } => {
                format!("kt_{}__set_f{field}", self.symbols.classes[*class as usize])
            }
            _ => unreachable!("only field accessors are synthesized"),
        }
    }

    /// The superclass chain of `class`, root first, ending with `class` itself.
    fn chain(&self, class: ClassId) -> Vec<ClassId> {
        let mut chain = vec![class];
        let mut at = class;
        while let Some(parent) = self.model().layout(at).superclass {
            chain.push(parent);
            at = parent;
        }
        chain.reverse();
        chain
    }

    /// The Kotlin-facing qualified name of a class: what its default `toString` prints.
    fn kotlin_name(&self, class: ClassId) -> String {
        self.ir.classes[class as usize]
            .fq_name()
            .replace(['/', '$'], ".")
    }

    fn class_struct(&mut self, class: ClassId) -> Result<(), Unsupported> {
        let name = self.struct_name(class);
        self.out
            .push_str(&format!("{name} {{\n    KObjectHeader header;\n"));
        let mut asserts = Vec::new();
        for owner in self.chain(class) {
            let fields = self.model().layout(owner).fields.clone();
            for (index, field) in fields.iter().enumerate() {
                let member = self.member(owner, index as u32);
                self.out
                    .push_str(&format!("    {} {member};\n", field.kind.spelling()));
                asserts.push(format!(
                    "_Static_assert(offsetof({name}, {member}) == {}, \"krusty: layout of {}.{}\");\n",
                    field.offset,
                    self.ir.classes[owner as usize].fq_name(),
                    self.ir.classes[owner as usize].fields[index].name
                ));
            }
        }
        self.out.push_str("};\n");
        for assert in asserts {
            self.out.push_str(&assert);
        }
        self.out.push_str(&format!(
            "_Static_assert(sizeof({name}) == {}, \"krusty: size of {}\");\n\n",
            self.model().layout(class).instance_size,
            self.ir.classes[class as usize].fq_name()
        ));
        Ok(())
    }

    /// The reference-offset table, the vtable and the `KType` of one class.
    fn class_descriptor(&mut self, class: ClassId) -> Result<(), Unsupported> {
        let layout = self.model().layout(class).clone();
        let base = self.symbols.classes[class as usize].clone();
        let references = if layout.reference_offsets.is_empty() {
            "NULL".to_string()
        } else {
            let offsets = layout
                .reference_offsets
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            self.out.push_str(&format!(
                "static const uint32_t kt_refs_{base}[] = {{{offsets}}};\n"
            ));
            format!("kt_refs_{base}")
        };

        let mut entries = Vec::with_capacity(layout.vtable.len());
        for slot in &layout.vtable {
            let symbol = match slot {
                Slot::Runtime(symbol) => (*symbol).to_string(),
                Slot::Function(fid) => self.symbols.functions[*fid as usize].clone(),
                Slot::Abstract => "kt_abstract_method_called".to_string(),
                Slot::FieldGetter { .. } | Slot::FieldSetter { .. } => {
                    if !self.synthesized.contains(slot) {
                        self.synthesized.push(slot.clone());
                    }
                    self.accessor_symbol(slot)
                }
            };
            entries.push(format!("(kt_fn){symbol}"));
        }
        // Synthesized accessors are defined after the descriptors; declare them here.
        for slot in self.synthesized.clone() {
            let symbol = self.accessor_symbol(&slot);
            let (Slot::FieldGetter { class, field } | Slot::FieldSetter { class, field }) = slot
            else {
                continue;
            };
            let kind = self.model().layout(class).fields[field as usize].kind;
            let declaration = if matches!(slot, Slot::FieldGetter { .. }) {
                format!("static {} {symbol}(KRef v0);\n", kind.spelling())
            } else {
                format!("static void {symbol}(KRef v0, {} v1);\n", kind.spelling())
            };
            if !self.out.contains(&declaration) {
                self.out.push_str(&declaration);
            }
        }
        self.out.push_str(&format!(
            "static const kt_fn kt_vtable_{base}[] = {{{}}};\n",
            entries.join(", ")
        ));

        let kotlin_name = self.kotlin_name(class);
        let parent = match layout.superclass {
            Some(parent) => self.type_symbol(parent),
            None => "kt_type_any".to_string(),
        };
        self.out.push_str(&format!(
            "static const KType kt_type_{base} = {{{}, {}, {}, {}, {references}, &{parent}, \
             kt_vtable_{base}, {}}};\n\n",
            c_literal_of(&kotlin_name),
            kotlin_name.len(),
            layout.instance_size,
            layout.reference_offsets.len(),
            layout.vtable.len()
        ));
        Ok(())
    }

    fn synthesized_accessor(&mut self, slot: &Slot) {
        let symbol = self.accessor_symbol(slot);
        let (Slot::FieldGetter { class, field } | Slot::FieldSetter { class, field }) = slot else {
            return;
        };
        let kind = self.model().layout(*class).fields[*field as usize].kind;
        let access = format!(
            "(({} *)v0)->{}",
            self.struct_name(*class),
            self.member(*class, *field)
        );
        if matches!(slot, Slot::FieldGetter { .. }) {
            self.out.push_str(&format!(
                "static {} {symbol}(KRef v0) {{\n    return {access};\n}}\n\n",
                kind.spelling()
            ));
        } else {
            self.out.push_str(&format!(
                "static void {symbol}(KRef v0, {} v1) {{\n    {access} = v1;\n}}\n\n",
                kind.spelling()
            ));
        }
    }

    fn constructor_signature(&self, class: ClassId) -> String {
        let declaration = &self.ir.classes[class as usize];
        let mut parameters = vec!["KRef v0".to_string()];
        for (index, argument) in declaration.ctor_args.iter().enumerate() {
            parameters.push(format!("{} v{}", c_kind(argument.ty).spelling(), index + 1));
        }
        format!(
            "static void {}({})",
            self.constructor_symbol(class),
            parameters.join(", ")
        )
    }

    /// The constructor: the superclass constructor first, then this class's parameter stores,
    /// then its initializers in source order — Kotlin's order, which a base-class `init` that
    /// prints can observe.
    fn constructor(&mut self, class: ClassId) -> Result<(), Unsupported> {
        let declaration = self.ir.classes[class as usize].clone();
        let signature = self.constructor_signature(class);
        self.begin_frame();
        self.values
            .insert(0, Ty::Obj(declaration.fq_name_id(), &[]));
        for (index, argument) in declaration.ctor_args.iter().enumerate() {
            if c_kind(argument.ty) == CKind::Void {
                return Err(format!(
                    "a `Unit` constructor parameter of `{}`",
                    declaration.fq_name()
                ));
            }
            self.values.insert(index as u32 + 1, argument.ty);
        }
        for &expression in declaration
            .super_arg_prelude
            .iter()
            .chain(&declaration.super_args)
            .chain(&declaration.init_body)
        {
            self.collect_variable_types(expression);
        }

        self.out.push_str(&format!("{signature} {{\n"));
        for &statement in &declaration.super_arg_prelude {
            self.statement(statement, 1)?;
        }
        if let Some(parent) = self.model().layout(class).superclass {
            let parent_declaration = &self.ir.classes[parent as usize];
            if declaration.super_args.len() != parent_declaration.ctor_args.len() {
                return Err(format!(
                    "a superclass constructor call with defaulted arguments (`{}`)",
                    declaration.fq_name()
                ));
            }
            let mut arguments = vec!["v0".to_string()];
            let parameter_types: Vec<Ty> = parent_declaration
                .ctor_args
                .iter()
                .map(|argument| argument.ty)
                .collect();
            for (&argument, ty) in declaration.super_args.iter().zip(parameter_types) {
                arguments.push(self.coerce(argument, ty)?);
            }
            let call = format!(
                "{}({});",
                self.constructor_symbol(parent),
                arguments.join(", ")
            );
            self.line(1, &call);
        } else if !declaration.super_args.is_empty() {
            return Err(format!(
                "a superclass constructor call to `{}`",
                declaration.superclass.render()
            ));
        }
        if !declaration.explicit_param_stores {
            let mut next_field = 0;
            for (index, argument) in declaration.ctor_args.iter().enumerate() {
                if !argument.is_field {
                    continue;
                }
                let field = argument.field_index.unwrap_or(next_field);
                next_field = field + 1;
                let field_type = declaration.fields[field as usize].ty;
                let value = self.convert(format!("v{}", index + 1), argument.ty, field_type)?;
                let store = format!(
                    "(({} *)v0)->{} = {value};",
                    self.struct_name(class),
                    self.member(class, field)
                );
                self.line(1, &store);
            }
        }
        if let Some(body) = declaration.init_body {
            self.result = CKind::Void;
            self.statement(body, 1)?;
        }
        self.out.push_str("}\n\n");
        Ok(())
    }

    /// The in-file class a name denotes, or the decline for one declared elsewhere.
    fn class_of(&self, internal: TypeName, what: &str) -> Result<ClassId, Unsupported> {
        self.ir.class_id_by_name(internal).ok_or_else(|| {
            format!(
                "{what} `{}`, which is not declared in this file",
                internal.render()
            )
        })
    }

    /// The runtime descriptor for a type an `is`/`as` names: an in-file class or a built-in.
    fn type_descriptor(&self, ty: Ty) -> Result<Option<String>, Unsupported> {
        let target = ty.non_null();
        if let Some(class) = target
            .obj_internal()
            .and_then(|name| self.ir.class_id_by_name(name))
        {
            return Ok(Some(self.type_symbol(class)));
        }
        Ok(match target {
            Ty::String => Some("kt_type_string".to_string()),
            Ty::Boolean => Some("kt_type_boolean".to_string()),
            Ty::Byte => Some("kt_type_byte".to_string()),
            Ty::Short => Some("kt_type_short".to_string()),
            Ty::Int => Some("kt_type_int".to_string()),
            Ty::Long => Some("kt_type_long".to_string()),
            Ty::Char => Some("kt_type_char".to_string()),
            _ => None,
        })
    }

    // ---- types -------------------------------------------------------------------------------

    /// The Kotlin type an expression produces, as far as the emitter needs it: enough to decide
    /// between a scalar and a reference carrier. `None` means "not determined", and every caller
    /// that cannot proceed without it declines.
    fn ty_of(&self, id: u32) -> Option<Ty> {
        Some(match self.ir.expr(id) {
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
            IrExpr::UnitInstance => Ty::obj("kotlin/Unit"),
            IrExpr::GetValue(slot) => *self.values.get(slot)?,
            IrExpr::TypeOp {
                op, type_operand, ..
            } => match op {
                IrTypeOp::InstanceOf | IrTypeOp::NotInstanceOf => Ty::Boolean,
                IrTypeOp::SafeCast => Ty::nullable(*type_operand),
                _ => *type_operand,
            },
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
                // produces `Int`, and a result typed `Byte` here would pick the wrong carrier.
                _ => match self.ty_of(*lhs)? {
                    Ty::Byte | Ty::Short | Ty::Char => Ty::Int,
                    other => other,
                },
            },
            IrExpr::Block { value, .. } => match value {
                Some(value) => self.ty_of(*value)?,
                None => Ty::Unit,
            },
            IrExpr::When { branches } => self.ty_of(branches.first()?.1)?,
            IrExpr::Call { callee, .. } => self.callee_result(callee)?,
            IrExpr::New { internal, .. } => Ty::Obj(*internal, &[]),
            IrExpr::MethodCall { class, index, .. } => {
                let fid = self.ir.classes[*class as usize].methods[*index as usize];
                self.ir.functions[fid as usize].ret
            }
            IrExpr::GetField { class, index, .. } => {
                self.ir.classes[*class as usize].fields[*index as usize].ty
            }
            IrExpr::PropertyRead { ty, .. } => *ty,
            IrExpr::Checked(IrCheckedOperation::PropertyRead { target, .. }) => {
                self.ir.checked_properties.get(target)?.ty
            }
            _ => return None,
        })
    }

    fn callee_result(&self, callee: &Callee) -> Option<Ty> {
        Some(match callee {
            Callee::Local(function) => self.ir.functions[*function as usize].ret,
            Callee::External { ret, .. }
            | Callee::Intrinsic { ret, .. }
            | Callee::Super { ret, .. } => *ret,
            Callee::Special { source, .. } => {
                let fid = self.ir.checked_callable_functions.get(source.as_ref()?)?;
                self.ir.functions[*fid as usize].ret
            }
            _ => return None,
        })
    }

    // ---- statements --------------------------------------------------------------------------

    fn statement(&mut self, id: u32, depth: usize) -> Result<(), Unsupported> {
        match self.ir.expr(id).clone() {
            IrExpr::Block { stmts, value } => {
                for statement in stmts {
                    self.statement(statement, depth)?;
                }
                if let Some(value) = value {
                    self.statement(value, depth)?;
                }
            }
            IrExpr::Return(value) => {
                let rendered = match value {
                    // A `return e` whose `e` is `Unit`-typed still has to EVALUATE `e`.
                    Some(value) if self.result == CKind::Void => {
                        let expression = self.expression(value)?;
                        format!("{expression};\n{}return;", indent_of(depth))
                    }
                    Some(value) => format!("return {};", self.expression(value)?),
                    None => "return;".to_string(),
                };
                self.line(depth, &rendered);
            }
            IrExpr::Variable {
                index, ty, init, ..
            } => {
                self.values.insert(index, ty);
                let kind = c_kind(ty);
                if kind == CKind::Void {
                    // A `Unit` local holds nothing, but its initializer is still program text that
                    // runs. Emit the initializer and drop the (absent) value.
                    if let Some(init) = init {
                        let expression = self.expression(init)?;
                        self.line(depth, &format!("{expression};"));
                    }
                } else {
                    let declaration = match init {
                        Some(init) => {
                            format!("{} v{index} = {};", kind.spelling(), self.coerce(init, ty)?)
                        }
                        None => format!("{} v{index};", kind.spelling()),
                    };
                    self.line(depth, &declaration);
                }
            }
            IrExpr::SetValue { var, value } => {
                let target = self.values.get(&var).copied();
                let value = match target {
                    Some(ty) => self.coerce(value, ty)?,
                    None => self.expression(value)?,
                };
                self.line(depth, &format!("v{var} = {value};"));
            }
            IrExpr::SetField {
                receiver,
                class,
                index,
                value,
            } => {
                let field_type = self.ir.classes[class as usize].fields[index as usize].ty;
                let value = self.coerce(value, field_type)?;
                let target = self.field_access(receiver, class, index)?;
                self.line(depth, &format!("{target} = {value};"));
            }
            IrExpr::Checked(IrCheckedOperation::PropertyWrite {
                target,
                dispatch_receiver,
                extension_receiver,
                context_arguments,
                value,
                ..
            }) => {
                if extension_receiver.is_some() || !context_arguments.is_empty() {
                    return Err("an extension or context property".to_string());
                }
                let written = self.property_write(target, dispatch_receiver, value)?;
                self.line(depth, &format!("{written};"));
            }
            IrExpr::When { branches } => {
                let mut first = true;
                for (condition, body) in branches {
                    match condition {
                        Some(condition) => {
                            let condition = self.expression(condition)?;
                            let keyword = if first { "if" } else { "else if" };
                            self.line(depth, &format!("{keyword} ({condition}) {{"));
                        }
                        None => self.line(depth, "else {"),
                    }
                    self.statement(body, depth + 1)?;
                    self.line(depth, "}");
                    first = false;
                }
            }
            IrExpr::While {
                cond,
                body,
                update,
                post_test,
                label,
            } => self.loop_statement(cond, body, update, post_test, label, depth)?,
            IrExpr::Break { label } => {
                let jump = self.jump(label.as_deref(), Exit::Break)?;
                self.line(depth, &jump);
            }
            IrExpr::Continue { label } => {
                let jump = self.jump(label.as_deref(), Exit::Continue)?;
                self.line(depth, &jump);
            }
            _ => {
                let expression = self.expression(id)?;
                self.line(depth, &format!("{expression};"));
            }
        }
        Ok(())
    }

    /// Emit a `while`/`do…while`, with the `goto` scaffolding C needs for anything a plain
    /// `break`/`continue` cannot express.
    ///
    /// Two things force `goto`. A Kotlin label (`break@outer`) has no C equivalent at all. And a
    /// loop whose `update` is a STATEMENT SEQUENCE — which is what a lowered `for` produces, since
    /// the step carries its own overflow guard — cannot put that update in a `for` header, so the
    /// update lands at the end of the body and every `continue`, labeled or not, has to jump to a
    /// point before it or the loop would not advance.
    fn loop_statement(
        &mut self,
        cond: u32,
        body: u32,
        update: Option<u32>,
        post_test: bool,
        label: Option<String>,
        depth: usize,
    ) -> Result<(), Unsupported> {
        self.label_count += 1;
        let base = format!(
            "kt_loop{}{}",
            self.label_count,
            label
                .as_deref()
                .map(c_identifier)
                .map_or(String::new(), |l| format!("_{l}"))
        );
        self.loops.push(LoopFrame {
            label,
            base: base.clone(),
            // With an update to run, `continue` must reach it; C's own `continue` would skip it.
            continue_is_goto: update.is_some(),
        });

        // The body is emitted into a buffer first so the `goto` targets can be omitted unless the
        // body actually jumped to them; an emitted-but-unused C label is dead text in the output.
        let saved = std::mem::take(&mut self.out);
        self.statement(body, depth + 1)?;
        let rendered_body = std::mem::replace(&mut self.out, saved);
        let rendered_update = match update {
            Some(update) => {
                let saved = std::mem::take(&mut self.out);
                self.statement(update, depth + 1)?;
                Some(std::mem::replace(&mut self.out, saved))
            }
            None => None,
        };
        self.loops.pop();

        // The update is scanned too: a lowered `for` puts its overflow guard's `break` there, and
        // a label emitted only for jumps found in the body would leave that one undefined.
        let jumped_to = |target: &str| {
            let jump = format!("goto {base}_{target};");
            rendered_body.contains(&jump)
                || rendered_update
                    .as_deref()
                    .is_some_and(|update| update.contains(&jump))
        };
        let jumped_to_continue = jumped_to("continue");
        let jumped_to_break = jumped_to("break");

        let condition = self.expression(cond)?;
        if post_test {
            self.line(depth, "do {");
        } else {
            self.line(depth, &format!("while ({condition}) {{"));
        }
        self.out.push_str(&rendered_body);
        if jumped_to_continue {
            self.line(depth + 1, &format!("{base}_continue: ;"));
        }
        if let Some(update) = rendered_update {
            self.out.push_str(&update);
        }
        if post_test {
            self.line(depth, &format!("}} while ({condition});"));
        } else {
            self.line(depth, "}");
        }
        if jumped_to_break {
            self.line(depth, &format!("{base}_break: ;"));
        }
        Ok(())
    }

    /// The C statement realizing a `break`/`continue`, labeled or not.
    fn jump(&self, label: Option<&str>, exit: Exit) -> Result<String, Unsupported> {
        let frame = match label {
            Some(label) => self
                .loops
                .iter()
                .rev()
                .find(|frame| frame.label.as_deref() == Some(label)),
            None => self.loops.last(),
        };
        let Some(frame) = frame else {
            return Err(match label {
                Some(label) => format!("`{exit}@{label}`, whose loop is not in scope"),
                None => format!("`{exit}` outside a loop"),
            });
        };
        let needs_goto = label.is_some() || (exit == Exit::Continue && frame.continue_is_goto);
        Ok(if needs_goto {
            format!("goto {}_{exit};", frame.base)
        } else {
            format!("{exit};")
        })
    }

    fn line(&mut self, depth: usize, text: &str) {
        self.out.push_str(&indent_of(depth));
        self.out.push_str(text);
        self.out.push('\n');
    }

    // ---- expressions -------------------------------------------------------------------------

    fn expression(&mut self, id: u32) -> Result<String, Unsupported> {
        match self.ir.expr(id).clone() {
            IrExpr::Const(constant) => constant_expression(&constant),
            IrExpr::UnitInstance => Ok("kt_unit()".to_string()),
            IrExpr::GetValue(slot) => Ok(format!("v{slot}")),
            IrExpr::PrimitiveNeg { operand, ty } => {
                let rendered = self.expression(operand)?;
                // `-Int.MIN_VALUE` is `Int.MIN_VALUE` in Kotlin and undefined in C.
                match scalar_suffix(c_kind(ty)) {
                    Some(width @ ("int" | "long")) => {
                        let unsigned = if width == "int" {
                            "uint32_t"
                        } else {
                            "uint64_t"
                        };
                        Ok(format!("((kt_{width})(0u - ({unsigned})({rendered})))"))
                    }
                    _ => Ok(format!("(-{rendered})")),
                }
            }
            IrExpr::PrimitiveBinOp { op, lhs, rhs } => self.binary(op, lhs, rhs),
            IrExpr::StringConcat(parts) => {
                let mut rendered = "kt_string_utf8(\"\", 0)".to_string();
                for part in parts {
                    let part = self.reference(part)?;
                    rendered = format!("kt_string_plus({rendered}, {part})");
                }
                Ok(rendered)
            }
            IrExpr::TypeOp {
                op,
                arg,
                type_operand,
            } => self.type_operation(op, arg, type_operand),
            IrExpr::Call {
                callee,
                dispatch_receiver,
                args,
            } => self.call(&callee, dispatch_receiver, &args),
            IrExpr::New {
                internal,
                args,
                ctor_params,
                defaults,
                ..
            } => self.construction(internal, &args, ctor_params.is_some(), !defaults.is_empty()),
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
            } => self.field_access(receiver, class, index),
            IrExpr::Checked(IrCheckedOperation::PropertyRead {
                target,
                dispatch_receiver,
                extension_receiver,
                context_arguments,
                ..
            }) => {
                if extension_receiver.is_some() || !context_arguments.is_empty() {
                    return Err("an extension or context property".to_string());
                }
                self.property_read(target, dispatch_receiver)
            }
            IrExpr::Checked(other) => {
                Err(format!("a checked {} operation", describe_debug(&other)))
            }
            IrExpr::Block { stmts, value } => {
                // A block with a value in expression position becomes a GNU statement expression.
                let Some(value) = value else {
                    return Err("a block with no value in expression position".to_string());
                };
                let saved = std::mem::take(&mut self.out);
                for statement in stmts {
                    self.statement(statement, 0)?;
                }
                let body = std::mem::replace(&mut self.out, saved);
                let value = self.expression(value)?;
                let body = body.replace('\n', " ");
                Ok(format!("({{ {body}{value}; }})"))
            }
            IrExpr::When { branches } => {
                // Right-fold the arms into nested conditionals. An arm that is not an expression
                // (a `break`, a `return`) has already been handled in statement position; reaching
                // one here means the IR wants its value, which a conditional cannot supply.
                let mut arms = branches.into_iter().rev();
                let Some((_, otherwise)) = arms.next() else {
                    return Err("an empty `when` in expression position".to_string());
                };
                let mut rendered = self.expression(otherwise)?;
                for (condition, body) in arms {
                    let Some(condition) = condition else {
                        return Err("a `when` whose `else` is not last".to_string());
                    };
                    rendered = format!(
                        "({} ? {} : {rendered})",
                        self.expression(condition)?,
                        self.expression(body)?
                    );
                }
                Ok(rendered)
            }
            other => Err(describe(&other)),
        }
    }

    /// Realize a built-in binary operator.
    ///
    /// Three families of operator cannot be C's spelling of the same symbol, and each is a silent
    /// wrong answer rather than a compile error if it is emitted naively:
    ///
    /// * `==` on references is Kotlin's STRUCTURAL equality; C's compares addresses. The one case
    ///   where the two agree — a comparison against the `null` literal — is emitted; the rest is
    ///   declined.
    /// * `+`, `-`, `*` and unary `-` WRAP on overflow in Kotlin; signed overflow is undefined in C,
    ///   which a compiler is free to assume never happens.
    /// * `/`, `%` and the shifts are defined by Kotlin for operands C leaves undefined.
    fn binary(&mut self, op: IrBinOp, lhs: u32, rhs: u32) -> Result<String, Unsupported> {
        let operand = self.ty_of(lhs).map(c_kind);
        let scalar = operand.and_then(scalar_suffix);

        if matches!(op, IrBinOp::Eq | IrBinOp::Ne) {
            let against_null = matches!(self.ir.expr(lhs), IrExpr::Const(IrConst::Null))
                || matches!(self.ir.expr(rhs), IrExpr::Const(IrConst::Null));
            match operand {
                // `x == null` is `x === null` in Kotlin: no `equals` is ever called.
                _ if against_null => {
                    let left = self.reference(lhs)?;
                    let right = self.reference(rhs)?;
                    return Ok(format!("({left} {} {right})", c_operator(op)?));
                }
                Some(CKind::Ref) => return Err("structural equality on references".to_string()),
                // Neither a known scalar nor a known reference: emitting either equality would be
                // a guess about which one Kotlin means.
                _ if scalar.is_none() => {
                    return Err("an equality on an undetermined operand type".to_string())
                }
                _ => {}
            }
        }

        let left = self.expression(lhs)?;
        let right = self.expression(rhs)?;

        // Wrapping arithmetic, spelled through the unsigned type of the same width: unsigned
        // overflow is defined to wrap in C, which is exactly Kotlin's rule.
        let wrapping = match (op, scalar) {
            (IrBinOp::Add, Some(width @ ("int" | "long"))) => Some((width, "+")),
            (IrBinOp::Sub, Some(width @ ("int" | "long"))) => Some((width, "-")),
            (IrBinOp::Mul, Some(width @ ("int" | "long"))) => Some((width, "*")),
            _ => None,
        };
        if let Some((width, operator)) = wrapping {
            let unsigned = if width == "int" {
                "uint32_t"
            } else {
                "uint64_t"
            };
            return Ok(format!(
                "((kt_{width})(({unsigned})({left}) {operator} ({unsigned})({right})))"
            ));
        }

        let helper = match (op, scalar) {
            (IrBinOp::Div, Some(width @ ("int" | "long"))) => Some(format!("kt_div_{width}")),
            (IrBinOp::Rem, Some(width @ ("int" | "long"))) => Some(format!("kt_rem_{width}")),
            (IrBinOp::Shl, Some(width @ ("int" | "long"))) => Some(format!("kt_shl_{width}")),
            (IrBinOp::Shr, Some(width @ ("int" | "long"))) => Some(format!("kt_shr_{width}")),
            (IrBinOp::Ushr, Some(width @ ("int" | "long"))) => Some(format!("kt_ushr_{width}")),
            // IEEE division and remainder are the same in both languages.
            (IrBinOp::Div | IrBinOp::Rem, Some("float" | "double")) => None,
            (IrBinOp::Div | IrBinOp::Rem | IrBinOp::Shl | IrBinOp::Shr | IrBinOp::Ushr, _) => {
                return Err(format!("`{}` on this operand type", c_operator(op)?))
            }
            _ => None,
        };
        if let Some(helper) = helper {
            return Ok(format!("{helper}({left}, {right})"));
        }

        Ok(format!("({left} {} {right})", c_operator(op)?))
    }

    /// An expression in a position that requires a `KRef`, boxing a scalar if necessary.
    fn reference(&mut self, id: u32) -> Result<String, Unsupported> {
        let rendered = self.expression(id)?;
        let Some(ty) = self.ty_of(id) else {
            return Ok(rendered);
        };
        let kind = c_kind(ty);
        match (box_suffix(kind), scalar_suffix(kind)) {
            (Some(suffix), _) => Ok(format!("kt_box_{suffix}({rendered})")),
            (None, Some(_)) => Err(unrenderable(kind)),
            (None, None) => Ok(rendered),
        }
    }

    /// Realize a representation change between two carriers.
    fn coerce(&mut self, arg: u32, target: Ty) -> Result<String, Unsupported> {
        let rendered = self.expression(arg)?;
        match self.ty_of(arg) {
            // Unknown source: the only safe move is to leave the value alone.
            None => Ok(rendered),
            Some(source) => self.convert(rendered, source, target),
        }
    }

    /// Convert an already-rendered value of type `source` to the carrier of `target`.
    fn convert(&self, rendered: String, source: Ty, target: Ty) -> Result<String, Unsupported> {
        let source = c_kind(source);
        let target = c_kind(target);
        match (source, target) {
            (source, target) if source == target => Ok(rendered),
            (source @ CKind::Scalar(_), CKind::Ref) => match box_suffix(source) {
                Some(suffix) => Ok(format!("kt_box_{suffix}({rendered})")),
                None => Err(unrenderable(source)),
            },
            (CKind::Ref, CKind::Scalar(_)) => match box_suffix(target) {
                Some(suffix) => Ok(format!("kt_unbox_{suffix}({rendered})")),
                None => Err(unrenderable(target)),
            },
            // Scalar to a different scalar is a widening/narrowing C cast.
            (CKind::Scalar(_), CKind::Scalar(name)) => Ok(format!("(({name}){rendered})")),
            (_, CKind::Void) => Ok(rendered),
            (CKind::Void, _) => Err("a coercion from `Unit`".to_string()),
            (CKind::Ref, CKind::Ref) => unreachable!("equal carriers are handled above"),
        }
    }

    /// `is`, `as`, `as?` and the coercions the frontend inserts.
    fn type_operation(
        &mut self,
        op: IrTypeOp,
        arg: u32,
        type_operand: Ty,
    ) -> Result<String, Unsupported> {
        let class_target = type_operand
            .non_null()
            .obj_internal()
            .and_then(|name| self.ir.class_id_by_name(name));
        match op {
            IrTypeOp::InstanceOf | IrTypeOp::NotInstanceOf => {
                let Some(descriptor) = self.type_descriptor(type_operand)? else {
                    return Err(format!(
                        "an `is` check against `{}`",
                        type_name_of(type_operand)
                    ));
                };
                if self.ty_of(arg).is_some_and(|ty| c_kind(ty) != CKind::Ref) {
                    return Err("an `is` check on a scalar".to_string());
                }
                let operand = self.expression(arg)?;
                let check = if type_operand.is_nullable() {
                    let temp = self.temp();
                    format!(
                        "({{ KRef {temp} = {operand}; ({temp} == NULL || kt_is_instance({temp}, \
                         &{descriptor})); }})"
                    )
                } else {
                    format!("kt_is_instance({operand}, &{descriptor})")
                };
                Ok(if op == IrTypeOp::InstanceOf {
                    check
                } else {
                    format!("(!{check})")
                })
            }
            IrTypeOp::Cast | IrTypeOp::CastNonNull if class_target.is_some() => {
                let descriptor = self.type_symbol(class_target.expect("checked"));
                let helper = if op == IrTypeOp::Cast || type_operand.is_nullable() {
                    "kt_cast"
                } else {
                    "kt_cast_non_null"
                };
                let operand = self.reference(arg)?;
                Ok(format!("{helper}({operand}, &{descriptor})"))
            }
            IrTypeOp::SafeCast => {
                let Some(class) = class_target else {
                    return Err(format!("an `as?` to `{}`", type_name_of(type_operand)));
                };
                let operand = self.reference(arg)?;
                Ok(format!(
                    "kt_safe_cast({operand}, &{})",
                    self.type_symbol(class)
                ))
            }
            IrTypeOp::ImplicitCoercion | IrTypeOp::Cast | IrTypeOp::CastNonNull => {
                self.coerce(arg, type_operand)
            }
        }
    }

    fn temp(&mut self) -> String {
        self.temp_count += 1;
        format!("kt_r{}", self.temp_count)
    }

    /// `receiver.<field>` as a C lvalue.
    fn field_access(
        &mut self,
        receiver: u32,
        class: ClassId,
        index: u32,
    ) -> Result<String, Unsupported> {
        let object = self.reference(receiver)?;
        Ok(format!(
            "(({} *)({object}))->{}",
            self.struct_name(class),
            self.member(class, index)
        ))
    }

    /// A call through the receiver's vtable: the receiver is evaluated once into a temporary,
    /// which both selects the implementation and is passed as its first argument.
    fn dispatch(
        &mut self,
        receiver: u32,
        slot: u32,
        ret: CKind,
        parameters: &[Ty],
        arguments: &[u32],
    ) -> Result<String, Unsupported> {
        let object = self.reference(receiver)?;
        let temp = self.temp();
        let mut rendered = vec![temp.clone()];
        let mut signature = vec!["KRef".to_string()];
        for (&argument, &ty) in arguments.iter().zip(parameters) {
            rendered.push(self.coerce(argument, ty)?);
            signature.push(c_kind(ty).spelling().to_string());
        }
        Ok(format!(
            "({{ KRef {temp} = {object}; (({} (*)({}))kt_dispatch({temp}, {slot}))({}); }})",
            ret.spelling(),
            signature.join(", "),
            rendered.join(", ")
        ))
    }

    fn construction(
        &mut self,
        internal: TypeName,
        args: &[u32],
        secondary: bool,
        defaulted: bool,
    ) -> Result<String, Unsupported> {
        let name = internal.render();
        if secondary {
            return Err(format!("a secondary constructor call (`{name}`)"));
        }
        if defaulted {
            return Err(format!("a constructor default argument (`{name}`)"));
        }
        let class = self.class_of(internal, "construction of")?;
        let declaration = &self.ir.classes[class as usize];
        if declaration.is_object {
            return Err(format!("construction of the object declaration `{name}`"));
        }
        if declaration.is_abstract || declaration.is_sealed {
            return Err(format!("construction of the abstract class `{name}`"));
        }
        if args.len() != declaration.ctor_args.len() {
            return Err(format!(
                "a constructor call with omitted arguments (`{name}`)"
            ));
        }
        let parameter_types: Vec<Ty> = declaration
            .ctor_args
            .iter()
            .map(|argument| argument.ty)
            .collect();
        let temp = self.temp();
        let mut arguments = vec![temp.clone()];
        for (&argument, ty) in args.iter().zip(parameter_types) {
            arguments.push(self.coerce(argument, ty)?);
        }
        Ok(format!(
            "({{ KRef {temp} = (KRef)kt_gc_allocate(&{}, sizeof({})); {}({}); {temp}; }})",
            self.type_symbol(class),
            self.struct_name(class),
            self.constructor_symbol(class),
            arguments.join(", ")
        ))
    }

    fn method_call(
        &mut self,
        class: ClassId,
        index: u32,
        receiver: u32,
        args: &[Option<u32>],
    ) -> Result<String, Unsupported> {
        let fid = self.ir.classes[class as usize].methods[index as usize];
        let function = &self.ir.functions[fid as usize];
        let arguments: Option<Vec<u32>> = args.iter().copied().collect();
        let Some(arguments) = arguments else {
            return Err(format!(
                "a call with a defaulted argument (`{}.{}`)",
                self.ir.classes[class as usize].fq_name(),
                function.name
            ));
        };
        if function.dispatch_receiver.is_none() {
            return Err(format!("a class-static call (`{}`)", function.name));
        }
        let key = super::classes::function_key(self.ir, class, fid);
        let Some(slot) = self.model().slot(class, &key) else {
            return Err(format!(
                "a method with no dispatch slot (`{}`)",
                function.name
            ));
        };
        let parameters = function.params.clone();
        let ret = c_kind(function.ret);
        self.dispatch(receiver, slot, ret, &parameters, &arguments)
    }

    /// The property a checked operation names, as (class, property index).
    fn checked_property(
        &self,
        target: crate::fir::PropertyId,
    ) -> Result<(ClassId, usize), Unsupported> {
        let Some(property) = self.ir.checked_properties.get(&target) else {
            return Err("a property with no checked declaration".to_string());
        };
        let Some(class) = property.class else {
            return Err(format!("a top-level property (`{}`)", property.name));
        };
        let declaration = &self.ir.classes[class as usize];
        let index = declaration
            .properties
            .iter()
            .position(|candidate| candidate.name == property.name)
            .ok_or_else(|| format!("an undeclared property (`{}`)", property.name))?;
        Ok((class, index))
    }

    fn property_read(
        &mut self,
        target: crate::fir::PropertyId,
        receiver: Option<u32>,
    ) -> Result<String, Unsupported> {
        let (class, index) = self.checked_property(target)?;
        let property = self.ir.classes[class as usize].properties[index].clone();
        let Some(receiver) = receiver else {
            return Err(format!("a receiver-less read of `{}`", property.name));
        };
        if property
            .storage_ty
            .is_some_and(|storage| c_kind(storage) != c_kind(property.ty))
        {
            return Err(format!(
                "a property whose storage differs from its type (`{}`)",
                property.name
            ));
        }
        let key = SlotKey::Getter(class, property.name.clone());
        if let Some(slot) = self.model().slot(class, &key) {
            let ret = c_kind(property.ty);
            return self.dispatch(receiver, slot, ret, &[], &[]);
        }
        if let Some(getter) = property.getter {
            let object = self.reference(receiver)?;
            return Ok(format!(
                "{}({object})",
                self.symbols.functions[getter as usize]
            ));
        }
        match property.backing_field {
            Some(field) => self.field_access(receiver, class, field),
            None => Err(format!(
                "a property with neither storage nor a getter (`{}`)",
                property.name
            )),
        }
    }

    fn property_write(
        &mut self,
        target: crate::fir::PropertyId,
        receiver: Option<u32>,
        value: u32,
    ) -> Result<String, Unsupported> {
        let (class, index) = self.checked_property(target)?;
        let property = self.ir.classes[class as usize].properties[index].clone();
        let Some(receiver) = receiver else {
            return Err(format!("a receiver-less write of `{}`", property.name));
        };
        let key = SlotKey::Setter(class, property.name.clone());
        if let Some(slot) = self.model().slot(class, &key) {
            return self.dispatch(receiver, slot, CKind::Void, &[property.ty], &[value]);
        }
        if let Some(setter) = property.setter {
            let object = self.reference(receiver)?;
            let value = self.coerce(value, property.ty)?;
            return Ok(format!(
                "{}({object}, {value})",
                self.symbols.functions[setter as usize]
            ));
        }
        match property.backing_field {
            Some(field) => {
                let field_type = self.ir.classes[class as usize].fields[field as usize].ty;
                let value = self.coerce(value, field_type)?;
                let target = self.field_access(receiver, class, field)?;
                Ok(format!("{target} = {value}"))
            }
            None => Err(format!(
                "a property with neither storage nor a setter (`{}`)",
                property.name
            )),
        }
    }

    fn call(
        &mut self,
        callee: &Callee,
        dispatch_receiver: Option<u32>,
        args: &[u32],
    ) -> Result<String, Unsupported> {
        let receiver = dispatch_receiver;
        match callee {
            Callee::Intrinsic { operation, .. } => {
                self.intrinsic(operation, dispatch_receiver, args)
            }
            Callee::Super {
                owner,
                name,
                source,
                params,
                ..
            } => {
                let Some(receiver) = receiver else {
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
                let Some(receiver) = receiver else {
                    return Err(format!("a `super` call without a receiver (`{name}`)"));
                };
                self.direct_call(*owner, name, *source, None, receiver, args)
            }
            _ if dispatch_receiver.is_some() && !matches!(callee, Callee::External { .. }) => {
                Err(format!("a {} call with a receiver", callee_kind(callee)))
            }
            Callee::Local(function) => {
                let symbol = self.symbols.functions[*function as usize].clone();
                let parameters = self.ir.functions[*function as usize].params.clone();
                let arguments = self.arguments(args, &parameters)?;
                Ok(format!("{symbol}({arguments})"))
            }
            Callee::External { target, params, .. } => {
                let Some(realization) = self.classpath.external_callable(*target) else {
                    return Err("an unresolvable dependency call".to_string());
                };
                let owner = realization.callable.owner.render();
                let name = realization.callable.name.clone();
                match receiver {
                    // A member: the receiver is the runtime function's first argument.
                    Some(receiver) => {
                        let Some(symbol) = super::intrinsics::runtime_member(&owner, &name, params)
                        else {
                            return Err(undeclared(&owner, &name));
                        };
                        let mut rendered = vec![self.reference(receiver)?];
                        for argument in args {
                            rendered.push(self.reference(*argument)?);
                        }
                        Ok(format!("{symbol}({})", rendered.join(", ")))
                    }
                    None => {
                        let Some(symbol) =
                            super::intrinsics::runtime_function(&owner, &name, params)
                        else {
                            return Err(undeclared(&owner, &name));
                        };
                        let arguments = self.arguments(args, params)?;
                        Ok(format!("{symbol}({arguments})"))
                    }
                }
            }
            other => Err(format!("a {} call", callee_kind(other))),
        }
    }

    /// A non-virtual call to the named class's own implementation: `super.f()`.
    fn direct_call(
        &mut self,
        owner: TypeName,
        name: &str,
        source: Option<crate::fir::CallableId>,
        params: Option<&[Ty]>,
        receiver: u32,
        args: &[u32],
    ) -> Result<String, Unsupported> {
        if owner.matches("kotlin/Any") {
            let symbol = match (name, args.len()) {
                ("toString", 0) => "kt_any_to_string",
                ("hashCode", 0) => "kt_any_hash_code",
                ("equals", 1) => "kt_any_equals",
                _ => return Err(format!("a `super` call to `Any.{name}`")),
            };
            let mut rendered = vec![self.reference(receiver)?];
            for argument in args {
                rendered.push(self.reference(*argument)?);
            }
            return Ok(format!("{symbol}({})", rendered.join(", ")));
        }
        let class = self.class_of(owner, "a `super` call to a method of")?;
        let fid = source
            .and_then(|callable| self.ir.checked_callable_functions.get(&callable).copied())
            .or_else(|| {
                self.ir.classes[class as usize]
                    .methods
                    .iter()
                    .copied()
                    .find(|&fid| {
                        let function = &self.ir.functions[fid as usize];
                        function.name == name
                            && params.is_none_or(|params| function.params == params)
                    })
            })
            .ok_or_else(|| format!("a `super` call to an unknown method (`{name}`)"))?;
        let function = &self.ir.functions[fid as usize];
        if function.body.is_none() {
            return Err(format!("a `super` call to the abstract method `{name}`"));
        }
        let parameters = function.params.clone();
        let mut rendered = vec![self.reference(receiver)?];
        for (&argument, ty) in args.iter().zip(parameters) {
            rendered.push(self.coerce(argument, ty)?);
        }
        Ok(format!(
            "{}({})",
            self.symbols.functions[fid as usize],
            rendered.join(", ")
        ))
    }

    /// Realize a compiler-supplied operation the frontend selected in place of a call.
    fn intrinsic(
        &mut self,
        operation: &crate::ir::IrIntrinsic,
        receiver: Option<u32>,
        args: &[u32],
    ) -> Result<String, Unsupported> {
        use crate::ir::IrIntrinsic;
        match operation {
            IrIntrinsic::PrimitiveCompare { operand } => {
                let (Some(receiver), [argument]) = (receiver, args) else {
                    return Err("a malformed `compareTo`".to_string());
                };
                let Some(suffix) = scalar_suffix(c_kind(*operand)) else {
                    return Err("`compareTo` on a non-scalar operand".to_string());
                };
                Ok(format!(
                    "kt_compare_{suffix}({}, {})",
                    self.expression(receiver)?,
                    self.expression(*argument)?
                ))
            }
            IrIntrinsic::StringPlus => {
                let (Some(receiver), [argument]) = (receiver, args) else {
                    return Err("a malformed `String.plus`".to_string());
                };
                Ok(format!(
                    "kt_string_plus({}, {})",
                    self.reference(receiver)?,
                    self.reference(*argument)?
                ))
            }
            IrIntrinsic::NullableAnyToString => {
                let Some(receiver) = receiver else {
                    return Err("a malformed `toString`".to_string());
                };
                Ok(format!("kt_to_string({})", self.reference(receiver)?))
            }
            other => Err(format!("the `{other:?}` intrinsic")),
        }
    }

    /// Arguments coerced to the parameter carriers they are passed as.
    fn arguments(&mut self, args: &[u32], parameters: &[Ty]) -> Result<String, Unsupported> {
        let mut rendered = Vec::with_capacity(args.len());
        for (index, argument) in args.iter().enumerate() {
            rendered.push(match parameters.get(index) {
                Some(ty) => self.coerce(*argument, *ty)?,
                None => self.expression(*argument)?,
            });
        }
        Ok(rendered.join(", "))
    }
}

/// A type's spelling for a diagnostic: the class name when it has one, else the debug form.
fn type_name_of(ty: Ty) -> String {
    match ty.non_null().obj_internal() {
        Some(name) => name.render(),
        None => format!("{:?}", ty.non_null()),
    }
}

/// The diagnostic for a value whose type the runtime cannot represent as a reference.
fn unrenderable(kind: CKind) -> Unsupported {
    format!(
        "a `{}` in a position that requires a reference (the runtime cannot render a \
         floating-point value; see src/native/runtime.rs)",
        match scalar_suffix(kind) {
            Some("float") => "Float",
            _ => "Double",
        }
    )
}

/// The diagnostic for a dependency declaration the runtime has no implementation of.
fn undeclared(owner: &str, name: &str) -> Unsupported {
    format!("the declaration `{}.{name}`", owner.replace('/', "."))
}

fn indent_of(depth: usize) -> String {
    "    ".repeat(depth)
}

fn constant_expression(constant: &IrConst) -> Result<String, Unsupported> {
    Ok(match constant {
        IrConst::Boolean(value) => value.to_string(),
        IrConst::Byte(value) => format!("(kt_byte){value}"),
        IrConst::Short(value) => format!("(kt_short){value}"),
        // `INT32_MIN` has no C literal: `-2147483648` is a negation of an out-of-range positive.
        IrConst::Int(value) => format!("(kt_int)INT32_C({value})"),
        IrConst::Long(value) => format!("(kt_long)INT64_C({value})"),
        IrConst::Float(value) if value.is_finite() => format!("{value:?}f"),
        IrConst::Double(value) if value.is_finite() => format!("{value:?}"),
        IrConst::Float(_) | IrConst::Double(_) => {
            return Err("a non-finite floating-point constant".to_string())
        }
        IrConst::Char(value) => format!("(kt_char){value}"),
        IrConst::String(value) => {
            let Some((literal, length)) = c_string_literal(value) else {
                return Err("a string constant containing an unpaired surrogate".to_string());
            };
            format!("kt_string_utf8({literal}, {length})")
        }
        IrConst::Null => "NULL".to_string(),
    })
}

fn c_operator(op: IrBinOp) -> Result<&'static str, Unsupported> {
    Ok(match op {
        IrBinOp::Add => "+",
        IrBinOp::Sub => "-",
        IrBinOp::Mul => "*",
        IrBinOp::Div => "/",
        IrBinOp::Rem => "%",
        IrBinOp::Lt => "<",
        IrBinOp::Le => "<=",
        IrBinOp::Gt => ">",
        IrBinOp::Ge => ">=",
        // `Eq`/`Ne` reach here only for scalar operands and comparisons against `null`; `binary`
        // declines the rest of the reference case, where Kotlin means structural equality and C's
        // `==` would compare addresses.
        IrBinOp::Eq | IrBinOp::RefEq => "==",
        IrBinOp::Ne | IrBinOp::RefNe => "!=",
        IrBinOp::And => "&&",
        IrBinOp::Or => "||",
        IrBinOp::BitAnd => "&",
        IrBinOp::BitOr => "|",
        IrBinOp::BitXor => "^",
        IrBinOp::Shl => "<<",
        IrBinOp::Shr => ">>",
        // Reached only for the diagnostic text of an operand type the helpers do not cover.
        IrBinOp::Ushr => "ushr",
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

/// The leading identifier of a `Debug` rendering: the variant name.
fn describe_debug<T: std::fmt::Debug>(node: &T) -> String {
    let debug = format!("{node:?}");
    let head = debug
        .split(|c: char| !c.is_ascii_alphanumeric())
        .find(|piece| !piece.is_empty())
        .unwrap_or("expression");
    format!("`{head}`")
}

/// A short phrase naming the construct, for the declining diagnostic.
fn describe(node: &IrExpr) -> String {
    describe_debug(node)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kt_string::KtStringBuf;

    fn kt(text: &str) -> crate::kt_string::KtString {
        let mut buffer = KtStringBuf::new();
        buffer.push_str(text);
        buffer.finish()
    }

    fn top_level(name: &str) -> crate::ir::IrFunction {
        crate::ir::IrFunction {
            name: name.to_string(),
            params: Vec::new(),
            ret: Ty::Unit,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        }
    }

    #[test]
    fn a_nullable_primitive_is_carried_as_a_reference() {
        assert_eq!(c_kind(Ty::Int), CKind::Scalar("kt_int"));
        assert_eq!(
            c_kind(Ty::nullable(Ty::Int)),
            CKind::Ref,
            "`Int?` must represent `null`, so it boxes exactly as it does on the JVM"
        );
        assert_eq!(c_kind(Ty::Unit), CKind::Void);
        assert_eq!(c_kind(Ty::String), CKind::Ref);
    }

    #[test]
    fn a_string_literal_escapes_without_running_escapes_together() {
        let (literal, length) = c_string_literal(&kt("a\"b\\c\n")).expect("encodable");
        assert_eq!(literal, "\"a\\\"b\\\\c\\n\"");
        assert_eq!(length, 6);
    }

    #[test]
    fn a_non_ascii_literal_uses_octal_escapes_with_a_fixed_width() {
        // A hex escape in C has no length limit, so `"\xC3\xA9" "1"` is fine but `"\xC3\xA91"` is one
        // enormous character. Octal is exactly three digits, which cannot run into the next byte.
        let (literal, length) = c_string_literal(&kt("é1")).expect("encodable");
        assert_eq!(literal, "\"\\303\\2511\"");
        assert_eq!(length, 3, "two UTF-8 bytes for `é`, one for `1`");
    }

    #[test]
    fn an_unpaired_surrogate_is_declined_rather_than_mangled() {
        let lone = crate::kt_string::KtString::from_units(vec![0xD800]);
        assert!(
            c_string_literal(&lone).is_none(),
            "UTF-8 cannot encode a lone surrogate; emitting something else would be a silent \
             miscompilation"
        );
    }

    #[test]
    fn an_identifier_keeps_only_what_c_accepts() {
        assert_eq!(c_identifier("greet"), "greet");
        assert_eq!(c_identifier("my.package"), "my_package");
        assert_eq!(c_identifier("$fir_control_0_1"), "_fir_control_0_1");
    }

    #[test]
    fn overloads_get_distinct_c_symbols() {
        // C has no overloading. Two Kotlin functions sharing a name must not share a symbol, or
        // the linker would silently pick one of them.
        let mut ir = IrFile::default();
        for _ in 0..3 {
            ir.functions.push(top_level("greet"));
        }
        assert_eq!(
            function_symbols(&ir),
            vec!["kt_greet", "kt_greet__1", "kt_greet__2"]
        );
    }

    #[test]
    fn a_name_that_collides_with_a_generated_suffix_still_gets_its_own_symbol() {
        // `greet` twice yields `kt_greet` and `kt_greet__1`. A Kotlin function actually NAMED
        // `greet__1` would otherwise be handed `kt_greet__1` as well, and the linker would quietly
        // keep one of the two bodies.
        let mut ir = IrFile::default();
        for name in ["greet", "greet__1", "greet"] {
            ir.functions.push(top_level(name));
        }
        let symbols = function_symbols(&ir);
        assert_eq!(symbols, vec!["kt_greet", "kt_greet__1", "kt_greet__2"]);
        assert_eq!(
            symbols
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            3
        );
    }

    #[test]
    fn a_package_qualifies_the_symbol() {
        let mut ir = IrFile::default();
        ir.package = Some("com.example.app".to_string());
        ir.functions.push(top_level("main"));
        assert_eq!(function_symbols(&ir), vec!["kt_com_example_app_main"]);
    }

    #[test]
    fn two_classes_whose_names_sanitize_alike_get_distinct_symbols() {
        // A nested class is spelled `Outer$Nested` and a top-level class may be spelled
        // `Outer_Nested`; both sanitize to the same C identifier. One of them has to move, or the
        // two descriptors and the two constructors would be the same symbols.
        let mut ir = IrFile::default();
        ir.classes
            .push(crate::ir::IrClass::synthetic(crate::types::type_name(
                "Outer$Nested",
            )));
        ir.classes
            .push(crate::ir::IrClass::synthetic(crate::types::type_name(
                "Outer_Nested",
            )));
        let symbols = symbols(&ir);
        assert_eq!(symbols.classes, vec!["Outer_Nested", "Outer_Nested__1"]);
    }

    #[test]
    fn a_class_and_a_function_cannot_share_a_symbol() {
        // A class `X` owns `kt_type_X`; a top-level function `type_X` would be `kt_type_X` too.
        let mut ir = IrFile::default();
        ir.classes
            .push(crate::ir::IrClass::synthetic(crate::types::type_name("X")));
        ir.functions.push(top_level("type_X"));
        ir.functions.push(top_level("X__init"));
        let symbols = symbols(&ir);
        assert_eq!(symbols.classes, vec!["X"]);
        assert_eq!(symbols.functions, vec!["kt_type_X__1", "kt_X__init__1"]);
    }

    #[test]
    fn a_method_is_named_by_its_class_and_kept_apart_from_a_like_named_function() {
        let mut ir = IrFile::default();
        let mut class = crate::ir::IrClass::synthetic(crate::types::type_name("A"));
        ir.functions.push(crate::ir::IrFunction {
            name: "f".to_string(),
            params: Vec::new(),
            ret: Ty::Unit,
            body: None,
            is_static: false,
            dispatch_receiver: Some(crate::types::type_name("A")),
            param_checks: Vec::new(),
        });
        class.methods.push(0);
        ir.classes.push(class);
        ir.functions.push(top_level("A_f"));
        assert_eq!(function_symbols(&ir), vec!["kt_A_f", "kt_A_f__1"]);
    }

    #[test]
    fn the_minimum_int_has_a_c_spelling() {
        // `-2147483648` is not an `int` literal in C; it is a negation of an out-of-range positive
        // one, which is `long`. `INT32_C` is what makes the constant typed correctly.
        assert_eq!(
            constant_expression(&IrConst::Int(i32::MIN)).expect("emittable"),
            "(kt_int)INT32_C(-2147483648)"
        );
        assert_eq!(
            constant_expression(&IrConst::Long(i64::MIN)).expect("emittable"),
            "(kt_long)INT64_C(-9223372036854775808)"
        );
    }

    #[test]
    fn a_non_finite_constant_is_declined() {
        // `inf` and `nan` have no C literal spelling at all.
        assert!(constant_expression(&IrConst::Double(f64::NAN)).is_err());
        assert!(constant_expression(&IrConst::Float(f32::INFINITY)).is_err());
    }
}
