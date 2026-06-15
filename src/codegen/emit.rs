//! Phase 4: lower a typechecked file to a `FileKt`-style class.
//!
//! v0 covers: numeric arithmetic + widening, unary, comparisons, `&&`/`||` (short-circuit),
//! `if`/`while`, block bodies with `val`/`var` locals and `return`, free-function calls,
//! `toString()`, string concat (`StringBuilder`), `println`, `.length`. Branchy methods rely on the
//! v50 type-inference verifier (no StackMapTable yet — see classfile.rs).

use std::cell::RefCell;
use std::collections::HashMap;

use crate::ast::*;
use crate::codegen::classfile::*;
use crate::diag::DiagSink;
use crate::resolve::{import_map, resolve_java_static, SymbolTable, TypeInfo};
use crate::types::Ty;

thread_local! {
    /// Lambda-generated anonymous class bytes accumulated during a single emit_file / emit_class
    /// call. Drained by the caller after emission. (Thread-local avoids threading through the entire
    /// emit API.)
    static LAMBDA_CLASSES: RefCell<Vec<(String, Vec<u8>)>> = const { RefCell::new(Vec::new()) };
    /// Tracks which FunctionN stub arities have already been pushed in this compilation unit,
    /// so we don't emit duplicate stub classes.
    static FUNCTION_STUBS: RefCell<std::collections::HashSet<u8>> = RefCell::new(std::collections::HashSet::new());
}

fn push_lambda_class(name: String, bytes: Vec<u8>) {
    LAMBDA_CLASSES.with(|lc| lc.borrow_mut().push((name, bytes)));
}

fn drain_lambda_classes() -> Vec<(String, Vec<u8>)> {
    FUNCTION_STUBS.with(|s| s.borrow_mut().clear());
    LAMBDA_CLASSES.with(|lc| std::mem::take(&mut *lc.borrow_mut()))
}

/// Emit a stub `kotlin/jvm/functions/FunctionN` interface class (just the declaration, no body)
/// and register it for output. The stub makes lambda-using programs self-contained without
/// requiring kotlin-stdlib on the runtime classpath.
fn ensure_function_stub(arity: u8) {
    FUNCTION_STUBS.with(|s| {
        if s.borrow().contains(&arity) {
            return;
        }
        s.borrow_mut().insert(arity);
        let iface_name = Ty::fun_interface(arity);
        let mut cw = ClassWriter::new(&iface_name, "java/lang/Object");
        cw.set_access(ACC_PUBLIC | ACC_INTERFACE | ACC_ABSTRACT);
        // invoke([Object...])Object — the single abstract method all FunctionN interfaces expose.
        let mut desc = String::from("(");
        for _ in 0..arity {
            desc.push_str("Ljava/lang/Object;");
        }
        desc.push_str(")Ljava/lang/Object;");
        cw.add_abstract_method(ACC_PUBLIC | ACC_ABSTRACT, "invoke", &desc);
        let bytes = cw.finish();
        push_lambda_class(iface_name, bytes);
    });
}

/// Class name kotlinc derives for top-level decls: `<File>Kt` (capitalized).
pub fn file_class_name(file_stem: &str, package: Option<&str>) -> String {
    let mut base = String::new();
    let mut chars = file_stem.chars();
    if let Some(c) = chars.next() {
        base.extend(c.to_uppercase());
    }
    base.push_str(chars.as_str());
    base.push_str("Kt");
    match package {
        Some(p) if !p.is_empty() => format!("{}/{}", p.replace('.', "/"), base),
        _ => base,
    }
}

/// Does expression `e` reference a bare name (`Expr::Name`)? Used to keep enum entry arguments
/// to name-free constant expressions, which emit correctly regardless of the current class.
fn expr_contains_name(file: &File, e: ExprId) -> bool {
    match file.expr(e) {
        Expr::Name(_) => true,
        Expr::Unary { operand, .. } | Expr::NotNull { operand } | Expr::Throw { operand } => expr_contains_name(file, *operand),
        Expr::Is { operand, .. } | Expr::As { operand, .. } => expr_contains_name(file, *operand),
        Expr::Binary { lhs, rhs, .. } | Expr::Elvis { lhs, rhs } => expr_contains_name(file, *lhs) || expr_contains_name(file, *rhs),
        Expr::Member { receiver, .. } => expr_contains_name(file, *receiver),
        Expr::Index { array, index } => expr_contains_name(file, *array) || expr_contains_name(file, *index),
        Expr::Call { callee, args } => expr_contains_name(file, *callee) || args.iter().any(|&a| expr_contains_name(file, a)),
        Expr::SafeCall { receiver, args, .. } => expr_contains_name(file, *receiver) || args.as_ref().map_or(false, |a| a.iter().any(|&x| expr_contains_name(file, x))),
        Expr::Template(parts) => parts.iter().any(|p| matches!(p, TemplatePart::Expr(x) if expr_contains_name(file, *x))),
        Expr::IntLit(_) | Expr::LongLit(_) | Expr::DoubleLit(_) | Expr::FloatLit(_) | Expr::BoolLit(_) | Expr::StringLit(_) | Expr::CharLit(_) | Expr::NullLit => false,
        _ => true,
    }
}

pub fn method_descriptor(params: &[Ty], ret: Ty) -> String {
    let mut s = String::from("(");
    for p in params {
        s.push_str(&p.descriptor());
    }
    s.push(')');
    s.push_str(&ret.descriptor());
    s
}

pub fn emit_file(
    file: &File,
    info: &TypeInfo,
    syms: &SymbolTable,
    internal_name: &str,
    diags: &mut DiagSink,
) -> (Vec<u8>, Vec<(String, Vec<u8>)>) {
    let mut cw = ClassWriter::new(internal_name, "java/lang/Object");
    let imports = import_map(file);
    let mut lambda_ctr: u32 = 0;
    for &d in &file.decls {
        if let Decl::Fun(f) = file.decl(d) {
            let mut e = MethodEmitter::new(file, info, syms, internal_name, &imports, diags);
            e.lambda_counter = lambda_ctr;
            e.emit_fun(f, &mut cw);
            lambda_ctr = e.lambda_counter;
        }
    }

    // Top-level properties: a private static backing field + accessors, initialized in `<clinit>`.
    let mut tl_props: Vec<(&PropDecl, Ty)> = file
        .decls
        .iter()
        .filter_map(|&d| match file.decl(d) {
            Decl::Property(p) => Some((p, syms.props.get(&p.name).map(|(t, _)| *t).unwrap_or(Ty::Error))),
            _ => None,
        })
        .collect();
    // A property with no determinable value type (e.g. `val x = unitReturningCall()`) can't be a
    // JVM field — reject (the file is skipped) rather than emit invalid bytecode.
    tl_props.retain(|(p, ty)| {
        if matches!(ty, Ty::Unit | Ty::Error) {
            diags.error(p.span, format!("krusty: top-level property '{}' has unsupported type '{}'", p.name, ty.name()));
            false
        } else {
            true
        }
    });
    for (p, ty) in &tl_props {
        let access = if p.is_var { ACC_PRIVATE | ACC_STATIC } else { ACC_PRIVATE | ACC_STATIC | ACC_FINAL };
        cw.add_field(access, &p.name, &ty.descriptor());
        let cap = capitalize(&p.name);
        // getter
        let mut g = CodeBuilder::new(0);
        let f = cw.fieldref(internal_name, &p.name, &ty.descriptor());
        g.getstatic(f, slot_words(*ty) as i32);
        emit_typed_return(*ty, &mut g);
        g.link();
        cw.add_method(ACC_PUBLIC | ACC_STATIC | ACC_FINAL, &format!("get{cap}"), &method_descriptor(&[], *ty), &g);
        if p.is_var {
            let mut s = CodeBuilder::new(slot_words(*ty));
            load_local(*ty, 0, &mut s);
            let f = cw.fieldref(internal_name, &p.name, &ty.descriptor());
            s.putstatic(f, slot_words(*ty) as i32);
            s.ret_void();
            s.link();
            cw.add_method(ACC_PUBLIC | ACC_STATIC | ACC_FINAL, &format!("set{cap}"), &method_descriptor(&[*ty], Ty::Unit), &s);
        }
    }
    // `<clinit>` runs every property initializer into its static field.
    if !tl_props.is_empty() {
        let mut clinit = CodeBuilder::new(0);
        {
            let mut e = MethodEmitter::new(file, info, syms, internal_name, &imports, diags);
            e.lambda_counter = lambda_ctr;
            for (p, ty) in &tl_props {
                if let Some(init) = p.init {
                    e.emit_expr_as(init, *ty, &mut clinit, &mut cw);
                    let f = cw.fieldref(internal_name, &p.name, &ty.descriptor());
                    clinit.putstatic(f, slot_words(*ty) as i32);
                }
            }
            lambda_ctr = e.lambda_counter;
        }
        clinit.ret_void();
        clinit.link();
        cw.add_method(ACC_STATIC, "<clinit>", "()V", &clinit);
    }

    // @kotlin.Metadata: describe the file facade so Kotlin consumers see the Kotlin API.
    let funcs: Vec<crate::metadata::builder::FnMeta> = file
        .decls
        .iter()
        .filter_map(|&d| {
            let Decl::Fun(f) = file.decl(d) else { return None };
            let sig = syms.funs.get(&f.name)?;
            Some(crate::metadata::builder::FnMeta {
                name: f.name.clone(),
                params: f.params.iter().zip(&sig.params).map(|(p, t)| (p.name.clone(), *t)).collect(),
                ret: sig.ret,
            })
        })
        .collect();
    let prop_metas: Vec<crate::metadata::builder::PropMeta> = tl_props
        .iter()
        .map(|(p, ty)| {
            let cap = capitalize(&p.name);
            crate::metadata::builder::PropMeta {
                name: p.name.clone(),
                ty: *ty,
                is_var: p.is_var,
                getter: (format!("get{cap}"), method_descriptor(&[], *ty)),
                setter: if p.is_var { Some((format!("set{cap}"), method_descriptor(&[*ty], Ty::Unit))) } else { None },
            }
        })
        .collect();
    let (d1_bytes, d2) = crate::metadata::builder::build_package(&funcs, &prop_metas);
    let d1 = crate::metadata::encoding::bytes_to_strings(&d1_bytes);
    cw.set_kotlin_metadata(2, &[1, 9, 0], 48, &d1, &d2);
    let extra = drain_lambda_classes();
    (cw.finish(), extra)
}

/// Lower a (v0) `enum class Name { A, B }` to a class extending `java/lang/Enum`: one
/// `public static final` field per entry, a private `(String,int)` constructor calling
/// `Enum.<init>`, and a `<clinit>` that constructs each entry. (`values()`/`valueOf()`/`$VALUES`
/// are deferred — programs needing them are skipped, not miscompiled.)
fn emit_enum(class: &ClassDecl, file: &File, info: &TypeInfo, internal: &str, syms: &SymbolTable, diags: &mut DiagSink) -> Vec<u8> {
    let mut cw = ClassWriter::new(internal, "java/lang/Enum");
    let self_desc = Ty::obj(internal).descriptor();
    let imports = import_map(file);

    for entry in &class.enum_entries {
        cw.add_field(ACC_PUBLIC | ACC_STATIC | ACC_FINAL, entry, &self_desc);
    }

    // Primary-constructor properties (`enum class C(val rgb: Int)`) → backing fields + getters.
    let props: Vec<(String, Ty)> = class
        .props
        .iter()
        .filter(|p| p.is_property)
        .map(|p| (p.name.clone(), resolve_ty(&p.ty, syms)))
        .collect();
    let ctor_param_tys: Vec<Ty> = class.props.iter().map(|p| resolve_ty(&p.ty, syms)).collect();
    for (name, ty) in &props {
        cw.add_field(ACC_PRIVATE | ACC_FINAL, name, &ty.descriptor());
        let cap = capitalize(name);
        let mut g = CodeBuilder::new(1);
        g.aload(0);
        let f = cw.fieldref(internal, name, &ty.descriptor());
        g.getfield(f, slot_words(*ty) as i32);
        emit_typed_return(*ty, &mut g);
        g.link();
        cw.add_method(ACC_PUBLIC | ACC_FINAL, &format!("get{cap}"), &method_descriptor(&[], *ty), &g);
    }

    // <init>(String name, int ordinal, <ctor params>) { super(name, ordinal); store property fields }
    let ctor_desc = {
        let mut d = String::from("(Ljava/lang/String;I");
        for t in &ctor_param_tys {
            d.push_str(&t.descriptor());
        }
        d.push_str(")V");
        d
    };
    let mut ctor = CodeBuilder::new(3 + ctor_param_tys.iter().map(|t| slot_words(*t)).sum::<u16>());
    ctor.aload(0);
    ctor.aload(1);
    ctor.iload(2);
    let enum_init = cw.methodref("java/lang/Enum", "<init>", "(Ljava/lang/String;I)V");
    ctor.invokespecial(enum_init, 2, 0);
    // store each property param (slots start at 3) into its field
    let mut slot = 3u16;
    for (pp, ty) in class.props.iter().zip(&ctor_param_tys) {
        if pp.is_property {
            ctor.aload(0);
            load_local(*ty, slot, &mut ctor);
            let f = cw.fieldref(internal, &pp.name, &ty.descriptor());
            ctor.putfield(f, slot_words(*ty) as i32);
        }
        slot += slot_words(*ty);
    }
    ctor.ret_void();
    ctor.link();
    cw.add_method(ACC_PRIVATE, "<init>", &ctor_desc, &ctor);

    // <clinit>: each entry = new C("ENTRY", ordinal, <args>).
    let mut cl = CodeBuilder::new(0);
    let cidx = cw.class_ref(internal);
    let self_init = cw.methodref(internal, "<init>", &ctor_desc);
    for (i, entry) in class.enum_entries.iter().enumerate() {
        cl.new_obj(cidx);
        cl.dup();
        cl.push_string(entry, &mut cw);
        cl.push_int(i as i32, &mut cw);
        let args = class.enum_entry_args.get(i).cloned().unwrap_or_default();
        // Entry arguments are emitted with the enum as the current class, so an unqualified name
        // (e.g. a top-level `val`) would resolve to the wrong owner. Restrict to name-free
        // expressions (literals/arithmetic); reject anything referencing a name.
        if args.iter().any(|a| expr_contains_name(file, *a)) {
            diags.error(file.expr_spans[args[0].0 as usize], "krusty: enum entry arguments referencing a name are not supported".to_string());
        }
        let mut ie = MethodEmitter::new(file, info, syms, internal, &imports, diags);
        for (a, ty) in args.iter().zip(&ctor_param_tys) {
            ie.emit_expr_as(*a, *ty, &mut cl, &mut cw);
        }
        let aw: i32 = ctor_param_tys.iter().map(|t| slot_words(*t) as i32).sum();
        cl.invokespecial(self_init, 2 + aw, 0);
        let f = cw.fieldref(internal, entry, &self_desc);
        cl.putstatic(f, 1);
    }
    cl.ret_void();
    cl.link();
    cw.add_method(ACC_STATIC, "<clinit>", "()V", &cl);

    // Member methods (after the `;`).
    let class_props: HashMap<String, Ty> = props.iter().cloned().collect();
    let mut method_metas = Vec::new();
    for m in &class.methods {
        let params: Vec<Ty> = m.params.iter().map(|p| resolve_ty(&p.ty, syms)).collect();
        let ret = m.ret.as_ref().map(|r| resolve_ty(r, syms)).unwrap_or(Ty::Unit);
        let mut e = MethodEmitter::new_instance(file, info, syms, internal, &imports, class_props.clone(), diags);
        e.emit_method(m, &params, ret, ACC_PUBLIC | ACC_FINAL, &mut cw);
        method_metas.push(crate::metadata::class_builder::FnMeta::plain(
            m.name.clone(),
            m.params.iter().zip(&params).map(|(p, t)| (p.name.clone(), *t)).collect(),
            ret,
        ));
    }

    // @kotlin.Metadata (kind=1, enum flags) — entries are JVM static fields.
    let (d1_bytes, d2) = crate::metadata::class_builder::build_class(internal, &[], "()V", &[], &method_metas, &class.enum_entries, 32902);
    let d1 = crate::metadata::encoding::bytes_to_strings(&d1_bytes);
    cw.set_kotlin_metadata(1, &[1, 9, 0], 48, &d1, &d2);
    cw.finish()
}

/// Lower an `interface Name { fun sig(): T }` to a JVM interface: `public abstract` methods, no
/// bodies. Extended interfaces (supertypes) become super-interfaces.
fn emit_interface(class: &ClassDecl, internal: &str, syms: &SymbolTable) -> Vec<u8> {
    let mut cw = ClassWriter::new(internal, "java/lang/Object");
    cw.set_access(ACC_PUBLIC | ACC_INTERFACE | ACC_ABSTRACT);
    for st in &class.supertypes {
        let si = syms.classes.get(st).map(|c| c.internal.clone()).unwrap_or_else(|| st.clone());
        cw.add_interface(&si);
    }
    let mut method_metas = Vec::new();
    for m in &class.methods {
        let params: Vec<Ty> = m.params.iter().map(|p| resolve_ty(&p.ty, syms)).collect();
        let ret = m.ret.as_ref().map(|r| resolve_ty(r, syms)).unwrap_or(Ty::Unit);
        cw.add_abstract_method(ACC_PUBLIC, &m.name, &method_descriptor(&params, ret));
        method_metas.push(crate::metadata::class_builder::FnMeta::plain(
            m.name.clone(),
            m.params.iter().zip(&params).map(|(p, t)| (p.name.clone(), *t)).collect(),
            ret,
        ));
    }
    // Abstract properties → abstract `getX` (and `setX` for `var`); implementing classes provide them.
    let mut prop_metas = Vec::new();
    for p in &class.body_props {
        let ty = p.ty.as_ref().map(|r| resolve_ty(r, syms)).unwrap_or(Ty::Error);
        let cap = capitalize(&p.name);
        cw.add_abstract_method(ACC_PUBLIC, &format!("get{cap}"), &method_descriptor(&[], ty));
        if p.is_var {
            cw.add_abstract_method(ACC_PUBLIC, &format!("set{cap}"), &method_descriptor(&[ty], Ty::Unit));
        }
        prop_metas.push(crate::metadata::class_builder::PropMeta {
            name: p.name.clone(),
            ty,
            is_var: p.is_var,
            getter: (format!("get{cap}"), method_descriptor(&[], ty)),
            setter: if p.is_var { Some((format!("set{cap}"), method_descriptor(&[ty], Ty::Unit))) } else { None },
        });
    }
    // @kotlin.Metadata (kind=1, interface flags) — declares the abstract members for consumers.
    let (d1_bytes, d2) = crate::metadata::class_builder::build_class(internal, &[], "()V", &prop_metas, &method_metas, &[], 102);
    let d1 = crate::metadata::encoding::bytes_to_strings(&d1_bytes);
    cw.set_kotlin_metadata(1, &[1, 9, 0], 48, &d1, &d2);
    cw.finish()
}

/// Kotlin's `String.trimIndent()`: drop a blank first/last line, then strip the minimum common
/// leading-whitespace of the non-blank lines from every line.
fn trim_indent(s: &str) -> String {
    let mut lines: Vec<&str> = s.split('\n').collect();
    if lines.first().map_or(false, |l| l.trim().is_empty()) {
        lines.remove(0);
    }
    if lines.last().map_or(false, |l| l.trim().is_empty()) {
        lines.pop();
    }
    let min_indent = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    lines
        .iter()
        .map(|l| if l.trim().is_empty() { String::new() } else { l[min_indent..].to_string() })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Kotlin's `String.trimMargin(prefix)`: drop a blank first/last line, then for each line remove
/// leading whitespace up to and including the first `prefix` marker (default `|`).
fn trim_margin(s: &str, prefix: &str) -> String {
    let mut lines: Vec<&str> = s.split('\n').collect();
    if lines.first().map_or(false, |l| l.trim().is_empty()) {
        lines.remove(0);
    }
    if lines.last().map_or(false, |l| l.trim().is_empty()) {
        lines.pop();
    }
    lines
        .iter()
        .map(|l| {
            let t = l.trim_start();
            match t.strip_prefix(prefix) {
                Some(rest) => rest.to_string(),
                None => l.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// True for the array-creation builtins krusty recognizes.
fn is_array_builtin(name: &str) -> bool {
    matches!(
        name,
        "arrayOf" | "intArrayOf" | "longArrayOf" | "doubleArrayOf" | "booleanArrayOf" | "charArrayOf"
    ) || Ty::primitive_array_element(name).is_some()
}

/// `(opcode, value-words)` for an array element load (`Xaload`).
fn array_load_op(elem: Ty) -> (u8, i32) {
    match elem {
        Ty::Int => (0x2e, 1),     // iaload
        Ty::Long => (0x2f, 2),    // laload
        Ty::Double => (0x31, 2),  // daload
        Ty::Boolean => (0x33, 1), // baload
        Ty::Char => (0x34, 1),    // caload
        _ => (0x32, 1),           // aaload (reference)
    }
}

/// `(opcode, value-words)` for an array element store (`Xastore`).
fn array_store_op(elem: Ty) -> (u8, i32) {
    match elem {
        Ty::Int => (0x4f, 1),     // iastore
        Ty::Long => (0x50, 2),    // lastore
        Ty::Double => (0x52, 2),  // dastore
        Ty::Boolean => (0x54, 1), // bastore
        Ty::Char => (0x55, 1),    // castore
        _ => (0x53, 1),           // aastore (reference)
    }
}

/// The JVM internal name for a reference `Ty`, used as the `instanceof`/`checkcast` operand.
/// `String` is special-cased (its `obj_internal()` is `None`); the resolver guarantees the type is a
/// reference here, so any other shape erases to `Object`.
fn ref_internal(ty: Ty) -> &'static str {
    match ty {
        Ty::String => "java/lang/String",
        Ty::Obj(n) => n,
        // An array's `instanceof`/`checkcast` operand is its descriptor (`[LData;`, `[I`), not a name.
        Ty::Array(_) => crate::types::intern(&ty.descriptor()),
        Ty::Fun(n) => crate::types::intern(&Ty::fun_interface(n)),
        _ => "java/lang/Object",
    }
}

/// Resolve a syntactic type to a `Ty`, including declared class types (→ `Ty::Obj`). An unknown name
/// is an erased generic type parameter → `java/lang/Object` (valid code reaching codegen has already
/// passed the resolver, so the only unknowns here are type parameters).
fn resolve_ty(r: &TypeRef, syms: &SymbolTable) -> Ty {
    // Function type `(A) -> B` — parsed with fun_params non-empty.
    if !r.fun_params.is_empty() || r.name == "<fun>" {
        return Ty::Fun(r.fun_params.len() as u8);
    }
    if let Some(elem) = Ty::primitive_array_element(&r.name) {
        return Ty::array(elem);
    }
    if r.name == "Array" {
        if let Some(a) = &r.arg {
            return Ty::array(resolve_ty(a, syms));
        }
    }
    Ty::from_name(&r.name)
        .or_else(|| syms.classes.get(&r.name).map(|c| Ty::obj(&c.internal)))
        .unwrap_or_else(|| Ty::obj("java/lang/Object"))
}

/// Capitalize the first character (`x` -> `X`) for Kotlin's `getX`/`setX` accessor naming.
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Lower a `class C(val a: T, var b: U)` to a JVM class with private backing fields, a primary
/// constructor that calls `super()` and stores each field, and `getX`/`setX` accessors — matching
/// kotlinc's public ABI for a simple property-holding class.
pub fn emit_class(
    class: &ClassDecl,
    file: &File,
    info: &TypeInfo,
    internal_name: &str,
    file_facade: &str,
    syms: &SymbolTable,
    diags: &mut DiagSink,
) -> (Vec<u8>, Vec<(String, Vec<u8>)>) {
    if class.is_enum {
        let bytes = emit_enum(class, file, info, internal_name, syms, diags);
        let extra = drain_lambda_classes();
        return (bytes, extra);
    }
    if class.is_interface {
        let bytes = emit_interface(class, internal_name, syms);
        let extra = drain_lambda_classes();
        return (bytes, extra);
    }
    // Base class (`: Base(args)`) → JVM super; otherwise `java/lang/Object`.
    let base_internal = class
        .base_class
        .as_ref()
        .map(|b| {
            if b == "Any" {
                "java/lang/Object".to_string()
            } else {
                syms.classes.get(b).map(|c| c.internal.clone()).unwrap_or_else(|| b.clone())
            }
        });
    let super_internal = base_internal.clone().unwrap_or_else(|| "java/lang/Object".to_string());
    let mut cw = ClassWriter::new(internal_name, &super_internal);
    // `open`/`abstract` classes are not `final`; `abstract` adds ACC_ABSTRACT.
    if class.is_open || class.is_abstract {
        let mut access = ACC_PUBLIC | ACC_SUPER;
        if class.is_abstract {
            access |= ACC_ABSTRACT;
        }
        cw.set_access(access);
    }
    // Implemented interfaces (supertypes without constructor args).
    for st in &class.supertypes {
        if st == "Any" { continue; }
        let iface_internal = syms.classes.get(st).map(|c| c.internal.clone()).unwrap_or_else(|| st.clone());
        cw.add_interface(&iface_internal);
    }

    // Resolve constructor-parameter property types (primitives/String or declared class types).
    let props: Vec<(&PropParam, Ty)> = class
        .props
        .iter()
        .map(|p| (p, resolve_ty(&p.ty, syms)))
        .collect();
    // Body properties (`class C { val x = … }`) with resolved types (parallel to `class.body_props`,
    // so `init_order` `PropInit` indices stay valid). A *computed* property (custom getter, no
    // initializer) has no backing field — excluded from the field/accessor set below.
    let body_props_t: Vec<(&PropDecl, Ty)> = class
        .body_props
        .iter()
        .map(|bp| (bp, bp.ty.as_ref().map(|r| resolve_ty(r, syms)).unwrap_or_else(|| bp.init.map(|i| info.ty(i)).unwrap_or(Ty::Error))))
        .collect();
    // (name, type, is_var) for every backing field: `val`/`var` ctor params then body properties
    // (excluding computed properties, which have no field).
    let all_props: Vec<(String, Ty, bool)> = props
        .iter()
        .filter(|(p, _)| p.is_property)
        .map(|(p, t)| (p.name.clone(), *t, p.is_var))
        .chain(body_props_t.iter().filter(|(bp, _)| bp.getter.is_none()).map(|(bp, t)| (bp.name.clone(), *t, bp.is_var)))
        .collect();

    // Backing fields: `private final` for `val`, `private` for `var`.
    for (name, ty, is_var) in &all_props {
        let access = if *is_var { ACC_PRIVATE } else { ACC_PRIVATE | ACC_FINAL };
        cw.add_field(access, name, &ty.descriptor());
    }

    // Primary constructor: super(base args) then store each parameter into its backing field.
    let ctor_desc = method_descriptor(&props.iter().map(|(_, t)| *t).collect::<Vec<_>>(), Ty::Unit);
    let total_locals: u16 = 1 + props.iter().map(|(_, t)| slot_words(*t)).sum::<u16>();
    let class_props_map: HashMap<String, Ty> = all_props.iter().map(|(n, t, _)| (n.clone(), *t)).collect();
    let ctor_imports = import_map(file);
    let mut lambda_ctr: u32 = 0;
    let mut code = CodeBuilder::new(total_locals);
    {
        // A MethodEmitter gives `this` (slot 0) + the ctor params (= the properties) so base-class
        // constructor arguments (which reference those params) can be lowered.
        let mut ce = MethodEmitter::new_instance(file, info, syms, internal_name, &ctor_imports, class_props_map.clone(), diags);
        ce.lambda_counter = lambda_ctr;
        ce.file_facade = file_facade.to_string();
        let mut slot = 1u16;
        for (p, ty) in &props {
            ce.slots.insert(p.name.clone(), (slot, *ty));
            slot += slot_words(*ty);
        }
        ce.next_slot = slot;
        code.aload(0);
        match &base_internal {
            Some(base) => {
                let base_tys: Vec<Ty> = syms.class_by_internal(base).map(|c| c.props.iter().map(|(_, t, _)| *t).collect()).unwrap_or_default();
                for (arg, pty) in class.base_args.iter().zip(&base_tys) {
                    ce.emit_expr_as(*arg, *pty, &mut code, &mut cw);
                }
                let base_words: i32 = base_tys.iter().take(class.base_args.len()).map(|t| slot_words(*t) as i32).sum();
                let bdesc = method_descriptor(&base_tys[..class.base_args.len().min(base_tys.len())], Ty::Unit);
                let m = cw.methodref(base, "<init>", &bdesc);
                code.invokespecial(m, base_words, 0);
            }
            None => {
                let obj_init = cw.methodref("java/lang/Object", "<init>", "()V");
                code.invokespecial(obj_init, 0, 0);
            }
        }
        lambda_ctr = ce.lambda_counter;
    }
    let mut slot = 1u16;
    for (p, ty) in &props {
        if p.is_property {
            code.aload(0);
            load_local(*ty, slot, &mut code);
            let f = cw.fieldref(internal_name, &p.name, &ty.descriptor());
            code.putfield(f, slot_words(*ty) as i32);
        }
        slot += slot_words(*ty);
    }
    // Body-property initializers and `init { }` blocks, in source order (after ctor-param stores).
    if !class.init_order.is_empty() {
        let mut ie = MethodEmitter::new_instance(file, info, syms, internal_name, &ctor_imports, class_props_map.clone(), diags);
        ie.lambda_counter = lambda_ctr;
        ie.file_facade = file_facade.to_string();
        let mut s = 1u16;
        for (p, ty) in &props {
            ie.slots.insert(p.name.clone(), (s, *ty));
            s += slot_words(*ty);
        }
        ie.next_slot = s;
        for step in &class.init_order {
            match step {
                ClassInit::PropInit(idx) => {
                    let (bp, bty) = &body_props_t[*idx];
                    // A `lateinit var` has no initializer — leave the field at its default (null).
                    if let Some(init) = bp.init {
                        code.aload(0);
                        ie.emit_expr_as(init, *bty, &mut code, &mut cw);
                        let f = cw.fieldref(internal_name, &bp.name, &bty.descriptor());
                        code.putfield(f, slot_words(*bty) as i32);
                    }
                }
                ClassInit::Block(b) => ie.emit_block_discard(*b, &mut code, &mut cw),
            }
        }
        lambda_ctr = ie.lambda_counter;
    }
    code.ret_void();
    code.link();
    // An `object`'s constructor is private (the singleton owns the only instance).
    let ctor_access = if class.is_object { ACC_PRIVATE } else { ACC_PUBLIC };
    cw.add_method(ctor_access, "<init>", &ctor_desc, &code);

    // An `object` exposes a single `public static final INSTANCE`, built in `<clinit>`.
    if class.is_object {
        let self_desc = Ty::obj(internal_name).descriptor();
        cw.add_field(ACC_PUBLIC | ACC_STATIC | ACC_FINAL, "INSTANCE", &self_desc);
        let mut clinit = CodeBuilder::new(0);
        let cidx = cw.class_ref(internal_name);
        clinit.new_obj(cidx);
        clinit.dup();
        let init = cw.methodref(internal_name, "<init>", "()V");
        clinit.invokespecial(init, 0, 0);
        let inst = cw.fieldref(internal_name, "INSTANCE", &self_desc);
        clinit.putstatic(inst, 1);
        clinit.ret_void();
        clinit.link();
        cw.add_method(ACC_STATIC, "<clinit>", "()V", &clinit);
    }

    // Two properties whose accessor names collide (e.g. case-only-differing names, as `@JvmField`
    // would otherwise allow) can't both get a `getX` — reject rather than emit a duplicate method.
    {
        let mut seen = std::collections::HashSet::new();
        for (name, _, _) in &all_props {
            if !seen.insert(capitalize(name)) {
                diags.error(class.span, format!("krusty: property accessor name clash on '{name}'"));
                return (Vec::new(), drain_lambda_classes());
            }
        }
    }

    // Members of an `open`/`abstract` class must not be `final` (so subclasses can override).
    let member_access = if class.is_open || class.is_abstract { ACC_PUBLIC } else { ACC_PUBLIC | ACC_FINAL };

    // Pre-build the set of explicitly defined method names so we can skip auto-generated accessors
    // that would conflict (e.g. `private var r` paired with a hand-written `fun getR()`).
    let explicit_methods: std::collections::HashSet<String> = class.methods.iter().map(|m| m.name.clone()).collect();

    // Accessors: `getX()` for every property (ctor + body); `setX(..)` for `var`.
    for (name, ty, is_var) in &all_props {
        let cap = capitalize(name);
        let getter_name = format!("get{cap}");
        let setter_name = format!("set{cap}");
        // getter
        if !explicit_methods.contains(&getter_name) {
            let is_lateinit = syms.class_by_internal(internal_name)
                .map_or(false, |c| c.lateinit_props.contains(name.as_str()));
            let mut g = CodeBuilder::new(1);
            g.aload(0);
            let f = cw.fieldref(internal_name, name, &ty.descriptor());
            g.getfield(f, slot_words(*ty) as i32);
            if is_lateinit {
                // Stack: {value (maybe null)}. dup then test — at `ok` the stack is {value}.
                g.dup();
                let ok = g.new_label();
                use crate::codegen::classfile::VerifType;
                let class_cidx = cw.class_ref(internal_name);
                let vt = match ty {
                    Ty::String => VerifType::Object(cw.class_ref("java/lang/String")),
                    Ty::Obj(internal) => VerifType::Object(cw.class_ref(internal)),
                    Ty::Array(elem) => {
                        let desc = crate::types::intern(&format!("[{}", elem.descriptor()));
                        VerifType::Object(cw.class_ref(desc))
                    }
                    _ => VerifType::Object(cw.class_ref("java/lang/Object")),
                };
                g.add_frame_if_new(ok, vec![VerifType::Object(class_cidx)], vec![vt.clone()]);
                g.ifnonnull(ok);
                // null path — stack: {null}; throw RuntimeException
                let exc = cw.class_ref("java/lang/RuntimeException");
                g.new_obj(exc);
                g.dup();
                g.push_string(&format!("lateinit property {name} has not been initialized"), &mut cw);
                let init = cw.methodref("java/lang/RuntimeException", "<init>", "(Ljava/lang/String;)V");
                g.invokespecial(init, 1, 0);
                g.athrow();
                g.add_frame_if_new(ok, vec![VerifType::Object(class_cidx)], vec![vt]);
                g.bind(ok);
                // stack: {value (non-null)} — fall through to typed return
            }
            emit_typed_return(*ty, &mut g);
            g.link();
            cw.add_method(member_access, &getter_name, &method_descriptor(&[], *ty), &g);
        }
        // setter (var only)
        if *is_var && !explicit_methods.contains(&setter_name) {
            let mut s = CodeBuilder::new(1 + slot_words(*ty));
            s.aload(0);
            load_local(*ty, 1, &mut s);
            let f = cw.fieldref(internal_name, name, &ty.descriptor());
            s.putfield(f, slot_words(*ty) as i32);
            s.ret_void();
            s.link();
            cw.add_method(member_access, &setter_name, &method_descriptor(&[*ty], Ty::Unit), &s);
        }
    }

    // Member functions → instance methods. Own property names (ctor + body) resolve to backing-field
    // access; inherited members are reached via accessors on a typed receiver.
    let class_props: HashMap<String, Ty> = class_props_map.clone();
    let imports = import_map(file);
    let mut method_metas: Vec<crate::metadata::class_builder::FnMeta> = Vec::new();
    for m in &class.methods {
        let params: Vec<Ty> = m.params.iter().map(|p| resolve_ty(&p.ty, syms)).collect();
        // Prefer the symbol-table return type (infer_lit_ty applied) over re-deriving from AST.
        let ret = syms.class_by_internal(internal_name)
            .and_then(|c| c.methods.get(&m.name))
            .map(|s| s.ret)
            .unwrap_or_else(|| m.ret.as_ref().map(|r| resolve_ty(r, syms)).unwrap_or(Ty::Unit));
        let mut e = MethodEmitter::new_instance(file, info, syms, internal_name, &imports, class_props.clone(), diags);
        e.lambda_counter = lambda_ctr;
        e.file_facade = file_facade.to_string();
        e.props_via_getter = class.is_open || class.is_abstract;
        e.emit_method(m, &params, ret, member_access, &mut cw);
        lambda_ctr = e.lambda_counter;
        method_metas.push(crate::metadata::class_builder::FnMeta::plain(
            m.name.clone(),
            m.params.iter().zip(&params).map(|(p, t)| (p.name.clone(), *t)).collect(),
            ret,
        ));
    }

    // Computed properties (`val x: T get() = …`) → a `getX()` method running the getter body.
    for bp in &class.body_props {
        let Some(getter) = &bp.getter else { continue };
        // Use the property's resolved type from the symbol table (which holds the *inferred* type for
        // an unannotated computed getter), so `getX`'s descriptor matches what callers expect.
        let ty = syms
            .class_by_internal(internal_name)
            .and_then(|c| c.prop(&bp.name))
            .map(|(t, _)| t)
            .or_else(|| bp.ty.as_ref().map(|r| resolve_ty(r, syms)))
            .unwrap_or(Ty::Error);
        let getter_fn = FunDecl {
            name: format!("get{}", capitalize(&bp.name)),
            params: Vec::new(),
            ret: bp.ty.clone(),
            body: getter.clone(),
            type_params: Vec::new(),
            span: bp.span,
            is_inline: false,
        };
        let mut e = MethodEmitter::new_instance(file, info, syms, internal_name, &imports, class_props.clone(), diags);
        e.lambda_counter = lambda_ctr;
        e.file_facade = file_facade.to_string();
        e.props_via_getter = class.is_open || class.is_abstract;
        e.emit_method(&getter_fn, &[], ty, member_access, &mut cw);
        lambda_ctr = e.lambda_counter;
    }

    // `companion object` members → `static`/`static final` members on this class.
    for m in &class.companion_methods {
        let params: Vec<Ty> = m.params.iter().map(|p| resolve_ty(&p.ty, syms)).collect();
        // Prefer the symbol-table signature (which has infer_lit_ty applied) over re-deriving
        // from AST, so that expression-body methods with no explicit return type get the right
        // JVM descriptor.
        let ret = syms.classes.get(&class.name)
            .and_then(|c| c.static_methods.get(&m.name))
            .map(|s| s.ret)
            .unwrap_or_else(|| m.ret.as_ref().map(|r| resolve_ty(r, syms)).unwrap_or(Ty::Unit));
        let mut e = MethodEmitter::new(file, info, syms, internal_name, &imports, diags);
        e.lambda_counter = lambda_ctr;
        e.file_facade = file_facade.to_string();
        e.companion_of = Some(class.name.clone());
        e.emit_method(m, &params, ret, ACC_PUBLIC | ACC_STATIC | ACC_FINAL, &mut cw);
        lambda_ctr = e.lambda_counter;
    }
    if !class.companion_props.is_empty() {
        // static final fields, initialized in <clinit>.
        let mut clinit = CodeBuilder::new(0);
        for p in &class.companion_props {
            let ty = p.ty.as_ref().map(|r| resolve_ty(r, syms)).unwrap_or_else(|| p.init.map(|i| info.ty(i)).unwrap_or(Ty::Error));
            cw.add_field(ACC_PUBLIC | ACC_STATIC | ACC_FINAL, &p.name, &ty.descriptor());
            if let Some(init) = p.init {
                let mut e = MethodEmitter::new(file, info, syms, internal_name, &imports, diags);
                e.lambda_counter = lambda_ctr;
                e.file_facade = file_facade.to_string();
                e.companion_of = Some(class.name.clone());
                e.emit_expr_as(init, ty, &mut clinit, &mut cw);
                lambda_ctr = e.lambda_counter;
                let f = cw.fieldref(internal_name, &p.name, &ty.descriptor());
                clinit.putstatic(f, slot_words(ty) as i32);
            }
        }
        clinit.ret_void();
        clinit.link();
        cw.add_method(ACC_STATIC, "<clinit>", "()V", &clinit);
    }

    // `data class`: synthesize equals/hashCode/toString/componentN/copy(+copy$default) and expose
    // componentN/copy in the metadata so Kotlin consumers can call them.
    let mut method_metas = method_metas;
    if class.is_data {
        let user_methods: std::collections::HashSet<&str> = class.methods.iter().map(|m| m.name.as_str()).collect();
        method_metas.extend(emit_data_members(&mut cw, internal_name, &props, &user_methods));
    }

    // @kotlin.Metadata (kind=1: class) so a Kotlin consumer sees this as a Kotlin class.
    let ctor_params: Vec<(String, Ty)> = props.iter().map(|(p, t)| (p.name.clone(), *t)).collect();
    let ctor_desc = method_descriptor(&props.iter().map(|(_, t)| *t).collect::<Vec<_>>(), Ty::Unit);
    let prop_metas: Vec<crate::metadata::class_builder::PropMeta> = all_props
        .iter()
        .map(|(name, t, is_var)| {
            let cap = capitalize(name);
            crate::metadata::class_builder::PropMeta {
                name: name.clone(),
                ty: *t,
                is_var: *is_var,
                getter: (format!("get{cap}"), method_descriptor(&[], *t)),
                setter: if *is_var { Some((format!("set{cap}"), method_descriptor(&[*t], Ty::Unit))) } else { None },
            }
        })
        .collect();
    let class_flags = if class.is_data { 1030 } else if class.is_object { 326 } else { 0 };
    let (d1_bytes, d2) = crate::metadata::class_builder::build_class(internal_name, &ctor_params, &ctor_desc, &prop_metas, &method_metas, &[], class_flags);
    let d1 = crate::metadata::encoding::bytes_to_strings(&d1_bytes);
    cw.set_kotlin_metadata(1, &[1, 9, 0], 48, &d1, &d2);

    let extra = drain_lambda_classes();
    (cw.finish(), extra)
}

/// StringBuilder.append descriptor + stack words for a value of type `t`.
fn sb_append(t: Ty) -> (&'static str, i32) {
    match t {
        Ty::Int | Ty::Byte | Ty::Short => ("(I)Ljava/lang/StringBuilder;", 1),
        Ty::Char => ("(C)Ljava/lang/StringBuilder;", 1),
        Ty::Boolean => ("(Z)Ljava/lang/StringBuilder;", 1),
        Ty::Long => ("(J)Ljava/lang/StringBuilder;", 2),
        Ty::Float => ("(F)Ljava/lang/StringBuilder;", 1),
        Ty::Double => ("(D)Ljava/lang/StringBuilder;", 2),
        Ty::String => ("(Ljava/lang/String;)Ljava/lang/StringBuilder;", 1),
        _ => ("(Ljava/lang/Object;)Ljava/lang/StringBuilder;", 1),
    }
}

/// Synthesize the `data class` members: `componentN`, `copy`, `copy$default`, `toString`,
/// `hashCode`, `equals` — matching kotlinc's public ABI and behavior. Returns metadata entries for
/// `componentN`/`copy` (so Kotlin consumers can call them).
fn emit_data_members(
    cw: &mut ClassWriter,
    internal: &str,
    props: &[(&PropParam, Ty)],
    user_methods: &std::collections::HashSet<&str>,
) -> Vec<crate::metadata::class_builder::FnMeta> {
    let simple = internal.rsplit('/').next().unwrap_or(internal).to_string();
    let prop_tys: Vec<Ty> = props.iter().map(|(_, t)| *t).collect();
    let total_words: u16 = prop_tys.iter().map(|t| slot_words(*t)).sum();
    let self_ty = Ty::obj(internal);
    let mut metas = Vec::new();

    // componentN() — returns each property.
    for (i, (p, ty)) in props.iter().enumerate() {
        if user_methods.contains(format!("component{}", i + 1).as_str()) {
            continue; // user declared it explicitly
        }
        let mut c = CodeBuilder::new(1);
        c.aload(0);
        let f = cw.fieldref(internal, &p.name, &ty.descriptor());
        c.getfield(f, slot_words(*ty) as i32);
        emit_typed_return(*ty, &mut c);
        c.link();
        let name = format!("component{}", i + 1);
        cw.add_method(ACC_PUBLIC | ACC_FINAL, &name, &method_descriptor(&[], *ty), &c);
        metas.push(crate::metadata::class_builder::FnMeta {
            name,
            params: vec![],
            ret: *ty,
            flags: crate::metadata::class_builder::COMPONENT_FN_FLAGS, // operator
            params_have_defaults: false,
        });
    }

    // copy(props...) -> Self.
    if !user_methods.contains("copy") {
        let mut c = CodeBuilder::new(1 + total_words);
        let cidx = cw.class_ref(internal);
        c.new_obj(cidx);
        c.dup();
        let mut slot = 1u16;
        for (_, ty) in props {
            load_local(*ty, slot, &mut c);
            slot += slot_words(*ty);
        }
        let init = cw.methodref(internal, "<init>", &method_descriptor(&prop_tys, Ty::Unit));
        c.invokespecial(init, total_words as i32, 0);
        c.areturn();
        c.link();
        cw.add_method(ACC_PUBLIC | ACC_FINAL, "copy", &method_descriptor(&prop_tys, self_ty), &c);
        let params = props.iter().map(|(p, t)| (p.name.clone(), *t)).collect();
        metas.push(crate::metadata::class_builder::FnMeta {
            name: "copy".into(),
            params,
            ret: self_ty,
            flags: crate::metadata::class_builder::COPY_FN_FLAGS,
            params_have_defaults: true, // each copy param defaults to the current value
        });
    }

    // copy$default(self, props..., mask:int, marker:Object) -> Self — synthetic default-applier.
    if !user_methods.contains("copy") {
        use crate::codegen::classfile::VerifType as VT;
        let mask_slot = 1 + total_words;
        let total_locals = mask_slot + 2; // mask + marker
        let mut c = CodeBuilder::new(total_locals);
        // Build the constant StackMapTable locals for all `skip` branch targets:
        // [this, props..., mask(Int), marker(Obj)].
        let mut skip_locals: Vec<VT> = vec![VT::Object(cw.class_ref(internal))];
        for (_, ty) in props.iter() {
            let vt = match ty {
                Ty::Int | Ty::Boolean | Ty::Byte | Ty::Short | Ty::Char => VT::Integer,
                Ty::Long => VT::Long,
                Ty::Float => VT::Float,
                Ty::Double => VT::Double,
                Ty::String => VT::Object(cw.class_ref("java/lang/String")),
                Ty::Obj(s) => VT::Object(cw.class_ref(s)),
                Ty::Array(e) => { let d = format!("[{}", e.descriptor()); VT::Object(cw.class_ref(&d)) }
                _ => VT::Top,
            };
            skip_locals.push(vt);
            if matches!(ty, Ty::Long | Ty::Double) { skip_locals.push(VT::Top); }
        }
        skip_locals.push(VT::Integer);                                   // mask
        skip_locals.push(VT::Object(cw.class_ref("java/lang/Object"))); // marker
        let mut slot = 1u16;
        for (i, (p, ty)) in props.iter().enumerate() {
            c.iload(mask_slot);
            c.push_int(1 << i, cw);
            c.iand();
            let skip = c.new_label();
            c.add_frame_if_new(skip, skip_locals.clone(), vec![]); // frame at branch target
            c.ifeq(skip);
            c.aload(0);
            let f = cw.fieldref(internal, &p.name, &ty.descriptor());
            c.getfield(f, slot_words(*ty) as i32);
            store_local(*ty, slot, &mut c);
            c.bind(skip);
            slot += slot_words(*ty);
        }
        c.aload(0);
        let mut slot = 1u16;
        for (_, ty) in props {
            load_local(*ty, slot, &mut c);
            slot += slot_words(*ty);
        }
        let copy = cw.methodref(internal, "copy", &method_descriptor(&prop_tys, self_ty));
        c.invokevirtual(copy, total_words as i32, 1);
        c.areturn();
        c.link();
        let mut desc = String::from("(");
        desc.push_str(&self_ty.descriptor());
        for t in &prop_tys {
            desc.push_str(&t.descriptor());
        }
        desc.push_str("ILjava/lang/Object;)");
        desc.push_str(&self_ty.descriptor());
        cw.add_method(ACC_PUBLIC | ACC_STATIC, "copy$default", &desc, &c);
    }

    // toString() -> "Name(p1=v1, p2=v2)".
    if !user_methods.contains("toString") {
        let mut c = CodeBuilder::new(1);
        let sb = cw.class_ref("java/lang/StringBuilder");
        let sbinit = cw.methodref("java/lang/StringBuilder", "<init>", "()V");
        c.new_obj(sb);
        c.dup();
        c.invokespecial(sbinit, 0, 0);
        let app_str = cw.methodref("java/lang/StringBuilder", "append", "(Ljava/lang/String;)Ljava/lang/StringBuilder;");
        for (i, (p, ty)) in props.iter().enumerate() {
            let lead = if i == 0 { format!("{simple}({}=", p.name) } else { format!(", {}=", p.name) };
            c.push_string(&lead, cw);
            c.invokevirtual(app_str, 1, 1);
            c.aload(0);
            let f = cw.fieldref(internal, &p.name, &ty.descriptor());
            c.getfield(f, slot_words(*ty) as i32);
            let (adesc, awords) = sb_append(*ty);
            let appv = cw.methodref("java/lang/StringBuilder", "append", adesc);
            c.invokevirtual(appv, awords, 1);
        }
        c.push_int(41, cw); // ')'
        let app_char = cw.methodref("java/lang/StringBuilder", "append", "(C)Ljava/lang/StringBuilder;");
        c.invokevirtual(app_char, 1, 1);
        let tos = cw.methodref("java/lang/StringBuilder", "toString", "()Ljava/lang/String;");
        c.invokevirtual(tos, 0, 1);
        c.areturn();
        c.link();
        cw.add_method(ACC_PUBLIC, "toString", "()Ljava/lang/String;", &c);
    }

    // hashCode(): result = hash(p0); result = result*31 + hash(pN); ...
    if !user_methods.contains("hashCode") {
        let mut c = CodeBuilder::new(1);
        for (i, (p, ty)) in props.iter().enumerate() {
            if i > 0 {
                c.iload(1);
                c.push_int(31, cw);
                c.imul();
            }
            emit_hash_of(cw, &mut c, internal, &p.name, *ty);
            if i > 0 {
                c.iadd();
            }
            c.istore(1);
        }
        c.iload(1);
        c.ireturn();
        c.link();
        cw.add_method(ACC_PUBLIC, "hashCode", "()I", &c);
    }

    // equals(Object): identity, instanceof, then per-property comparison.
    if !user_methods.contains("equals") {
        use crate::codegen::classfile::VerifType as VT;
        let mut c = CodeBuilder::new(3); // this=0, other=1; cast_other=2
        let cidx = cw.class_ref(internal);
        // Frame used at `ne` and `is_inst` targets: [this, other_as_Object].
        let frm2 = vec![VT::Object(cidx), VT::Object(cw.class_ref("java/lang/Object"))];
        // Frame used at each `eq` target: [this, other_as_Object, cast_other_as_self].
        let frm3 = vec![VT::Object(cidx), VT::Object(cw.class_ref("java/lang/Object")), VT::Object(cidx)];
        c.aload(0);
        c.aload(1);
        let ne = c.new_label();
        c.add_frame_if_new(ne, frm2.clone(), vec![]);
        c.if_acmpne(ne);
        c.push_int(1, cw);
        c.ireturn();
        c.bind(ne);
        c.aload(1);
        c.instance_of(cidx);
        let is_inst = c.new_label();
        c.add_frame_if_new(is_inst, frm2.clone(), vec![]);
        c.ifne(is_inst);
        c.push_int(0, cw);
        c.ireturn();
        c.bind(is_inst);
        c.aload(1);
        c.checkcast(cidx);
        c.astore(2);
        for (p, ty) in props {
            let f = cw.fieldref(internal, &p.name, &ty.descriptor());
            c.aload(0);
            c.getfield(f, slot_words(*ty) as i32);
            c.aload(2);
            c.getfield(f, slot_words(*ty) as i32);
            let eq = c.new_label();
            c.add_frame_if_new(eq, frm3.clone(), vec![]);
            match ty {
                Ty::Int | Ty::Byte | Ty::Short | Ty::Boolean | Ty::Char => c.if_icmpeq(eq),
                Ty::Long => {
                    c.lcmp();
                    c.ifeq(eq);
                }
                Ty::Double => {
                    c.dcmpg();
                    c.ifeq(eq);
                }
                Ty::Float => {
                    c.fcmpg();
                    c.ifeq(eq);
                }
                _ => {
                    // reference: null-safe `Objects.equals(this.field, other.field)` (a field may be
                    // null — `data class A(val v: Any?)` — so a plain `.equals` would NPE).
                    let eqm = cw.methodref("java/util/Objects", "equals", "(Ljava/lang/Object;Ljava/lang/Object;)Z");
                    c.invokestatic(eqm, 2, 1);
                    c.ifne(eq);
                }
            }
            c.push_int(0, cw);
            c.ireturn();
            c.bind(eq);
        }
        c.push_int(1, cw);
        c.ireturn();
        c.link();
        cw.add_method(ACC_PUBLIC, "equals", "(Ljava/lang/Object;)Z", &c);
    }

    metas
}

/// Emit `hash(this.<field>)` onto the stack as an int, matching kotlinc's per-type hashing.
fn emit_hash_of(cw: &mut ClassWriter, c: &mut CodeBuilder, internal: &str, field: &str, ty: Ty) {
    c.aload(0);
    let f = cw.fieldref(internal, field, &ty.descriptor());
    c.getfield(f, slot_words(ty) as i32);
    match ty {
        // Byte/Short/Char are int on the stack; their hashCode is that int value.
        Ty::Int | Ty::Byte | Ty::Short | Ty::Char => {
            let m = cw.methodref("java/lang/Integer", "hashCode", "(I)I");
            c.invokestatic(m, 1, 1);
        }
        Ty::Boolean => {
            let m = cw.methodref("java/lang/Boolean", "hashCode", "(Z)I");
            c.invokestatic(m, 1, 1);
        }
        Ty::Long => {
            let m = cw.methodref("java/lang/Long", "hashCode", "(J)I");
            c.invokestatic(m, 2, 1);
        }
        Ty::Double => {
            let m = cw.methodref("java/lang/Double", "hashCode", "(D)I");
            c.invokestatic(m, 2, 1);
        }
        Ty::Float => {
            let m = cw.methodref("java/lang/Float", "hashCode", "(F)I");
            c.invokestatic(m, 1, 1);
        }
        _ => {
            let m = cw.methodref("java/lang/Object", "hashCode", "()I");
            c.invokevirtual(m, 0, 1);
        }
    }
}

fn store_local(ty: Ty, slot: u16, code: &mut CodeBuilder) {
    match ty {
        Ty::Int | Ty::Byte | Ty::Short | Ty::Boolean | Ty::Char => code.istore(slot),
        Ty::Long => code.lstore(slot),
        Ty::Float => code.fstore(slot),
        Ty::Double => code.dstore(slot),
        _ => code.astore(slot),
    }
}

fn load_local(ty: Ty, slot: u16, code: &mut CodeBuilder) {
    match ty {
        Ty::Int | Ty::Byte | Ty::Short | Ty::Boolean | Ty::Char => code.iload(slot),
        Ty::Long => code.lload(slot),
        Ty::Float => code.fload(slot),
        Ty::Double => code.dload(slot),
        _ => code.aload(slot),
    }
}

fn emit_typed_return(ty: Ty, code: &mut CodeBuilder) {
    match ty {
        Ty::Int | Ty::Byte | Ty::Short | Ty::Boolean | Ty::Char => code.ireturn(),
        Ty::Long => code.lreturn(),
        Ty::Float => code.freturn(),
        Ty::Double => code.dreturn(),
        _ => code.areturn(),
    }
}

struct MethodEmitter<'a> {
    file: &'a File,
    info: &'a TypeInfo,
    syms: &'a SymbolTable,
    class: String,
    diags: &'a mut DiagSink,
    slots: HashMap<String, (u16, Ty)>,
    next_slot: u16,
    ret_ty: Ty,
    imports: &'a HashMap<String, String>,
    /// Class properties visible to an instance method (implicit `this`); empty for static funcs.
    class_props: HashMap<String, Ty>,
    /// True when emitting an instance method (slot 0 = `this`; bare property names → getfield).
    is_instance: bool,
    /// In an `open`/`abstract` class, `this.<prop>` must dispatch through the (virtual) accessor so
    /// overrides are honored, instead of reading the backing field directly.
    props_via_getter: bool,
    /// When emitting a `companion object` member, the enclosing class — its static members are then
    /// reachable unqualified (`MAX` → `getstatic`, `create(…)` → `invokestatic`).
    companion_of: Option<String>,
    /// Enclosing loops' `(continue_target, break_target)` labels for `continue`/`break`.
    loop_labels: Vec<(crate::codegen::classfile::Label, crate::codegen::classfile::Label)>,
    /// Implicit receiver for an inlined `run`/`with`/`apply` body: `(slot, class internal)`. When set,
    /// `this` and unqualified member access target this slot/class instead of `this` (slot 0).
    recv: Option<(u16, String)>,
    /// Counter for generating unique anonymous class names for lambda literals.
    lambda_counter: u32,
    /// Maps mangled local-function name → the class it was emitted on (for correct invokestatic).
    local_fun_emitted_class: HashMap<String, String>,
    /// True when this emitter is inside a lambda body.
    inside_lambda: bool,
    /// Internal name of the file-facade class (e.g. `FooKt`) that owns top-level functions.
    /// Equals `class` for top-level emitters; differs when emitting inside a class declaration.
    file_facade: String,
}

impl<'a> MethodEmitter<'a> {
    fn new(file: &'a File, info: &'a TypeInfo, syms: &'a SymbolTable, class: &str, imports: &'a HashMap<String, String>, diags: &'a mut DiagSink) -> Self {
        MethodEmitter {
            file, info, syms, class: class.to_string(), diags,
            slots: HashMap::new(), next_slot: 0, ret_ty: Ty::Unit, imports,
            class_props: HashMap::new(), is_instance: false, props_via_getter: false, companion_of: None,
            loop_labels: Vec::new(), recv: None,
            lambda_counter: 0,
            local_fun_emitted_class: HashMap::new(),
            inside_lambda: false,
            file_facade: class.to_string(),
        }
    }

    /// Load the current implicit receiver (`run`/`with`/`apply` slot, else `this` in slot 0).
    fn emit_implicit_this(&self, code: &mut CodeBuilder) {
        match &self.recv {
            Some((slot, _)) => code.aload(*slot),
            None => code.aload(0),
        }
    }
    /// The internal class name of the current implicit receiver.
    fn implicit_class(&self) -> String {
        match &self.recv {
            Some((_, internal)) => internal.clone(),
            None => self.class.clone(),
        }
    }

    fn new_instance(file: &'a File, info: &'a TypeInfo, syms: &'a SymbolTable, class: &str, imports: &'a HashMap<String, String>, class_props: HashMap<String, Ty>, diags: &'a mut DiagSink) -> Self {
        let mut e = MethodEmitter::new(file, info, syms, class, imports, diags);
        e.class_props = class_props;
        e.is_instance = true;
        e.next_slot = 1; // slot 0 reserved for `this`
        e
    }

    /// Allocate an anonymous local slot of `ty` and register it in `self.slots` under a synthetic
    /// name so `make_verif_locals` picks it up in StackMapTable frames.
    fn alloc_temp(&mut self, ty: Ty) -> u16 {
        let slot = self.next_slot;
        let name = format!("$$tmp_{slot}");
        self.next_slot += slot_words(ty);
        self.slots.insert(name, (slot, ty));
        slot
    }

    /// Store a default (zero/null) value to `slot` so the verifier sees it as initialized.
    /// Must be called right after `alloc_temp` for slots that precede any `rec(...)` call.
    fn init_temp(&mut self, ty: Ty, slot: u16, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        match ty {
            Ty::Int | Ty::Boolean | Ty::Byte | Ty::Short | Ty::Char => {
                code.push_int(0, cw);
                code.istore(slot);
            }
            Ty::Long => {
                code.push_long(0, cw);
                code.lstore(slot);
            }
            Ty::Float => {
                code.push_float(0.0, cw);
                code.fstore(slot);
            }
            Ty::Double => {
                code.push_double(0.0, cw);
                code.dstore(slot);
            }
            _ => {
                code.aconst_null();
                code.astore(slot);
            }
        }
    }

    /// Build the verification-type locals list for the current `self.slots` state.
    /// Used to record StackMapTable frames. Slot 0 for instance methods = `this`.
    fn make_verif_locals(&self, cw: &mut ClassWriter) -> Vec<crate::codegen::classfile::VerifType> {
        use crate::codegen::classfile::VerifType;
        let max = self.next_slot as usize;
        if max == 0 { return Vec::new(); }

        let mut raw: Vec<VerifType> = vec![VerifType::Top; max];

        if self.is_instance {
            raw[0] = VerifType::Object(cw.class_ref(&self.class));
        }

        for &(slot, ty) in self.slots.values() {
            let vt = match ty {
                Ty::Int | Ty::Boolean | Ty::Byte | Ty::Short | Ty::Char => VerifType::Integer,
                Ty::Long => VerifType::Long,
                Ty::Float => VerifType::Float,
                Ty::Double => VerifType::Double,
                Ty::Null => VerifType::Null,
                Ty::String => VerifType::Object(cw.class_ref("java/lang/String")),
                Ty::Obj(ref internal) => VerifType::Object(cw.class_ref(internal)),
                Ty::Array(ref elem) => {
                    let desc = format!("[{}", elem.descriptor());
                    VerifType::Object(cw.class_ref(&desc))
                }
                Ty::Fun(n) => {
                    let iname = format!("kotlin/jvm/functions/Function{}", n);
                    VerifType::Object(cw.class_ref(&iname))
                }
                Ty::Unit | Ty::Nothing | Ty::Error => VerifType::Top,
            };
            if (slot as usize) < raw.len() {
                raw[slot as usize] = vt;
            }
        }

        // Build result: Long/Double take 2 slots but appear once; skip the second slot.
        let mut result = Vec::new();
        let mut i = 0;
        while i < raw.len() {
            let is_wide = matches!(raw[i], VerifType::Long | VerifType::Double);
            result.push(raw[i].clone());
            i += if is_wide { 2 } else { 1 };
        }
        while result.last() == Some(&VerifType::Top) { result.pop(); }
        result
    }

    /// Record a StackMapTable frame for `label` with empty operand stack (first call wins).
    /// Always registers; StackMapTable is emitted for any method that has branch targets.
    fn rec(&self, label: crate::codegen::classfile::Label, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        let locals = self.make_verif_locals(cw);
        code.add_frame_if_new(label, locals, vec![]);
    }

    /// Record a StackMapTable frame for `label` with a single item on the operand stack.
    /// Used for exception-handler entry points (JVM places the caught exception on the stack).
    fn rec_s(&self, label: crate::codegen::classfile::Label, stack_ty: Ty, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        use crate::codegen::classfile::VerifType;
        let locals = self.make_verif_locals(cw);
        let stack_item = match stack_ty {
            Ty::Int | Ty::Boolean | Ty::Byte | Ty::Short | Ty::Char => VerifType::Integer,
            Ty::Long => VerifType::Long,
            Ty::Float => VerifType::Float,
            Ty::Double => VerifType::Double,
            Ty::Null => VerifType::Null,
            Ty::String => VerifType::Object(cw.class_ref("java/lang/String")),
            Ty::Obj(internal) => VerifType::Object(cw.class_ref(internal)),
            Ty::Array(ref elem) => {
                let desc = crate::types::intern(&format!("[{}", elem.descriptor()));
                VerifType::Object(cw.class_ref(desc))
            }
            Ty::Fun(n) => {
                let iname = crate::types::intern(&Ty::fun_interface(n));
                VerifType::Object(cw.class_ref(iname))
            }
            _ => VerifType::Top,
        };
        code.add_frame_if_new(label, locals, vec![stack_item]);
    }

    /// Emit an instance method: `this` in slot 0, params from slot 1, `public final` (non-static).
    fn emit_method(&mut self, f: &FunDecl, params: &[Ty], ret: Ty, access: u16, cw: &mut ClassWriter) {
        self.ret_ty = ret;
        for (p, ty) in f.params.iter().zip(params) {
            self.alloc_slot(&p.name, *ty);
        }
        let mut code = CodeBuilder::new(self.next_slot);
        match &f.body {
            FunBody::Expr(e) => {
                self.emit_expr_as(*e, ret, &mut code, cw);
                self.emit_return(ret, &mut code);
            }
            FunBody::Block(b) => self.emit_block_as_body(*b, &mut code, cw),
            FunBody::None => self.emit_default_return(ret, &mut code, cw),
        }
        code.ensure_locals(self.next_slot);
        code.link();
        cw.add_method(access, &f.name, &method_descriptor(params, ret), &code);
    }

    fn alloc_slot(&mut self, name: &str, ty: Ty) -> u16 {
        let slot = self.next_slot;
        self.next_slot += slot_words(ty);
        self.slots.insert(name.to_string(), (slot, ty));
        slot
    }

    /// Allocate an unnamed temporary local slot (e.g. a `when` subject).
    fn fresh_slot(&mut self, ty: Ty) -> u16 {
        let slot = self.next_slot;
        self.next_slot += slot_words(ty);
        slot
    }

    /// Allocate an array of `elem` whose length is already on the stack (`newarray` for a primitive
    /// element, `anewarray` for a reference element).
    /// Emit `e` and leave its `String` form on the stack (identity for a `String`, `String.valueOf`
    /// for a primitive, `Object.toString`/`String.valueOf(Object)` for a reference).
    fn emit_string_of(&mut self, e: ExprId, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        let t = self.info.ty(e);
        self.emit_expr(e, code, cw);
        let (desc, words) = match t {
            Ty::String => return,
            Ty::Int | Ty::Byte | Ty::Short => ("(I)Ljava/lang/String;", 1),
            Ty::Boolean => ("(Z)Ljava/lang/String;", 1),
            Ty::Char => ("(C)Ljava/lang/String;", 1),
            Ty::Long => ("(J)Ljava/lang/String;", 2),
            Ty::Float => ("(F)Ljava/lang/String;", 1),
            Ty::Double => ("(D)Ljava/lang/String;", 2),
            _ => ("(Ljava/lang/Object;)Ljava/lang/String;", 1),
        };
        let m = cw.methodref("java/lang/String", "valueOf", desc);
        code.invokestatic(m, words, 1);
    }

    /// Emit a numeric conversion `from` → `to` (no-op if equal).
    fn emit_numeric_conversion(&self, from: Ty, to: Ty, code: &mut CodeBuilder) {
        use Ty::*;
        // `Byte`/`Short` (int on the stack): convert the source to `Int`, then narrow (i2b/i2s).
        if matches!(to, Byte | Short) {
            self.emit_numeric_conversion(from, Int, code);
            match to {
                Byte => code.i2b(),
                Short => code.i2s(),
                _ => {}
            }
            return;
        }
        match (from, to) {
            (a, b) if a == b => {}
            // Byte/Short are already int on the stack → widening from them == from Int.
            (Byte | Short, Int) => {}
            (Byte | Short, Long) => code.i2l(),
            (Byte | Short, Float) => code.i2f(),
            (Byte | Short, Double) => code.i2d(),
            (Int, Long) => code.i2l(),
            (Int, Float) => code.i2f(),
            (Int, Double) => code.i2d(),
            (Long, Int) => code.l2i(),
            (Long, Float) => code.l2f(),
            (Long, Double) => code.l2d(),
            (Float, Int) => code.f2i(),
            (Float, Long) => code.f2l(),
            (Float, Double) => code.f2d(),
            (Double, Int) => code.d2i(),
            (Double, Long) => code.d2l(),
            (Double, Float) => code.d2f(),
            _ => {}
        }
    }

    fn emit_new_array(&mut self, elem: Ty, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        match elem {
            Ty::Int => code.newarray(10),
            Ty::Long => code.newarray(11),
            Ty::Double => code.newarray(7),
            Ty::Boolean => code.newarray(4),
            Ty::Char => code.newarray(5),
            _ => {
                let ci = cw.class_ref(ref_internal(elem));
                code.anewarray(ci);
            }
        }
    }

    /// True if property `name` on class `internal` is `lateinit`.
    fn is_lateinit(&self, internal: &str, name: &str) -> bool {
        self.syms.class_by_internal(internal).map_or(false, |c| c.lateinit_props.contains(name))
    }

    /// After a `lateinit` companion-static property value is on the stack, throw if null.
    /// NOTE: this is only called for companion `getstatic` paths where the stack has exactly
    /// 1 item (the property value). Instance/this property paths use the getter, which has the
    /// null check built in (so the stack is always clean at the branch target).
    fn emit_lateinit_guard(&mut self, prop: &str, prop_ty: Ty, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        code.dup();
        let ok = code.new_label();
        // At ok: the dup'd value is on the stack (ifnonnull pops its operand).
        self.rec_s(ok, prop_ty, code, cw);
        code.ifnonnull(ok);
        let exc = cw.class_ref("java/lang/RuntimeException");
        code.new_obj(exc);
        code.dup();
        code.push_string(&format!("lateinit property {prop} has not been initialized"), cw);
        let init = cw.methodref("java/lang/RuntimeException", "<init>", "(Ljava/lang/String;)V");
        code.invokespecial(init, 1, 0);
        code.athrow();
        self.rec_s(ok, prop_ty, code, cw);
        code.bind(ok);
    }

    /// The internal name for a `catch` clause's exception type (a JDK exception / import / declared
    /// class) — mirrors the resolver's `catch_internal`.
    fn catch_internal(&self, name: &str) -> String {
        crate::resolve::builtin_exception(name)
            .map(|s| s.to_string())
            .or_else(|| self.imports.get(name).cloned())
            .or_else(|| self.syms.classes.get(name).map(|c| c.internal.clone()))
            .unwrap_or_else(|| "java/lang/Throwable".to_string())
    }

    /// `try { body } catch (e: T) { … } …`: the body is guarded by an exception-table range; each
    /// handler stores the caught exception, binds the catch variable, and produces the result value
    /// (or discards it, when the `try` is a statement). The body and handlers all converge at `after`.
    fn emit_try(&mut self, e: ExprId, body: ExprId, catches: &[CatchClause], finally: Option<ExprId>, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        let result = self.info.ty(e);
        // A `try` is only sound where the operand stack is empty at entry: when the body throws, the
        // JVM clears the whole stack to just the exception, so any partially-computed values that the
        // surrounding expression left on the stack (e.g. `"" + try { … }`) would be destroyed. Reject
        // (skip) such positions rather than emit code that fails verification / loses values.
        if code.stack_height() != 0 {
            self.diags.error(
                self.file.expr_spans[e.0 as usize],
                "krusty: try/catch is only supported in statement, initializer, return, or argument position (empty operand stack)".to_string(),
            );
            return;
        }
        // `finally` runs on every exit. krusty inlines it on the normal path and in a catch-all that
        // re-throws — but a `return`/`break`/`continue` inside the guarded code bypasses that and would
        // skip the finally. Support only the common "pure cleanup" case (no non-local exit) + require a
        // Unit result (no value to thread across the finally); otherwise reject (skip).
        if finally.is_some() {
            let exits = self.has_nonlocal_exit(body) || catches.iter().any(|c| self.has_nonlocal_exit(c.body));
            if exits || !matches!(result, Ty::Unit | Ty::Nothing) {
                self.diags.error(
                    self.file.expr_spans[e.0 as usize],
                    "krusty: try/finally is only supported for a Unit/Nothing body with no return/break/continue inside".to_string(),
                );
                return;
            }
        }
        // For value-producing try/catch, allocate a temp slot for the result BEFORE the try body so
        // every branch-target frame sees the slot as a known type (needed for StackMapTable).
        let result_tmp = if result != Ty::Unit && result != Ty::Nothing && result != Ty::Error {
            let tmp = self.alloc_temp(result);
            self.init_temp(result, tmp, code, cw); // must precede any rec()
            Some(tmp)
        } else {
            None
        };

        let try_start = code.new_label();
        let try_end = code.new_label();
        let after = code.new_label();

        // Emit a try/catch sub-expression: if value-producing, store result to `result_tmp`;
        // otherwise discard. Stack is always empty after this (safe for StackMapTable frames).
        let emit_value = |this: &mut Self, ex: ExprId, code: &mut CodeBuilder, cw: &mut ClassWriter, result_tmp: Option<u16>| {
            if let Some(tmp) = result_tmp {
                this.emit_expr_as(ex, result, code, cw);
                store_local(result, tmp, code);
            } else {
                this.emit_expr(ex, code, cw);
                this.discard(this.info.ty(ex), code);
            }
        };

        // Snapshot locals BEFORE the try body. All try/catch merge-point frames must use this snapshot:
        // locals declared inside the try body (via alloc_slot) go out of scope after the try, so the
        // handler frames and the `after` convergence point must not include them. Pre-registering here
        // (first-wins) prevents any inner alloc_slot from polluting these frames.
        let handler_locals = self.make_verif_locals(cw);
        // Pre-register `after`: try-body locals are out of scope at the convergence point.
        code.add_frame_if_new(after, handler_locals.clone(), vec![]);
        let catch_handler_labels: Vec<(crate::codegen::classfile::Label, String)> = catches.iter().map(|c| {
            use crate::codegen::classfile::VerifType;
            let internal = self.catch_internal(&c.ty.name);
            let handler = code.new_label();
            let vt = VerifType::Object(cw.class_ref(&internal));
            code.add_frame_if_new(handler, handler_locals.clone(), vec![vt]);
            (handler, internal)
        }).collect();
        let fin_label: Option<crate::codegen::classfile::Label> = if finally.is_some() {
            use crate::codegen::classfile::VerifType;
            let l = code.new_label();
            let vt = VerifType::Object(cw.class_ref("java/lang/Throwable"));
            code.add_frame_if_new(l, handler_locals.clone(), vec![vt]);
            Some(l)
        } else {
            None
        };

        self.rec(try_start, code, cw);
        code.bind(try_start);
        emit_value(self, body, code, cw, result_tmp);
        self.rec(try_end, code, cw);
        code.bind(try_end);
        // On normal completion, run the finally inline then jump past everything. If the body diverges
        // (e.g. ends in `throw`), this is unreachable — skip it (the catch-all below runs the finally).
        if !self.expr_diverges(body) {
            if let Some(f) = finally {
                self.emit_block_discard(f, code, cw);
            }
            self.rec(after, code, cw); // stack: empty
            code.goto(after);
        }

        // Range of the catch handler bodies so the finally catch-all also covers throws in catches.
        // No frame needed here: catches_start is just an exception-table boundary, not a branch target.
        let catches_start = code.new_label();
        code.bind(catches_start);
        for (c, (handler, internal)) in catches.iter().zip(catch_handler_labels.iter()) {
            let cty = Ty::obj(internal);
            // Frame was pre-registered above (first-wins); bind now at the handler bytecode position.
            code.bind(*handler);
            code.set_stack(1); // the JVM places the caught exception on the stack
            let slot = self.fresh_slot(cty);
            code.ensure_locals(slot + slot_words(cty));
            self.store(cty, slot, code);
            let prev = self.slots.insert(c.name.clone(), (slot, cty));
            emit_value(self, c.body, code, cw, result_tmp);
            match prev {
                Some(p) => {
                    self.slots.insert(c.name.clone(), p);
                }
                None => {
                    self.slots.remove(&c.name);
                }
            }
            if !self.expr_diverges(c.body) {
                if let Some(f) = finally {
                    self.emit_block_discard(f, code, cw); // finally after a normally-completing catch
                }
                self.rec(after, code, cw); // stack: empty
                code.goto(after);
            }
            let cti = cw.class_ref(internal);
            code.add_exception(try_start, try_end, *handler, cti);
        }
        // catch-all `finally` handler: run the finally, then re-throw the in-flight exception.
        if let Some(f) = finally {
            let fin = fin_label.unwrap();
            // catches_end is just an exception-table boundary, not a branch target; no frame needed.
            let catches_end = code.new_label();
            code.bind(catches_end);
            // Frame was pre-registered above (first-wins); bind now at the handler bytecode position.
            code.bind(fin);
            code.set_stack(1);
            let ex_slot = self.fresh_slot(Ty::obj("java/lang/Throwable"));
            code.ensure_locals(ex_slot + 1);
            code.astore(ex_slot);
            self.emit_block_discard(f, code, cw);
            code.aload(ex_slot);
            code.athrow();
            code.add_exception(try_start, try_end, fin, 0); // body throw not matched by a catch
            // A throw from within a catch body (only a non-empty range is a legal exception entry).
            if !catches.is_empty() {
                code.add_exception(catches_start, catches_end, fin, 0);
            }
        }
        self.rec(after, code, cw); // stack: empty
        code.bind(after);
        if let Some(tmp) = result_tmp {
            load_local(result, tmp, code); // push result
        }
    }

    /// True if `e` contains a control transfer that would leave an enclosing `try`, bypassing a
    /// `finally`. `return` always escapes; `break`/`continue` escape only when *not* inside a loop
    /// nested within the guarded region (those are local to that loop). `throw` is fine — the finally
    /// catch-all handles it. Recurses into nested `try` so an inner break/return is still seen.
    fn has_nonlocal_exit(&self, e: ExprId) -> bool {
        self.exit_walk(e, false)
    }

    fn exit_walk(&self, e: ExprId, in_loop: bool) -> bool {
        match self.file.expr(e) {
            Expr::Block { stmts, trailing } => {
                stmts.iter().any(|s| self.stmt_exit_walk(*s, in_loop)) || trailing.map_or(false, |t| self.exit_walk(t, in_loop))
            }
            Expr::If { cond, then_branch, else_branch } => {
                self.exit_walk(*cond, in_loop)
                    || self.exit_walk(*then_branch, in_loop)
                    || else_branch.map_or(false, |b| self.exit_walk(b, in_loop))
            }
            Expr::When { subject, arms } => {
                subject.map_or(false, |s| self.exit_walk(s, in_loop))
                    || arms.iter().any(|a| a.conditions.iter().any(|c| self.exit_walk(*c, in_loop)) || self.exit_walk(a.body, in_loop))
            }
            Expr::Try { body, catches, finally } => {
                self.exit_walk(*body, in_loop)
                    || catches.iter().any(|c| self.exit_walk(c.body, in_loop))
                    || finally.map_or(false, |f| self.exit_walk(f, in_loop))
            }
            _ => false,
        }
    }

    fn stmt_exit_walk(&self, s: StmtId, in_loop: bool) -> bool {
        match self.file.stmt(s) {
            Stmt::Return(_) => true,
            Stmt::Break | Stmt::Continue => !in_loop, // local to a loop nested inside the guarded region
            Stmt::Expr(e) => self.exit_walk(*e, in_loop),
            Stmt::Local { init, .. } => self.exit_walk(*init, in_loop),
            Stmt::While { body, .. } => self.exit_walk(*body, true),
            Stmt::For { body, .. } => self.exit_walk(*body, true),
            _ => false,
        }
    }

    fn emit_fun(&mut self, f: &FunDecl, cw: &mut ClassWriter) {
        let mut sig = match self.syms.funs.get(&f.name) {
            Some(s) => s.clone(),
            None => return,
        };
        // Use the inferred return type if the checker overrode the defaulted-to-Unit signature.
        if let Some(&inferred) = self.info.fun_ret_overrides.get(&f.name) {
            sig.ret = inferred;
        }
        self.ret_ty = sig.ret;
        for (p, ty) in f.params.iter().zip(&sig.params) {
            self.alloc_slot(&p.name, *ty);
        }
        let mut code = CodeBuilder::new(self.next_slot);
        match &f.body {
            FunBody::Expr(e) => {
                self.emit_expr_as(*e, sig.ret, &mut code, cw);
                self.emit_return(sig.ret, &mut code);
            }
            FunBody::Block(b) => {
                self.emit_block_as_body(*b, &mut code, cw);
            }
            FunBody::None => self.emit_default_return(sig.ret, &mut code, cw),
        }
        code.ensure_locals(self.next_slot);
        code.link();
        cw.add_method(ACC_PUBLIC | ACC_STATIC | ACC_FINAL, &f.name, &method_descriptor(&sig.params, sig.ret), &code);
    }

    /// A `{ ... }` used directly as a function body: emit statements; a trailing expr (if the fn is
    /// non-Unit) becomes the returned value; otherwise rely on explicit `return`s + a Unit fallthrough.
    fn emit_block_as_body(&mut self, block: ExprId, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        let Expr::Block { stmts, trailing } = self.file.expr(block).clone() else { return };
        for s in &stmts {
            self.emit_stmt(*s, code, cw);
        }
        match trailing {
            // A trailing `when`/`if` that yields no value (Unit) but whose arms `return` (an
            // exhaustive/diverging body): emit it as a statement, then a dead default return so the
            // fall-through path still verifies. The arm `return`s carry the real result.
            Some(te) if self.ret_ty != Ty::Unit && self.info.ty(te) == Ty::Unit => {
                self.emit_expr(te, code, cw);
                self.emit_default_return(self.ret_ty, code, cw);
            }
            Some(te) if self.ret_ty != Ty::Unit => {
                self.emit_expr_as(te, self.ret_ty, code, cw);
                self.emit_return(self.ret_ty, code);
            }
            Some(te) => {
                self.emit_expr(te, code, cw);
                self.discard(self.info.ty(te), code);
                code.ret_void();
            }
            None => {
                if self.ret_ty == Ty::Unit {
                    code.ret_void();
                }
                // non-Unit: explicit `return`s carry control flow (verifier checks completeness)
            }
        }
    }

    fn emit_return(&mut self, ret: Ty, code: &mut CodeBuilder) {
        match ret {
            Ty::Int | Ty::Byte | Ty::Short | Ty::Boolean | Ty::Char => code.ireturn(),
            Ty::Long => code.lreturn(),
            Ty::Float => code.freturn(),
            Ty::Double => code.dreturn(),
            Ty::String | Ty::Obj(_) | Ty::Null | Ty::Array(_) | Ty::Fun(_) => code.areturn(),
            // `Nothing` only reaches here if a diverging expression somehow fell through; it never
            // produces a value, so a void return is a safe (unreachable) default.
            Ty::Unit | Ty::Nothing | Ty::Error => code.ret_void(),
        }
    }

    fn emit_default_return(&mut self, ret: Ty, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        match ret {
            Ty::Int | Ty::Byte | Ty::Short | Ty::Boolean | Ty::Char => { code.push_int(0, cw); code.ireturn(); }
            Ty::Long => { code.push_long(0, cw); code.lreturn(); }
            Ty::Double => { code.push_double(0.0, cw); code.dreturn(); }
            Ty::String => { code.push_string("", cw); code.areturn(); }
            Ty::Obj(_) | Ty::Null | Ty::Fun(_) => { code.aconst_null(); code.areturn(); }
            _ => code.ret_void(),
        }
    }

    fn discard(&self, t: Ty, code: &mut CodeBuilder) {
        if t == Ty::Nothing { return; } // Never returns; athrow already consumed the value
        match slot_words(t) {
            1 => code.pop(),
            2 => code.pop2(),
            _ => {}
        }
    }

    // ---- statements ----
    fn emit_stmt(&mut self, s: StmtId, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        match self.file.stmt(s).clone() {
            Stmt::Local { name, ty, init, .. } => {
                let lty = ty.as_ref().and_then(|r| Ty::from_name(&r.name)).unwrap_or_else(|| self.info.ty(init));
                // A `Unit`-typed local holds no JVM value: evaluate the initializer for its effect.
                if lty == Ty::Unit {
                    self.emit_expr(init, code, cw);
                    self.discard(self.info.ty(init), code);
                } else {
                    self.emit_expr_as(init, lty, code, cw);
                    let slot = self.alloc_slot(&name, lty);
                    self.store(lty, slot, code);
                }
            }
            Stmt::Assign { name, value } => {
                if let Some(&(slot, ty)) = self.slots.get(&name) {
                    self.emit_expr_as(value, ty, code, cw);
                    self.store(ty, slot, code);
                } else if let Some(&ty) = self.class_props.get(&name).filter(|_| self.is_instance || self.recv.is_some()) {
                    // implicit `this.<prop> = value`: write via the setter (open class / receiver
                    // whose field is private) or the backing field directly.
                    let owner = self.implicit_class();
                    self.emit_implicit_this(code);
                    self.emit_expr_as(value, ty, code, cw);
                    if self.recv.is_some() || self.props_via_getter {
                        let setter = format!("set{}", capitalize(&name));
                        let m = cw.methodref(&owner, &setter, &method_descriptor(&[ty], Ty::Unit));
                        code.invokevirtual(m, slot_words(ty) as i32, 0);
                    } else {
                        let f = cw.fieldref(&owner, &name, &ty.descriptor());
                        code.putfield(f, slot_words(ty) as i32);
                    }
                } else if let Some(&(ty, _)) = self.syms.props.get(&name).filter(|_| !self.is_instance && self.recv.is_none()) {
                    // top-level `var` property write → putstatic on the file facade.
                    self.emit_expr_as(value, ty, code, cw);
                    let f = cw.fieldref(&self.class.clone(), &name, &ty.descriptor());
                    code.putstatic(f, slot_words(ty) as i32);
                } else if self.syms.props.contains_key(&name) {
                    // top-level property write from an instance method/`init` → would target the
                    // class, not the facade (and silently mis-store). Reject rather than miscompile.
                    self.diags.error(self.file.stmt_spans[s.0 as usize], "krusty: top-level property access from a member method is not supported".to_string());
                }
            }
            Stmt::AssignMember { receiver, name, value } => {
                if let Ty::Obj(internal) = self.info.ty(receiver) {
                    let prop_ty = self.syms.prop_of(internal, &name).map(|(t, _)| t).unwrap_or(Ty::Error);
                    let is_iface = self.syms.class_by_internal(internal).map_or(false, |c| c.is_interface);
                    self.emit_expr(receiver, code, cw);
                    self.emit_expr_as(value, prop_ty, code, cw);
                    // Write via the public setter (backing fields are private, so a cross-instance
                    // putfield would fail; the setter also dispatches correctly for open classes).
                    let setter = format!("set{}", capitalize(&name));
                    let desc = method_descriptor(&[prop_ty], Ty::Unit);
                    if is_iface {
                        let m = cw.interface_methodref(internal, &setter, &desc);
                        code.invokeinterface(m, slot_words(prop_ty) as i32, 0);
                    } else {
                        let m = cw.methodref(internal, &setter, &desc);
                        code.invokevirtual(m, slot_words(prop_ty) as i32, 0);
                    }
                }
            }
            Stmt::AssignIndex { array, index, value } => {
                let elem = self.info.ty(array).array_elem().unwrap_or(Ty::Error);
                self.emit_expr(array, code, cw);
                self.emit_expr_as(index, Ty::Int, code, cw);
                self.emit_expr_as(value, elem, code, cw);
                let (op, words) = array_store_op(elem);
                code.array_store(op, words);
            }
            Stmt::Return(e) => match e {
                Some(ex) => {
                    self.emit_expr_as(ex, self.ret_ty, code, cw);
                    self.emit_return(self.ret_ty, code);
                }
                None => code.ret_void(),
            },
            Stmt::Break => {
                if let Some(&(_, brk)) = self.loop_labels.last() {
                    self.rec(brk, code, cw);
                    code.goto(brk);
                } else {
                    self.diags.error(self.file.stmt_spans[s.0 as usize], "krusty: 'break' outside a loop".to_string());
                }
            }
            Stmt::Continue => {
                if let Some(&(cont, _)) = self.loop_labels.last() {
                    self.rec(cont, code, cw);
                    code.goto(cont);
                } else {
                    self.diags.error(self.file.stmt_spans[s.0 as usize], "krusty: 'continue' outside a loop".to_string());
                }
            }
            Stmt::While { cond, body } => {
                let start = code.new_label();
                let end = code.new_label();
                self.rec(start, code, cw);
                code.bind(start);
                self.emit_cond_jump(cond, end, false, code, cw); // if !cond goto end
                self.loop_labels.push((start, end)); // continue → re-test, break → end
                self.emit_block_discard(body, code, cw);
                self.loop_labels.pop();
                self.rec(start, code, cw);
                code.goto(start);
                self.rec(end, code, cw);
                code.bind(end);
            }
            Stmt::For { name, range, body } => {
                // Lower an integer range `for` to a counted while loop.
                self.emit_expr_as(range.start, Ty::Int, code, cw);
                let i = self.alloc_slot(&name, Ty::Int);
                code.istore(i);
                self.emit_expr_as(range.end, Ty::Int, code, cw);
                // alloc_temp so the end/step slots appear in StackMapTable frames at loop back-edge.
                let end_slot = self.alloc_temp(Ty::Int);
                code.istore(end_slot);
                let step_slot = range.step.map(|s| {
                    self.emit_expr_as(s, Ty::Int, code, cw);
                    let ss = self.alloc_temp(Ty::Int);
                    code.istore(ss);
                    ss
                });
                let start = code.new_label();
                let cont = code.new_label();
                let end = code.new_label();
                self.rec(start, code, cw);
                code.bind(start);
                code.iload(i);
                code.iload(end_slot);
                self.rec(end, code, cw);
                match range.kind {
                    RangeKind::Through => code.if_icmpgt(end), // exit when i > end
                    RangeKind::Until => code.if_icmpge(end),   // exit when i >= end
                    RangeKind::DownTo => code.if_icmplt(end),  // exit when i < end
                }
                self.loop_labels.push((cont, end)); // continue → the increment step, break → end
                self.emit_block_discard(body, code, cw);
                self.loop_labels.pop();
                self.rec(cont, code, cw);
                code.bind(cont);
                code.iload(i);
                match step_slot {
                    Some(ss) => code.iload(ss),
                    None => code.push_int(1, cw),
                }
                if range.kind == RangeKind::DownTo {
                    code.isub();
                } else {
                    code.iadd();
                }
                code.istore(i);
                self.rec(start, code, cw);
                code.goto(start);
                self.rec(end, code, cw);
                code.bind(end);
            }
            Stmt::ForEach { name, iterable, body } => {
                // Lower `for (x in arr)` / `for (c in str)` to an index loop.
                let iter_ty = self.info.ty(iterable);
                let is_string = iter_ty == Ty::String;
                let elem = if is_string { Ty::Char } else { iter_ty.array_elem().unwrap_or(Ty::Error) };
                self.emit_expr(iterable, code, cw);
                // alloc_temp so recv/i slots appear in StackMapTable frames at loop back-edge.
                let recv_slot = self.alloc_temp(iter_ty);
                code.astore(recv_slot);
                let i_slot = self.alloc_temp(Ty::Int);
                code.push_int(0, cw);
                code.istore(i_slot);
                let x_slot = self.alloc_slot(&name, elem);
                self.init_temp(elem, x_slot, code, cw); // slot must be initialized before rec()
                let start = code.new_label();
                let cont = code.new_label();
                let end = code.new_label();
                self.rec(start, code, cw);
                code.bind(start);
                code.iload(i_slot);
                code.aload(recv_slot);
                if is_string {
                    let m = cw.methodref("java/lang/String", "length", "()I");
                    code.invokevirtual(m, 0, 1);
                } else {
                    code.arraylength();
                }
                self.rec(end, code, cw);
                code.if_icmpge(end); // i >= size → done
                code.aload(recv_slot);
                code.iload(i_slot);
                if is_string {
                    let m = cw.methodref("java/lang/String", "charAt", "(I)C");
                    code.invokevirtual(m, 1, 1);
                } else {
                    let (lop, lwords) = array_load_op(elem);
                    code.array_load(lop, lwords);
                }
                store_local(elem, x_slot, code);
                self.loop_labels.push((cont, end));
                self.emit_block_discard(body, code, cw);
                self.loop_labels.pop();
                self.rec(cont, code, cw);
                code.bind(cont);
                code.iinc(i_slot, 1);
                self.rec(start, code, cw);
                code.goto(start);
                self.rec(end, code, cw);
                code.bind(end);
            }
            Stmt::Expr(e) => {
                self.emit_expr(e, code, cw);
                self.discard(self.info.ty(e), code);
            }
            Stmt::LocalFun(f) => {
                // Emit as a private static method on the same class. Save and restore all method-local
                // state so the parent function's emission context is unaffected.
                let Some((mangled, sig)) = self.info.local_fun_sigs.get(&s).cloned() else { return };
                let saved_slots = std::mem::take(&mut self.slots);
                let saved_next = self.next_slot;
                let saved_ret = self.ret_ty;
                let saved_loop = std::mem::take(&mut self.loop_labels);
                let saved_is_inst = self.is_instance;
                let saved_props = std::mem::take(&mut self.class_props);

                self.next_slot = 0;
                self.is_instance = false;
                self.ret_ty = sig.ret;
                for (p, &ty) in f.params.iter().zip(&sig.params) {
                    self.alloc_slot(&p.name, ty);
                }
                let mut lcode = CodeBuilder::new(self.next_slot);
                match &f.body {
                    FunBody::Expr(e) => {
                        self.emit_expr_as(*e, sig.ret, &mut lcode, cw);
                        self.emit_return(sig.ret, &mut lcode);
                    }
                    FunBody::Block(b) => self.emit_block_as_body(*b, &mut lcode, cw),
                    FunBody::None => self.emit_default_return(sig.ret, &mut lcode, cw),
                }
                lcode.ensure_locals(self.next_slot);
                lcode.link();
                cw.add_method(ACC_STATIC, &mangled, &method_descriptor(&sig.params, sig.ret), &lcode);
                self.local_fun_emitted_class.insert(mangled.clone(), cw.internal_name.clone());

                self.slots = saved_slots;
                self.next_slot = saved_next;
                self.ret_ty = saved_ret;
                self.loop_labels = saved_loop;
                self.is_instance = saved_is_inst;
                self.class_props = saved_props;
            }
        }
    }

    fn store(&self, ty: Ty, slot: u16, code: &mut CodeBuilder) {
        store_local(ty, slot, code);
    }

    /// Emit a block for its side effects, discarding any trailing value.
    fn emit_block_discard(&mut self, block: ExprId, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        if let Expr::Block { stmts, trailing } = self.file.expr(block).clone() {
            for s in &stmts {
                self.emit_stmt(*s, code, cw);
            }
            if let Some(te) = trailing {
                self.emit_expr(te, code, cw);
                self.discard(self.info.ty(te), code);
            }
        } else {
            // a non-block while body
            self.emit_expr(block, code, cw);
            self.discard(self.info.ty(block), code);
        }
    }

    // ---- expressions ----
    fn emit_expr_as(&mut self, e: ExprId, target: Ty, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        let from = self.info.ty(e);
        self.emit_expr(e, code, cw);
        if from.is_numeric() && target.is_numeric() {
            // Handles both widening (i2l, i2d, …) and narrowing to Byte/Short (i2b/i2s).
            self.emit_numeric_conversion(from, target, code);
        } else if from == Ty::obj("java/lang/Object") && target.is_reference() && target != Ty::obj("java/lang/Object") {
            // Erased Object (e.g. from FunctionN.invoke()) narrowed to a concrete reference type.
            let ci = cw.class_ref(ref_internal(target));
            code.checkcast(ci);
        }
    }

    fn emit_expr(&mut self, e: ExprId, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        match self.file.expr(e).clone() {
            Expr::IntLit(v) => code.push_int(v as i32, cw),
            Expr::LongLit(v) => code.push_long(v, cw),
            Expr::DoubleLit(v) => code.push_double(v, cw),
            Expr::FloatLit(v) => code.push_float(v, cw),
            Expr::BoolLit(b) => code.push_int(if b { 1 } else { 0 }, cw),
            Expr::StringLit(s) => code.push_string(&s, cw),
            Expr::CharLit(c) => code.push_int(c as i32, cw),
            Expr::NullLit => code.aconst_null(),
            Expr::Lambda { param, body } => {
                self.emit_lambda(e, param.as_deref(), body, code, cw);
            }
            Expr::NotNull { operand } if !self.info.ty(operand).is_reference() => {
                // `!!` on a non-null primitive (`42!!`) is the operand itself — no null check.
                self.emit_expr(operand, code, cw);
            }
            Expr::NotNull { operand } => {
                let result = self.info.ty(e);
                self.emit_expr(operand, code, cw);
                code.dup();
                let ok = code.new_label();
                // At `ok` the dup'd value is on the stack (ifnonnull pops from the top).
                self.rec_s(ok, result, code, cw);
                code.ifnonnull(ok);
                let npe = cw.class_ref("java/lang/NullPointerException");
                code.new_obj(npe);
                code.dup();
                let init = cw.methodref("java/lang/NullPointerException", "<init>", "()V");
                code.invokespecial(init, 0, 0);
                code.athrow();
                self.rec_s(ok, result, code, cw);
                code.bind(ok);
            }
            Expr::Throw { operand } => {
                self.emit_expr(operand, code, cw);
                code.athrow();
            }
            Expr::Index { array, index } => {
                let elem = self.info.ty(array).array_elem().unwrap_or(Ty::Error);
                self.emit_expr(array, code, cw);
                self.emit_expr_as(index, Ty::Int, code, cw);
                let (op, words) = array_load_op(elem);
                code.array_load(op, words);
            }
            Expr::Try { body, catches, finally } => self.emit_try(e, body, &catches, finally, code, cw),
            Expr::Is { operand, ty, negated } => {
                self.emit_expr(operand, code, cw);
                let tt = resolve_ty(&ty, self.syms);
                let internal = ref_internal(tt);
                let ci = cw.class_ref(internal);
                code.instance_of(ci);
                if negated {
                    // boolean NOT: x ^ 1
                    code.push_int(1, cw);
                    code.ixor();
                }
            }
            Expr::As { operand, ty, nullable } => {
                let op_ty = self.info.ty(operand);
                self.emit_expr(operand, code, cw);
                let tt = resolve_ty(&ty, self.syms);
                let internal = ref_internal(tt);
                let ci = cw.class_ref(internal);
                let cast_ty = Ty::obj(crate::types::intern(internal));
                if nullable {
                    // `as?`: keep the value if `instanceof T`, else replace with null.
                    // dup; instanceof pops the dup'd copy → at is_inst, stack = {operand}.
                    // At end, either {null} or {T}.
                    code.dup();
                    code.instance_of(ci);
                    let is_inst = code.new_label();
                    let end = code.new_label();
                    self.rec_s(is_inst, op_ty, code, cw);
                    code.ifne(is_inst);
                    code.pop();
                    code.aconst_null();
                    self.rec_s(end, cast_ty, code, cw);
                    code.goto(end);
                    self.rec_s(is_inst, op_ty, code, cw);
                    code.bind(is_inst);
                    code.checkcast(ci);
                    self.rec_s(end, cast_ty, code, cw);
                    code.bind(end);
                } else {
                    code.checkcast(ci);
                    // `x as T` to a *non-nullable* `T`: a null value throws (Kotlin's null check —
                    // `checkcast` alone lets null through). `x as T?` (nullable) keeps null.
                    if !ty.nullable {
                        // dup; ifnonnull(ok): at ok, stack = {dup'd cast result}.
                        code.dup();
                        let ok = code.new_label();
                        self.rec_s(ok, cast_ty, code, cw);
                        code.ifnonnull(ok);
                        let npe = cw.class_ref("java/lang/NullPointerException");
                        code.new_obj(npe);
                        code.dup();
                        let init = cw.methodref("java/lang/NullPointerException", "<init>", "()V");
                        code.invokespecial(init, 0, 0);
                        code.athrow();
                        self.rec_s(ok, cast_ty, code, cw);
                        code.bind(ok);
                    }
                }
            }
            Expr::Elvis { lhs, rhs } => {
                let result = self.info.ty(e);
                if !self.info.ty(lhs).is_reference() {
                    // a non-null primitive lhs (`42 ?: 239`) is never null — the elvis is just the lhs.
                    self.emit_expr_as(lhs, result, code, cw);
                } else {
                    self.emit_expr(lhs, code, cw);
                    code.dup();
                    let end = code.new_label();
                    // At `end` the stack has 1 item (the non-null lhs or the rhs).
                    self.rec_s(end, result, code, cw);
                    code.ifnonnull(end);
                    code.pop(); // discard the null
                    self.emit_expr_as(rhs, result, code, cw);
                    self.rec_s(end, result, code, cw);
                    code.bind(end);
                }
            }
            Expr::Template(parts) => {
                let sb = cw.class_ref("java/lang/StringBuilder");
                let ctor = cw.methodref("java/lang/StringBuilder", "<init>", "()V");
                code.new_obj(sb);
                code.dup();
                code.invokespecial(ctor, 0, 0);
                for p in &parts {
                    match p {
                        TemplatePart::Str(s) => {
                            code.push_string(s, cw);
                            let m = cw.methodref("java/lang/StringBuilder", "append", "(Ljava/lang/String;)Ljava/lang/StringBuilder;");
                            code.invokevirtual(m, 1, 1);
                        }
                        TemplatePart::Expr(pe) => self.emit_append(*pe, code, cw),
                    }
                }
                let tos = cw.methodref("java/lang/StringBuilder", "toString", "()Ljava/lang/String;");
                code.invokevirtual(tos, 0, 1);
            }
            Expr::SafeCall { receiver, name, args } => {
                let rt = self.info.ty(receiver);
                let result = self.info.ty(e);
                // Use temp slots so all branch targets (lnull, end) have empty operand stacks.
                // Use the precise types so the verifier knows the actual type when loading these slots.
                let recv_slot = self.alloc_temp(rt);
                let result_slot = self.alloc_temp(result);
                code.ensure_locals(self.next_slot);
                self.emit_expr(receiver, code, cw);
                code.astore(recv_slot);       // recv_slot initialized; must precede any rec()
                code.aconst_null();
                code.astore(result_slot);     // result_slot initialized to null; must precede any rec()
                let lnull = code.new_label();
                let end = code.new_label();
                code.aload(recv_slot);
                self.rec(lnull, code, cw);    // frame: stack empty (ifnull pops its operand)
                code.ifnull(lnull);
                // non-null path: load receiver for the call.
                code.aload(recv_slot);
                match &args {
                    None => {
                        // property getter
                        let internal = rt.obj_internal().unwrap_or("java/lang/Object");
                        let getter = format!("get{}", capitalize(&name));
                        let m = cw.methodref(internal, &getter, &method_descriptor(&[], result));
                        code.invokevirtual(m, 0, slot_words(result) as i32);
                    }
                    Some(call_args) => {
                        let arg_tys: Vec<Ty> = call_args.iter().map(|a| self.info.ty(*a)).collect();
                        if rt == Ty::String {
                            if let Some((desc, ret)) = crate::resolve::resolve_string_instance(&name, &arg_tys) {
                                for a in call_args {
                                    self.emit_expr(*a, code, cw);
                                }
                                let aw: i32 = arg_tys.iter().map(|t| slot_words(*t) as i32).sum();
                                let m = cw.methodref("java/lang/String", &name, desc);
                                code.invokevirtual(m, aw, slot_words(ret) as i32);
                            }
                        } else if let Ty::Obj(internal) = rt {
                            let sig = self.syms.method_of(internal, &name);
                            let is_iface = self.syms.class_by_internal(internal).map_or(false, |c| c.is_interface);
                            if let Some(sig) = sig {
                                for (a, pty) in call_args.iter().zip(&sig.params) {
                                    self.emit_expr_as(*a, *pty, code, cw);
                                }
                                let aw: i32 = sig.params.iter().map(|t| slot_words(*t) as i32).sum();
                                let desc = method_descriptor(&sig.params, sig.ret);
                                if is_iface {
                                    let m = cw.interface_methodref(internal, &name, &desc);
                                    code.invokeinterface(m, aw, slot_words(sig.ret) as i32);
                                } else {
                                    let m = cw.methodref(internal, &name, &desc);
                                    code.invokevirtual(m, aw, slot_words(sig.ret) as i32);
                                }
                            } else if let Some((desc, ret)) = crate::resolve::resolve_java_instance(&self.syms.classpath, internal, &name, &arg_tys) {
                                for a in call_args {
                                    self.emit_expr(*a, code, cw);
                                }
                                let aw: i32 = arg_tys.iter().map(|t| slot_words(*t) as i32).sum();
                                let m = cw.methodref(internal, &name, &desc);
                                code.invokevirtual(m, aw, slot_words(ret) as i32);
                            }
                        }
                    }
                }
                code.astore(result_slot);     // store result; stack empty
                self.rec(end, code, cw);      // frame: stack empty
                code.goto(end);
                self.rec(lnull, code, cw);    // frame: stack empty (first-wins already set above)
                code.bind(lnull);
                code.aconst_null();
                code.astore(result_slot);     // store null result
                self.rec(end, code, cw);
                code.bind(end);
                code.aload(result_slot);      // push result (reference or null)
                let _ = result;
            }
            Expr::Name(n) if n == "this" && (self.is_instance || self.recv.is_some()) => {
                self.emit_implicit_this(code);
            }
            Expr::Name(n) => {
                if let Some(&(slot, ty)) = self.slots.get(&n) {
                    load_local(ty, slot, code);
                    // Smart-cast: the checker narrowed this use to a more specific reference type
                    // (e.g. inside `if (x is T)`). The slot holds the wider type, so insert the cast.
                    let narrowed = self.info.ty(e);
                    if narrowed != ty && narrowed.is_reference() && ty.is_reference() {
                        let ci = cw.class_ref(ref_internal(narrowed));
                        code.checkcast(ci);
                    }
                } else if let Some(&ty) = self.class_props.get(&n).filter(|_| self.is_instance || self.recv.is_some()) {
                    // implicit `this.<prop>`: read via the getter in an open class / `run`-`with`-`apply`
                    // receiver (whose backing field is private to its own class); else the field.
                    // `lateinit` always uses the getter — the null check lives there so it runs with a
                    // clean stack (exactly one item), regardless of what the caller has accumulated.
                    let owner = self.implicit_class();
                    self.emit_implicit_this(code);
                    if self.recv.is_some() || self.props_via_getter || self.is_lateinit(&owner, &n) {
                        let getter = format!("get{}", capitalize(&n));
                        let m = cw.methodref(&owner, &getter, &method_descriptor(&[], ty));
                        code.invokevirtual(m, 0, slot_words(ty) as i32);
                    } else {
                        let f = cw.fieldref(&owner, &n, &ty.descriptor());
                        code.getfield(f, slot_words(ty) as i32);
                    }
                } else if let Some((cls, ty)) = self
                    .companion_of
                    .as_ref()
                    .and_then(|c| self.syms.classes.get(c))
                    .and_then(|c| c.static_props.get(&n).map(|&t| (c.internal.clone(), t)))
                {
                    // Unqualified companion property inside a companion member → getstatic.
                    let f = cw.fieldref(&cls, &n, &ty.descriptor());
                    code.getstatic(f, slot_words(ty) as i32);
                    if self.is_lateinit(&cls, &n) {
                        self.emit_lateinit_guard(&n, ty, code, cw);
                    }
                } else if let Some(&(ty, _)) = self.syms.props.get(&n) {
                    // top-level property → static field on the file facade.
                    if self.is_instance {
                        self.diags.error(self.file.expr_spans[e.0 as usize], "krusty: top-level property access from a member method is not supported");
                    } else {
                        let f = cw.fieldref(&self.class.clone(), &n, &ty.descriptor());
                        code.getstatic(f, slot_words(ty) as i32);
                    }
                } else {
                    self.diags.error(self.file.expr_spans[e.0 as usize], format!("krusty: unbound local '{n}' in codegen"));
                }
            }
            Expr::Unary { op, operand } => {
                let t = self.info.ty(e);
                match op {
                    UnOp::Neg => {
                        self.emit_expr(operand, code, cw);
                        match t {
                            Ty::Int | Ty::Byte | Ty::Short => code.ineg(),
                            Ty::Long => code.lneg(),
                            Ty::Float => code.fneg(),
                            Ty::Double => code.dneg(),
                            _ => {}
                        }
                    }
                    UnOp::Not => self.emit_bool(e, code, cw),
                }
            }
            Expr::Binary { op, lhs, rhs } => {
                if is_arith(op) {
                    self.emit_arith_expr(e, op, lhs, rhs, code, cw);
                } else {
                    self.emit_bool(e, code, cw); // comparison / && / ||
                }
            }
            Expr::Call { callee, args } => self.emit_call(e, callee, &args, code, cw),
            Expr::Member { receiver, name } => {
                // `EnumName.ENTRY` → getstatic the entry's static field.
                if let Expr::Name(en) = self.file.expr(receiver).clone() {
                    if !self.slots.contains_key(&en) {
                        if let Some(entries) = self.syms.enums.get(&en) {
                            if entries.iter().any(|x| x == &name) {
                                let internal = self.syms.classes.get(&en).map(|c| c.internal.clone()).unwrap_or(en.clone());
                                let f = cw.fieldref(&internal, &name, &Ty::obj(&internal).descriptor());
                                code.getstatic(f, 1);
                                return;
                            }
                        }
                        // `ClassName.PROP` — a companion (static) field read.
                        if let Some(cs) = self.syms.classes.get(&en) {
                            if let Some(&ty) = cs.static_props.get(&name) {
                                let internal = cs.internal.clone();
                                let lateinit = cs.lateinit_props.contains(&name);
                                let f = cw.fieldref(&internal, &name, &ty.descriptor());
                                code.getstatic(f, slot_words(ty) as i32);
                                if lateinit {
                                    self.emit_lateinit_guard(&name, ty, code, cw);
                                }
                                return;
                            }
                        }
                        // `ObjectName.prop` — getstatic INSTANCE; invokevirtual getProp().
                        if self.syms.objects.contains(&en) {
                            if let Some(cs) = self.syms.classes.get(&en) {
                                if let Some((ty, _)) = cs.prop(&name) {
                                    let internal = cs.internal.clone();
                                    let inst = cw.fieldref(&internal, "INSTANCE", &Ty::obj(&internal).descriptor());
                                    code.getstatic(inst, 1);
                                    let getter = format!("get{}", capitalize(&name));
                                    let m = cw.methodref(&internal, &getter, &method_descriptor(&[], ty));
                                    code.invokevirtual(m, 0, slot_words(ty) as i32);
                                    return;
                                }
                            }
                        }
                    }
                }
                if name == "size" && matches!(self.info.ty(receiver), Ty::Array(_)) {
                    self.emit_expr(receiver, code, cw);
                    code.arraylength();
                } else if name == "length" {
                    let owner = if self.info.ty(receiver) == Ty::obj("java/lang/StringBuilder") {
                        "java/lang/StringBuilder"
                    } else {
                        "java/lang/String"
                    };
                    self.emit_expr(receiver, code, cw);
                    let m = cw.methodref(owner, "length", "()I");
                    code.invokevirtual(m, 0, 1);
                } else if let Ty::Obj(internal) = self.info.ty(receiver) {
                    // Enum `.name` / `.ordinal` → java.lang.Enum accessors.
                    if (name == "name" || name == "ordinal")
                        && self.syms.enums.keys().any(|en| self.syms.classes.get(en).map_or(false, |c| c.internal == internal))
                    {
                        self.emit_expr(receiver, code, cw);
                        let (m, rw) = if name == "name" {
                            (cw.methodref("java/lang/Enum", "name", "()Ljava/lang/String;"), 1)
                        } else {
                            (cw.methodref("java/lang/Enum", "ordinal", "()I"), 1)
                        };
                        code.invokevirtual(m, 0, rw);
                        return;
                    }
                    // Property read on a class value: `p.prop` → invokevirtual get<Prop>().
                    // For `lateinit` properties the null check is inside the getter itself.
                    let pty = self.syms.prop_of(internal, &name).map(|(t, _)| t).unwrap_or(Ty::Error);
                    let is_iface = self.syms.class_by_internal(internal).map_or(false, |c| c.is_interface);
                    self.emit_expr(receiver, code, cw);
                    let getter = format!("get{}", capitalize(&name));
                    let desc = method_descriptor(&[], pty);
                    if is_iface {
                        let m = cw.interface_methodref(internal, &getter, &desc);
                        code.invokeinterface(m, 0, slot_words(pty) as i32);
                    } else {
                        let m = cw.methodref(internal, &getter, &desc);
                        code.invokevirtual(m, 0, slot_words(pty) as i32);
                    }
                } else {
                    self.diags.error(self.file.expr_spans[e.0 as usize], format!("krusty v0: member '{name}' not emittable"));
                }
            }
            Expr::If { cond, then_branch, else_branch } => {
                let result = self.info.ty(e);
                match else_branch {
                    Some(eb) => {
                        let l_else = code.new_label();
                        let l_end = code.new_label();
                        if result == Ty::Unit {
                            // Statement-if with else: discard both branches; stack empty at l_end.
                            self.emit_cond_jump(cond, l_else, false, code, cw);
                            self.emit_expr_as(then_branch, result, code, cw);
                            // Skip goto if then-branch already transferred control (return/throw).
                            // Emitting dead code after areturn/athrow would require an extra frame.
                            if !self.expr_diverges(then_branch) {
                                self.rec(l_end, code, cw);
                                code.goto(l_end);
                            }
                            self.rec(l_else, code, cw); // first-wins; may already be set
                            code.bind(l_else);
                            self.emit_expr_as(eb, result, code, cw);
                            // Always register l_end: needed for dead-code frame when both diverge.
                            self.rec(l_end, code, cw);
                            code.bind(l_end);
                        } else {
                            // Value-producing if: use a temp slot so the stack is empty at l_end.
                            let tmp = self.alloc_temp(result);
                            self.init_temp(result, tmp, code, cw); // must precede any rec()
                            self.emit_cond_jump(cond, l_else, false, code, cw);
                            self.emit_expr_as(then_branch, result, code, cw);
                            if !self.expr_diverges(then_branch) {
                                store_local(result, tmp, code);
                                self.rec(l_end, code, cw); // stack: empty
                                code.goto(l_end);
                            }
                            self.rec(l_else, code, cw); // first-wins; may already be set
                            code.bind(l_else);
                            self.emit_expr_as(eb, result, code, cw);
                            if !self.expr_diverges(eb) {
                                store_local(result, tmp, code);
                            }
                            self.rec(l_end, code, cw); // always: dead-code frame when both diverge
                            code.bind(l_end);
                            load_local(result, tmp, code); // push result onto stack
                        }
                    }
                    None => {
                        // statement-if (Unit value)
                        let l_end = code.new_label();
                        self.emit_cond_jump(cond, l_end, false, code, cw);
                        self.emit_block_discard(then_branch, code, cw);
                        self.rec(l_end, code, cw);
                        code.bind(l_end);
                    }
                }
            }
            Expr::Block { stmts, trailing } => {
                for s in &stmts {
                    self.emit_stmt(*s, code, cw);
                }
                if let Some(te) = trailing {
                    self.emit_expr(te, code, cw);
                }
            }
            Expr::When { subject, arms } => self.emit_when(e, subject, &arms, code, cw),
        }
    }

    /// Lower `when` to an if-chain. With a subject, it is stored once in a temp local and each arm
    /// condition becomes a `subject == cond` test; without, each condition is a boolean test.
    fn emit_when(&mut self, e: ExprId, subject: Option<ExprId>, arms: &[WhenArm], code: &mut CodeBuilder, cw: &mut ClassWriter) {
        let result = self.info.ty(e);
        let end = code.new_label();

        // For value-producing `when`, allocate a temp slot BEFORE processing arms so that every
        // StackMapTable frame (including those inside condition branches) sees the slot's type.
        let result_tmp = if result != Ty::Unit && result != Ty::Nothing && result != Ty::Error {
            let tmp = self.alloc_temp(result);
            self.init_temp(result, tmp, code, cw); // must precede any rec()
            Some(tmp)
        } else {
            None
        };

        let subj = subject.map(|s| {
            let st = self.info.ty(s);
            self.emit_expr(s, code, cw);
            // alloc_temp (not fresh_slot) so the subject slot appears in all arm frames.
            let slot = self.alloc_temp(st);
            self.store(st, slot, code); // initializes the slot before any rec() is called
            (slot, st)
        });

        // Emit the body of one `when` arm. If value-producing, stores to `result_tmp` (stack empty
        // after); otherwise discards. Stack is always empty after this closure.
        let emit_body = |this: &mut Self, body: ExprId, code: &mut CodeBuilder, cw: &mut ClassWriter, result_tmp: Option<u16>| {
            if let Some(tmp) = result_tmp {
                this.emit_expr_as(body, result, code, cw);
                store_local(result, tmp, code);
            } else {
                this.emit_expr(body, code, cw);
                this.discard(this.info.ty(body), code);
            }
        };

        for arm in arms.iter().filter(|a| !a.conditions.is_empty()) {
            let body = code.new_label();
            let next = code.new_label();
            for &cnd in &arm.conditions {
                match (subj, self.file.expr(cnd).clone()) {
                    // `is T` arm: instanceof against the subject slot (don't re-evaluate the subject).
                    (Some((slot, st)), Expr::Is { ty, negated, .. }) => {
                        load_local(st, slot, code);
                        let ci = cw.class_ref(ref_internal(resolve_ty(&ty, self.syms)));
                        code.instance_of(ci);
                        if negated {
                            self.rec(body, code, cw);
                            code.ifeq(body);
                        } else {
                            self.rec(body, code, cw);
                            code.ifne(body);
                        }
                    }
                    (Some((slot, st)), _) => self.emit_eq_jump(slot, st, cnd, body, code, cw),
                    (None, _) => self.emit_cond_jump(cnd, body, true, code, cw),
                }
            }
            self.rec(next, code, cw);
            code.goto(next); // no condition matched → try the next arm
            self.rec(body, code, cw);
            code.bind(body);
            // Locals declared inside a when arm are scoped to that arm. Save and restore
            // self.slots so they don't leak into StackMapTable frames for subsequent arms.
            let saved_slots = self.slots.clone();
            let saved_next_slot = self.next_slot;
            emit_body(self, arm.body, code, cw, result_tmp);
            self.slots = saved_slots;
            self.next_slot = saved_next_slot;
            // Skip the (dead) jump to `end` if the body already diverges (e.g. all arms `return`),
            // which would otherwise leave a branch targeting the method end.
            if !self.expr_diverges(arm.body) {
                self.rec(end, code, cw); // stack: empty (result stored to tmp)
                code.goto(end);
            }
            self.rec(next, code, cw);
            code.bind(next);
        }
        // Falls here when nothing matched: the `else` body (if any) produces the value.
        if let Some(arm) = arms.iter().find(|a| a.conditions.is_empty()) {
            emit_body(self, arm.body, code, cw, result_tmp);
        } else if result != Ty::Unit {
            // An exhaustive `when` used as an expression (the checker proved sealed-exhaustiveness, so
            // there is no `else`): the no-match path is unreachable, but every path must produce a
            // value or diverge — throw, mirroring Kotlin's `NoWhenBranchMatchedException`.
            let exc = cw.class_ref("java/lang/IllegalStateException");
            code.new_obj(exc);
            code.dup();
            let init = cw.methodref("java/lang/IllegalStateException", "<init>", "()V");
            code.invokespecial(init, 0, 0);
            code.athrow();
        }
        self.rec(end, code, cw); // stack: empty
        code.bind(end);
        if let Some(tmp) = result_tmp {
            load_local(result, tmp, code); // push result onto stack
        }
    }

    /// Conservatively: does evaluating `e` always transfer control away (never fall through)?
    /// True for a `return`, or a block whose last statement diverges.
    fn expr_diverges(&self, e: ExprId) -> bool {
        match self.file.expr(e) {
            Expr::Throw { .. } => true,
            Expr::Block { stmts, trailing } => {
                if let Some(te) = trailing {
                    self.expr_diverges(*te)
                } else if let Some(&last) = stmts.last() {
                    matches!(self.file.stmt(last), Stmt::Return(_))
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    /// Emit `if (subject == cond) goto target`, with subject in local `slot` of type `st`.
    fn emit_eq_jump(&mut self, slot: u16, st: Ty, cond: ExprId, target: Label, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        match st {
            Ty::Int | Ty::Byte | Ty::Short | Ty::Boolean | Ty::Char => {
                code.iload(slot);
                self.emit_expr_as(cond, st, code, cw);
                self.rec(target, code, cw);
                code.if_icmpeq(target);
            }
            Ty::Long => {
                code.lload(slot);
                self.emit_expr_as(cond, st, code, cw);
                code.lcmp();
                self.rec(target, code, cw);
                code.ifeq(target);
            }
            Ty::Double => {
                code.dload(slot);
                self.emit_expr_as(cond, st, code, cw);
                code.dcmpg();
                self.rec(target, code, cw);
                code.ifeq(target);
            }
            Ty::Float => {
                code.fload(slot);
                self.emit_expr_as(cond, st, code, cw);
                code.fcmpg();
                self.rec(target, code, cw);
                code.ifeq(target);
            }
            _ => {
                // reference: null-safe `Objects.equals(subject, cond)` (Kotlin's `==`).
                code.aload(slot);
                self.emit_expr(cond, code, cw);
                let eqm = cw.methodref("java/util/Objects", "equals", "(Ljava/lang/Object;Ljava/lang/Object;)Z");
                code.invokestatic(eqm, 2, 1);
                self.rec(target, code, cw);
                code.ifne(target);
            }
        }
    }

    /// Emit `e` as a boolean value (0/1 int on the stack).
    fn emit_bool(&mut self, e: ExprId, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        match self.file.expr(e).clone() {
            Expr::Binary { op, lhs, rhs } if is_cmp(op) || op == BinOp::And || op == BinOp::Or || op == BinOp::RefEq || op == BinOp::RefNe => {
                let l_true = code.new_label();
                let l_end = code.new_label();
                // Use a temp slot so the stack is empty at l_end (required for StackMapTable).
                let tmp = self.alloc_temp(Ty::Int);
                self.init_temp(Ty::Int, tmp, code, cw); // must precede any rec(); verifier needs slot initialized
                self.emit_cond_jump(e, l_true, true, code, cw);
                code.push_int(0, cw);
                code.istore(tmp);
                self.rec(l_end, code, cw); // stack: empty
                code.goto(l_end);
                self.rec(l_true, code, cw);
                code.bind(l_true);
                code.push_int(1, cw);
                code.istore(tmp);
                self.rec(l_end, code, cw);
                code.bind(l_end);
                code.iload(tmp); // push result
                let _ = (op, lhs, rhs);
            }
            Expr::Unary { op: UnOp::Not, operand } => {
                self.emit_bool(operand, code, cw);
                code.push_int(1, cw);
                code.ixor();
            }
            _ => self.emit_expr(e, code, cw), // already a 0/1 boolean value (var/call)
        }
    }

    /// Emit code that jumps to `target` when `cond` is `want` (true/false). Short-circuits `&&`/`||`.
    fn emit_cond_jump(&mut self, cond: ExprId, target: Label, want: bool, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        match self.file.expr(cond).clone() {
            Expr::Binary { op: BinOp::And, lhs, rhs } => {
                if want {
                    // jump if (lhs && rhs): if !lhs skip; if rhs jump
                    let skip = code.new_label();
                    self.emit_cond_jump(lhs, skip, false, code, cw);
                    self.emit_cond_jump(rhs, target, true, code, cw);
                    self.rec(skip, code, cw);
                    code.bind(skip);
                } else {
                    // jump if !(lhs && rhs) = !lhs || !rhs
                    self.emit_cond_jump(lhs, target, false, code, cw);
                    self.emit_cond_jump(rhs, target, false, code, cw);
                }
            }
            Expr::Binary { op: BinOp::Or, lhs, rhs } => {
                if want {
                    self.emit_cond_jump(lhs, target, true, code, cw);
                    self.emit_cond_jump(rhs, target, true, code, cw);
                } else {
                    let skip = code.new_label();
                    self.emit_cond_jump(lhs, skip, true, code, cw);
                    self.emit_cond_jump(rhs, target, false, code, cw);
                    self.rec(skip, code, cw);
                    code.bind(skip);
                }
            }
            Expr::Unary { op: UnOp::Not, operand } => {
                self.emit_cond_jump(operand, target, !want, code, cw);
            }
            Expr::Binary { op, lhs, rhs } if is_cmp(op) => {
                let cmp = if want { op } else { negate_cmp(op) };
                self.emit_compare_jump(cmp, lhs, rhs, target, code, cw);
            }
            Expr::Binary { op: op @ (BinOp::RefEq | BinOp::RefNe), lhs, rhs } => {
                // Referential equality: `===` → if_acmpeq, `!==` → if_acmpne.
                // Null literal on either side → ifnull / ifnonnull.
                let lt = self.info.ty(lhs);
                let rt = self.info.ty(rhs);
                let jump_if_eq = (op == BinOp::RefEq) == want;
                if lt == Ty::Null || rt == Ty::Null {
                    let val = if lt == Ty::Null { rhs } else { lhs };
                    self.emit_expr(val, code, cw);
                    self.rec(target, code, cw);
                    if jump_if_eq { code.ifnull(target); } else { code.ifnonnull(target); }
                } else {
                    self.emit_expr(lhs, code, cw);
                    self.emit_expr(rhs, code, cw);
                    self.rec(target, code, cw);
                    if jump_if_eq { code.if_acmpeq(target); } else { code.if_acmpne(target); }
                }
            }
            _ => {
                // arbitrary boolean value: compare against 0
                self.emit_expr(cond, code, cw);
                if want {
                    self.rec(target, code, cw);
                    code.ifne(target);
                } else {
                    self.rec(target, code, cw);
                    code.ifeq(target);
                }
            }
        }
    }

    fn emit_compare_jump(&mut self, op: BinOp, lhs: ExprId, rhs: ExprId, target: Label, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        let lt = self.info.ty(lhs);
        let rt = self.info.ty(rhs);
        // `x == null` / `x != null` → ifnull / ifnonnull on the non-null-literal operand.
        if lt == Ty::Null || rt == Ty::Null {
            let val = if lt == Ty::Null { rhs } else { lhs };
            self.emit_expr(val, code, cw);
            self.rec(target, code, cw);
            match op {
                BinOp::Eq => code.ifnull(target),
                BinOp::Ne => code.ifnonnull(target),
                _ => {}
            }
            return;
        }
        let common = Ty::promote(lt, rt).unwrap_or(lt);
        self.emit_expr_as(lhs, common, code, cw);
        self.emit_expr_as(rhs, common, code, cw);
        self.rec(target, code, cw);
        match common {
            Ty::Int | Ty::Byte | Ty::Short | Ty::Boolean | Ty::Char => match op {
                BinOp::Lt => code.if_icmplt(target),
                BinOp::Le => code.if_icmple(target),
                BinOp::Gt => code.if_icmpgt(target),
                BinOp::Ge => code.if_icmpge(target),
                BinOp::Eq => code.if_icmpeq(target),
                BinOp::Ne => code.if_icmpne(target),
                _ => {}
            },
            Ty::Long => {
                code.lcmp();
                self.cmp0(op, target, code);
            }
            Ty::Double => {
                code.dcmpg();
                self.cmp0(op, target, code);
            }
            Ty::Float => {
                code.fcmpg();
                self.cmp0(op, target, code);
            }
            // Reference equality (`==`/`!=`) via null-safe `Objects.equals` (Kotlin structural,
            // null-safe equality — a plain `a.equals(b)` would NPE when `a` is null).
            Ty::String | Ty::Obj(_) | Ty::Array(_) => {
                let eqm = cw.methodref("java/util/Objects", "equals", "(Ljava/lang/Object;Ljava/lang/Object;)Z");
                code.invokestatic(eqm, 2, 1);
                match op {
                    BinOp::Eq => code.ifne(target), // areEqual==true ⇒ jump
                    BinOp::Ne => code.ifeq(target), // areEqual==false ⇒ jump
                    _ => self.diags.error(self.file.expr_spans[lhs.0 as usize], "krusty: only == / != on reference types"),
                }
            }
            _ => {
                self.diags.error(self.file.expr_spans[lhs.0 as usize], "krusty v0: unsupported comparison operand type");
            }
        }
    }

    fn cmp0(&mut self, op: BinOp, target: Label, code: &mut CodeBuilder) {
        // NOTE: rec() cannot be called here (no cw), but cmp0 is only reached from
        // emit_compare_jump which calls self.rec(target, code, cw) before the dispatch.
        match op {
            BinOp::Lt => code.iflt(target),
            BinOp::Le => code.ifle(target),
            BinOp::Gt => code.ifgt(target),
            BinOp::Ge => code.ifge(target),
            BinOp::Eq => code.ifeq(target),
            BinOp::Ne => code.ifne(target),
            _ => {}
        }
    }

    fn emit_arith_expr(&mut self, e: ExprId, op: BinOp, lhs: ExprId, rhs: ExprId, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        let result = self.info.ty(e);
        if op == BinOp::Add && result == Ty::String {
            self.emit_concat(lhs, rhs, code, cw);
            return;
        }
        self.emit_expr_as(lhs, result, code, cw);
        self.emit_expr_as(rhs, result, code, cw);
        self.emit_arith(op, result, code);
    }

    fn emit_arith(&mut self, op: BinOp, t: Ty, code: &mut CodeBuilder) {
        match (op, t) {
            (BinOp::Add, Ty::Int) => code.iadd(),
            (BinOp::Sub, Ty::Int) => code.isub(),
            (BinOp::Mul, Ty::Int) => code.imul(),
            (BinOp::Div, Ty::Int) => code.idiv(),
            (BinOp::Rem, Ty::Int) => code.irem(),
            (BinOp::Add, Ty::Long) => code.ladd(),
            (BinOp::Sub, Ty::Long) => code.lsub(),
            (BinOp::Mul, Ty::Long) => code.lmul(),
            (BinOp::Div, Ty::Long) => code.ldiv(),
            (BinOp::Rem, Ty::Long) => code.lrem(),
            (BinOp::Add, Ty::Double) => code.dadd(),
            (BinOp::Sub, Ty::Double) => code.dsub(),
            (BinOp::Mul, Ty::Double) => code.dmul(),
            (BinOp::Div, Ty::Double) => code.ddiv(),
            (BinOp::Rem, Ty::Double) => code.drem(),
            (BinOp::Add, Ty::Float) => code.fadd(),
            (BinOp::Sub, Ty::Float) => code.fsub(),
            (BinOp::Mul, Ty::Float) => code.fmul(),
            (BinOp::Div, Ty::Float) => code.fdiv(),
            (BinOp::Rem, Ty::Float) => code.frem(),
            _ => {}
        }
    }

    fn emit_concat(&mut self, lhs: ExprId, rhs: ExprId, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        let sb = cw.class_ref("java/lang/StringBuilder");
        let ctor = cw.methodref("java/lang/StringBuilder", "<init>", "()V");
        code.new_obj(sb);
        code.dup();
        code.invokespecial(ctor, 0, 0);
        self.emit_append(lhs, code, cw);
        self.emit_append(rhs, code, cw);
        let to_s = cw.methodref("java/lang/StringBuilder", "toString", "()Ljava/lang/String;");
        code.invokevirtual(to_s, 0, 1);
    }

    /// Returns true when `emit_bool(e)` emits branch instructions that register StackMapTable
    /// frames requiring an empty operand stack.
    fn bool_uses_branching(&self, e: ExprId) -> bool {
        match self.file.expr(e) {
            Expr::Binary { op, .. } => is_cmp(*op) || *op == BinOp::And || *op == BinOp::Or || *op == BinOp::RefEq || *op == BinOp::RefNe,
            Expr::Unary { op: UnOp::Not, operand } => self.bool_uses_branching(*operand),
            _ => false,
        }
    }

    fn emit_append(&mut self, e: ExprId, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        let t = self.info.ty(e);
        if t == Ty::Boolean && self.bool_uses_branching(e) {
            // Branching boolean evaluation registers StackMapTable frames that require an empty
            // operand stack. Save the StringBuilder so those inner frames see an empty stack,
            // then rebuild {SB, bool} for the append call.
            // alloc_temp (not fresh_slot) so that frames inside emit_bool correctly type sb_tmp.
            let sb_tmp = self.alloc_temp(Ty::obj("java/lang/StringBuilder"));
            code.astore(sb_tmp); // astore initializes sb_tmp; no init_temp needed before rec()
            self.emit_expr(e, code, cw); // bool on stack; inner frames see empty stack ✓
            // After emit_bool, the bool is on the stack. Save it and rebuild {SB, bool}.
            // fresh_slot: no rec() calls follow, so the slot needn't appear in self.slots.
            let bool_raw = self.fresh_slot(Ty::Int);
            code.istore(bool_raw);
            code.aload(sb_tmp);
            code.iload(bool_raw);
        } else {
            self.emit_expr(e, code, cw);
        }
        let (desc, words) = match t {
            Ty::Int | Ty::Byte | Ty::Short => ("(I)Ljava/lang/StringBuilder;", 1),
            Ty::Boolean => ("(Z)Ljava/lang/StringBuilder;", 1),
            Ty::Char => ("(C)Ljava/lang/StringBuilder;", 1),
            Ty::Long => ("(J)Ljava/lang/StringBuilder;", 2),
            Ty::Float => ("(F)Ljava/lang/StringBuilder;", 1),
            Ty::Double => ("(D)Ljava/lang/StringBuilder;", 2),
            Ty::String => ("(Ljava/lang/String;)Ljava/lang/StringBuilder;", 1),
            _ => ("(Ljava/lang/Object;)Ljava/lang/StringBuilder;", 1),
        };
        let append = cw.methodref("java/lang/StringBuilder", "append", desc);
        code.invokevirtual(append, words, 1);
    }

    /// Inline a `run`/`with`/`apply` body with `recv_expr` as the implicit receiver: store the
    /// receiver, set the receiver context, emit the body. `is_apply` yields the receiver, else the body.
    fn emit_with_receiver(&mut self, e: ExprId, recv_expr: ExprId, body: ExprId, is_apply: bool, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        let rt = self.info.ty(recv_expr);
        let internal = match rt.obj_internal() {
            Some(i) => i.to_string(),
            None => return, // checker already rejected a non-class receiver
        };
        self.emit_expr(recv_expr, code, cw);
        let slot = self.fresh_slot(rt);
        code.ensure_locals(slot + slot_words(rt));
        self.store(rt, slot, code);
        // Enter the receiver context: implicit `this`/members target the stored receiver's class.
        let prev_recv = self.recv.take();
        let prev_props = std::mem::take(&mut self.class_props);
        self.recv = Some((slot, internal.clone()));
        self.class_props = self
            .syms
            .class_by_internal(&internal)
            .map(|c| c.props.iter().map(|(n, t, _)| (n.clone(), *t)).collect())
            .unwrap_or_default();
        let result = self.info.ty(e);
        if is_apply {
            self.emit_expr(body, code, cw);
            self.discard(self.info.ty(body), code);
        } else if result == Ty::Unit {
            self.emit_expr(body, code, cw);
            self.discard(self.info.ty(body), code);
        } else {
            self.emit_expr_as(body, result, code, cw);
        }
        // Restore the enclosing context, then (for `apply`) leave the receiver as the result.
        self.recv = prev_recv;
        self.class_props = prev_props;
        if is_apply && result != Ty::Unit {
            load_local(rt, slot, code);
        }
    }

    /// Emit a lambda literal `{ [param ->] body }` as an anonymous class implementing
    /// `kotlin/jvm/functions/FunctionN`. The anonymous class is registered via the thread-local
    /// `LAMBDA_CLASSES` and the call site pushes a fresh instance: `new Anon; dup; invokespecial`.
    fn emit_lambda(&mut self, e: ExprId, param: Option<&str>, body: ExprId, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        let n = self.lambda_counter;
        self.lambda_counter += 1;
        // Infer arity: explicit param → 1; no param but body uses `it` → 1 (implicit param); else 0.
        let (arity, bind_name): (u8, Option<&str>) = if let Some(p) = param {
            (1, Some(p))
        } else if crate::resolve::expr_uses_name_pub(self.file, body, "it") {
            (1, Some("it"))
        } else {
            (0, None)
        };
        let iface = Ty::fun_interface(arity);
        let anon_name = format!("{}$lambda${n}", self.class);

        // Build the anonymous class implementing FunctionN.
        let mut acw = ClassWriter::new(&anon_name, "java/lang/Object");
        acw.add_interface(&iface);

        // <init>()V
        {
            let mut init_code = CodeBuilder::new(1);
            init_code.aload(0);
            let obj_init = acw.methodref("java/lang/Object", "<init>", "()V");
            init_code.invokespecial(obj_init, 0, 0);
            init_code.ret_void();
            init_code.link();
            acw.add_method(ACC_PUBLIC, "<init>", "()V", &init_code);
        }

        // invoke([Object...])Object — emit the lambda body.
        {
            // Build invoke descriptor: (Object*)Object
            let invoke_desc_str: String = {
                let mut d = String::from("(");
                for _ in 0..arity {
                    d.push_str("Ljava/lang/Object;");
                }
                d.push_str(")Ljava/lang/Object;");
                d
            };
            let body_locals = 1 + arity as u16; // slot 0 = this, slots 1..arity = params
            let mut invoke_code = CodeBuilder::new(body_locals + 16); // extra headroom

            // Build a scratch MethodEmitter to emit the lambda body.
            // We need to bind the lambda parameter (if any) to the Object-typed slot.
            let mut le = MethodEmitter::new(self.file, self.info, self.syms, &self.class.clone(), self.imports, self.diags);
            le.lambda_counter = self.lambda_counter;
            le.ret_ty = Ty::obj("java/lang/Object");
            le.is_instance = true; // slot 0 = this (the anonymous object)
            le.local_fun_emitted_class = self.local_fun_emitted_class.clone();
            le.inside_lambda = true;
            le.file_facade = self.file_facade.clone();
            le.next_slot = 1; // skip `this`
            if let Some(p) = bind_name {
                le.slots.insert(p.to_string(), (1, Ty::obj("java/lang/Object")));
                le.next_slot = 1 + arity as u16;
            }

            // Emit the body expression; box the result if it's a primitive.
            let body_ty = self.info.ty(body);
            le.emit_expr(body, &mut invoke_code, &mut acw);
            emit_box(body_ty, &mut invoke_code, &mut acw);
            if body_ty == Ty::Unit {
                // Unit body: push null as the Object result.
                invoke_code.aconst_null();
            }
            invoke_code.areturn();
            invoke_code.ensure_locals(le.next_slot.max(body_locals));
            invoke_code.link();
            acw.add_method(ACC_PUBLIC, "invoke", &invoke_desc_str, &invoke_code);

            // Propagate sub-lambda counter and local fun class tracking.
            self.lambda_counter = le.lambda_counter;
            self.local_fun_emitted_class.extend(le.local_fun_emitted_class);
        }

        let anon_bytes = acw.finish();
        // Ensure the FunctionN interface stub class is in the output so the program is
        // self-contained without kotlin-stdlib on the runtime classpath.
        ensure_function_stub(arity);
        push_lambda_class(anon_name.clone(), anon_bytes);

        // At the call site: `new AnonClass; dup; invokespecial <init>()V`.
        // Mark the enclosing method as needing a StackMapTable (lambda `new` + branches triggers
        // the Java 25 type-checking verifier to require explicit frame entries).
        let ci = cw.class_ref(&anon_name);
        code.set_needs_stackmap();
        code.new_obj(ci);
        code.dup();
        let init = cw.methodref(&anon_name, "<init>", "()V");
        code.invokespecial(init, 0, 0);

        // The type of this expression is Fun(arity) — set it so that callers can use the value.
        let _ = e;
    }

    fn emit_call(&mut self, e: ExprId, callee: ExprId, args: &[ExprId], code: &mut CodeBuilder, cw: &mut ClassWriter) {
        // IIFE: `{ [p ->] body }([arg])` — inline the lambda body so it can read/write outer locals.
        if let Expr::Lambda { param, body } = self.file.expr(callee).clone() {
            if let Some(pname) = param {
                if let Some(&arg) = args.first() {
                    let arg_ty = self.info.ty(arg);
                    self.emit_expr(arg, code, cw);
                    let slot = self.fresh_slot(arg_ty);
                    code.ensure_locals(slot + slot_words(arg_ty));
                    self.store(arg_ty, slot, code);
                    let prev = self.slots.insert(pname.clone(), (slot, arg_ty));
                    self.emit_expr(body, code, cw);
                    match prev {
                        Some(p) => { self.slots.insert(pname, p); }
                        None => { self.slots.remove(&pname); }
                    }
                }
            } else {
                self.emit_expr(body, code, cw);
            }
            return;
        }
        // Inlined scope functions: `recv.let { … }` / `recv.also { … }`.
        if let Expr::Member { receiver, name } = self.file.expr(callee).clone() {
            if matches!(name.as_str(), "let" | "also") && args.len() == 1 {
                if let Expr::Lambda { param, body } = self.file.expr(args[0]).clone() {
                    let rt = self.info.ty(receiver);
                    self.emit_expr(receiver, code, cw);
                    let slot = self.fresh_slot(rt);
                    code.ensure_locals(slot + slot_words(rt));
                    self.store(rt, slot, code);
                    let pname = param.unwrap_or_else(|| "it".to_string());
                    let prev = self.slots.insert(pname.clone(), (slot, rt));
                    let result = self.info.ty(e);
                    if name == "let" {
                        if result == Ty::Unit {
                            self.emit_expr(body, code, cw);
                            self.discard(self.info.ty(body), code);
                        } else {
                            self.emit_expr_as(body, result, code, cw);
                        }
                    } else {
                        // `also`: run the body for effect, then the receiver is the result.
                        self.emit_expr(body, code, cw);
                        self.discard(self.info.ty(body), code);
                        if result != Ty::Unit {
                            load_local(rt, slot, code);
                        }
                    }
                    match prev {
                        Some(p) => {
                            self.slots.insert(pname, p);
                        }
                        None => {
                            self.slots.remove(&pname);
                        }
                    }
                    return;
                }
            }
            // `recv.run { … }` / `recv.apply { … }` — receiver becomes the body's implicit `this`.
            if matches!(name.as_str(), "run" | "apply") && args.len() == 1 {
                if let Expr::Lambda { param: None, body } = self.file.expr(args[0]).clone() {
                    self.emit_with_receiver(e, receiver, body, name == "apply", code, cw);
                    return;
                }
            }
        }
        // `with(x) { … }` — `x` becomes the body's implicit `this`.
        if let Expr::Name(fname) = self.file.expr(callee).clone() {
            if fname == "with" && args.len() == 2 && !self.syms.funs.contains_key(&fname) {
                if let Expr::Lambda { param: None, body } = self.file.expr(args[1]).clone() {
                    self.emit_with_receiver(e, args[0], body, false, code, cw);
                    return;
                }
            }
        }
        // `super.method(args)` → aload 0; args; invokespecial Super.method (non-virtual dispatch).
        if let Expr::Member { receiver, name } = self.file.expr(callee).clone() {
            if matches!(self.file.expr(receiver), Expr::Name(r) if r == "super") {
                if self.inside_lambda {
                    self.diags.error(self.file.expr_spans[e.0 as usize],
                        "krusty: super call inside lambda not supported".to_string());
                    return;
                }
                let sup = self
                    .syms
                    .class_by_internal(&self.class.clone())
                    .and_then(|c| c.super_internal.clone())
                    .unwrap_or_else(|| "java/lang/Object".to_string());
                let sig = self.syms.method_of(&sup, &name);
                code.aload(0);
                if let Some(sig) = sig {
                    for (a, pty) in args.iter().zip(&sig.params) {
                        self.emit_expr_as(*a, *pty, code, cw);
                    }
                    let arg_words: i32 = sig.params.iter().map(|t| slot_words(*t) as i32).sum();
                    let m = cw.methodref(&sup, &name, &method_descriptor(&sig.params, sig.ret));
                    code.invokespecial(m, arg_words, slot_words(sig.ret) as i32);
                }
                return;
            }
        }
        // Companion (static) method call: `ClassName.fn(args)` → args; invokestatic.
        if let Expr::Member { receiver, name } = self.file.expr(callee).clone() {
            if let Expr::Name(cls) = self.file.expr(receiver).clone() {
                if !self.slots.contains_key(&cls) {
                    if let Some(sig) = self.syms.classes.get(&cls).and_then(|c| c.static_methods.get(&name)).cloned() {
                        let internal = self.syms.classes[&cls].internal.clone();
                        for (a, pty) in args.iter().zip(&sig.params) {
                            self.emit_expr_as(*a, *pty, code, cw);
                        }
                        let arg_words: i32 = sig.params.iter().map(|t| slot_words(*t) as i32).sum();
                        let m = cw.methodref(&internal, &name, &method_descriptor(&sig.params, sig.ret));
                        code.invokestatic(m, arg_words, slot_words(sig.ret) as i32);
                        return;
                    }
                }
            }
        }
        // Object member call: `Object.method(args)` → getstatic INSTANCE; args; invokevirtual.
        if let Expr::Member { receiver, name } = self.file.expr(callee).clone() {
            if let Expr::Name(cls) = self.file.expr(receiver).clone() {
                if !self.slots.contains_key(&cls) && self.syms.objects.contains(&cls) {
                    if let Some(sig) = self.syms.classes.get(&cls).and_then(|c| c.methods.get(&name)).cloned() {
                        let internal = self.syms.classes[&cls].internal.clone();
                        let self_desc = Ty::obj(&internal).descriptor();
                        let inst = cw.fieldref(&internal, "INSTANCE", &self_desc);
                        code.getstatic(inst, 1);
                        for (a, pty) in args.iter().zip(&sig.params) {
                            self.emit_expr_as(*a, *pty, code, cw);
                        }
                        let arg_words: i32 = sig.params.iter().map(|t| slot_words(*t) as i32).sum();
                        let m = cw.methodref(&internal, &name, &method_descriptor(&sig.params, sig.ret));
                        code.invokevirtual(m, arg_words, slot_words(sig.ret) as i32);
                        return;
                    }
                }
            }
        }
        // Java static call: ClassName.method(args) resolved via the classpath.
        if let Expr::Member { receiver, name } = self.file.expr(callee).clone() {
            if let Expr::Name(cls) = self.file.expr(receiver).clone() {
                if !self.slots.contains_key(&cls) {
                    if let Some(internal) = self.imports.get(&cls).cloned() {
                        let arg_tys: Vec<Ty> = args.iter().map(|a| self.info.ty(*a)).collect();
                        if let Some((owner, desc, ret)) = resolve_java_static(&self.syms.classpath, &internal, &name, &arg_tys) {
                            for a in args {
                                self.emit_expr(*a, code, cw);
                            }
                            let arg_words: i32 = arg_tys.iter().map(|t| slot_words(*t) as i32).sum();
                            let m = cw.methodref(&owner, &name, &desc);
                            code.invokestatic(m, arg_words, slot_words(ret) as i32);
                            return;
                        }
                    }
                }
            }
        }
        match self.file.expr(callee).clone() {
            Expr::Member { receiver, name } if name == "toString" && args.is_empty() => {
                let rt = self.info.ty(receiver);
                self.emit_expr(receiver, code, cw);
                let (desc, words) = match rt {
                    Ty::String => return, // identity
                    Ty::Int | Ty::Byte | Ty::Short => ("(I)Ljava/lang/String;", 1),
                    Ty::Boolean => ("(Z)Ljava/lang/String;", 1),
                    Ty::Char => ("(C)Ljava/lang/String;", 1),
                    Ty::Long => ("(J)Ljava/lang/String;", 2),
                    Ty::Float => ("(F)Ljava/lang/String;", 1),
                    Ty::Double => ("(D)Ljava/lang/String;", 2),
                    // reference type: virtual call to the object's real toString().
                    Ty::Obj(_) | Ty::Null => {
                        let m = cw.methodref("java/lang/Object", "toString", "()Ljava/lang/String;");
                        code.invokevirtual(m, 0, 1);
                        return;
                    }
                    _ => return,
                };
                let m = cw.methodref("java/lang/String", "valueOf", desc);
                code.invokestatic(m, words, 1);
            }
            // Numeric conversion intrinsic: `n.toInt()`/`toLong()`/`toFloat()`/`toDouble()`.
            Expr::Member { receiver, name }
                if args.is_empty()
                    && self.info.ty(receiver).is_numeric()
                    && crate::resolve::conversion_target(&name).is_some() =>
            {
                let from = self.info.ty(receiver);
                let to = crate::resolve::conversion_target(&name).unwrap();
                self.emit_expr(receiver, code, cw);
                self.emit_numeric_conversion(from, to, code);
            }
            // java.lang.String instance method: recv.method(args) -> invokevirtual
            Expr::Member { receiver, name }
                if self.info.ty(receiver) == Ty::String
                    && crate::resolve::resolve_string_instance(&name, &args.iter().map(|a| self.info.ty(*a)).collect::<Vec<_>>()).is_some() =>
            {
                let arg_tys: Vec<Ty> = args.iter().map(|a| self.info.ty(*a)).collect();
                let (desc, ret) = crate::resolve::resolve_string_instance(&name, &arg_tys).unwrap();
                self.emit_expr(receiver, code, cw);
                for a in args {
                    self.emit_expr(*a, code, cw);
                }
                let arg_words: i32 = arg_tys.iter().map(|t| slot_words(*t) as i32).sum();
                let jvm_name = crate::resolve::string_kotlin_to_jvm(&name);
                let m = cw.methodref("java/lang/String", jvm_name, desc);
                code.invokevirtual(m, arg_words, slot_words(ret) as i32);
            }
            // `"literal".trimIndent()` / `.trimMargin()` — folded at compile time (the receiver must
            // be a string literal, since krusty can't call the kotlin-stdlib extension).
            Expr::Member { receiver, name }
                if matches!(name.as_str(), "trimIndent" | "trimMargin")
                    && args.is_empty()
                    && self.info.ty(receiver) == Ty::String =>
            {
                if let Expr::StringLit(s) = self.file.expr(receiver).clone() {
                    let folded = if name == "trimIndent" { trim_indent(&s) } else { trim_margin(&s, "|") };
                    code.push_string(&folded, cw);
                } else {
                    self.diags.error(self.file.expr_spans[e.0 as usize], format!("krusty: '{name}' is only supported on a string literal"));
                }
            }
            // Curated java.lang.StringBuilder instance method (append/toString/length).
            Expr::Member { receiver, name }
                if self.info.ty(receiver) == Ty::obj("java/lang/StringBuilder")
                    && crate::resolve::resolve_stringbuilder_instance(&name, &args.iter().map(|a| self.info.ty(*a)).collect::<Vec<_>>()).is_some() =>
            {
                let arg_tys: Vec<Ty> = args.iter().map(|a| self.info.ty(*a)).collect();
                let (desc, ret) = crate::resolve::resolve_stringbuilder_instance(&name, &arg_tys).unwrap();
                self.emit_expr(receiver, code, cw);
                for a in args {
                    self.emit_expr(*a, code, cw);
                }
                let arg_words: i32 = arg_tys.iter().map(|t| slot_words(*t) as i32).sum();
                let m = cw.methodref("java/lang/StringBuilder", &name, &desc);
                code.invokevirtual(m, arg_words, slot_words(ret) as i32);
            }
            // Instance method call on a class value: `p.method(args)` (own or inherited).
            Expr::Member { receiver, name }
                if matches!(self.info.ty(receiver), Ty::Obj(_))
                    && self.syms.method_of(self.info.ty(receiver).obj_internal().unwrap(), &name).is_some() =>
            {
                let internal = self.info.ty(receiver).obj_internal().unwrap();
                let sig = self.syms.method_of(internal, &name).unwrap();
                let is_iface = self.syms.class_by_internal(internal).map_or(false, |c| c.is_interface);
                self.emit_expr(receiver, code, cw);
                for (a, pty) in args.iter().zip(&sig.params) {
                    self.emit_expr_as(*a, *pty, code, cw);
                }
                let arg_words: i32 = sig.params.iter().map(|t| slot_words(*t) as i32).sum();
                let desc = method_descriptor(&sig.params, sig.ret);
                // Dispatch through an interface uses `invokeinterface`; a class uses `invokevirtual`.
                if is_iface {
                    let m = cw.interface_methodref(internal, &name, &desc);
                    code.invokeinterface(m, arg_words, slot_words(sig.ret) as i32);
                } else {
                    let m = cw.methodref(internal, &name, &desc);
                    code.invokevirtual(m, arg_words, slot_words(sig.ret) as i32);
                }
            }
            // Instance method on a classpath Java object → invokevirtual via the `.class` reader.
            Expr::Member { receiver, name }
                if matches!(self.info.ty(receiver), Ty::Obj(_))
                    && crate::resolve::resolve_java_instance(&self.syms.classpath, self.info.ty(receiver).obj_internal().unwrap(), &name, &args.iter().map(|a| self.info.ty(*a)).collect::<Vec<_>>()).is_some() =>
            {
                let internal = self.info.ty(receiver).obj_internal().unwrap();
                let arg_tys: Vec<Ty> = args.iter().map(|a| self.info.ty(*a)).collect();
                let (desc, ret) = crate::resolve::resolve_java_instance(&self.syms.classpath, internal, &name, &arg_tys).unwrap();
                self.emit_expr(receiver, code, cw);
                for a in args {
                    self.emit_expr(*a, code, cw);
                }
                let arg_words: i32 = arg_tys.iter().map(|t| slot_words(*t) as i32).sum();
                let m = cw.methodref(internal, &name, &desc);
                code.invokevirtual(m, arg_words, slot_words(ret) as i32);
            }
            // Extension / static method from classpath (e.g. Kotlin stdlib StringsKt.uppercase).
            // Receiver is pushed first, then the rest of the args → invokestatic.
            Expr::Member { receiver, .. }
                if self.info.ext_calls.contains_key(&e) =>
            {
                let (owner, jvm_name, desc) = self.info.ext_calls[&e].clone();
                let recv_ty = self.info.ty(receiver);
                let arg_tys: Vec<Ty> = args.iter().map(|a| self.info.ty(*a)).collect();
                let ret_desc = desc[desc.find(')').unwrap() + 1..].to_string();
                let ret = crate::resolve::desc_to_ty(&ret_desc);
                self.emit_expr(receiver, code, cw);
                for &a in args {
                    self.emit_expr(a, code, cw);
                }
                let recv_words = slot_words(recv_ty) as i32;
                let arg_words: i32 = arg_tys.iter().map(|t| slot_words(*t) as i32).sum();
                let m = cw.methodref(&owner, &jvm_name, &desc);
                code.invokestatic(m, recv_words + arg_words, slot_words(ret) as i32);
            }
            // Precondition intrinsics (`require`/`check`/`assert(cond)`, `error(msg)`, `TODO()`).
            Expr::Name(fname)
                if !self.syms.funs.contains_key(&fname)
                    && matches!(fname.as_str(), "require" | "check" | "assert" | "error" | "TODO" | "assertEquals" | "assertTrue" | "assertFalse") =>
            {
                let throw = |code: &mut CodeBuilder, cw: &mut ClassWriter, exc: &str, msg: Option<&ExprId>, this: &mut Self| {
                    let c = cw.class_ref(exc);
                    code.new_obj(c);
                    code.dup();
                    if let Some(m) = msg {
                        this.emit_string_of(*m, code, cw);
                        let init = cw.methodref(exc, "<init>", "(Ljava/lang/String;)V");
                        code.invokespecial(init, 1, 0);
                    } else {
                        let init = cw.methodref(exc, "<init>", "()V");
                        code.invokespecial(init, 0, 0);
                    }
                    code.athrow();
                };
                match fname.as_str() {
                    "require" | "check" | "assert" => {
                        let exc = if fname == "require" { "java/lang/IllegalArgumentException" } else if fname == "check" { "java/lang/IllegalStateException" } else { "java/lang/AssertionError" };
                        self.emit_expr_as(args[0], Ty::Boolean, code, cw);
                        let ok = code.new_label();
                        self.rec(ok, code, cw);
                        code.ifne(ok); // condition true → continue
                        throw(code, cw, exc, None, self);
                        self.rec(ok, code, cw);
                        code.bind(ok);
                    }
                    "error" => throw(code, cw, "java/lang/IllegalStateException", Some(&args[0]), self),
                    "TODO" => throw(code, cw, "java/lang/RuntimeException", args.first(), self),
                    // assertEquals(expected, actual[, msg]) — pass when `expected == actual` (Kotlin
                    // structural equality, reused from the `==` comparison emission).
                    "assertEquals" => {
                        let ok = code.new_label();
                        self.emit_compare_jump(BinOp::Eq, args[0], args[1], ok, code, cw);
                        throw(code, cw, "java/lang/AssertionError", args.get(2), self);
                        self.rec(ok, code, cw);
                        code.bind(ok);
                    }
                    // assertTrue(cond[, msg]) / assertFalse(cond[, msg]).
                    "assertTrue" | "assertFalse" => {
                        self.emit_expr_as(args[0], Ty::Boolean, code, cw);
                        let ok = code.new_label();
                        if fname == "assertTrue" {
                            self.rec(ok, code, cw);
                            code.ifne(ok); // true → pass
                        } else {
                            self.rec(ok, code, cw);
                            code.ifeq(ok); // false → pass
                        }
                        throw(code, cw, "java/lang/AssertionError", args.get(1), self);
                        self.rec(ok, code, cw);
                        code.bind(ok);
                    }
                    _ => unreachable!(),
                }
                return;
            }
            Expr::Name(fname) if fname == "println" => {
                let out = cw.fieldref("java/lang/System", "out", "Ljava/io/PrintStream;");
                code.getstatic(out, 1);
                let at = args.first().map(|a| self.info.ty(*a)).unwrap_or(Ty::Unit);
                if let Some(a) = args.first() {
                    self.emit_expr(*a, code, cw);
                }
                let (desc, words) = match at {
                    Ty::Int | Ty::Byte | Ty::Short | Ty::Boolean => ("(I)V", 1),
                    Ty::Char => ("(C)V", 1),
                    Ty::Long => ("(J)V", 2),
                    Ty::Float => ("(F)V", 1),
                    Ty::Double => ("(D)V", 2),
                    Ty::String => ("(Ljava/lang/String;)V", 1),
                    _ => ("()V", 0),
                };
                let m = cw.methodref("java/io/PrintStream", "println", desc);
                code.invokevirtual(m, words, 0);
            }
            // Constructor call: `ClassName(args)` → new + dup + invokespecial <init>.
            Expr::Name(fname) if !self.slots.contains_key(&fname) && self.syms.classes.contains_key(&fname) => {
                let cls = self.syms.classes.get(&fname).unwrap();
                let internal = cls.internal.clone();
                let ctor_tys: Vec<Ty> = cls.ctor_params.clone();
                let class_idx = cw.class_ref(&internal);
                code.new_obj(class_idx);
                code.dup();
                for (a, pty) in args.iter().zip(&ctor_tys) {
                    self.emit_expr_as(*a, *pty, code, cw);
                }
                let arg_words: i32 = ctor_tys.iter().map(|t| slot_words(*t) as i32).sum();
                let desc = method_descriptor(&ctor_tys, Ty::Unit);
                let m = cw.methodref(&internal, "<init>", &desc);
                code.invokespecial(m, arg_words, 0);
            }
            // Constructing a classpath Java type: `Calc()` → new + dup + args + invokespecial <init>.
            Expr::Name(fname)
                if !self.slots.contains_key(&fname)
                    && self.imports.get(&fname).and_then(|internal| crate::resolve::resolve_java_ctor(&self.syms.classpath, internal, &args.iter().map(|a| self.info.ty(*a)).collect::<Vec<_>>())).is_some() =>
            {
                let internal = self.imports.get(&fname).unwrap().clone();
                let arg_tys: Vec<Ty> = args.iter().map(|a| self.info.ty(*a)).collect();
                let desc = crate::resolve::resolve_java_ctor(&self.syms.classpath, &internal, &arg_tys).unwrap();
                let class_idx = cw.class_ref(&internal);
                code.new_obj(class_idx);
                code.dup();
                for a in args {
                    self.emit_expr(*a, code, cw);
                }
                let arg_words: i32 = arg_tys.iter().map(|t| slot_words(*t) as i32).sum();
                let m = cw.methodref(&internal, "<init>", &desc);
                code.invokespecial(m, arg_words, 0);
            }
            // Array-creation builtins: `intArrayOf(…)`/`arrayOf(…)`/`IntArray(n)`/… (the result type
            // recorded by the checker is the array `Ty`, so element/word sizing follows from it).
            Expr::Name(fname)
                if !self.slots.contains_key(&fname)
                    && matches!(self.info.ty(e), Ty::Array(_))
                    && is_array_builtin(&fname) =>
            {
                let arr = self.info.ty(e);
                let elem = arr.array_elem().unwrap_or(Ty::Error);
                if fname.ends_with("Of") {
                    // `*ArrayOf(a, b, …)` — allocate `args.len()` and store each element.
                    code.push_int(args.len() as i32, cw);
                    self.emit_new_array(elem, code, cw);
                    let (sop, swords) = array_store_op(elem);
                    for (i, a) in args.iter().enumerate() {
                        code.dup();
                        code.push_int(i as i32, cw);
                        self.emit_expr_as(*a, elem, code, cw);
                        code.array_store(sop, swords);
                    }
                } else {
                    // Size constructor `IntArray(n)` — allocate a zero-filled array of length `n`.
                    self.emit_expr_as(args[0], Ty::Int, code, cw);
                    self.emit_new_array(elem, code, cw);
                }
            }
            // `StringBuilder()` / `StringBuilder("init")` / `StringBuilder(capacity)`.
            Expr::Name(fname)
                if fname == "StringBuilder" && !self.slots.contains_key(&fname) && !self.syms.funs.contains_key(&fname) =>
            {
                let sb = cw.class_ref("java/lang/StringBuilder");
                code.new_obj(sb);
                code.dup();
                let desc = match args.first().map(|a| self.info.ty(*a)) {
                    Some(Ty::String) => "(Ljava/lang/String;)V",
                    Some(_) => "(I)V",
                    None => "()V",
                };
                let mut aw = 0;
                if let Some(a) = args.first() {
                    self.emit_expr(*a, code, cw);
                    aw = 1;
                }
                let m = cw.methodref("java/lang/StringBuilder", "<init>", desc);
                code.invokespecial(m, aw, 0);
            }
            // A common JDK exception by simple name (`RuntimeException("msg")`): new + dup + (msg) +
            // invokespecial <init>()V or <init>(String)V.
            Expr::Name(fname)
                if !self.slots.contains_key(&fname)
                    && !self.syms.funs.contains_key(&fname)
                    && crate::resolve::builtin_exception(&fname).is_some() =>
            {
                let internal = crate::resolve::builtin_exception(&fname).unwrap();
                let class_idx = cw.class_ref(internal);
                code.new_obj(class_idx);
                code.dup();
                let desc = if args.is_empty() { "()V" } else { "(Ljava/lang/String;)V" };
                for a in args {
                    self.emit_expr_as(*a, Ty::String, code, cw);
                }
                let arg_words: i32 = if args.is_empty() { 0 } else { 1 };
                let m = cw.methodref(internal, "<init>", desc);
                code.invokespecial(m, arg_words, 0);
            }
            Expr::Name(fname) => {
                // Calling a local variable of function type: `f()` where `f: () -> String`.
                if let Some(&(slot, Ty::Fun(arity))) = self.slots.get(&fname) {
                    load_local(Ty::Fun(arity), slot, code);
                    self.emit_fun_invoke(arity, args, code, cw);
                    return;
                }
                // Calling a class property of function type: `fnc()` where `val fnc: () -> String`.
                if let Some(&Ty::Fun(arity)) = self.class_props.get(&fname).filter(|_| self.is_instance || self.recv.is_some()) {
                    let owner = self.implicit_class();
                    self.emit_implicit_this(code);
                    if self.recv.is_some() || self.props_via_getter {
                        let getter = format!("get{}", capitalize(&fname));
                        let m = cw.methodref(&owner, &getter, &method_descriptor(&[], Ty::Fun(arity)));
                        code.invokevirtual(m, 0, 1);
                    } else {
                        let f = cw.fieldref(&owner, &fname, &Ty::Fun(arity).descriptor());
                        code.getfield(f, 1);
                    }
                    self.emit_fun_invoke(arity, args, code, cw);
                    return;
                }
                // Local function call → invokestatic with mangled name on the same class.
                if let Some((mangled, sig)) = self.info.local_fun_for_call(e).map(|(n, s)| (n.to_string(), s.clone())) {
                    for (a, &pty) in args.iter().zip(&sig.params) {
                        self.emit_expr_as(*a, pty, code, cw);
                    }
                    let arg_words: i32 = sig.params.iter().map(|t| slot_words(*t) as i32).sum();
                    let ret_words = slot_words(sig.ret) as i32;
                    let host = self.local_fun_emitted_class.get(&mangled).cloned().unwrap_or_else(|| self.class.clone());
                    let m = cw.methodref(&host, &mangled, &method_descriptor(&sig.params, sig.ret));
                    code.invokestatic(m, arg_words, ret_words);
                    return;
                }
                // Unqualified sibling instance-method call `foo()` → `this.foo()` (invokevirtual),
                // where `this` is the enclosing instance or a `run`/`with`/`apply` receiver.
                if (self.is_instance || self.recv.is_some()) && !self.syms.funs.contains_key(&fname) {
                    let owner = self.implicit_class();
                    if let Some(sig) = self.syms.method_of(&owner, &fname) {
                        self.emit_implicit_this(code);
                        for (a, pty) in args.iter().zip(&sig.params) {
                            self.emit_expr_as(*a, *pty, code, cw);
                        }
                        let arg_words: i32 = sig.params.iter().map(|t| slot_words(*t) as i32).sum();
                        let m = cw.methodref(&owner, &fname, &method_descriptor(&sig.params, sig.ret));
                        code.invokevirtual(m, arg_words, slot_words(sig.ret) as i32);
                        return;
                    }
                }
                // Unqualified companion (static) method call inside a companion member → invokestatic
                // on the enclosing class.
                if !self.syms.funs.contains_key(&fname) {
                    if let Some((internal, sig)) = self
                        .companion_of
                        .as_ref()
                        .and_then(|c| self.syms.classes.get(c))
                        .and_then(|c| c.static_methods.get(&fname).map(|s| (c.internal.clone(), s.clone())))
                    {
                        for (a, pty) in args.iter().zip(&sig.params) {
                            self.emit_expr_as(*a, *pty, code, cw);
                        }
                        let arg_words: i32 = sig.params.iter().map(|t| slot_words(*t) as i32).sum();
                        let m = cw.methodref(&internal, &fname, &method_descriptor(&sig.params, sig.ret));
                        code.invokestatic(m, arg_words, slot_words(sig.ret) as i32);
                        return;
                    }
                }
                let mut sig = match self.syms.funs.get(&fname) {
                    Some(s) => s.clone(),
                    None => return,
                };
                if let Some(&inferred) = self.info.fun_ret_overrides.get(&fname) {
                    sig.ret = inferred;
                }
                if sig.vararg {
                    // Emit the fixed args, then pack the trailing args into a fresh array.
                    let fixed = sig.params.len() - 1;
                    for i in 0..fixed {
                        self.emit_expr_as(args[i], sig.params[i], code, cw);
                    }
                    let arr_ty = sig.params[fixed];
                    let elem = arr_ty.array_elem().unwrap_or(Ty::Error);
                    let n = args.len() - fixed;
                    code.push_int(n as i32, cw);
                    self.emit_new_array(elem, code, cw);
                    let (sop, swords) = array_store_op(elem);
                    for (k, &a) in args[fixed..].iter().enumerate() {
                        code.dup();
                        code.push_int(k as i32, cw);
                        self.emit_expr_as(a, elem, code, cw);
                        code.array_store(sop, swords);
                    }
                    let arg_words: i32 = sig.params[..fixed].iter().map(|t| slot_words(*t) as i32).sum::<i32>() + 1;
                    let owner = self.file_facade.clone();
                    let m = cw.methodref(&owner, &fname, &method_descriptor(&sig.params, sig.ret));
                    code.invokestatic(m, arg_words, slot_words(sig.ret) as i32);
                    return;
                }
                // The default-value expressions for any omitted parameters (the emitted method always
                // takes the full parameter list).
                let defaults: Vec<Option<ExprId>> = self
                    .file
                    .decls
                    .iter()
                    .find_map(|&d| match self.file.decl(d) {
                        crate::ast::Decl::Fun(f) if f.name == fname => Some(f.params.iter().map(|p| p.default).collect()),
                        _ => None,
                    })
                    .unwrap_or_default();
                let names = self.file.call_arg_names.get(&e.0).cloned();
                if let Some(names) = names {
                    // Named arguments may be reordered relative to the parameters. Evaluate the
                    // supplied arguments in SOURCE order into fresh locals (preserving Kotlin's
                    // left-to-right evaluation), then load them — or emit a default — in parameter
                    // order so the stack matches the method descriptor.
                    let slots = crate::resolve::map_call_args(args, Some(&names), &sig.param_names, sig.required).unwrap_or_default();
                    let mut locals: Vec<Option<(u16, Ty)>> = vec![None; sig.params.len()];
                    for &a in args {
                        let pi = slots.iter().position(|s| *s == Some(a)).expect("checker mapped every arg");
                        let pty = sig.params[pi];
                        self.emit_expr_as(a, pty, code, cw);
                        let slot = self.fresh_slot(pty);
                        code.ensure_locals(slot + slot_words(pty));
                        self.store(pty, slot, code);
                        locals[pi] = Some((slot, pty));
                    }
                    for (i, &pty) in sig.params.iter().enumerate() {
                        match locals[i] {
                            Some((slot, t)) => load_local(t, slot, code),
                            None => {
                                let dx = defaults.get(i).copied().flatten().expect("checker guarantees a default");
                                self.emit_expr_as(dx, pty, code, cw);
                            }
                        }
                    }
                } else {
                    // Positional: supplied args are already in parameter order; omitted trailing
                    // parameters fall back to their defaults.
                    for (i, &pty) in sig.params.iter().enumerate() {
                        let arg = args.get(i).copied().or_else(|| defaults.get(i).copied().flatten()).expect("checker guarantees a value or default");
                        self.emit_expr_as(arg, pty, code, cw);
                    }
                }
                let arg_words: i32 = sig.params.iter().map(|t| slot_words(*t) as i32).sum();
                let ret_words = slot_words(sig.ret) as i32;
                let owner = self.file_facade.clone();
                let m = cw.methodref(&owner, &fname, &method_descriptor(&sig.params, sig.ret));
                code.invokestatic(m, arg_words, ret_words);
            }
            _ => {
                // Generic callee expression of function type.
                let callee_ty = self.info.ty(callee);
                if let Ty::Fun(arity) = callee_ty {
                    self.emit_expr(callee, code, cw);
                    self.emit_fun_invoke(arity, args, code, cw);
                } else {
                    self.diags.error(self.file.expr_spans[e.0 as usize], "krusty v0: unsupported call form");
                }
            }
        }
    }

    /// Emit the `invokeinterface FunctionN.invoke(...)Object` sequence, with the function value
    /// already on the stack. Boxes primitive arguments.
    fn emit_fun_invoke(&mut self, arity: u8, args: &[ExprId], code: &mut CodeBuilder, cw: &mut ClassWriter) {
        let iface = Ty::fun_interface(arity);
        // Push each argument, boxing primitives to Object.
        for &a in args {
            let aty = self.info.ty(a);
            self.emit_expr(a, code, cw);
            emit_box(aty, code, cw);
        }
        // Descriptor: invoke(Object*)Object
        let invoke_desc: String = {
            let mut d = String::from("(");
            for _ in 0..arity {
                d.push_str("Ljava/lang/Object;");
            }
            d.push_str(")Ljava/lang/Object;");
            d
        };
        let m = cw.interface_methodref(&iface, "invoke", &invoke_desc);
        // invokeinterface: consumes 1 (receiver) + arity (args), pushes 1 (result).
        code.invokeinterface(m, arity as i32, 1);
    }
}

fn is_arith(op: BinOp) -> bool {
    matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem)
}
fn is_cmp(op: BinOp) -> bool {
    matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne)
}
fn negate_cmp(op: BinOp) -> BinOp {
    match op {
        BinOp::Lt => BinOp::Ge,
        BinOp::Le => BinOp::Gt,
        BinOp::Gt => BinOp::Le,
        BinOp::Ge => BinOp::Lt,
        BinOp::Eq => BinOp::Ne,
        BinOp::Ne => BinOp::Eq,
        other => other,
    }
}

fn slot_words(t: Ty) -> u16 {
    match t {
        Ty::Long | Ty::Double => 2,
        Ty::Unit => 0,
        _ => 1,
    }
}

/// Box a primitive type to its wrapper — leaves reference types unchanged.
fn emit_box(ty: Ty, code: &mut CodeBuilder, cw: &mut ClassWriter) {
    let (wrapper, desc) = match ty {
        Ty::Int | Ty::Byte | Ty::Short => ("java/lang/Integer", "(I)Ljava/lang/Integer;"),
        Ty::Long => ("java/lang/Long", "(J)Ljava/lang/Long;"),
        Ty::Float => ("java/lang/Float", "(F)Ljava/lang/Float;"),
        Ty::Double => ("java/lang/Double", "(D)Ljava/lang/Double;"),
        Ty::Boolean => ("java/lang/Boolean", "(Z)Ljava/lang/Boolean;"),
        Ty::Char => ("java/lang/Character", "(C)Ljava/lang/Character;"),
        _ => return, // already a reference, no boxing needed
    };
    let m = cw.methodref(wrapper, "valueOf", desc);
    code.invokestatic(m, slot_words(ty) as i32, 1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_name_capitalization() {
        assert_eq!(file_class_name("foo", None), "FooKt");
        assert_eq!(file_class_name("file_1", None), "File_1Kt");
        assert_eq!(file_class_name("foo", Some("a.b")), "a/b/FooKt");
    }

    #[test]
    fn descriptors() {
        assert_eq!(method_descriptor(&[Ty::Int, Ty::Int], Ty::Int), "(II)I");
        assert_eq!(method_descriptor(&[Ty::Int, Ty::String], Ty::String), "(ILjava/lang/String;)Ljava/lang/String;");
        assert_eq!(method_descriptor(&[Ty::Double, Ty::Long], Ty::Double), "(DJ)D");
    }
}
