//! `krusty-ir` → JVM bytecode. The JVM backend's lowering of the backend-agnostic IR — it maps
//! Kotlin FqNames to JVM descriptors here (the IR never carries descriptors). Covers the core
//! subset (functions, simple classes); shares `CodeBuilder`/`ClassWriter` with the AST emitter.

use std::collections::HashMap;

use crate::ir::{Callee, IrBinOp, IrClass, IrConst, IrCtorArg, IrExpr, IrField, IrFile, IrTypeOp};
use crate::jvm::classfile::{ClassWriter, CodeBuilder, Label, VerifType};
use crate::jvm::classreader::{MethodCode, C};
use crate::jvm::inline::MethodBodies;
use crate::jvm::names::{
    method_descriptor, property_getter_name, property_setter_name, type_descriptor,
};
use crate::types::{Ty, TypeName};

struct InlineStaticTarget<'a> {
    owner: &'a str,
    name: &'a str,
    descriptor: &'a str,
    splice_desc: &'a str,
}

/// Mutable per-emit-run accumulators, owned by the caller and shared (by `&`, via interior mutability)
/// down the emit callgraph — formerly three thread-locals. The caller reads `inline_bail`/`emit_bail`
/// after `emit_all_with_opts` returns `None` to distinguish an inline-splice failure (a backend bug to
/// fix) from an unsupported construct (skip the file).
#[derive(Default)]
pub struct EmitRun {
    /// The reason an inline splice failed during emission (a required stdlib-inline call the backend
    /// could not splice), else `None`.
    inline_bail: std::cell::RefCell<Option<String>>,
    /// Set when a `GetValue`/`SetValue` references a value slot that was never allocated (malformed IR
    /// from an unsupported lowering). The emitter never panics: it sets this and the file is dropped —
    /// a compiler must never crash on its own IR.
    emit_bail: std::cell::Cell<bool>,
    /// Lambda impl `FunId`s that got a REAL `invokedynamic` this pass. A lambda spliced by the inliner
    /// (a `require { … }` message, an inlined `flatMap { … }` body) never emits one, so its standalone
    /// `$lambda$N` method is dead — dropped on the re-emit (kotlinc emits neither it nor its facade).
    used_lambdas: std::cell::RefCell<std::collections::HashSet<u32>>,
}

impl EmitRun {
    /// The inline-splice failure reason recorded this run, if any (read by the caller after `None`).
    pub fn inline_bail(&self) -> Option<String> {
        self.inline_bail.borrow().clone()
    }
    fn set_inline_bail(&self, reason: String) {
        *self.inline_bail.borrow_mut() = Some(reason);
    }
}

/// The emit environment threaded (by `&`) through the whole emit callgraph in place of the bare
/// `bodies` provider: the bytecode provider plus the mutable run accumulators, so the deep `Emitter`
/// records a used lambda / an emit-or-inline bail without an ambient thread-local. Replacing `bodies`
/// keeps every function's argument count unchanged.
pub struct EmitEnv<'a> {
    bodies: &'a dyn MethodBodies,
    run: &'a EmitRun,
}

/// A built `@kotlin.Metadata` annotation for a file facade: the `k`/`mv`/`xi` ints and the `d1` (the
/// encoded protobuf, one byte per `char`) / `d2` (string table) arrays. Attached to the facade class so
/// another Kotlin/krusty compilation can resolve its top-level declarations — in particular reading the
/// `IS_SUSPEND` flag + logical signature of a `suspend fun`.
#[derive(Clone)]
pub struct KotlinMetadata {
    pub k: i32,
    pub mv: Vec<i32>,
    pub xi: i32,
    pub d1: Vec<String>,
    pub d2: Vec<String>,
}

/// Per-file emission configuration passed explicitly down the emit callgraph and stamped onto every
/// `ClassWriter` (via [`new_writer`]) so synthetic serializer/companion/DefaultImpls classes inherit
/// it too. The `Default` (v52, no `SourceFile`) keeps [`emit_all`]'s output byte-identical to before —
/// only the CLI-driven backend path overrides it (`-jvm-target`, the source `.kt` name).
#[derive(Clone, Default)]
pub struct EmitOptions {
    /// Class-file major version to emit (default v52; `-jvm-target 25` ⇒ v69).
    pub class_major: Option<u16>,
    /// Source-file simple name for the `SourceFile` attribute (e.g. `Foo.kt`); `None` ⇒ no attribute.
    pub source_file: Option<String>,
    /// `-module-name` value, recorded in each class's `@Metadata` (`classModuleName`). kotlinc omits it
    /// for the default module `main`; `None` here matches that.
    pub module_name: Option<String>,
    /// Emit a computed `@kotlin.Metadata` for supported class shapes (WIP — [`build_class_metadata`]).
    /// Byte-verified vs kotlinc for a plain `val`/`var`-property class and a `data class` (its IS_DATA
    /// flag + synthesized `componentN`/`copy`/`equals`/`hashCode`/`toString`); other shapes are gated
    /// out and emit no metadata. OFF by default: an unverified payload breaks kotlin-reflect (a
    /// box-corpus case caught this), so the default emit stays unchanged until a shape is verified.
    pub emit_class_metadata: bool,
}

/// Compute a class's `@kotlin.Metadata` from its IR — WIRING [`crate::metadata::class_builder::build_class`]
/// into emission. Bounded (for now) to a PLAIN class with a primary constructor of `val`/`var` properties
/// and NO user methods — the shape whose metadata is byte-verified against kotlinc. Returns `None` for
/// any other shape (object/enum/interface/annotation/companion/data-class-with-methods/…), so those
/// classes emit no `@Metadata` yet (unchanged). Broader shapes follow as `build_class` grows.
/// `Class.flags` (proto field 1) for any Kotlin class kind — ONE bitfield, not a per-kind constant.
/// Decoded from kotlinc 2.4.0 across every kind (plain 6, open 22, abstract 38, sealed 54, interface
/// 102, annotation 262, object 326, data 1030, value 8199, enum 32902):
///   bit0 hasAnnotations | bits1-3 visibility (PUBLIC=3) | bits4-5 modality (FINAL0/OPEN1/ABSTRACT2/
///   SEALED3) | bits6-8 classKind (CLASS0/INTERFACE1/ENUM2/ENUM_ENTRY3/ANNOTATION4/OBJECT5/COMPANION6)
///   | bit10 isData | bit13 isValue | bit15 hasEnumEntries.
/// The writer omits the field at [`DEFAULT_CLASS_FLAGS`] (a public final class).
fn class_metadata_flags(c: &crate::ir::IrClass) -> u64 {
    const VIS_PUBLIC: u64 = 3;
    let modality: u64 = if c.is_sealed {
        3
    } else if c.is_abstract || c.is_interface {
        2
    } else if c.is_open {
        1
    } else {
        0
    };
    let kind: u64 = if c.is_annotation {
        4
    } else if c.is_interface {
        1
    } else if !c.enum_entries.is_empty() {
        2
    } else if c.enum_entry_of.is_some() {
        3
    } else if c.is_companion {
        6
    } else if c.is_object {
        5
    } else {
        0
    };
    // A value class carries `@JvmInline`, which sets `hasAnnotations`.
    let has_annotations = u64::from(c.is_value);
    has_annotations
        | (VIS_PUBLIC << 1)
        | (modality << 4)
        | (kind << 6)
        | (u64::from(c.is_data) << 10)
        | (u64::from(c.is_value) << 13)
        | (u64::from(!c.enum_entries.is_empty()) << 15)
}

/// `Function.flags` (proto field 9) — ONE bitfield like [`class_metadata_flags`], not a per-shape
/// constant. Decoded from kotlinc 2.4.0 (copy 198, componentN 454, hashCode/toString 65750, equals
/// 66006): bit0 hasAnnotations | bits1-3 visibility (PUBLIC=3, PRIVATE=1) | bits4-5 modality
/// (FINAL=0, OPEN=1, ABSTRACT=2) | bits6-7 memberKind (DECLARATION=0, SYNTHESIZED=3) | bit8 isOperator.
/// Used for a class's REAL declared members; the data/value-class synthesized sets keep their own
/// (already kotlinc-verified) constants.
fn function_flags(ir: &IrFile, fid: u32, f: &crate::ir::IrFunction) -> u64 {
    let visibility: u64 = if ir.private_methods.contains(&fid) {
        1
    } else {
        3
    };
    let modality: u64 = if f.body.is_none() {
        2 // abstract (an interface method or an `abstract fun`)
    } else if ir.open_methods.contains(&fid) {
        1
    } else {
        0
    };
    (visibility << 1) | (modality << 4)
}

fn build_class_metadata(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    opts: &EmitOptions,
) -> Option<KotlinMetadata> {
    use crate::metadata::class_builder::{
        build_class, ClassTail, FnMeta, PropMeta, COMPONENT_FN_FLAGS, COPY_FN_FLAGS,
        EQUALS_FN_FLAGS, HASHCODE_TOSTRING_FN_FLAGS, SEALED_CTOR_FLAGS,
    };
    if c.is_companion
        || c.is_annotation
        || c.enum_entry_of.is_some()
        || c.prop_ref.is_some()
        || c.func_ref.is_some()
        || c.companion_class.is_some()
        || !c.secondary_ctors.is_empty()
        // An interface legitimately has NO constructor; every other kind must have its primary.
        || (!c.has_primary_ctor && !c.is_interface)
        || (c.fields.len() as u32) < c.ctor_param_count
    {
        return None;
    }
    let cap = |s: &str| {
        let mut c = s.chars();
        c.next()
            .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
            .unwrap_or_default()
    };
    // A `data class` also carries kotlinc's synthesized `componentN`/`copy`/`equals`/`hashCode`/
    // `toString` — derivable from the primary-ctor properties alone, so allowed alongside accessors.
    if c.is_value && (c.fields.len() != 1 || !c.fields[0].is_final) {
        return None; // multi-field / `var` value classes are a shape not computed yet
    }
    // A value class's compiler-synthesized members (the static `-impl` family + their instance
    // delegators); allowed alongside the property accessor without disqualifying the shape.
    let value_method_names: std::collections::HashSet<String> = if c.is_value {
        [
            "equals",
            "hashCode",
            "toString",
            "equals-impl",
            "equals-impl0",
            "hashCode-impl",
            "toString-impl",
            "box-impl",
            "unbox-impl",
            "constructor-impl",
        ]
        .map(String::from)
        .into_iter()
        .collect()
    } else {
        std::collections::HashSet::new()
    };
    let data_method_names: std::collections::HashSet<String> = if c.is_data {
        let mut s: std::collections::HashSet<String> = (1..=c.fields.len())
            .map(|i| format!("component{i}"))
            .collect();
        s.extend(["copy", "equals", "hashCode", "toString"].map(String::from));
        s
    } else {
        std::collections::HashSet::new()
    };
    // The only methods allowed in this bounded shape are the properties' own accessors (`getX`/`setX`)
    // plus a data class's synthesized set; any other real method is a shape not computed yet.
    let accessor_names: std::collections::HashSet<String> = c
        .fields
        .iter()
        .flat_map(|f| {
            let cn = cap(&f.name);
            [format!("get{cn}"), format!("set{cn}")]
        })
        .collect();
    // Any member that is NOT an accessor and NOT part of a data/value class's synthesized set is a REAL
    // declared function — emit it (with derived flags) rather than declining the whole class.
    let declared_fids: Vec<u32> = c
        .methods
        .iter()
        .copied()
        .filter(|&fid| {
            let n = &ir.functions[fid as usize].name;
            !accessor_names.contains(n)
                && !data_method_names.contains(n)
                && !value_method_names.contains(n)
        })
        .collect();
    let desc = |t: Ty| crate::jvm::names::type_descriptor(t);
    let props: Vec<PropMeta> = c
        .fields
        .iter()
        .map(|f| PropMeta {
            name: f.name.clone(),
            ty: f.ty,
            is_var: !f.is_final,
            getter: (format!("get{}", cap(&f.name)), format!("(){}", desc(f.ty))),
            setter: (!f.is_final)
                .then(|| (format!("set{}", cap(&f.name)), format!("({})V", desc(f.ty)))),
        })
        .collect();
    // Only the LEADING `ctor_param_count` fields are constructor parameters; any remainder are BODY
    // properties (an `object`'s `val x = 1`, a class's non-ctor `val`) — they are properties, not params.
    let ctor_params: Vec<(String, Ty)> = c
        .fields
        .iter()
        .take(c.ctor_param_count as usize)
        .map(|f| (f.name.clone(), f.ty))
        .collect();
    let ctor_param_defaults: Vec<bool> = c
        .fields
        .iter()
        .take(c.ctor_param_count as usize)
        .map(|f| f.has_default)
        .collect();
    let ctor_desc = format!(
        "({})V",
        ctor_params
            .iter()
            .map(|(_, t)| desc(*t))
            .collect::<String>()
    );
    // kotlinc's synthesized data-class methods, in declaration order: componentN, copy, equals,
    // hashCode, toString. Their shapes come entirely from the primary-ctor properties.
    // A boxed nullable primitive (`Int?` → `Ljava/lang/Integer;`): its JVM descriptor is not derivable
    // from the proto type alone, so kotlinc records a `JvmMethodSignature` on any synthesized method
    // whose param/return is one. The descriptor (name derivable) is emitted only when needed.
    let is_boxed_prim = |t: Ty| t.is_nullable() && t.non_null().is_jvm_scalar();
    let boxed_fn_sig = |params: &[Ty], ret: Ty| -> Option<String> {
        (is_boxed_prim(ret) || params.iter().copied().any(is_boxed_prim))
            .then(|| method_descriptor(params, ret))
    };
    let methods: Vec<FnMeta> = if c.is_data {
        let class_ty = Ty::obj(&c.fq_name());
        let field_tys: Vec<Ty> = c.fields.iter().map(|f| f.ty).collect();
        let mut m: Vec<FnMeta> = c
            .fields
            .iter()
            .enumerate()
            .map(|(i, f)| FnMeta {
                name: format!("component{}", i + 1),
                params: vec![],
                ret: f.ty,
                flags: COMPONENT_FN_FLAGS,
                params_have_defaults: false,
                jvm_sig: boxed_fn_sig(&[], f.ty),
                jvm_sig_name: None,
            })
            .collect();
        m.push(FnMeta {
            name: "copy".into(),
            params: c.fields.iter().map(|f| (f.name.clone(), f.ty)).collect(),
            ret: class_ty,
            flags: COPY_FN_FLAGS,
            params_have_defaults: true,
            jvm_sig: boxed_fn_sig(&field_tys, class_ty),
            jvm_sig_name: None,
        });
        m.push(FnMeta {
            name: "equals".into(),
            params: vec![("other".into(), Ty::nullable(Ty::obj("kotlin/Any")))],
            ret: Ty::Boolean,
            flags: EQUALS_FN_FLAGS,
            params_have_defaults: false,
            jvm_sig: None,
            jvm_sig_name: None,
        });
        m.push(FnMeta {
            name: "hashCode".into(),
            params: vec![],
            ret: Ty::Int,
            flags: HASHCODE_TOSTRING_FN_FLAGS,
            params_have_defaults: false,
            jvm_sig: None,
            jvm_sig_name: None,
        });
        m.push(FnMeta {
            name: "toString".into(),
            params: vec![],
            ret: Ty::String,
            flags: HASHCODE_TOSTRING_FN_FLAGS,
            params_have_defaults: false,
            jvm_sig: None,
            jvm_sig_name: None,
        });
        m
    } else if c.is_value {
        // A value class's Kotlin-visible overrides. Each dispatches to a differently-named static
        // `-impl` taking the erased underlying, so each records a `JvmMethodSignature` (name + desc).
        let u = desc(c.fields[0].ty);
        vec![
            FnMeta {
                name: "equals".into(),
                params: vec![("other".into(), Ty::nullable(Ty::obj("kotlin/Any")))],
                ret: Ty::Boolean,
                flags: EQUALS_FN_FLAGS,
                params_have_defaults: false,
                jvm_sig: Some(format!("({u}Ljava/lang/Object;)Z")),
                jvm_sig_name: Some("equals-impl".into()),
            },
            FnMeta {
                name: "hashCode".into(),
                params: vec![],
                ret: Ty::Int,
                flags: HASHCODE_TOSTRING_FN_FLAGS,
                params_have_defaults: false,
                jvm_sig: Some(format!("({u})I")),
                jvm_sig_name: Some("hashCode-impl".into()),
            },
            FnMeta {
                name: "toString".into(),
                params: vec![],
                ret: Ty::String,
                flags: HASHCODE_TOSTRING_FN_FLAGS,
                params_have_defaults: false,
                jvm_sig: Some(format!("({u})Ljava/lang/String;")),
                jvm_sig_name: Some("toString-impl".into()),
            },
        ]
    } else {
        declared_fids
            .iter()
            .filter_map(|&fid| {
                let f = ir.functions.get(fid as usize)?;
                Some(FnMeta {
                    name: f.name.clone(),
                    params: f
                        .params
                        .iter()
                        .enumerate()
                        .map(|(i, t)| {
                            let n = ir
                                .fn_param_names
                                .get(&fid)
                                .and_then(|ns| ns.get(i).cloned())
                                .unwrap_or_else(|| format!("p{i}"));
                            (n, *t)
                        })
                        .collect(),
                    ret: f.ret,
                    flags: function_flags(ir, fid, f),
                    params_have_defaults: false,
                    jvm_sig: None,
                    jvm_sig_name: None,
                })
            })
            .collect()
    };
    // A value class's primary constructor is realized as the static `constructor-impl` returning the
    // erased underlying, not `<init>`; its `@Metadata` signature records that.
    let vc_ctor_desc = c
        .is_value
        .then(|| format!("({0}){0}", desc(c.fields[0].ty)));
    // `Class.enumEntry` (f13) — the builder has always accepted these; only the caller withheld them.
    let enum_entry_names: Vec<String> = c.enum_entries.iter().map(|e| e.name.clone()).collect();
    let (d1_bytes, d2) = build_class(
        &c.fq_name(),
        &ctor_params,
        vc_ctor_desc.as_deref().unwrap_or(&ctor_desc),
        &props,
        &methods,
        &enum_entry_names,
        &ClassTail {
            flags: class_metadata_flags(c),
            primary_ctor_flags: if c.is_sealed { SEALED_CTOR_FLAGS } else { 0 },
            module_name: opts.module_name.as_deref(),
            ctor_param_defaults: &ctor_param_defaults,
            inline_underlying: c
                .is_value
                .then(|| (c.fields[0].name.as_str(), c.fields[0].ty)),
            ctor_sig_name: c.is_value.then_some("constructor-impl"),
            // An interface has no constructor at all, whatever the IR records.
            emit_primary_ctor: !c.is_interface,
            jvm_class_flags: c.is_interface.then_some(3),
            ..Default::default()
        },
    );
    // d1 is the protobuf payload as one `char` per byte (the constant pool writes it as modified-UTF-8).
    let d1 = vec![d1_bytes.iter().map(|&b| b as char).collect()];
    Some(KotlinMetadata {
        k: 1,
        mv: vec![2, 4, 0],
        xi: 48,
        d1,
        d2,
    })
}

/// Attach kotlinc-style `LineNumberTable` + `LocalVariableTable` debug tables to a plain property
/// class's synthesized members (primary ctor + property accessors). Every such member maps to the
/// class declaration line, and its locals (`this` + params) live for the whole method — so the tables
/// are computable from `c.fields` alone. Call BEFORE `@Metadata` is attached so the debug strings
/// (`this`, member param names, the attribute names) intern into the constant pool ahead of the
/// annotation, matching kotlinc's ordering. Scoped to non-data classes for now (data-class synthesized
/// methods carry branches/stack maps and need their own line mapping).
/// Compute a plain property class's ctor/field/accessor descriptors and seed the constant pool in
/// kotlinc's interning order (see [`ClassWriter::seed_plain_class_pool`]). Mirrors the descriptors that
/// `attach_synth_debug_tables` and the natural emission produce, so the seeded entries are reused.
fn seed_plain_class_pool(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    fq_name: &str,
    superclass: &str,
    ctor_signature: Option<&str>,
    cw: &mut ClassWriter,
) {
    let desc = |t: Ty| crate::jvm::names::type_descriptor(t);
    let cap = |s: &str| {
        let mut ch = s.chars();
        ch.next()
            .map(|f| f.to_uppercase().collect::<String>() + ch.as_str())
            .unwrap_or_default()
    };
    // Reference-type annotation kind: 0 = primitive (no annotation), 1 = non-null reference (@NotNull +
    // a `checkNotNullParameter` guard), 2 = nullable reference (@Nullable, no guard).
    let ann_kind = |t: Ty| -> u8 {
        let d = desc(t);
        if !(d.starts_with('L') || d.starts_with('[')) {
            0
        } else if matches!(t, Ty::Nullable(_)) {
            2
        } else {
            1
        }
    };
    let is_nonnull_ref = |t: Ty| ann_kind(t) == 1;
    let ctor_desc = format!(
        "({})V",
        c.fields.iter().map(|f| desc(f.ty)).collect::<String>()
    );
    let fields: Vec<(String, String, u8)> = c
        .fields
        .iter()
        .map(|f| (f.name.clone(), desc(f.ty), ann_kind(f.ty)))
        .collect();
    // (name, descriptor, setter_kind): 0 getter, 1 primitive setter, 2 non-null reference setter.
    let mut accessors: Vec<(String, String, u8)> = Vec::new();
    for f in &c.fields {
        accessors.push((
            format!("get{}", cap(&f.name)),
            format!("(){}", desc(f.ty)),
            0,
        ));
        if !f.is_final {
            let kind = if is_nonnull_ref(f.ty) { 2 } else { 1 };
            accessors.push((
                format!("set{}", cap(&f.name)),
                format!("({})V", desc(f.ty)),
                kind,
            ));
        }
    }
    // Generic `Signature`s for PARAMETERIZED-type members (`List<String>` → `Ljava/util/List<Ljava/lang/String;>;`).
    // Only for a class with NO bare type-parameter fields — a generic class's bare-`T` members are handled by
    // the existing tparam path, left untouched. Seeded here so the natural emission (add_field_sig/
    // add_method_sig) dedupes to kotlinc's interning positions.
    let no_tparam_fields = ir.field_signatures(fq_name).is_none();
    let ctor_sig = no_tparam_fields.then_some(ctor_signature).flatten();
    let field_sigs: Vec<Option<String>> = if no_tparam_fields {
        c.fields.iter().map(|f| parameterized_sig(&f.ty)).collect()
    } else {
        Vec::new()
    };
    let mut accessor_sigs: Vec<Option<String>> = Vec::new();
    if no_tparam_fields {
        for f in &c.fields {
            let g = parameterized_sig(&f.ty);
            accessor_sigs.push(g.as_ref().map(|s| format!("(){s}"))); // getter → `()<sig>`
            if !f.is_final {
                accessor_sigs.push(g.as_ref().map(|s| format!("({s})V"))); // setter → `(<sig>)V`
            }
        }
    }
    // A `data class` interns its parameterized-field `Signature`s LATE (in `seed_data_class_pool`, before
    // `@Metadata`), not with the field/accessors — so hand the plain seeder an EMPTY field-sig list and
    // pass the real ones to the data seeder below.
    cw.seed_plain_class_pool(
        fq_name,
        superclass,
        &ctor_desc,
        &fields,
        &accessors,
        &crate::jvm::classfile::MemberSignatures {
            ctor: ctor_sig,
            accessors: &accessor_sigs,
            fields: if c.is_data { &[] } else { &field_sigs },
        },
    );
    if c.is_data {
        let simple = fq_name.rsplit('/').next().unwrap_or(fq_name);
        let data_fields: Vec<(String, String)> = c
            .fields
            .iter()
            .map(|f| (f.name.clone(), desc(f.ty)))
            .collect();
        // Per-field `hashCode` owner (interface field → `java/lang/Object`), recorded by `field_hash`.
        let hashcode_owners: Vec<Option<String>> = c
            .fields
            .iter()
            .map(|f| ir.data_hashcode_owner(fq_name, &f.name).map(str::to_string))
            .collect();
        // `copy`'s generic Signature shares the ctor's parameter list, returning `self` instead of `void`.
        let copy_sig = ctor_sig
            .and_then(|s| s.strip_suffix('V'))
            .map(|params| format!("{params}L{fq_name};"));
        cw.seed_data_class_pool(
            fq_name,
            &ctor_desc,
            simple,
            &data_fields,
            &crate::jvm::classfile::DataMemberInfo {
                hashcode_owners: &hashcode_owners,
                copy_sig: copy_sig.as_deref(),
                field_sigs: &field_sigs,
            },
        );
    }
}

/// One synthesized value-class member's debug shape: `(jvm name, jvm descriptor, LocalVariableTable
/// entries as `(name, descriptor, slot)`)`.
type VcDebugMethod = (String, String, Vec<(String, String, u16)>);

/// Attach kotlinc's `LineNumberTable` + `LocalVariableTable` to a class's DECLARED methods (as opposed
/// to the synthesized ctor/accessors handled by [`attach_synth_debug_tables`]). kotlinc maps a method's
/// table to its own `fun` line — recorded per-FunId by the lowering — and lists `this` plus each
/// parameter for the whole method.
fn attach_declared_method_debug(ir: &IrFile, c: &crate::ir::IrClass, cw: &mut ClassWriter) {
    let this_desc = format!("L{};", c.fq_name());
    for &fid in &c.methods {
        let Some(f) = ir.functions.get(fid as usize) else {
            continue;
        };
        let Some(&line) = ir.fn_decl_lines.get(&fid) else {
            continue;
        };
        if f.body.is_none() {
            continue; // abstract: no Code, so no debug tables
        }
        let param_tys = jvm_tys(&f.params);
        let ret = ir_ty_to_jvm(&f.ret);
        let desc = method_descriptor(&param_tys, ret);
        let mut locals: Vec<(String, String, u16)> = Vec::new();
        let mut slot = 0u16;
        if !f.is_static {
            locals.push(("this".to_string(), this_desc.clone(), 0));
            slot = 1;
        }
        for (i, t) in param_tys.iter().enumerate() {
            locals.push((
                ir.fn_param_names
                    .get(&fid)
                    .and_then(|ns| ns.get(i).cloned())
                    .or_else(|| f.param_checks.get(i).and_then(|n| n.clone()))
                    .unwrap_or_else(|| format!("p{i}")),
                crate::jvm::names::type_descriptor(*t),
                slot,
            ));
            slot += slot_words(*t);
        }
        cw.set_method_debug(&f.name, &desc, Some((0, line)), &locals);
    }
}

fn attach_synth_debug_tables(c: &crate::ir::IrClass, cw: &mut ClassWriter) {
    let line = c.decl_line;
    if line == 0 {
        return;
    }
    let desc = |t: Ty| crate::jvm::names::type_descriptor(t);
    let slot_size = |t: Ty| -> u16 {
        match desc(t).as_str() {
            "J" | "D" => 2,
            _ => 1,
        }
    };
    let cap = |s: &str| {
        let mut ch = s.chars();
        ch.next()
            .map(|f| f.to_uppercase().collect::<String>() + ch.as_str())
            .unwrap_or_default()
    };
    // A non-null reference param carries a `checkNotNullParameter` guard (`aload <slot>; ldc <name>;
    // invokestatic`) before the body; kotlinc's LineNumberTable maps the decl line to the post-prologue
    // offset. The guard's length is SLOT-dependent: `aload_0..3` is 1 byte but `aload <u1>` (slot ≥ 4)
    // is 2, so a class with enough (or wide) ctor params pushes a non-null-ref param past slot 3 and its
    // guard grows — the fixed-6 assumption was wrong there.
    let is_nonnull_ref = |t: Ty| -> bool {
        let d = desc(t);
        (d.starts_with('L') || d.starts_with('[')) && !matches!(t, Ty::Nullable(_))
    };
    // `aload <slot>` byte length: 1 (aload_0..3), 2 (aload u1), or 4 (wide aload u2).
    let aload_len = |slot: u16| -> u16 {
        if slot <= 3 {
            1
        } else if slot <= 255 {
            2
        } else {
            4
        }
    };
    let this_desc = format!("L{};", c.fq_name());
    // Primary constructor: `this` + one local per ctor parameter (a property-backed param).
    let ctor_desc = format!(
        "({})V",
        c.fields.iter().map(|f| desc(f.ty)).collect::<String>()
    );
    let mut ctor_locals = vec![("this".to_string(), this_desc.clone(), 0u16)];
    let mut slot = 1u16;
    let mut ctor_pc = 0u16;
    for f in &c.fields {
        ctor_locals.push((f.name.clone(), desc(f.ty), slot));
        if is_nonnull_ref(f.ty) {
            // guard = aload(slot) + ldc(param-name String) + invokestatic checkNotNullParameter(3).
            // The ldc width is read off the REAL pool (2, or 3 for `ldc_w` past index 255) — the
            // guard was already emitted, so the String constant exists.
            let ldc = cw.string_ldc_len(&f.name).unwrap_or(2);
            ctor_pc += aload_len(slot) + ldc + 3;
        }
        slot += slot_size(f.ty);
    }
    let this_only = [("this".to_string(), this_desc.clone(), 0u16)];
    cw.set_method_debug("<init>", &ctor_desc, Some((ctor_pc, line)), &ctor_locals);
    // A SEALED class's primary ctor is private; kotlinc pairs it with a PUBLIC|SYNTHETIC
    // `(…args, DefaultConstructorMarker)` accessor carrying `this`, the ctor params, and the marker
    // (named `$constructor_marker`). It gets a LocalVariableTable but no LineNumberTable.
    if c.is_sealed {
        const MARKER: &str = "Lkotlin/jvm/internal/DefaultConstructorMarker;";
        let mut acc_locals = ctor_locals.clone();
        let marker_slot = c.fields.iter().map(|f| slot_size(f.ty)).sum::<u16>() + 1;
        acc_locals.push((
            "$constructor_marker".to_string(),
            MARKER.to_string(),
            marker_slot,
        ));
        let acc_desc = format!(
            "({}{MARKER})V",
            c.fields.iter().map(|f| desc(f.ty)).collect::<String>()
        );
        cw.set_method_debug("<init>", &acc_desc, None, &acc_locals);
    }
    // Property accessors: getter has only `this`; a `var` setter also has its value parameter (named
    // `<set-?>` by kotlinc), guarded when the property type is a non-null reference.
    for f in &c.fields {
        let g = format!("get{}", cap(&f.name));
        cw.set_method_debug(
            &g,
            &format!("(){}", desc(f.ty)),
            Some((0, line)),
            &this_only,
        );
        if !f.is_final {
            let s = format!("set{}", cap(&f.name));
            let pd = desc(f.ty);
            // The setter's value param is always slot 1 (`this`=0): guard = `aload_1`(1) + the
            // `<set-?>` String's real ldc width + invokestatic(3).
            let set_pc = if is_nonnull_ref(f.ty) {
                aload_len(1) + cw.string_ldc_len("<set-?>").unwrap_or(2) + 3
            } else {
                0
            };
            cw.set_method_debug(
                &s,
                &format!("({pd})V"),
                Some((set_pc, line)),
                &[
                    ("this".to_string(), this_desc.clone(), 0),
                    ("<set-?>".to_string(), pd, 1),
                ],
            );
        }
    }
    // A `@JvmInline value class`'s synthesized members: the static `-impl` family (taking the erased
    // underlying) and their instance delegators. kotlinc gives each a LocalVariableTable but no
    // LineNumberTable; the static impls name their parameter positionally (`arg0`/`v`/`p1`/`p2`) except
    // `constructor-impl`, which keeps the property name.
    if c.is_value {
        if let Some(f0) = c.fields.first() {
            let u = desc(f0.ty);
            let obj = "Ljava/lang/Object;".to_string();
            let w = slot_size(f0.ty);
            let one = |n: &str, d: &String, slot: u16| vec![(n.to_string(), d.clone(), slot)];
            let vc_methods: Vec<VcDebugMethod> = vec![
                (
                    "toString-impl".into(),
                    format!("({u})Ljava/lang/String;"),
                    one("arg0", &u, 0),
                ),
                (
                    "toString".into(),
                    "()Ljava/lang/String;".into(),
                    one("this", &this_desc, 0),
                ),
                (
                    "hashCode-impl".into(),
                    format!("({u})I"),
                    one("arg0", &u, 0),
                ),
                ("hashCode".into(), "()I".into(), one("this", &this_desc, 0)),
                (
                    "equals-impl".into(),
                    format!("({u}Ljava/lang/Object;)Z"),
                    vec![
                        ("arg0".to_string(), u.clone(), 0),
                        ("other".to_string(), obj.clone(), w),
                    ],
                ),
                (
                    "equals".into(),
                    "(Ljava/lang/Object;)Z".into(),
                    vec![
                        ("this".to_string(), this_desc.clone(), 0),
                        ("other".to_string(), obj.clone(), 1),
                    ],
                ),
                (
                    "constructor-impl".into(),
                    format!("({u}){u}"),
                    one(&f0.name, &u, 0),
                ),
                (
                    "box-impl".into(),
                    format!("({u}){this_desc}"),
                    one("v", &u, 0),
                ),
                (
                    "unbox-impl".into(),
                    format!("(){u}"),
                    one("this", &this_desc, 0),
                ),
                (
                    "equals-impl0".into(),
                    format!("({u}{u})Z"),
                    vec![
                        ("p1".to_string(), u.clone(), 0),
                        ("p2".to_string(), u.clone(), w),
                    ],
                ),
            ];
            for (name, d, locals) in &vc_methods {
                cw.set_method_debug(name, d, None, locals);
            }
        }
    }
    // A `data class`'s synthesized methods carry a LocalVariableTable (this + params) but NO
    // LineNumberTable (kotlinc gives them none). componentN/hashCode/toString/equals have only `this`
    // (equals also `other`); `copy` has the ctor parameters.
    if c.is_data {
        let self_ref = format!("L{};", c.fq_name());
        for (i, f) in c.fields.iter().enumerate() {
            cw.set_method_debug(
                &format!("component{}", i + 1),
                &format!("(){}", desc(f.ty)),
                None,
                &this_only,
            );
        }
        let mut copy_locals = vec![("this".to_string(), this_desc.clone(), 0u16)];
        let mut slot = 1u16;
        for f in &c.fields {
            copy_locals.push((f.name.clone(), desc(f.ty), slot));
            slot += slot_size(f.ty);
        }
        cw.set_method_debug(
            "copy",
            &format!(
                "{ctor_desc_no_v}{self_ref}",
                ctor_desc_no_v = &ctor_desc[..ctor_desc.len() - 1]
            ),
            None,
            &copy_locals,
        );
        cw.set_method_debug(
            "equals",
            "(Ljava/lang/Object;)Z",
            None,
            &[
                ("this".to_string(), this_desc.clone(), 0),
                ("other".to_string(), "Ljava/lang/Object;".to_string(), 1),
            ],
        );
        // hashCode: a ≥2-field data class folds into a `result` accumulator local — kotlinc lists it
        // (partial live-range) before `this`. A single-field hashCode is a bare `return h(f0)` (this only).
        if c.fields.len() >= 2 {
            cw.set_hashcode_result_debug(&this_desc);
        } else {
            cw.set_method_debug("hashCode", "()I", None, &this_only);
        }
        cw.set_method_debug("toString", "()Ljava/lang/String;", None, &this_only);
    }
}

/// Attach kotlinc's `@org.jetbrains.annotations.NotNull` / `@Nullable` to a plain property class's
/// synthesized members: each non-null reference-typed return/parameter gets `@NotNull`, each nullable
/// reference gets `@Nullable` (primitives get nothing). Covers the ctor's reference params, each
/// getter's reference return, and each `var` setter's reference param — the shape kotlinc emits for a
/// class with reference-typed properties. Call after `attach_synth_debug_tables`.
fn attach_synth_nullability(c: &crate::ir::IrClass, cw: &mut ClassWriter) {
    let desc = |t: Ty| crate::jvm::names::type_descriptor(t);
    let cap = |s: &str| {
        let mut ch = s.chars();
        ch.next()
            .map(|f| f.to_uppercase().collect::<String>() + ch.as_str())
            .unwrap_or_default()
    };
    // A reference type (descriptor `L…;`/`[…`) gets `@NotNull` unless it is `Ty::Nullable`, then
    // `@Nullable`; a primitive gets no annotation.
    let ann = |t: Ty| -> Option<&'static str> {
        let d = desc(t);
        if !(d.starts_with('L') || d.starts_with('[')) {
            return None;
        }
        Some(if matches!(t, Ty::Nullable(_)) {
            "Lorg/jetbrains/annotations/Nullable;"
        } else {
            "Lorg/jetbrains/annotations/NotNull;"
        })
    };
    // Backing field of a reference property: kotlinc annotates it (`@NotNull` for non-null).
    for f in &c.fields {
        if let Some(a) = ann(f.ty) {
            cw.set_field_nullability(&f.name, a);
        }
    }
    // Primary constructor: one parameter annotation slot per property-backed parameter.
    let ctor_params: Vec<Option<&str>> = c.fields.iter().map(|f| ann(f.ty)).collect();
    if ctor_params.iter().any(|p| p.is_some()) {
        let ctor_desc = format!(
            "({})V",
            c.fields.iter().map(|f| desc(f.ty)).collect::<String>()
        );
        cw.set_method_nullability("<init>", &ctor_desc, None, &ctor_params);
    }
    // Accessors: a reference getter annotates its return; a `var` reference setter its parameter.
    for f in &c.fields {
        let Some(a) = ann(f.ty) else { continue };
        cw.set_method_nullability(
            &format!("get{}", cap(&f.name)),
            &format!("(){}", desc(f.ty)),
            Some(a),
            &[],
        );
        if !f.is_final {
            cw.set_method_nullability(
                &format!("set{}", cap(&f.name)),
                &format!("({})V", desc(f.ty)),
                None,
                &[Some(a)],
            );
        }
    }
    // Data-class synthesized methods: `copy` returns the class (`@NotNull`), `toString` returns
    // `String` (`@NotNull`), `equals`' `other` param is `@Nullable`, and a reference-typed `componentN`
    // return is `@NotNull`.
    if c.is_data {
        let not_null = "Lorg/jetbrains/annotations/NotNull;";
        let self_ref = format!("L{};", c.fq_name());
        let copy_desc = format!(
            "({}){self_ref}",
            c.fields.iter().map(|f| desc(f.ty)).collect::<String>()
        );
        // `copy`'s parameters mirror the primary-constructor properties, so each reference param takes
        // the SAME `@NotNull`/`@Nullable` annotation kotlinc puts on the constructor's.
        let copy_params: Vec<Option<&str>> = c.fields.iter().map(|f| ann(f.ty)).collect();
        cw.set_method_nullability("copy", &copy_desc, Some(not_null), &copy_params);
        cw.set_method_nullability("toString", "()Ljava/lang/String;", Some(not_null), &[]);
        cw.set_method_nullability(
            "equals",
            "(Ljava/lang/Object;)Z",
            None,
            &[Some("Lorg/jetbrains/annotations/Nullable;")],
        );
        for (i, f) in c.fields.iter().enumerate() {
            if let Some(a) = ann(f.ty) {
                cw.set_method_nullability(
                    &format!("component{}", i + 1),
                    &format!("(){}", desc(f.ty)),
                    Some(a),
                    &[],
                );
            }
        }
    }
    // A value class's `constructor-impl` returns the erased underlying; kotlinc annotates that return
    // exactly like the property's (a non-null reference underlying → `@NotNull`).
    if c.is_value {
        if let Some(f0) = c.fields.first() {
            if let Some(a) = ann(f0.ty) {
                let u = desc(f0.ty);
                cw.set_method_nullability("constructor-impl", &format!("({u}){u}"), Some(a), &[]);
            }
        }
    }
}

/// Register the file's nested-class `InnerClasses` candidates on `cw`; the writer's `finish` keeps only
/// the entries it references as a class constant (kotlinc's rule). Covers the `@Serializable` model
/// shape — a class's `$$serializer` (inner name `$serializer`) and its `Companion`, both `public static
/// final` — emitted in kotlinc's order ($serializer before Companion). Anonymous nested classes (the
/// suspend continuations) are not yet registered (they also need an `EnclosingMethod` attribute).
fn register_inner_classes(cw: &mut ClassWriter, ir: &IrFile) {
    use crate::jvm::classfile::InnerClassSpec;
    const ACC_PSF: u16 = 0x0019; // ACC_PUBLIC | ACC_STATIC | ACC_FINAL
    for c in &ir.classes {
        let fq = c.fq_name();
        if let Some(outer) = fq.strip_suffix("$$serializer") {
            cw.add_inner_class(InnerClassSpec {
                inner: fq.clone(),
                outer: Some(outer.to_string()),
                name: Some("$serializer".to_string()),
                access: ACC_PSF,
            });
        }
    }
    for c in &ir.classes {
        if let Some(comp) = c.companion_class() {
            cw.add_inner_class(InnerClassSpec {
                inner: comp,
                outer: Some(c.fq_name()),
                name: Some("Companion".to_string()),
                access: ACC_PSF,
            });
        }
    }
}

/// Construct a `ClassWriter` with the per-file [`EmitOptions`] stamped on — the single place emission
/// builds a writer, so class version + `SourceFile` reach every class (incl. synthetics) explicitly.
fn new_writer(internal: &str, super_internal: &str, opts: &EmitOptions) -> ClassWriter {
    let mut cw = ClassWriter::new(internal, super_internal);
    if let Some(major) = opts.class_major {
        cw.set_major(major);
    }
    cw.set_source_file(opts.source_file.clone());
    cw
}

/// Emit a whole IR file: the facade class of top-level `static` functions, plus one `.class` per
/// `IrClass`. Returns `(internal_name, bytes)` for each, or `None` when the IR uses a construct the
/// JVM backend can't represent (so every emission path skips it rather than miscompiling).
/// Mark the lambda-argument impls of a MUST-INLINE call (`require`/`check`/`error` — a non-public
/// `@InlineOnly` callee the backend always splices, never invokes) as `inline_only`, so the standalone
/// `$lambda$N` method is NOT emitted. It is dead: the message lambda is spliced at the call site, so a
/// leftover impl would only force a spurious facade class (`OrganizationIdKt` holding a dead
/// `$lambda$0`) that kotlinc never emits. Safe because a `MustInline` callee is guaranteed spliced (or
/// the whole file is skipped — then nothing is emitted anyway).
/// Reparent lambda impl methods into the CLASS whose code emits their `invokedynamic`. An impl is
/// PRIVATE (kotlinc's placement: same class as the call site), so a cross-class method handle would
/// throw `IllegalAccessError`. Lowering attaches impls per `cur_class`; this pass covers the code
/// that reaches a class only later: enum-entry constructor arguments (lowered class-less, emitted in
/// the enum's `<clinit>`) and suspend-lambda state-machine bodies (moved into the machine class).
/// Transitive: an impl reparented into a class drags the impls of its own nested lambdas along.
pub fn reparent_lambda_impls(ir: &mut IrFile) {
    let mut owned: std::collections::HashSet<u32> = ir
        .classes
        .iter()
        .flat_map(|c| c.methods.iter().copied())
        .collect();
    // Impls whose `invokedynamic` (also) emits from FACADE code — facade-owned function bodies and
    // static initializers — must STAY on the facade: a suspend-lambda state machine SHARES its body
    // exprs with facade code, so a class walk alone would move an impl the facade still references.
    let facade_reachable: std::collections::HashSet<u32> = {
        let mut roots: Vec<crate::ir::ExprId> = Vec::new();
        for (i, f) in ir.functions.iter().enumerate() {
            // A lambda IMPL's body emits wherever the impl itself lands (facade or a class), so it
            // is NOT a facade root — its nested lambdas are marked transitively below only when the
            // impl is genuinely reachable from real facade code.
            if !owned.contains(&(i as u32))
                && f.dispatch_receiver.is_none()
                && !ir.lambda_own_params_from.contains_key(&(i as u32))
            {
                if let Some(b) = f.body {
                    roots.push(b);
                }
            }
        }
        for st in &ir.statics {
            roots.push(st.init);
        }
        let mut out = std::collections::HashSet::new();
        let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let mut stack = roots;
        while let Some(cur) = stack.pop() {
            if !seen.insert(cur) {
                continue;
            }
            if let IrExpr::Lambda { impl_fn, .. } = &ir.exprs[cur as usize] {
                if out.insert(*impl_fn) {
                    // Its nested lambdas emit wherever it does — keep the whole chain facade-side.
                    if let Some(b) = ir.functions.get(*impl_fn as usize).and_then(|f| f.body) {
                        stack.push(b);
                    }
                }
            }
            crate::ir::for_each_child(&ir.exprs, cur, &mut |ch| stack.push(ch));
        }
        out
    };
    for cid in 0..ir.classes.len() {
        // Class-context roots whose code emits inside this class: member/method bodies (covers a
        // suspend machine's `invokeSuspend`), the instance initializer, super/delegate arguments,
        // and enum-entry constructor arguments (emitted in `<clinit>`).
        let c = &ir.classes[cid];
        let mut roots: Vec<crate::ir::ExprId> = Vec::new();
        for &fid in &c.methods {
            if let Some(b) = ir.functions.get(fid as usize).and_then(|f| f.body) {
                roots.push(b);
            }
        }
        roots.extend(c.init_body);
        roots.extend(c.super_args.iter().copied());
        for sc in &c.secondary_ctors {
            roots.extend(sc.body);
            roots.extend(sc.delegate_args.iter().copied());
        }
        for en in &c.enum_entries {
            roots.extend(en.args.iter().copied());
        }
        let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let mut stack = roots;
        while let Some(cur) = stack.pop() {
            if !seen.insert(cur) {
                continue;
            }
            if let IrExpr::Lambda { impl_fn, .. } = &ir.exprs[cur as usize] {
                let fid = *impl_fn;
                // Only a free (facade-owned) standalone impl moves; one already owned by a class —
                // including THIS one — stays. A spliced (inline-only) impl never emits a method.
                if !owned.contains(&fid)
                    && !facade_reachable.contains(&fid)
                    && !ir.inline_only_fns.contains(&fid)
                    && ir
                        .functions
                        .get(fid as usize)
                        .is_some_and(|f| f.dispatch_receiver.is_none())
                {
                    owned.insert(fid);
                    ir.classes[cid].methods.push(fid);
                    // The impl's own body now emits in this class too — walk it for nested lambdas.
                    if let Some(b) = ir.functions.get(fid as usize).and_then(|f| f.body) {
                        stack.push(b);
                    }
                }
            }
            crate::ir::for_each_child(&ir.exprs, cur, &mut |ch| stack.push(ch));
        }
    }
}

pub fn mark_must_inline_lambdas(ir: &mut IrFile) {
    let mut dead: Vec<u32> = Vec::new();
    for i in 0..ir.exprs.len() {
        let args = match &ir.exprs[i] {
            IrExpr::Call {
                callee:
                    Callee::Static {
                        inline: crate::libraries::InlineKind::MustInline,
                        ..
                    },
                args,
                ..
            } => args.clone(),
            _ => continue,
        };
        for a in args {
            if let IrExpr::Lambda { impl_fn, .. } = &ir.exprs[a as usize] {
                dead.push(*impl_fn);
            }
        }
    }
    for fid in dead {
        ir.inline_only_fns.insert(fid);
        ir.must_inline_lambdas.insert(fid);
    }
}

pub fn emit_all(
    ir: &IrFile,
    facade: &str,
    bodies: &dyn MethodBodies,
    metadata: Option<&KotlinMetadata>,
) -> Option<Vec<(String, Vec<u8>)>> {
    // Default: no per-class `@Metadata` — krusty-core emit is byte-identical to before (the
    // `bytecode_parity_e2e` gate compares classes byte-for-byte vs kotlinc, so the default path must
    // stay untouched). A caller that needs cross-module class metadata (krusty-compose's LibraryBinary
    // modules) uses [`emit_all_with_class_meta`]. The run accumulators are discarded here (callers that
    // need the inline-bail reason use `emit_all_with_opts` with their own `EmitRun`).
    let run = EmitRun::default();
    let env = EmitEnv { bodies, run: &run };
    emit_all_with_class_meta(ir, facade, &env, metadata, &EmitOptions::default(), &|_| {
        None
    })
}

/// Like [`emit_all`], but with explicit per-file [`EmitOptions`] (class version, source name) and a
/// caller-owned [`EmitRun`] the caller inspects after a `None` return (the inline-bail reason). The CLI
/// backend uses this so `-jvm-target` and the `SourceFile` name reach every emitted class.
pub fn emit_all_with_opts(
    ir: &IrFile,
    facade: &str,
    bodies: &dyn MethodBodies,
    metadata: Option<&KotlinMetadata>,
    opts: &EmitOptions,
    run: &EmitRun,
) -> Option<Vec<(String, Vec<u8>)>> {
    let env = EmitEnv { bodies, run };
    emit_all_with_class_meta(ir, facade, &env, metadata, opts, &|_| None)
}

/// Like [`emit_all`], but `class_meta` may supply a per-class `@kotlin.Metadata` (keyed by the class's
/// internal/fq name) attached to that emitted class. This lets a separately-compiled module expose its
/// classes' Kotlin signatures (member source params, etc.) so a dependent module resolves them — the
/// cross-module analogue of the facade `metadata`. OPT-IN: the default [`emit_all`] passes a provider
/// that returns `None` for every class, so krusty-core's emit is unchanged.
pub fn emit_all_with_class_meta(
    ir: &IrFile,
    facade: &str,
    env: &EmitEnv,
    metadata: Option<&KotlinMetadata>,
    opts: &EmitOptions,
    class_meta: &dyn Fn(&str) -> Option<KotlinMetadata>,
) -> Option<Vec<(String, Vec<u8>)>> {
    // Pass 1 (discovery): emit everything, recording which lambda impls actually get an `invokedynamic`
    // (`run.used_lambdas`). A lambda spliced by the inliner never emits one — its standalone `$lambda$N`
    // is dead, and kotlinc emits neither the method nor (for a class-only file) the facade holding it.
    env.run.used_lambdas.borrow_mut().clear();
    let empty = std::collections::HashSet::new();
    let first = emit_pass(
        ir,
        facade,
        env,
        metadata,
        opts,
        class_meta,
        &LambdaSelection {
            dead: &empty,
            rescued: &empty,
        },
    )?;
    let used = env.run.used_lambdas.borrow().clone();
    // A MUST-INLINE message lambda whose call-site splice FELL BACK to a real `invokedynamic`
    // (pass 1 recorded the use): its impl was pre-marked `inline_only` on the assumption the splice
    // always succeeds — emitting the reference without the method would be a broken class
    // (`NoSuchMethodError`). RESCUE it: re-emit with the impl method present. (A bare-return impl is
    // never rescued — it is not a valid standalone method — and is not in `must_inline_lambdas`.)
    let rescued: std::collections::HashSet<u32> = used
        .iter()
        .copied()
        .filter(|fid| ir.must_inline_lambdas.contains(fid))
        .collect();
    let class_member_fids: std::collections::HashSet<u32> = ir
        .classes
        .iter()
        .flat_map(|c| c.methods.iter().copied())
        .collect();
    // Dead = a FACADE-owned lambda impl (no receiver, not a class member — a class-owned or
    // suspend-state-machine lambda may be reached through paths discovery doesn't model) that no emitted
    // `invokedynamic` references. NB single iteration: an indy inside a dead lambda still marks its inner
    // lambda used, so a nested-dead chain keeps the inner method — rare, and strictly better than today.
    let dead: std::collections::HashSet<u32> = ir
        .lambda_own_params_from
        .keys()
        .filter(|&&fid| {
            !used.contains(&fid)
                && !ir.inline_only_fns.contains(&fid)
                && !class_member_fids.contains(&fid)
                && ir
                    .functions
                    .get(fid as usize)
                    .is_some_and(|f| f.dispatch_receiver.is_none())
                && !ir.suspend_lambda_sm.iter().any(|(f2, _, _)| *f2 == fid)
        })
        .copied()
        .collect();
    if dead.is_empty() && rescued.is_empty() {
        return Some(first);
    }
    // Pass 2: re-emit without the dead facade impls, plus any rescued must-inline impls
    // (deterministic — identical decisions, minus the dead methods, plus the rescued ones; the
    // facade itself drops when the dead impls were its only members).
    emit_pass(
        ir,
        facade,
        env,
        metadata,
        opts,
        class_meta,
        &LambdaSelection {
            dead: &dead,
            rescued: &rescued,
        },
    )
}

/// Which facade-owned lambda impls a pass drops (`dead`) or keeps despite a pre-marked inline
/// (`rescued`) — the only state that differs between emit pass 1 (both empty) and pass 2.
struct LambdaSelection<'a> {
    dead: &'a std::collections::HashSet<u32>,
    rescued: &'a std::collections::HashSet<u32>,
}

fn emit_pass(
    ir: &IrFile,
    facade: &str,
    env: &EmitEnv,
    metadata: Option<&KotlinMetadata>,
    opts: &EmitOptions,
    class_meta: &dyn Fn(&str) -> Option<KotlinMetadata>,
    lambdas: &LambdaSelection,
) -> Option<Vec<(String, Vec<u8>)>> {
    if !jvm_can_emit(ir) {
        return None;
    }
    *env.run.inline_bail.borrow_mut() = None;
    env.run.emit_bail.set(false);
    let mut out = Vec::new();
    // Facade: the static top-level functions (those with no dispatch receiver). A function that BELONGS
    // to a class — including a `static` member like the serialization plugin's `serializer()` accessor,
    // which has no dispatch receiver — is emitted on its class (below), NOT here; otherwise two classes'
    // same-signature statics (`C.serializer()`/`D.serializer()`) would collide on the facade.
    let class_member_fids: std::collections::HashSet<u32> = ir
        .classes
        .iter()
        .flat_map(|c| c.methods.iter().copied())
        .collect();
    let mut cw = new_writer(facade, "java/lang/Object", opts);
    // PRIVATE facade functions a CLASS body calls (`Callee::Local` from a lambda impl, a
    // continuation class, or any class member): a cross-class private invokestatic is illegal, so
    // kotlinc emits a `public static final synthetic access$<name>` forwarding bridge on the facade
    // and the class calls that (the `Callee::Local` emit arm does the routing).
    let facade_access_bridges: std::collections::HashSet<u32> = {
        let mut roots: Vec<crate::ir::ExprId> = Vec::new();
        for c in &ir.classes {
            for &fid in &c.methods {
                if let Some(b) = ir.functions.get(fid as usize).and_then(|f| f.body) {
                    roots.push(b);
                }
            }
            roots.extend(c.init_body);
            roots.extend(c.super_args.iter().copied());
            for sc in &c.secondary_ctors {
                roots.extend(sc.body);
                roots.extend(sc.delegate_args.iter().copied());
            }
            for en in &c.enum_entries {
                roots.extend(en.args.iter().copied());
            }
        }
        let mut out = std::collections::HashSet::new();
        let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let mut stack = roots;
        while let Some(cur) = stack.pop() {
            if !seen.insert(cur) {
                continue;
            }
            if let crate::ir::IrExpr::Call {
                callee: Callee::Local(fid),
                ..
            } = &ir.exprs[cur as usize]
            {
                if ir.private_methods.contains(fid) && !class_member_fids.contains(fid) {
                    out.insert(*fid);
                }
            }
            crate::ir::for_each_child(&ir.exprs, cur, &mut |ch| stack.push(ch));
        }
        // A function-reference class dispatching to a PRIVATE facade function (its `invoke` is
        // synthesized bytecode, not IR) needs the same bridge.
        for c in &ir.classes {
            if let Some(fr) = &c.func_ref {
                if fr.call_owner_is_facade() {
                    for (i, f) in ir.functions.iter().enumerate() {
                        if f.name == fr.call_name
                            && f.params.len() == fr.target_param_tys.len()
                            && f.dispatch_receiver.is_none()
                            && ir.private_methods.contains(&(i as u32))
                            && !class_member_fids.contains(&(i as u32))
                        {
                            out.insert(i as u32);
                        }
                    }
                }
            }
        }
        out
    };
    let mut facade_has_method = false;
    for (i, f) in ir.functions.iter().enumerate() {
        if class_member_fids.contains(&(i as u32)) {
            continue;
        }
        if f.dispatch_receiver.is_some() || f.body.is_none() {
            continue;
        }
        // An inline-only lambda impl is never emitted (it's spliced) — don't count it as a facade method,
        // else an otherwise class-only file emits an empty facade kotlinc omits. A DEAD lambda impl
        // (inlined at every use — pass-1 discovery) is dropped the same way.
        let rescued = lambdas.rescued.contains(&(i as u32));
        if (ir.inline_only_fns.contains(&(i as u32)) && !rescued)
            || lambdas.dead.contains(&(i as u32))
        {
            continue;
        }
        emit_method_maybe_rescued(ir, i as u32, facade, facade, &mut cw, false, env, rescued);
        facade_has_method = true;
        if facade_access_bridges.contains(&(i as u32)) {
            let param_tys = jvm_tys(&f.params);
            let ret = ir_ty_to_jvm(&f.ret);
            let desc = method_descriptor(&param_tys, ret);
            let words: u16 = param_tys.iter().map(|t| slot_words(*t)).sum();
            let mut g = CodeBuilder::new(words);
            let mut slot: u16 = 0;
            for &t in &param_tys {
                load(t, slot, &mut g);
                slot += slot_words(t);
            }
            let m = cw.methodref(facade, &f.name, &desc);
            let aw: i32 = words as i32;
            g.invokestatic(m, aw, slot_words(ret) as i32);
            emit_return(ret, &mut g);
            g.ensure_locals(words);
            g.link();
            cw.add_method(
                0x1019, /* PUBLIC | STATIC | FINAL | SYNTHETIC */
                &format!("access${}", f.name),
                &desc,
                &g,
            );
        }
        // A top-level function (or extension) with SIMPLE parameter defaults gets kotlinc's
        // `foo$default(params…, int mask, Object marker)` synthetic (dispatches to the real method,
        // filling the masked slots from the defaults), so an omitted-argument caller — same-file or
        // cross-module — resolves against the same ABI kotlinc emits. A value-class-mangled function or a
        // complex default (lambda / construction / spilled temp) is skipped (`toplevel_default_stub_safe`).
        if crate::ir::toplevel_default_stub_safe(ir, i as u32) {
            let defaults = ir.param_defaults(i as u32).unwrap();
            // A top-level function's `$default` marker is a plain `Object` (kotlinc's function ABI).
            emit_facade_default_stub(
                ir,
                i as u32,
                facade,
                &mut cw,
                defaults,
                env,
                Ty::obj("java/lang/Object"),
            );
        }
    }
    emit_statics(ir, facade, &mut cw, env);
    // kotlinc emits the `<File>Kt` facade class ONLY when the file has top-level callables/properties
    // (or a facade `@Metadata` payload). A file of only classes/objects gets no facade — emitting an
    // empty one is an ABI divergence (spurious extra class). A facade static is owner-less.
    let facade_has_static = ir.statics.iter().any(|s| s.is_facade_owned());
    let facade_needed = facade_has_method || facade_has_static || metadata.is_some();
    if facade_needed {
        if let Some(m) = metadata {
            cw.set_kotlin_metadata(m.k, &m.mv, m.xi, &m.d1, &m.d2);
        }
        out.push((facade.to_string(), cw.finish()));
    }
    // Each class — with its optional `@Metadata` (the provider returns `None` for the default emit).
    for c in &ir.classes {
        let fq_name = c.fq_name();
        let cm = class_meta(&fq_name);
        let mut extra: Vec<(String, Vec<u8>)> = Vec::new();
        out.push((
            fq_name,
            emit_class(ir, c, facade, env, opts, cm.as_ref(), &mut extra),
        ));
        // An interface's `$DefaultImpls` holder (its `name$default` synthetics), when any exist.
        out.extend(extra);
    }
    if env.run.inline_bail.borrow().is_some() {
        return None;
    }
    if env.run.emit_bail.get() {
        return None; // a value slot was never allocated (malformed IR) — skip, never miscompile
    }
    Some(out)
}

/// Whether the JVM backend can represent this IR. The JVM stdlib provides fixed-arity
/// `kotlin/jvm/functions/Function0..22`; a function type or lambda of higher arity needs a different
/// vararg representation krusty doesn't emit, so such a file is skipped — never miscompiled. This is a
/// JVM constraint (the language allows any arity), so it lives in the JVM emitter, not common lowering.
/// Map every `IrExpr::Variable`'s declaration index → its JVM type, across the whole file. `value_ty`
/// consults this so a `GetValue` of a slot whose `Variable` hasn't been emit-registered yet (e.g. an
/// inline-expansion result/`this` temp queried by a comparison before its block emits) still types
/// correctly, instead of falling back to `Ty::Error` and picking the wrong (reference) operator path.
fn collect_var_types(ir: &IrFile) -> HashMap<u32, Ty> {
    let mut m = HashMap::new();
    for e in &ir.exprs {
        if let IrExpr::Variable { index, ty, .. } = e {
            m.insert(*index, ir_ty_to_jvm(ty));
        }
    }
    m
}

/// Attach any user annotations recorded for `field` (by name) to the most recently added field.
fn apply_field_annotations(cw: &mut ClassWriter, c: &crate::ir::IrClass, field: &str) {
    if let Some(fa) = c.field_annotations.iter().find(|fa| fa.field == field) {
        cw.set_last_field_annotations(&fa.visible, &fa.invisible);
    }
}

pub(crate) fn jvm_can_emit(ir: &IrFile) -> bool {
    const UNSUPPORTED_STDLIB_VALUE_CLASSES: &[&str] = &["kotlin/UByte", "kotlin/UShort"];

    fn unsupported_stdlib_value_class(internal: &str) -> bool {
        UNSUPPORTED_STDLIB_VALUE_CLASSES.contains(&internal)
    }
    fn mentions_unsupported_stdlib_value_class(s: &str) -> bool {
        UNSUPPORTED_STDLIB_VALUE_CLASSES
            .iter()
            .any(|internal| s.contains(internal))
    }
    fn ty_ok(t: &Ty) -> bool {
        match t.non_null() {
            Ty::Fun(s) => s.params.len() <= 22 && s.params.iter().all(ty_ok) && ty_ok(&s.ret),
            Ty::Obj(internal, _) if unsupported_stdlib_value_class(&internal.render()) => false,
            Ty::Obj(_, type_args) => type_args.iter().all(ty_ok),
            _ => true,
        }
    }
    fn callee_ok(callee: &Callee) -> bool {
        match callee {
            Callee::Static {
                owner,
                name: _,
                descriptor,
                ..
            }
            | Callee::Virtual {
                owner,
                name: _,
                descriptor,
                ..
            }
            | Callee::Special {
                owner,
                name: _,
                descriptor,
                ..
            } => {
                !mentions_unsupported_stdlib_value_class(&owner.render())
                    && !mentions_unsupported_stdlib_value_class(descriptor)
            }
            Callee::CrossFileVirtual {
                owner, params, ret, ..
            } => {
                !mentions_unsupported_stdlib_value_class(&owner.render())
                    && params.iter().all(ty_ok)
                    && ty_ok(ret)
            }
            Callee::CrossFile { params, ret, .. } => params.iter().all(ty_ok) && ty_ok(ret),
            Callee::Local(_) | Callee::LocalDefault(_) | Callee::External(_) => true,
        }
    }
    fn generic_value_class_ok(ir: &IrFile, class_idx: usize) -> bool {
        let c = &ir.classes[class_idx];
        if !c.is_value || c.type_params.is_empty() {
            return true;
        }
        if c.fields.iter().any(|f| {
            matches!(
                f.ty.non_null(),
                Ty::Obj(n, _) if n.matches("java/lang/Comparable") || n.matches("kotlin/Comparable")
            )
        }) {
            return false;
        }
        true
    }
    if ir
        .functions
        .iter()
        .any(|f| !ty_ok(&f.ret) || !f.params.iter().all(ty_ok))
    {
        return false;
    }
    if ir.statics.iter().any(|s| !ty_ok(&s.ty)) {
        return false;
    }
    if !(0..ir.classes.len()).all(|idx| generic_value_class_ok(ir, idx)) {
        return false;
    }
    ir.exprs.iter().all(|e| match e {
        IrExpr::Lambda { arity, .. } => *arity <= 22,
        IrExpr::Variable { ty, .. } => ty_ok(ty),
        IrExpr::Call { callee, .. } => callee_ok(callee),
        // A plugin placeholder that reached emit means its owning plugin didn't run (or couldn't
        // specialize it) — decline the file rather than miscompile (the node has no JVM lowering).
        IrExpr::PluginPlaceholder { .. } => false,
        _ => true,
    })
}

/// Emit the facade's top-level properties as `public static` fields plus a `<clinit>` that runs
/// their initializers in declaration order.
/// Convert the inliner's `VType` (a relocated frame verification type) to the class-writer's
/// `VerifType`. `Uninitialized` types shouldn't reach here (`splice_unified` bails on them).
/// A method's `StackMapTable` frames resolved to byte offsets: `(offset, locals, stack)` each.
type ResolvedFrames = Vec<(usize, Vec<VerifType>, Vec<VerifType>)>;

/// The internal class name to `checkcast` a value to when narrowing an erased `Object` to `ty` — or
/// `None` when no narrowing is needed (`Object`/`Any`, a primitive, `Unit`/`Nothing`).
fn checkcast_internal(ty: Ty) -> Option<String> {
    match ty {
        Ty::String => Some("java/lang/String".to_string()),
        _ if ty.is_array() => Some(type_descriptor(ty)),
        Ty::Obj(n, _) if n != "java/lang/Object" && n != "kotlin/Any" => Some(n.to_string()),
        _ => None,
    }
}

fn vtype_to_verif(v: &crate::jvm::inline::VType) -> VerifType {
    use crate::jvm::inline::VType;
    match v {
        VType::Top => VerifType::Top,
        VType::Int => VerifType::Integer,
        VType::Float => VerifType::Float,
        VType::Long => VerifType::Long,
        VType::Double => VerifType::Double,
        VType::Null => VerifType::Null,
        VType::Object(idx) => VerifType::Object(*idx),
        VType::UninitThis | VType::Uninit(_) => VerifType::Top,
    }
}

/// Expand a COLLAPSED frame-locals list (long/double = one entry) to SLOT-indexed (long/double = the
/// type + a trailing `Top` filler), so per-slot overlays line up.
fn expand_collapsed_locals(collapsed: &[VerifType]) -> Vec<VerifType> {
    let mut out = Vec::with_capacity(collapsed.len());
    for v in collapsed {
        let wide = matches!(v, VerifType::Long | VerifType::Double);
        out.push(v.clone());
        if wide {
            out.push(VerifType::Top);
        }
    }
    out
}

/// Collapse a SLOT-indexed locals list back to the JVM `StackMapTable` form (long/double = one entry,
/// its second slot dropped).
fn collapse_locals(slots: &[VerifType]) -> Vec<VerifType> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < slots.len() {
        let wide = matches!(slots[i], VerifType::Long | VerifType::Double);
        out.push(slots[i].clone());
        i += if wide { 2 } else { 1 };
    }
    out
}

/// The constant-pool index for a `const val`'s `ConstantValue` attribute when its initializer is a
/// compile-time literal; `None` otherwise (then the field is initialized in `<clinit>` as before).
fn const_value_idx(ir: &IrFile, init: crate::ir::ExprId, cw: &mut ClassWriter) -> Option<u16> {
    use crate::ir::{IrConst, IrExpr};
    match ir.expr(init) {
        IrExpr::Const(c) => Some(match c {
            IrConst::Boolean(b) => cw.const_int(*b as i32),
            IrConst::Byte(v) => cw.const_int(*v as i32),
            IrConst::Short(v) => cw.const_int(*v as i32),
            IrConst::Int(v) => cw.const_int(*v),
            IrConst::Char(c) => cw.const_int(*c as i32),
            IrConst::Long(v) => cw.const_long(*v),
            IrConst::Float(v) => cw.const_float(*v),
            IrConst::Double(v) => cw.const_double(*v),
            IrConst::String(s) => cw.const_string(s),
            IrConst::Null => return None,
        }),
        _ => None,
    }
}

/// Whether `init` is a `ConstantValue`-eligible literal (mirrors [`const_value_idx`] without interning).
fn const_value_idx_peek(ir: &IrFile, init: crate::ir::ExprId) -> bool {
    matches!(ir.expr(init), crate::ir::IrExpr::Const(c) if !matches!(c, crate::ir::IrConst::Null))
}

fn emit_statics(ir: &IrFile, facade: &str, cw: &mut ClassWriter, env: &EmitEnv) {
    let bodies = env.bodies;
    // Statics OWNED by a specific class (a companion `const val`) are emitted on that class, not the
    // facade — see `emit_owned_consts`.
    let facade_statics: Vec<&crate::ir::IrStatic> =
        ir.statics.iter().filter(|s| s.is_facade_owned()).collect();
    if facade_statics.is_empty() {
        return;
    }
    for s in &facade_statics {
        // kotlinc: `const val` → `public static final`; a plain `val` → `private static final`; a `var`
        // → `private static` (mutated through the synthesized setter). The private field is read/written
        // directly only from within the facade; other classes go through the get/set accessors.
        let acc = if s.is_const {
            0x0019 // PUBLIC | STATIC | FINAL
        } else if s.is_var {
            0x000A // PRIVATE | STATIC
        } else {
            0x001A // PRIVATE | STATIC | FINAL
        };
        let desc = ir_type_desc(&s.ty);
        // A `const val` initialized by a compile-time literal carries a `ConstantValue` attribute (the
        // JVM initializes the field; its `<clinit>` store is omitted below) — byte-identical to kotlinc.
        if s.is_const {
            if let Some(cv) = const_value_idx(ir, s.init, cw) {
                cw.add_field_const(acc, &s.name, &desc, cv);
                continue;
            }
        }
        cw.add_field(acc, &s.name, &desc);
    }
    // Which statics a CLASS body (a different JVM class than the facade) reads/writes — a PRIVATE
    // top-level property has no public accessors, so those references need kotlinc's `access$get<X>$p` /
    // `access$set<X>$p` bridges (emitted below, only when actually referenced).
    let mut cross_get: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut cross_set: std::collections::HashSet<u32> = std::collections::HashSet::new();
    {
        let mut roots: Vec<u32> = Vec::new();
        for c in &ir.classes {
            for &fid in &c.methods {
                if let Some(b) = ir.functions.get(fid as usize).and_then(|f| f.body) {
                    roots.push(b);
                }
            }
            roots.extend(c.init_body);
            roots.extend(c.super_args.iter().copied());
            for sc in &c.secondary_ctors {
                roots.extend(sc.body);
                roots.extend(sc.delegate_args.iter().copied());
            }
            for en in &c.enum_entries {
                roots.extend(en.args.iter().copied());
            }
        }
        let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let mut stack = roots;
        while let Some(cur) = stack.pop() {
            if !seen.insert(cur) {
                continue;
            }
            match &ir.exprs[cur as usize] {
                IrExpr::GetStatic(i) => {
                    cross_get.insert(*i);
                }
                IrExpr::SetStatic { index, .. } => {
                    cross_set.insert(*index);
                }
                _ => {}
            }
            crate::ir::for_each_child(&ir.exprs, cur, &mut |ch| stack.push(ch));
        }
    }
    // Accessors: a plain top-level `val`/`var` gets a `public static final getX()` (and `setX()` for a
    // `var`), so other classes read/write it the way kotlinc compiles cross-file property access. A
    // `const val` is `public static final` with no accessor (kotlinc inlines const reads). A PRIVATE
    // property gets NO public accessors — only the `access$…$p` bridges, and only when referenced.
    for (sidx, s) in ir
        .statics
        .iter()
        .enumerate()
        .filter(|(_, s)| s.is_facade_owned())
    {
        // A `const val` inlines (no accessor); a CUSTOM-accessor property emits its `getX`/`setX` as
        // ordinary facade methods (from `ir.functions`), so skip the trivial auto-accessor here.
        if s.is_const || s.custom_accessor {
            continue;
        }
        let jt = ir_ty_to_jvm(&s.ty);
        let desc = type_descriptor(jt);
        if s.visibility.is_private() {
            if cross_get.contains(&(sidx as u32)) {
                let mut g = CodeBuilder::new(0);
                let fref = cw.fieldref(facade, &s.name, &desc);
                g.getstatic(fref, slot_words(jt) as i32);
                emit_return(jt, &mut g);
                g.ensure_locals(0);
                g.link();
                cw.add_method(
                    0x1019, /* PUBLIC | STATIC | FINAL | SYNTHETIC */
                    &format!("access${}$p", property_getter_name(&s.name)),
                    &format!("(){desc}"),
                    &g,
                );
            }
            if s.is_var && cross_set.contains(&(sidx as u32)) {
                let words = slot_words(jt);
                let mut st = CodeBuilder::new(words);
                load(jt, 0, &mut st);
                let fref = cw.fieldref(facade, &s.name, &desc);
                st.putstatic(fref, slot_words(jt) as i32);
                st.ret_void();
                st.ensure_locals(words);
                st.link();
                cw.add_method(
                    0x1019,
                    &format!("access${}$p", property_setter_name(&s.name)),
                    &format!("({desc})V"),
                    &st,
                );
            }
            continue;
        }
        let mut g = CodeBuilder::new(0);
        let fref = cw.fieldref(facade, &s.name, &desc);
        g.getstatic(fref, slot_words(jt) as i32);
        emit_return(jt, &mut g);
        finish_code::<0x0019>(
            cw,
            &property_getter_name(&s.name),
            &format!("(){desc}"),
            &mut g,
            0,
        );
        if s.is_var {
            let words = slot_words(jt);
            let mut st = CodeBuilder::new(words);
            // kotlinc guards a non-null reference setter parameter with checkNotNullParameter("<set-?>").
            if jt.is_reference() && !ir_ty_nullable(&s.ty) {
                st.aload(0);
                st.push_string("<set-?>", cw);
                let m = cw.methodref(
                    "kotlin/jvm/internal/Intrinsics",
                    "checkNotNullParameter",
                    "(Ljava/lang/Object;Ljava/lang/String;)V",
                );
                st.invokestatic(m, 2, 0);
            }
            load(jt, 0, &mut st);
            let fref = cw.fieldref(facade, &s.name, &desc);
            st.putstatic(fref, slot_words(jt) as i32);
            st.ret_void();
            finish_code::<0x0019>(
                cw,
                &property_setter_name(&s.name),
                &format!("({desc})V"),
                &mut st,
                words,
            );
        }
    }
    let mut e = Emitter {
        ir,
        cw,
        bodies,
        run: env.run,
        owner: facade.to_string(),
        facade: facade.to_string(),
        slots: HashMap::new(),
        var_types: collect_var_types(ir),
        next_slot: 0,
        ret: Ty::Unit,
        loop_stack: Vec::new(),
        pending_stack: Vec::new(),
    };
    let mut code = CodeBuilder::new(0);
    let mut any_init = false;
    for s in &facade_statics {
        // A `const val` folded into a `ConstantValue` attribute (literal init) is initialized by the JVM
        // — kotlinc emits no `<clinit>` store for it, so skip it here too (byte-identical).
        if s.is_const && const_value_idx_peek(ir, s.init) {
            continue;
        }
        any_init = true;
        e.emit_value(s.init, &mut code);
        let jt = ir_ty_to_jvm(&s.ty);
        let fref = e.cw.fieldref(facade, &s.name, &type_descriptor(jt));
        code.putstatic(fref, slot_words(jt) as i32);
    }
    // When every static is a `ConstantValue`-folded `const val`, there is nothing to initialize —
    // kotlinc emits NO `<clinit>` at all (not an empty one), so skip it.
    if !any_init {
        return;
    }
    code.ret_void();
    finish_code::<0x0008>(e.cw, "<clinit>", "()V", &mut code, e.next_slot);
}

fn emit_class(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    facade: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
    class_meta: Option<&KotlinMetadata>,
    extra: &mut Vec<(String, Vec<u8>)>,
) -> Vec<u8> {
    let bodies = env.bodies;
    if !c.enum_entries.is_empty() {
        return emit_enum_class(ir, c, facade, env, opts);
    }
    if let Some(iface) = &c.annotation_impl_of {
        return emit_annotation_impl_class(c, &iface.render(), opts);
    }
    if c.is_annotation {
        return emit_annotation_class(c, opts, class_meta);
    }
    if c.is_interface {
        return emit_interface_class(ir, c, facade, env, opts, class_meta, extra);
    }
    if let Some(user_tys) = &c.enum_entry_of {
        return emit_enum_entry_subclass(ir, c, facade, env, opts, user_tys);
    }
    if c.prop_ref.is_some() {
        return emit_prop_ref_class(c, facade, opts);
    }
    if c.func_ref.is_some() {
        return emit_func_ref_class(ir, c, facade, opts);
    }
    let fq_name = c.fq_name();
    let superclass = c.superclass();
    let mut cw = new_writer(&fq_name, &superclass, opts);
    register_inner_classes(&mut cw, ir);
    // Seed the constant pool in kotlinc's interning order for a plain property class that will carry a
    // computed `@Metadata` + debug tables — so the emitted class is byte-identical, not just
    // structurally equal. Gated exactly like the debug tables (opt-in, non-data, qualifying shape).
    // A cross-module `class_meta` PROVIDER record (none exists today) deliberately does NOT seed:
    // provider metadata makes the class correct, not byte-identical — byte identity is only claimed
    // for the computed path this gate mirrors.
    // For a generic class, the `<init>` carries a `Signature` whose type-parameter params read `T<tp>;`
    // (`class Box<T>(var a: T)` → `(TT;)V`); a PARAMETERIZED concrete-type param reads its full generic
    // signature (`List<String>` → `Ljava/util/List<Ljava/lang/String;>;`). `None` when no param needs it.
    // Computed once here: the pool seeder interns it and the `<init>` emission attaches it.
    let ctor_signature: Option<String> = class_ctor_generic_sig(ir, c, &fq_name);
    if opts.emit_class_metadata && build_class_metadata(ir, c, opts).is_some() {
        seed_plain_class_pool(
            ir,
            c,
            &fq_name,
            &superclass,
            ctor_signature.as_deref(),
            &mut cw,
        );
    }
    // Access: an extended or abstract class must not be `final`; a class with an abstract method
    // (body `None`) is `ACC_ABSTRACT`.
    let extended = ir.classes.iter().any(|o| o.superclass_matches(&fq_name));
    let has_abstract = c
        .methods
        .iter()
        .any(|&fid| ir.functions[fid as usize].body.is_none());
    // A synthesized `$fn$1` continuation class is PACKAGE-PRIVATE in kotlinc (`0x0030` FINAL|SUPER) —
    // it is only touched by its own file's classes. Detected by its superclass; everything about its
    // member access follows kotlinc's continuation layout below.
    let is_continuation = c.superclass_matches("kotlin/coroutines/jvm/internal/ContinuationImpl");
    let mut access = if is_continuation {
        0x0020 // SUPER (package-private)
    } else {
        0x0001 | 0x0020 // PUBLIC | SUPER
    };
    // A SEALED class is abstract (kotlinc: sealed implies no direct instantiation), and an
    // `abstract class` is too — both alongside any class with an abstract (body-less) member.
    let is_abstract = has_abstract || c.is_sealed || c.is_abstract;
    if !extended && !is_abstract && !c.is_open {
        access |= 0x0010;
    } // FINAL
    if is_abstract {
        access |= 0x0400;
    } // ABSTRACT
    if ir.is_synthetic_class(&fq_name) {
        access |= 0x1000;
    } // ACC_SYNTHETIC (a `$$serializer` object)
    cw.set_access(access);
    if ir.is_deprecated_class(&fq_name) {
        cw.set_deprecated();
    } // Deprecated attribute (a HIDDEN-deprecated `$$serializer` object)
    let raw_class_sig = ir.class_signature(&fq_name);
    let jvm_sig = raw_class_sig.and_then(jvm_class_signature);
    crate::trace_compiler!(
        "value_classes",
        "class {} signature: raw={:?} jvm={:?}",
        fq_name,
        raw_class_sig,
        jvm_sig
    );
    if let Some(s) = &jvm_sig {
        cw.set_signature(s);
    }
    for itf in c.interfaces.iter_rendered() {
        cw.add_interface(&itf);
    }
    // Public fields (the IR slice reads them cross-class directly; kotlinc uses private + getters —
    // an ABI refinement, not a runtime difference).
    // Backing fields are private; access goes through the synthesized `getX()`/`setX()` accessors
    // (kotlinc does the same) — for both normal classes and objects.
    for field in c.fields.iter() {
        let name = &field.name;
        let ty = &field.ty;
        // Map the field's (platform-neutral) visibility to JVM access flags: a `private` field →
        // `ACC_PRIVATE` (the default — Kotlin backing fields are private, reached via accessors); a
        // non-private field → `ACC_PUBLIC` (read/written cross-class, e.g. a coroutine continuation's
        // `result`/`label`).
        let private = field.is_private;
        let acc = if is_continuation {
            // kotlinc's continuation field layout: everything package-private; `result` is SYNTHETIC,
            // the captured receiver `this$0` is FINAL|SYNTHETIC; `label` and the `L$N` spills are plain.
            match name.as_str() {
                "result" => 0x1000,
                "this$0" => 0x1010,
                _ => 0x0000,
            }
        } else {
            (if private { 0x0002 } else { 0x0001 }) | if field.is_final { 0x0010 } else { 0 }
        };
        // A field typed by a bare type parameter (`val a: A`) carries a `Signature` (`TA;`); a
        // PARAMETERIZED concrete type (`val xs: List<String>`) carries its full generic signature. Both
        // like kotlinc; disjoint (a field is one or the other).
        let field_sig = ir
            .field_signatures(&fq_name)
            .and_then(|fs| fs.iter().find(|(fname, _)| fname == name))
            .map(|(_, tp)| format!("T{tp};"))
            .or_else(|| parameterized_sig(ty));
        cw.add_field_sig(acc, name, &ir_type_desc(ty), field_sig.as_deref());
    }
    // A `companion object`'s `const val`s live on THIS (outer) class as `public static final` +
    // `ConstantValue` fields (kotlinc's layout); they have no `<clinit>` store (the JVM initializes them).
    for s in ir.statics.iter().filter(|s| s.owner_matches(&fq_name)) {
        let desc = ir_type_desc(&s.ty);
        // A `private const val`/`private val` on an object/companion keeps its declared visibility
        // (kotlinc: PRIVATE static final; const reads are inlined so no cross-class getstatic needs it).
        let acc = if s.visibility.is_private() {
            0x001A // PRIVATE | STATIC | FINAL
        } else {
            0x0019 // PUBLIC | STATIC | FINAL
        };
        if let Some(cv) = const_value_idx(ir, s.init, &mut cw) {
            cw.add_field_const(acc, &s.name, &desc, cv);
        } else {
            cw.add_field(acc, &s.name, &desc);
        }
    }
    // Constructor: super(); store each ctor *parameter* into its field; then run `init_body`
    // (body-property initializers + `init {}` blocks). Fields past `ctor_param_count` are body
    // properties — not parameters — so the descriptor covers only the leading parameter fields.
    // The constructor takes ALL primary-ctor params (`ctor_args`), in declaration order — `val`/`var`
    // params back a field, plain params are arguments only. (Synthesized classes have empty `ctor_args`
    // and fall back to the leading `ctor_param_count` fields.)
    let param_tys = class_ctor_jvm_tys(c);
    // A class with NO primary constructor emits no primary `<init>` — every `<init>` comes from a
    // secondary constructor (below). Otherwise emit the primary `<init>` here.
    if c.has_primary_ctor {
        let params_words: u16 = param_tys.iter().map(|t| slot_words(*t)).sum();
        let mut ctor = CodeBuilder::new(1 + params_words);
        // The superclass constructor's parameter types (empty for the erased top type — the front end
        // names it `kotlin/Any`, which this backend maps to `java/lang/Object`).
        let mut super_param_tys: Vec<Ty> =
            if crate::jvm::jvm_class_map::to_jvm_internal(&superclass) == "java/lang/Object" {
                Vec::new()
            } else {
                ir.classes
                    .iter()
                    .find(|sc| sc.fq_name_matches(&superclass))
                    .map(class_ctor_jvm_tys)
                    .unwrap_or_default()
            };
        let max_slot;
        let mut init_diverges = false;
        {
            let mut e = Emitter {
                ir,
                cw: &mut cw,
                bodies,
                run: env.run,
                owner: fq_name.clone(),
                facade: facade.to_string(),
                slots: HashMap::new(),
                var_types: collect_var_types(ir),
                next_slot: 1 + params_words,
                ret: Ty::Unit,
                loop_stack: Vec::new(),
                pending_stack: Vec::new(),
            };
            e.slots.insert(0, (0, Ty::obj(&fq_name)));
            let mut s = 1u16;
            for (vi, t) in param_tys.iter().enumerate() {
                e.slots.insert(vi as u32 + 1, (s, *t));
                s += slot_words(*t);
            }
            // A classpath superclass (not an IR class) with `super(args)`: the IR-class lookup above
            // found no parameter types, so derive the super constructor's descriptor from the argument
            // expressions themselves (e.g. a synthesized coroutine continuation's `super(completion)`).
            if super_param_tys.is_empty() && !c.super_args.is_empty() {
                super_param_tys = c.super_args.iter().map(|&a| e.value_ty(a)).collect();
            }
            // kotlinc guards each non-null reference constructor parameter with checkNotNullParameter at
            // the very start of `<init>` — before the super() call.
            for (i, a) in c.ctor_args.iter().enumerate() {
                if let Some(name) = &a.check {
                    if let Some(&(slot, _)) = e.slots.get(&(i as u32 + 1)) {
                        ctor.aload(slot);
                        ctor.push_string(name, e.cw);
                        let m = e.cw.methodref(
                            "kotlin/jvm/internal/Intrinsics",
                            "checkNotNullParameter",
                            "(Ljava/lang/Object;Ljava/lang/String;)V",
                        );
                        ctor.invokestatic(m, 2, 0);
                    }
                }
            }
            // An inner class stores its captured outer instance (`this$0`, field 0) BEFORE `super(…)`,
            // so a super-constructor argument can read the outer instance (`inner class Inner :
            // Base(run { outerProp })`) — kotlinc emits the same. A `putfield` of the current class's own
            // field on the still-uninitialized `this` is legal per JVMS 4.10.2.4.
            let stores_this0 = c.fields.first().is_some_and(|f0| f0.name == "this$0");
            if stores_this0 {
                let f0 = &c.fields[0];
                ctor.aload(0);
                ctor.aload(1); // the outer instance = first constructor parameter
                let fref = e.cw.fieldref(&fq_name, "this$0", &type_descriptor(f0.ty));
                ctor.putfield(fref, slot_words(f0.ty) as i32);
            }
            // `super(args)` — `this` is loaded first, so spill any branchy arg to temps before it.
            let super_args = c.super_args.clone();
            if super_args.iter().any(|&a| e.records_frame(a)) {
                let temps = e.spill_to_temps(&super_args, &mut ctor);
                ctor.aload(0);
                for &(slot, t, _) in &temps {
                    load(t, slot, &mut ctor);
                }
                for &(_, _, key) in &temps {
                    e.slots.remove(&key);
                }
            } else {
                ctor.aload(0);
                for &a in &super_args {
                    e.emit_value(a, &mut ctor);
                }
            }
            // A base whose primary ctor takes a value-class param — or a SEALED base — has a PRIVATE
            // primary; a subclass `super(…)` must reach it through the PUBLIC|SYNTHETIC
            // `(…args, DefaultConstructorMarker)` accessor (a trailing `null` marker), never the
            // inaccessible private primary.
            let super_accessor = e.ir.has_value_param_ctor(&superclass)
                || e.ir
                    .classes
                    .iter()
                    .any(|o| o.fq_name_matches(&superclass) && o.is_sealed);
            let mut super_param_tys = super_param_tys.clone();
            if super_accessor {
                ctor.aconst_null();
                super_param_tys.push(Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker"));
            }
            let aw: i32 = super_param_tys.iter().map(|t| slot_words(*t) as i32).sum();
            let super_init = e.cw.methodref(
                &superclass,
                "<init>",
                &method_descriptor(&super_param_tys, Ty::Unit),
            );
            ctor.invokespecial(super_init, aw, 0);
            // Store this class's own primary-constructor parameter fields: each `val`/`var` param's arg is
            // stored to its field (the property fields are `fields[0..]` in declaration order among params);
            // a plain param is skipped (it stays a local for the initializer body). `is_field` flags come
            // from `ctor_args`; a synthesized class (empty `ctor_args`) stores all leading param fields.
            // SKIPPED when `explicit_param_stores` is set — a desugared class already stores them via
            // explicit `SetField`s at the head of `init_body`; auto-storing too would double-store.
            if !c.explicit_param_stores {
                let mut slot = 1u16;
                let mut field_i = 0usize;
                let is_field: Vec<bool> = if c.ctor_args.is_empty() {
                    vec![true; param_tys.len()]
                } else {
                    c.ctor_args.iter().map(|a| a.is_field).collect()
                };
                for (i, t) in param_tys.iter().enumerate() {
                    if is_field.get(i).copied().unwrap_or(true) {
                        let name = &c.fields[field_i].name;
                        // `this$0` is already stored BEFORE `super(…)` above — don't store it again.
                        if name != "this$0" {
                            ctor.aload(0);
                            load(*t, slot, &mut ctor);
                            let fref = e.cw.fieldref(&fq_name, name, &type_descriptor(*t));
                            ctor.putfield(fref, slot_words(*t) as i32);
                        }
                        field_i += 1;
                    }
                    slot += slot_words(*t);
                }
            }
            if let Some(init_body) = c.init_body {
                e.emit(init_body, &mut ctor);
                init_diverges = e.diverges(init_body);
            }
            max_slot = e.next_slot;
        }
        // A diverging `init` (e.g. `init { throw … }`) leaves no fall-through — the trailing `return`
        // would be dead code after `athrow` (which the verifier rejects without a frame).
        if !init_diverges {
            ctor.ret_void();
        }
        ctor.ensure_locals(max_slot);
        ctor.link();
        // An `object`'s constructor is private; a `@JvmInline value class`'s is private too (instances are
        // created via `constructor-impl`/`box-impl`, never `new`); a class whose primary ctor takes a
        // value-class-typed parameter is private too (kotlinc routes construction through a synthetic
        // `(…args, DefaultConstructorMarker)` accessor — emitted below); a `C$Companion`'s is
        // package-private (so the outer class's `<clinit>` can call it without nestmate attributes); a
        // normal class's is public.
        let value_param_ctor = ir.has_value_param_ctor(&fq_name);
        // A SEALED class's primary ctor is private too — subclasses (and Java/reflection) construct
        // through the PUBLIC|SYNTHETIC `(…args, DefaultConstructorMarker)` accessor (kotlinc's shape).
        let ctor_access =
            if c.is_object || c.is_value || value_param_ctor || c.is_companion || c.is_sealed {
                // A companion's real ctor is PRIVATE too — the outer `<clinit>` constructs it through the
                // PUBLIC|SYNTHETIC `(DefaultConstructorMarker)` accessor emitted below (kotlinc's shape).
                0x0002
            } else if is_continuation {
                // A continuation class's ctor is package-private (constructed only by its own file).
                0x0000
            } else {
                0x0001
            };
        cw.add_method_sig(
            ctor_access,
            "<init>",
            &method_descriptor(&param_tys, Ty::Unit),
            &ctor,
            ctor_signature.as_deref(),
        );
        // A default on any primary-ctor parameter → kotlinc's synthetic
        // `<init>(params…, int mask, DefaultConstructorMarker)` overload (fills the masked slots from the
        // defaults, then `invokespecial` the real `<init>`).
        if let Some(defaults) = ir.class_ctor_defaults(&fq_name) {
            emit_ctor_default_stub(ir, &fq_name, &param_tys, defaults, &mut cw, env);
            // EVERY parameter defaulted → kotlinc also emits the no-arg convenience `<init>()`
            // (`AuditFilters()` in Java/reflection), delegating to the `$default` overload with a
            // full mask.
            if !param_tys.is_empty()
                && defaults.len() == param_tys.len()
                && defaults.iter().all(Option::is_some)
                && !c.is_sealed
                && ctor_access == 0x0001
            {
                let mut z = CodeBuilder::new(1);
                z.aload(0);
                for &t in &param_tys {
                    push_zero(t, &mut z, &mut cw);
                }
                for mask in full_default_masks(param_tys.len()) {
                    z.push_int(mask, &mut cw);
                }
                z.aconst_null();
                let mut stub_params = param_tys.clone();
                stub_params.extend(std::iter::repeat_n(
                    Ty::Int,
                    default_mask_count(param_tys.len()),
                ));
                stub_params.push(Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker"));
                let aw: i32 = 1 + stub_params
                    .iter()
                    .map(|t| slot_words(*t) as i32)
                    .sum::<i32>();
                let m = cw.methodref(
                    &fq_name,
                    "<init>",
                    &method_descriptor(&stub_params, Ty::Unit),
                );
                z.invokespecial(m, aw, 0);
                z.ret_void();
                z.ensure_locals(1);
                z.link();
                cw.add_method(0x0001, "<init>", "()V", &z);
            }
        }
        // A value-class-param primary ctor is private (above); kotlinc exposes a PUBLIC|SYNTHETIC accessor
        // `<init>(…args, DefaultConstructorMarker)` that simply delegates to it, so Java/reflection can
        // still construct the class.
        if value_param_ctor || c.is_companion || c.is_sealed {
            emit_ctor_marker_accessor(&fq_name, &param_tys, &mut cw);
        }
    } // end `if c.has_primary_ctor`

    // Secondary constructors: each `<init>(p)` delegates (via `this(…)` to an own `<init>`, or via
    // `super(…)` to the base `<init>`) then runs its body. A `super(…)`-reaching ctor's `body` already
    // has the class init steps prepended (the lowering does that). `this` is slot 0, parameters follow.
    for sc in &c.secondary_ctors {
        let sc_param_tys = jvm_tys(&sc.params);
        let sc_words: u16 = sc_param_tys.iter().map(|t| slot_words(*t)).sum();
        let mut sctor = CodeBuilder::new(1 + sc_words);
        let sec_max;
        let mut sec_diverges = false;
        {
            let mut e = Emitter {
                ir,
                cw: &mut cw,
                bodies,
                run: env.run,
                owner: fq_name.clone(),
                facade: facade.to_string(),
                slots: HashMap::new(),
                var_types: collect_var_types(ir),
                next_slot: 1 + sc_words,
                ret: Ty::Unit,
                loop_stack: Vec::new(),
                pending_stack: Vec::new(),
            };
            e.slots.insert(0, (0, Ty::obj(&fq_name)));
            let mut s = 1u16;
            for (vi, t) in sc_param_tys.iter().enumerate() {
                e.slots.insert(vi as u32 + 1, (s, *t));
                s += slot_words(*t);
            }
            // Delegation target: `this(…)` → an own `<init>(target_params)`; `super(…)` → the base
            // `<init>(super_params)`. `this` is loaded first, so spill any branchy arg to a temp before.
            use crate::ir::CtorDelegateTarget;
            let (target_class, target_jvm_tys): (String, Vec<Ty>) = match &sc.delegate {
                // `this(…)` targets an own `<init>` — the primary OR a sibling secondary. A delegation
                // to the PRIMARY uses its LIVE signature `param_tys` (already rewritten by any IR→IR
                // pass, e.g. value-class erasure of a value-class-typed ctor param); a sibling target
                // uses the lower-time `target_params` (the sibling's own `<init>` descriptor).
                CtorDelegateTarget::This {
                    target_params,
                    to_primary,
                } => (
                    fq_name.clone(),
                    if *to_primary {
                        param_tys.clone()
                    } else {
                        jvm_tys(target_params)
                    },
                ),
                // `super(…)` targets the base `<init>`, whose signature is read LIVE from the base
                // class's (post-transform) ctor — mirrors the primary path, so any IR→IR pass that
                // rewrote the base ctor's parameter types (e.g. value-class erasure) is reflected here.
                // A base with no primary constructor exposes only SECONDARY `<init>`s; pick the one
                // whose parameter count matches this `super(...)`'s arguments (the lowering already
                // validated a unique match).
                CtorDelegateTarget::Super => {
                    let owner = crate::jvm::jvm_class_map::to_jvm_internal(&superclass).to_string();
                    let tys: Vec<Ty> = if owner == "java/lang/Object" {
                        Vec::new()
                    } else if let Some(base) =
                        ir.classes.iter().find(|sc| sc.fq_name_matches(&superclass))
                    {
                        let argc = sc.delegate_args.len();
                        let mut cands: Vec<Vec<Ty>> = Vec::new();
                        if base.has_primary_ctor {
                            cands.push(class_ctor_jvm_tys(base));
                        }
                        for bsc in &base.secondary_ctors {
                            cands.push(jvm_tys(&bsc.params));
                        }
                        let unique: Vec<&Vec<Ty>> =
                            cands.iter().filter(|p| p.len() == argc).collect();
                        if unique.len() == 1 {
                            unique[0].clone()
                        } else {
                            class_ctor_jvm_tys(base)
                        }
                    } else {
                        Vec::new()
                    };
                    (owner, tys)
                }
            };
            let dargs = sc.delegate_args.clone();
            if dargs.iter().any(|&a| e.records_frame(a)) {
                let temps = e.spill_to_temps(&dargs, &mut sctor);
                sctor.aload(0);
                for &(slot, t, _) in &temps {
                    load(t, slot, &mut sctor);
                }
                for &(_, _, key) in &temps {
                    e.slots.remove(&key);
                }
            } else {
                sctor.aload(0);
                for &a in &dargs {
                    e.emit_value(a, &mut sctor);
                }
            }
            // A cross-class delegation target (`super(…)` to a base) whose primary ctor takes a value-class
            // param has a PRIVATE primary — reach it through the `(…args, DefaultConstructorMarker)`
            // accessor. A same-class `this(…)` to the own private primary stays direct (accessible).
            let mut target_jvm_tys = target_jvm_tys;
            let target_sealed = target_class != fq_name
                && e.ir
                    .classes
                    .iter()
                    .any(|o| o.fq_name_matches(&target_class) && o.is_sealed);
            if (target_class != fq_name && e.ir.has_value_param_ctor(&target_class))
                || target_sealed
            {
                sctor.aconst_null();
                target_jvm_tys.push(Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker"));
            }
            let aw: i32 = target_jvm_tys.iter().map(|t| slot_words(*t) as i32).sum();
            let delegate_init = e.cw.methodref(
                &target_class,
                "<init>",
                &method_descriptor(&target_jvm_tys, Ty::Unit),
            );
            sctor.invokespecial(delegate_init, aw, 0);
            if let Some(body) = sc.body {
                e.emit(body, &mut sctor);
                sec_diverges = e.diverges(body);
            }
            sec_max = e.next_slot;
        }
        if !sec_diverges {
            sctor.ret_void();
        }
        sctor.ensure_locals(sec_max);
        sctor.link();
        // A SEALED class's secondary ctor is private too, with its own PUBLIC
        // `(…args, DefaultConstructorMarker)` accessor (kotlinc: EVERY sealed ctor pairs with one).
        let sc_access =
            (if c.is_sealed { 0x0002 } else { 0x0001 }) | if sc.synthetic { 0x1000 } else { 0 };
        cw.add_method(
            sc_access,
            "<init>",
            &method_descriptor(&sc_param_tys, Ty::Unit),
            &sctor,
        );
        if c.is_sealed {
            emit_ctor_marker_accessor(&fq_name, &sc_param_tys, &mut cw);
        }
    }
    // A class with a `companion object`: a `public static final Companion` field of the companion
    // type, constructed in this class's `<clinit>`.
    // A class with a `companion object` gets a `public static final Companion` field constructed in
    // `<clinit>`; a non-const companion `val` (a static field on this class whose initializer is not a
    // compile-time literal) is initialized in the SAME `<clinit>` (the `ConstantValue` path covers only
    // folded `const val`s). Both share one `<clinit>` so it is never emitted twice.
    {
        let clinit_statics: Vec<&crate::ir::IrStatic> = ir
            .statics
            .iter()
            .filter(|s| {
                s.owner_matches(&fq_name) && !(s.is_const && const_value_idx_peek(ir, s.init))
            })
            .collect();
        if let Some(comp_fq) = c.companion_class() {
            cw.add_field(0x0019, "Companion", &format!("L{comp_fq};")); // PUBLIC | STATIC | FINAL
        }
        if c.companion_class.is_some() || !clinit_statics.is_empty() {
            let mut e = Emitter {
                ir,
                cw: &mut cw,
                bodies,
                run: env.run,
                owner: fq_name.clone(),
                facade: facade.to_string(),
                slots: HashMap::new(),
                var_types: collect_var_types(ir),
                next_slot: 0,
                ret: Ty::Unit,
                loop_stack: Vec::new(),
                pending_stack: Vec::new(),
            };
            let mut clinit = CodeBuilder::new(0);
            if let Some(comp_fq) = c.companion_class() {
                let comp_desc = format!("L{comp_fq};");
                let ci = e.cw.class_ref(&comp_fq);
                clinit.new_obj(ci);
                clinit.dup();
                // The companion's real ctor is PRIVATE (kotlinc); construct through its
                // PUBLIC|SYNTHETIC `(DefaultConstructorMarker)` accessor with a null marker.
                clinit.aconst_null();
                let init = e.cw.methodref(
                    &comp_fq,
                    "<init>",
                    "(Lkotlin/jvm/internal/DefaultConstructorMarker;)V",
                );
                clinit.invokespecial(init, 1, 0);
                let fref = e.cw.fieldref(&fq_name, "Companion", &comp_desc);
                clinit.putstatic(fref, 1);
            }
            for s in &clinit_statics {
                e.emit_value(s.init, &mut clinit);
                let jt = ir_ty_to_jvm(&s.ty);
                let fref = e.cw.fieldref(&fq_name, &s.name, &type_descriptor(jt));
                clinit.putstatic(fref, slot_words(jt) as i32);
            }
            clinit.ret_void();
            clinit.ensure_locals(e.next_slot);
            clinit.link();
            e.cw.add_method(0x0008, "<clinit>", "()V", &clinit);
        }
    }
    // A singleton `object`: a `public static final INSTANCE` built in `<clinit>`.
    if c.is_object {
        let self_desc = format!("L{};", fq_name);
        cw.add_field(0x0019, "INSTANCE", &self_desc); // PUBLIC | STATIC | FINAL
        let mut clinit = CodeBuilder::new(0);
        let ci = cw.class_ref(&fq_name);
        clinit.new_obj(ci);
        clinit.dup();
        let init = cw.methodref(&fq_name, "<init>", "()V");
        clinit.invokespecial(init, 0, 0);
        let fref = cw.fieldref(&fq_name, "INSTANCE", &self_desc);
        clinit.putstatic(fref, 1);
        clinit.ret_void();
        clinit.ensure_locals(0);
        clinit.link();
        cw.add_method(0x0008, "<clinit>", "()V", &clinit);
    }
    // Instance methods (concrete emitted; abstract declared with `ACC_ABSTRACT`, no Code).
    // kotlinc emits a property's ACCESSORS before the class's declared functions (source order: ctor
    // properties precede the body). The IR appends synthesized accessors after the declared methods, so
    // reorder here — accessors first (in field order), then everything else in IR order.
    let accessor_order: Vec<String> = c
        .fields
        .iter()
        .flat_map(|f| {
            let cn = {
                let mut ch = f.name.chars();
                ch.next()
                    .map(|x| x.to_uppercase().collect::<String>() + ch.as_str())
                    .unwrap_or_default()
            };
            [format!("get{cn}"), format!("set{cn}")]
        })
        .collect();
    let mut ordered: Vec<u32> = Vec::with_capacity(c.methods.len());
    for name in &accessor_order {
        ordered.extend(
            c.methods
                .iter()
                .copied()
                .filter(|&fid| &ir.functions[fid as usize].name == name),
        );
    }
    ordered.extend(
        c.methods
            .iter()
            .copied()
            .filter(|&fid| !accessor_order.contains(&ir.functions[fid as usize].name)),
    );
    for &fid in &ordered {
        let f = &ir.functions[fid as usize];
        if f.body.is_some() {
            // A `static` member (e.g. a value class's `box-impl`/`constructor-impl`) emits with no
            // `this` slot; an ordinary member is an instance method.
            emit_method(ir, fid, &fq_name, facade, &mut cw, !f.is_static, env);
        } else {
            cw.add_abstract_method(0x0001 | 0x0400, &f.name, &ir_method_desc(&f.params, &f.ret));
        }
        // A method with default-valued parameters gets a `<name>$default(…, mask, marker)` synthetic stub
        // (the JVM realization of default arguments). A STATIC method (a value class's `constructor-impl`)
        // has no `self`, so it uses the facade-style stub keyed on the class as owner; an instance member
        // uses the self-carrying variant.
        if let Some(defaults) = ir.param_defaults(fid) {
            if f.is_static {
                // A constructor's `$default` marker is `DefaultConstructorMarker` (kotlinc's ctor ABI),
                // NOT the plain `Object` a function `$default` uses — the value class's `constructor-impl`.
                emit_facade_default_stub(
                    ir,
                    fid,
                    &fq_name,
                    &mut cw,
                    defaults,
                    env,
                    Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker"),
                );
            } else {
                emit_default_stub(ir, fid, &fq_name, facade, &mut cw, defaults, env, false);
            }
        }
    }
    emit_bridges(c, &mut cw);
    cw.set_runtime_annotations(&c.applied_annotations);
    // A cross-module provider's `@Metadata` wins; otherwise compute one from the IR (bounded shapes).
    let computed = (class_meta.is_none() && opts.emit_class_metadata)
        .then(|| build_class_metadata(ir, c, opts))
        .flatten();
    // Debug tables + nullability annotations (opt-in with metadata) for any class that qualified for a
    // computed `@Metadata` — including data classes (their synthesized methods get a LocalVariableTable
    // + @NotNull/@Nullable). NOTE: the constant-pool seeding (above) is still plain-class only, so a
    // data class is not yet FULLY byte-identical (its pool order differs) — but the attributes match.
    if computed.is_some() {
        attach_synth_debug_tables(c, &mut cw);
        attach_declared_method_debug(ir, c, &mut cw);
        attach_synth_nullability(c, &mut cw);
    }
    if let Some(m) = class_meta.or(computed.as_ref()) {
        cw.set_kotlin_metadata(m.k, &m.mv, m.xi, &m.d1, &m.d2);
    }
    cw.finish()
}

/// Emit a synthesized enum-entry subclass (`Enum$ENTRY extends Enum`) for an entry with a body: a
/// package-private `final` class with one constructor `(String name, int ordinal, <user fields>)V`
/// that delegates to the enum's `(String,int,<user>)V` constructor, plus the entry's overriding
/// methods. It has no fields of its own — overrides read the enum's fields via the inherited `this`.
fn emit_enum_entry_subclass(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    facade: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
    user_tys: &[Ty],
) -> Vec<u8> {
    let bodies = env.bodies;
    let superclass = c.superclass();
    let fq_name = c.fq_name();
    let mut cw = new_writer(&fq_name, &superclass, opts);
    cw.set_access(0x0010 | 0x0020); // FINAL | SUPER (package-private)

    // Entry-body PROPERTIES are private backing fields (read via synthesized getters, like kotlinc).
    for field in c.fields.iter() {
        let acc = 0x0002 | if field.is_final { 0x0010 } else { 0 };
        cw.add_field(acc, &field.name, &ir_type_desc(&field.ty));
    }

    // Constructor: `(String, int, <user>)V` → `super(name, ordinal, <user>)`, then the property
    // initializers (`this.<prop> = <init>`, from `init_body`).
    let user_jvm = jvm_tys(user_tys);
    let ctor_params: Vec<Ty> = [Ty::String, Ty::Int]
        .into_iter()
        .chain(user_jvm.iter().copied())
        .collect();
    let ctor_words: u16 = ctor_params.iter().map(|t| slot_words(*t)).sum();
    let mut ctor = CodeBuilder::new(1 + ctor_words);
    ctor.aload(0);
    let mut slot = 1u16;
    for t in &ctor_params {
        load(*t, slot, &mut ctor);
        slot += slot_words(*t);
    }
    let super_init = cw.methodref(
        &superclass,
        "<init>",
        &method_descriptor(&ctor_params, Ty::Unit),
    );
    let argw: i32 = ctor_params.iter().map(|t| slot_words(*t) as i32).sum();
    ctor.invokespecial(super_init, argw, 0);
    let mut ctor_max = 1 + ctor_words;
    if let Some(init_body) = c.init_body {
        let mut e = Emitter {
            ir,
            cw: &mut cw,
            bodies,
            run: env.run,
            owner: fq_name.clone(),
            facade: facade.to_string(),
            slots: HashMap::new(),
            var_types: collect_var_types(ir),
            next_slot: 1 + ctor_words,
            ret: Ty::Unit,
            loop_stack: Vec::new(),
            pending_stack: Vec::new(),
        };
        e.slots.insert(0, (0, Ty::obj(&fq_name))); // `this`
        e.emit(init_body, &mut ctor);
        ctor_max = e.next_slot;
    }
    ctor.ret_void();
    ctor.ensure_locals(ctor_max);
    ctor.link();
    cw.add_method(
        0x0000,
        "<init>",
        &method_descriptor(&ctor_params, Ty::Unit),
        &ctor,
    );

    // The overriding methods + synthesized property getters.
    for &fid in &c.methods {
        emit_method(ir, fid, &fq_name, facade, &mut cw, true, env);
    }
    cw.finish()
}

/// Emit a synthesized property-reference singleton (`Type$prop$N extends PropertyReference1Impl`):
/// a package-private `final` class with a `public static final INSTANCE`, a constructor
/// `super(owner.class, name, "getName()desc", 0)`, a `get(Object)Object` override that reads
/// `((Owner) it).getName()` (boxing a primitive), and a `<clinit>` that builds the singleton. `.name`
/// is inherited from `PropertyReference1Impl` (returns the constructor's name argument).
fn emit_prop_ref_class(c: &crate::ir::IrClass, facade: &str, opts: &EmitOptions) -> Vec<u8> {
    let pr = c.prop_ref.as_ref().unwrap();
    if pr.static_dispatch {
        return emit_toplevel_prop_ref_class(c, pr, facade, opts);
    }
    if pr.bound {
        return emit_bound_prop_ref_class(c, pr, facade, opts);
    }
    let fq = c.fq_name();
    let self_desc = format!("L{fq};");
    let superclass = c.superclass();
    let mut cw = new_writer(&fq, &superclass, opts);
    cw.set_access(0x0010 | 0x0020); // FINAL | SUPER (package-private)
    cw.add_field(0x0019, "INSTANCE", &self_desc); // PUBLIC | STATIC | FINAL

    let owner_internal = pr.owner().expect("unbound property reference owner");
    let prop_jvm = ir_ty_to_jvm(&pr.prop_ty);
    let getter_desc = format!("(){}", type_descriptor(prop_jvm));
    let signature = format!("{}{}", pr.getter_name, getter_desc); // e.g. "getX()I"

    // `<init>()V`: super(owner.class, "name", "getName()desc", 0).
    let mut ctor = CodeBuilder::new(1);
    ctor.aload(0);
    ctor.ldc_class(&owner_internal, &mut cw);
    ctor.push_string(&pr.prop_name, &mut cw);
    ctor.push_string(&signature, &mut cw);
    ctor.push_int(0, &mut cw);
    let sup = cw.methodref(
        &superclass,
        "<init>",
        "(Ljava/lang/Class;Ljava/lang/String;Ljava/lang/String;I)V",
    );
    ctor.invokespecial(sup, 4, 0);
    ctor.ret_void();
    finish_code::<0x0000>(&mut cw, "<init>", "()V", &mut ctor, 1);

    // `get(Object)Object`: ((Owner) it).getName(), boxed if primitive.
    let mut get = CodeBuilder::new(2);
    get.aload(1);
    let owner_ref = cw.class_ref(&owner_internal);
    get.checkcast(owner_ref);
    let gref = cw.methodref(&owner_internal, &pr.getter_name, &getter_desc);
    get.invokevirtual(gref, 0, slot_words(prop_jvm) as i32);
    if prop_jvm.is_jvm_scalar() {
        box_prim_free(&mut cw, &mut get, prop_jvm);
    }
    get.areturn();
    finish_code::<0x0001>(
        &mut cw,
        "get",
        "(Ljava/lang/Object;)Ljava/lang/Object;",
        &mut get,
        2,
    );

    // `set(Object, Object)V` (an unbound `var` reference): `((Owner) it).setName(v)` after
    // casting/unboxing the value argument to the property type.
    if pr.mutable {
        let setter = property_setter_name(&pr.prop_name);
        let setter_desc = format!("({}){}", type_descriptor(prop_jvm), "V");
        let mut set = CodeBuilder::new(3);
        set.aload(1);
        let owner_ref = cw.class_ref(&owner_internal);
        set.checkcast(owner_ref);
        set.aload(2);
        if prop_jvm.is_jvm_scalar() {
            let wref = cw.class_ref(
                crate::jvm::jvm_class_map::wrapper_internal(prop_jvm).unwrap_or("java/lang/Object"),
            );
            set.checkcast(wref);
            unbox_prim(&mut cw, &mut set, prop_jvm);
        } else if let Some(internal) = checkcast_internal(prop_jvm) {
            let cref = cw.class_ref(&internal);
            set.checkcast(cref);
        }
        let sref = cw.methodref(&owner_internal, &setter, &setter_desc);
        set.invokevirtual(sref, slot_words(prop_jvm) as i32, 0);
        set.ret_void();
        finish_code::<0x0001>(
            &mut cw,
            "set",
            "(Ljava/lang/Object;Ljava/lang/Object;)V",
            &mut set,
            3,
        );
    }

    // `<clinit>`: INSTANCE = new.
    let mut clinit = CodeBuilder::new(0);
    let cls = cw.class_ref(&fq);
    clinit.new_obj(cls);
    clinit.dup();
    let init = cw.methodref(&fq, "<init>", "()V");
    clinit.invokespecial(init, 0, 0);
    let fref = cw.fieldref(&fq, "INSTANCE", &self_desc);
    clinit.putstatic(fref, 1);
    clinit.ret_void();
    finish_code::<0x0008>(&mut cw, "<clinit>", "()V", &mut clinit, 0);
    cw.finish()
}

/// Emit a bound property-reference (`obj::prop` → `PropertyReference0Impl` subclass): a constructor
/// `(Object receiver)` delegating to `super(receiver, owner.class, name, "getName()desc", 0)` (the base
/// stores the receiver), and a no-arg `get()` reading `((Owner) this.receiver).getName()`. Constructed
/// per use with the captured receiver — no `INSTANCE` singleton.
fn emit_bound_prop_ref_class(
    c: &crate::ir::IrClass,
    pr: &crate::ir::PropRef,
    facade: &str,
    opts: &EmitOptions,
) -> Vec<u8> {
    let fq = c.fq_name();
    let superclass = c.superclass();
    let mut cw = new_writer(&fq, &superclass, opts);
    cw.set_access(0x0010 | 0x0020); // FINAL | SUPER

    let prop_jvm = ir_ty_to_jvm(&pr.prop_ty);
    let getter_desc = format!("(){}", type_descriptor(prop_jvm));
    let signature = format!("{}{}", pr.getter_name, getter_desc);
    let owner_internal = pr.owner().expect("bound property reference owner");
    // An EXTENSION property: get/set dispatch to a STATIC accessor `getName(Recv)`/`setName(Recv, v)`
    // on the facade, with the captured receiver passed as the first argument.
    let ext = pr.ext_facade_or_facade(facade);
    let ext_get_desc = format!("(L{};){}", owner_internal, type_descriptor(prop_jvm));

    // `<init>(Object)V`: super(receiver, owner.class, name, "getName()desc", 0).
    let mut ctor = CodeBuilder::new(2);
    ctor.aload(0);
    ctor.aload(1);
    ctor.ldc_class(&owner_internal, &mut cw);
    ctor.push_string(&pr.prop_name, &mut cw);
    ctor.push_string(&signature, &mut cw);
    ctor.push_int(0, &mut cw);
    let sup = cw.methodref(
        &superclass,
        "<init>",
        "(Ljava/lang/Object;Ljava/lang/Class;Ljava/lang/String;Ljava/lang/String;I)V",
    );
    ctor.invokespecial(sup, 5, 0);
    ctor.ret_void();
    finish_code::<0x0000>(&mut cw, "<init>", "(Ljava/lang/Object;)V", &mut ctor, 2);

    // `get()Object`: for a member ref `((Owner) this.receiver).getName()`; for an extension ref
    // `Facade.getName((Owner) this.receiver)`. Boxed if primitive.
    let mut get = CodeBuilder::new(1);
    get.aload(0);
    let recv_f = cw.fieldref(&superclass, "receiver", "Ljava/lang/Object;");
    get.getfield(recv_f, 1);
    let owner_ref = cw.class_ref(&owner_internal);
    get.checkcast(owner_ref);
    if let Some(facade) = &ext {
        let gref = cw.methodref(facade, &pr.getter_name, &ext_get_desc);
        get.invokestatic(gref, 1, slot_words(prop_jvm) as i32);
    } else {
        let gref = cw.methodref(&owner_internal, &pr.getter_name, &getter_desc);
        get.invokevirtual(gref, 0, slot_words(prop_jvm) as i32);
    }
    if prop_jvm.is_jvm_scalar() {
        box_prim_free(&mut cw, &mut get, prop_jvm);
    }
    get.areturn();
    finish_code::<0x0001>(&mut cw, "get", "()Ljava/lang/Object;", &mut get, 1);

    // `set(Object)V` (a bound `var` reference): `((Owner) this.receiver).setName(v)` after
    // casting/unboxing the argument to the property type.
    if pr.mutable {
        let setter = property_setter_name(&pr.prop_name);
        let setter_desc = format!("({}){}", type_descriptor(prop_jvm), "V");
        let mut set = CodeBuilder::new(2);
        set.aload(0);
        let recv_f = cw.fieldref(&superclass, "receiver", "Ljava/lang/Object;");
        set.getfield(recv_f, 1);
        let owner_ref = cw.class_ref(&owner_internal);
        set.checkcast(owner_ref);
        set.aload(1);
        if prop_jvm.is_jvm_scalar() {
            let wref = cw.class_ref(
                crate::jvm::jvm_class_map::wrapper_internal(prop_jvm).unwrap_or("java/lang/Object"),
            );
            set.checkcast(wref);
            unbox_prim(&mut cw, &mut set, prop_jvm);
        } else if let Some(internal) = checkcast_internal(prop_jvm) {
            let cref = cw.class_ref(&internal);
            set.checkcast(cref);
        }
        if let Some(facade) = &ext {
            let ext_set_desc = format!("(L{};{})V", owner_internal, type_descriptor(prop_jvm));
            let sref = cw.methodref(facade, &setter, &ext_set_desc);
            set.invokestatic(sref, 1 + slot_words(prop_jvm) as i32, 0);
        } else {
            let sref = cw.methodref(&owner_internal, &setter, &setter_desc);
            set.invokevirtual(sref, slot_words(prop_jvm) as i32, 0);
        }
        set.ret_void();
        finish_code::<0x0001>(&mut cw, "set", "(Ljava/lang/Object;)V", &mut set, 2);
    }
    cw.finish()
}

/// Emit a top-level property reference (`::foo` → `(Mutable)PropertyReference0Impl` subclass): an
/// `INSTANCE` singleton whose `get()` does `invokestatic <facade>.getFoo()` (no receiver), and — for a
/// `var` — a `set(Object)` doing `invokestatic <facade>.setFoo(v)`. The super ctor is the 4-arg
/// `(Class, String, String, int)` form with top-level flags = 1. `owner_internal = None` is the facade
/// sentinel (the declaring file class, unknown until emit).
fn emit_toplevel_prop_ref_class(
    c: &crate::ir::IrClass,
    pr: &crate::ir::PropRef,
    facade: &str,
    opts: &EmitOptions,
) -> Vec<u8> {
    let owner = pr.owner_or_facade(facade);
    let fq = c.fq_name();
    let self_desc = format!("L{fq};");
    let superclass = c.superclass();
    let mut cw = new_writer(&fq, &superclass, opts);
    cw.set_access(0x0010 | 0x0020); // FINAL | SUPER
    cw.add_field(0x0019, "INSTANCE", &self_desc); // PUBLIC | STATIC | FINAL

    let prop_jvm = ir_ty_to_jvm(&pr.prop_ty);
    let prop_desc = type_descriptor(prop_jvm);
    let getter_desc = format!("(){prop_desc}");
    let signature = format!("{}{}", pr.getter_name, getter_desc); // e.g. "getFoo()LBox;"

    // `<init>()V`: super(owner.class, "name", "getName()desc", 1).
    let mut ctor = CodeBuilder::new(1);
    ctor.aload(0);
    ctor.ldc_class(&owner, &mut cw);
    ctor.push_string(&pr.prop_name, &mut cw);
    ctor.push_string(&signature, &mut cw);
    ctor.push_int(1, &mut cw);
    let sup = cw.methodref(
        &superclass,
        "<init>",
        "(Ljava/lang/Class;Ljava/lang/String;Ljava/lang/String;I)V",
    );
    ctor.invokespecial(sup, 4, 0);
    ctor.ret_void();
    finish_code::<0x0000>(&mut cw, "<init>", "()V", &mut ctor, 1);

    // `get()Object`: invokestatic <facade>.getName(), boxed if primitive.
    let mut get = CodeBuilder::new(1);
    let gref = cw.methodref(&owner, &pr.getter_name, &getter_desc);
    get.invokestatic(gref, 0, slot_words(prop_jvm) as i32);
    if prop_jvm.is_jvm_scalar() {
        box_prim_free(&mut cw, &mut get, prop_jvm);
    }
    get.areturn();
    finish_code::<0x0001>(&mut cw, "get", "()Ljava/lang/Object;", &mut get, 1);

    // `set(Object)V` (a `var`): invokestatic <facade>.setName(v) after casting/unboxing the argument.
    if pr.mutable {
        let setter = property_setter_name(&pr.prop_name);
        let setter_desc = format!("({prop_desc})V");
        let mut set = CodeBuilder::new(2);
        set.aload(1);
        if prop_jvm.is_jvm_scalar() {
            let wref = cw.class_ref(
                crate::jvm::jvm_class_map::wrapper_internal(prop_jvm).unwrap_or("java/lang/Object"),
            );
            set.checkcast(wref);
            unbox_prim(&mut cw, &mut set, prop_jvm);
        } else if let Some(internal) = checkcast_internal(prop_jvm) {
            let cref = cw.class_ref(&internal);
            set.checkcast(cref);
        }
        let sref = cw.methodref(&owner, &setter, &setter_desc);
        set.invokestatic(sref, slot_words(prop_jvm) as i32, 0);
        set.ret_void();
        finish_code::<0x0001>(&mut cw, "set", "(Ljava/lang/Object;)V", &mut set, 2);
    }

    // `<clinit>`: INSTANCE = new.
    let mut clinit = CodeBuilder::new(0);
    let cls = cw.class_ref(&fq);
    clinit.new_obj(cls);
    clinit.dup();
    let init = cw.methodref(&fq, "<init>", "()V");
    clinit.invokespecial(init, 0, 0);
    let fref = cw.fieldref(&fq, "INSTANCE", &self_desc);
    clinit.putstatic(fref, 1);
    clinit.ret_void();
    finish_code::<0x0008>(&mut cw, "<clinit>", "()V", &mut clinit, 0);
    cw.finish()
}

/// The wrapper class internal name for a primitive (`Int` → `java/lang/Integer`), for casting an
/// erased `Object` argument before unboxing.
/// Emit a synthesized function-reference subclass (`<Owner>$ref$N extends FunctionReferenceImpl
/// implements Function<arity>`): an UNBOUND ref gets a `public static final INSTANCE` + a no-arg ctor
/// `super(arity, owner.class, name, sig, flags)`; a BOUND ref gets a `(Object)` ctor delegating to
/// `super(arity, receiver, owner.class, name, sig, flags)` (the base stores the receiver). The single
/// erased `invoke(Object…)Object` casts/unboxes its args and dispatches to the target, boxing the
/// result (or returning the `Unit` singleton for a `void` target). Reference EQUALITY (`::f == ::f`,
/// `a::m != b::m`) is inherited from `FunctionReferenceImpl` (compares owner/name/signature/receiver).
/// Whether the facade declares a PRIVATE top-level function `name` with `arity` parameters — the
/// target of a function reference that must route through the `access$<name>` bridge.
fn private_facade_fn(ir: &IrFile, name: &str, arity: usize) -> bool {
    ir.functions.iter().enumerate().any(|(i, f)| {
        f.name == name
            && f.params.len() == arity
            && f.dispatch_receiver.is_none()
            && ir.private_methods.contains(&(i as u32))
    })
}

fn emit_func_ref_class(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    facade: &str,
    opts: &EmitOptions,
) -> Vec<u8> {
    use crate::ir::FrDispatch;
    let fr = c.func_ref.as_ref().unwrap();
    // A missing `owner_class`/`call_owner` is the facade sentinel (a top-level function lives on the
    // file facade, whose name isn't known until emit) — resolve it here.
    let owner_class = fr.owner_class_or_facade(facade);
    let call_owner = fr.call_owner_or_facade(facade);
    let fq = c.fq_name();
    let self_desc = format!("L{fq};");
    let superclass = c.superclass();
    let mut cw = new_writer(&fq, &superclass, opts);
    cw.set_access(0x0010 | 0x0020); // FINAL | SUPER
    cw.add_interface(&format!("kotlin/jvm/functions/Function{}", fr.arity));

    // The call argument param types begin AFTER the receiver for an unbound member ref.
    let first_arg = match fr.dispatch {
        FrDispatch::VirtualUnbound => 1usize,
        _ => 0,
    };
    // For `StaticBound` the captured receiver is target arg 0, so invoke arg `k` maps to
    // `target_param_tys[k + 1]`.
    let target_offset = match fr.dispatch {
        FrDispatch::StaticBound => 1usize,
        _ => 0,
    };
    let ret_jvm = ir_ty_to_jvm(&fr.ret_ty);
    let returns_void = matches!(fr.ret_ty, Ty::Unit | Ty::Nothing);
    // The Kotlin reference metadata signature stays logical; the target JVM call descriptor follows
    // backend lowerings such as value-class erasure. Both exclude the unbound receiver parameter.
    let mut signature_desc = String::from("(");
    for pt in fr.param_tys.iter().skip(first_arg) {
        signature_desc.push_str(&ir_type_desc(pt));
    }
    signature_desc.push(')');
    let signature_ret = if returns_void {
        "V".to_string()
    } else {
        type_descriptor(ret_jvm)
    };
    signature_desc.push_str(&signature_ret);
    let signature = format!("{}{}", fr.fn_name, signature_desc);

    let mut call_desc = String::from("(");
    for pt in fr.target_param_tys.iter().skip(first_arg) {
        call_desc.push_str(&ir_type_desc(pt));
    }
    call_desc.push(')');
    let target_ret_jvm = ir_ty_to_jvm(&fr.target_ret_ty);
    let ret_desc = if returns_void {
        "V".to_string()
    } else {
        type_descriptor(target_ret_jvm)
    };
    call_desc.push_str(&ret_desc);

    if fr.bound {
        // `<init>(Object)V`: super(arity, receiver, owner.class, name, sig, flags).
        let mut ctor = CodeBuilder::new(2);
        ctor.aload(0);
        ctor.push_int(fr.arity as i32, &mut cw);
        ctor.aload(1);
        ctor.ldc_class(&owner_class, &mut cw);
        ctor.push_string(&fr.fn_name, &mut cw);
        ctor.push_string(&signature, &mut cw);
        ctor.push_int(fr.flags, &mut cw);
        let sup = cw.methodref(
            &superclass,
            "<init>",
            "(ILjava/lang/Object;Ljava/lang/Class;Ljava/lang/String;Ljava/lang/String;I)V",
        );
        ctor.invokespecial(sup, 6, 0);
        ctor.ret_void();
        finish_code::<0x0000>(&mut cw, "<init>", "(Ljava/lang/Object;)V", &mut ctor, 2);
    } else {
        cw.add_field(0x0019, "INSTANCE", &self_desc); // PUBLIC|STATIC|FINAL
                                                      // `<init>()V`: super(arity, owner.class, name, sig, flags).
        let mut ctor = CodeBuilder::new(1);
        ctor.aload(0);
        ctor.push_int(fr.arity as i32, &mut cw);
        ctor.ldc_class(&owner_class, &mut cw);
        ctor.push_string(&fr.fn_name, &mut cw);
        ctor.push_string(&signature, &mut cw);
        ctor.push_int(fr.flags, &mut cw);
        let sup = cw.methodref(
            &superclass,
            "<init>",
            "(ILjava/lang/Class;Ljava/lang/String;Ljava/lang/String;I)V",
        );
        ctor.invokespecial(sup, 5, 0);
        ctor.ret_void();
        finish_code::<0x0000>(&mut cw, "<init>", "()V", &mut ctor, 1);
        // `<clinit>`: INSTANCE = new <self>().
        let mut clinit = CodeBuilder::new(0);
        let cls = cw.class_ref(&fq);
        clinit.new_obj(cls);
        clinit.dup();
        let init = cw.methodref(&fq, "<init>", "()V");
        clinit.invokespecial(init, 0, 0);
        let fref = cw.fieldref(&fq, "INSTANCE", &self_desc);
        clinit.putstatic(fref, 1);
        clinit.ret_void();
        finish_code::<0x0008>(&mut cw, "<clinit>", "()V", &mut clinit, 0);
    }

    // The erased `invoke(Object×arity)Object`.
    let arity = fr.arity as u16;
    let mut invoke_desc = String::from("(");
    for _ in 0..arity {
        invoke_desc.push_str("Ljava/lang/Object;");
    }
    invoke_desc.push_str(")Ljava/lang/Object;");
    let mut inv = CodeBuilder::new(1 + arity);
    // Push the receiver for a member dispatch (`first_arg`, computed above, skips it in the arg loop).
    match fr.dispatch {
        FrDispatch::VirtualBound => {
            inv.aload(0);
            let recv_f = cw.fieldref(&superclass, "receiver", "Ljava/lang/Object;");
            inv.getfield(recv_f, 1);
            let owner_ref = cw.class_ref(&call_owner);
            inv.checkcast(owner_ref);
        }
        FrDispatch::VirtualUnbound => {
            inv.aload(1);
            let owner_ref = cw.class_ref(&call_owner);
            inv.checkcast(owner_ref);
        }
        FrDispatch::Static => {}
        FrDispatch::StaticBound => {
            // The captured receiver is the FIRST static argument: load `this.receiver`, cast to the
            // target receiver type (`target_param_tys[0]`).
            inv.aload(0);
            let recv_f = cw.fieldref(&superclass, "receiver", "Ljava/lang/Object;");
            inv.getfield(recv_f, 1);
            if let Some(vc) = &fr.staticbound_recv_unbox {
                // A VALUE-CLASS receiver (`Z(42)::ext`) is stored BOXED: `checkcast` to the box class then
                // `unbox-impl` to the underlying the mangled target expects (`Z`→`int`).
                let vc = vc.render();
                let cref = cw.class_ref(&vc);
                inv.checkcast(cref);
                let under = ir_ty_to_jvm(
                    fr.target_param_tys
                        .first()
                        .copied()
                        .as_ref()
                        .unwrap_or(&Ty::Error),
                );
                let m = cw.methodref(&vc, "unbox-impl", &format!("(){}", type_descriptor(under)));
                inv.invokevirtual(m, 0, slot_words(under) as i32);
            } else if let Some(internal) = fr
                .target_param_tys
                .first()
                .map(ir_ty_to_jvm)
                .and_then(checkcast_internal)
            {
                let cref = cw.class_ref(&internal);
                inv.checkcast(cref);
            }
        }
    };
    // Push the call arguments (cast/unbox each erased `Object`).
    let mut call_arg_words = match fr.dispatch {
        // The captured receiver already pushed above occupies one (reference) target slot.
        FrDispatch::StaticBound => fr
            .target_param_tys
            .first()
            .map_or(0, |t| slot_words(ir_ty_to_jvm(t)) as i32),
        _ => 0,
    };
    for (k, pt) in fr.param_tys.iter().enumerate().skip(first_arg) {
        inv.aload(1 + k as u16);
        let jt = ir_ty_to_jvm(pt);
        if jt.is_jvm_scalar() {
            let wref = cw.class_ref(
                crate::jvm::jvm_class_map::wrapper_internal(jt).unwrap_or("java/lang/Object"),
            );
            inv.checkcast(wref);
            unbox_prim(&mut cw, &mut inv, jt);
        } else if let Some(internal) = checkcast_internal(jt) {
            let cref = cw.class_ref(&internal);
            inv.checkcast(cref);
        }
        let target_jt = fr
            .target_param_tys
            .get(k + target_offset)
            .map(ir_ty_to_jvm)
            .unwrap_or(jt);
        if let Some(vc) = fr.unbox_params.get(k).and_then(|v| v.as_ref()) {
            let locals = func_ref_invoke_locals(&mut cw, &fq, arity);
            let stack_prefix = func_ref_call_stack_prefix(&mut cw, &fr.dispatch, &call_owner);
            emit_value_class_unbox_adapter(
                &mut cw,
                &mut inv,
                *vc,
                target_jt,
                fr.unbox_param_nullable.get(k).copied().unwrap_or(false),
                Some(locals),
                stack_prefix,
            );
        }
        call_arg_words += slot_words(target_jt) as i32;
    }
    // Dispatch to the target.
    let ret_words = if returns_void {
        0
    } else {
        slot_words(target_ret_jvm) as i32
    };
    // A reference to a PRIVATE same-file top-level function can't invokestatic it from this
    // (separate) class — call kotlinc's `access$<name>` facade bridge instead (`emit_pass` emits it
    // for exactly these referenced targets).
    let static_call_name = if fr.call_owner_is_facade()
        && private_facade_fn(ir, &fr.call_name, fr.target_param_tys.len())
    {
        format!("access${}", fr.call_name)
    } else {
        fr.call_name.clone()
    };
    match fr.dispatch {
        FrDispatch::Static | FrDispatch::StaticBound => {
            let m = cw.methodref(&call_owner, &static_call_name, &call_desc);
            inv.invokestatic(m, call_arg_words, ret_words);
        }
        // A bound reference to a mapped-builtin member (`"KOTLIN"::get`) invokes the same PHYSICAL JVM
        // method a direct call would (`String.get` → `charAt`) — apply the backend's name mapping here too.
        _ if fr.call_interface => {
            let vn = mapped_builtin_virtual_name(&call_owner, &fr.call_name);
            let m = cw.interface_methodref(&call_owner, vn, &call_desc);
            inv.invokeinterface(m, call_arg_words, ret_words);
        }
        _ => {
            let vn = mapped_builtin_virtual_name(&call_owner, &fr.call_name);
            let m = cw.methodref(&call_owner, vn, &call_desc);
            inv.invokevirtual(m, call_arg_words, ret_words);
        }
    }
    // Adapt the result to `Object`: a `void` target yields the `Unit` singleton; a value-class-returning
    // reference boxes the erased underlying back to the value class; a plain primitive is wrapper-boxed.
    if returns_void {
        let unit = cw.fieldref("kotlin/Unit", "INSTANCE", "Lkotlin/Unit;");
        inv.getstatic(unit, 1);
    } else if let Some(owner) = &fr.box_ret {
        let owner = owner.render();
        // A value-class-returning reference: the target returns the ERASED underlying (primitive or the
        // reference underlying) — exactly what `call_desc` requested. Box it back to the value class via
        // `box-impl` so the `Function` result is the boxed VC (`X` object) the invariant requires — a VC in
        // a `FunctionN` slot is boxed. Without it a `typeAdapter::decode` returning `X` hands back the bare
        // underlying that the caller then `checkcast X`es → `ClassCastException`.
        let bi = cw.methodref(
            &owner,
            "box-impl",
            &format!(
                "({}){}",
                type_descriptor(target_ret_jvm),
                type_descriptor(Ty::obj(&owner))
            ),
        );
        inv.invokestatic(bi, slot_words(target_ret_jvm) as i32, 1);
    } else if target_ret_jvm.is_jvm_scalar() {
        box_prim_free(&mut cw, &mut inv, target_ret_jvm);
    }
    inv.areturn();
    finish_code::<0x0001>(&mut cw, "invoke", &invoke_desc, &mut inv, 1 + arity);
    cw.finish()
}

fn func_ref_invoke_locals(cw: &mut ClassWriter, self_class: &str, arity: u16) -> Vec<VerifType> {
    let mut locals = vec![VerifType::Object(cw.class_ref(self_class))];
    let obj = VerifType::Object(cw.class_ref("java/lang/Object"));
    locals.extend(std::iter::repeat_n(obj, arity as usize));
    locals
}

fn func_ref_call_stack_prefix(
    cw: &mut ClassWriter,
    dispatch: &crate::ir::FrDispatch,
    call_owner: &str,
) -> Vec<VerifType> {
    match dispatch {
        crate::ir::FrDispatch::Static => Vec::new(),
        crate::ir::FrDispatch::VirtualBound
        | crate::ir::FrDispatch::VirtualUnbound
        | crate::ir::FrDispatch::StaticBound => {
            vec![VerifType::Object(cw.class_ref(call_owner))]
        }
    }
}

fn verif_for_jvm_free(cw: &mut ClassWriter, t: Ty) -> VerifType {
    match t {
        t if is_jvm_int_category(t) => VerifType::Integer,
        Ty::Long => VerifType::Long,
        Ty::Double => VerifType::Double,
        Ty::Float => VerifType::Float,
        Ty::String => VerifType::Object(cw.class_ref("java/lang/String")),
        t if t.is_array() => VerifType::Object(cw.class_ref(&type_descriptor(t))),
        Ty::Obj(n, _) => VerifType::Object(cw.class_ref(&n.render())),
        Ty::Null => VerifType::Null,
        _ => VerifType::Top,
    }
}

fn emit_value_class_unbox_adapter(
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
    value_class: TypeName,
    target: Ty,
    nullable: bool,
    locals: Option<Vec<VerifType>>,
    stack_prefix: Vec<VerifType>,
) {
    let value_class = value_class.render();
    let unbox = cw.methodref(
        &value_class,
        "unbox-impl",
        &format!("(){}", type_descriptor(target)),
    );
    if !nullable {
        code.invokevirtual(unbox, 0, slot_words(target) as i32);
        return;
    }
    let null = code.new_label();
    let end = code.new_label();
    if let Some(locals) = locals {
        let mut null_stack = stack_prefix.clone();
        null_stack.push(VerifType::Object(cw.class_ref(&value_class)));
        let mut end_stack = stack_prefix;
        end_stack.push(verif_for_jvm_free(cw, target));
        code.add_frame_if_new(null, locals.clone(), null_stack);
        code.add_frame_if_new(end, locals, end_stack);
    }
    code.dup();
    code.ifnull(null);
    code.invokevirtual(unbox, 0, slot_words(target) as i32);
    code.goto(end);
    code.bind(null);
    code.pop();
    code.aconst_null();
    code.bind(end);
}

/// The `kotlin/jvm/internal/Ref$XxxRef` holder class and its `element` field descriptor for a boxed
/// mutable local of element type `elem` (a primitive picks its specialized `Ref`, any reference uses
/// `Ref$ObjectRef` whose `element` is `Object`).
fn ref_class(elem: &Ty) -> (&'static str, &'static str) {
    match ir_ty_to_jvm(elem) {
        Ty::Int => ("kotlin/jvm/internal/Ref$IntRef", "I"),
        Ty::Long => ("kotlin/jvm/internal/Ref$LongRef", "J"),
        Ty::Float => ("kotlin/jvm/internal/Ref$FloatRef", "F"),
        Ty::Double => ("kotlin/jvm/internal/Ref$DoubleRef", "D"),
        Ty::Boolean => ("kotlin/jvm/internal/Ref$BooleanRef", "Z"),
        Ty::Char => ("kotlin/jvm/internal/Ref$CharRef", "C"),
        Ty::Byte => ("kotlin/jvm/internal/Ref$ByteRef", "B"),
        Ty::Short => ("kotlin/jvm/internal/Ref$ShortRef", "S"),
        _ => ("kotlin/jvm/internal/Ref$ObjectRef", "Ljava/lang/Object;"),
    }
}

fn throw_assertion_error(cw: &mut ClassWriter, code: &mut CodeBuilder) {
    let ae = cw.class_ref("java/lang/AssertionError");
    code.new_obj(ae);
    code.dup();
    let init = cw.methodref("java/lang/AssertionError", "<init>", "()V");
    code.invokespecial(init, 0, 0);
    code.athrow();
}

fn finish_code<const ACCESS: u16>(
    cw: &mut ClassWriter,
    name: &str,
    desc: &str,
    code: &mut CodeBuilder,
    locals: u16,
) {
    code.ensure_locals(locals);
    code.link();
    cw.add_method(ACCESS, name, desc, code);
}

fn finish_bridge(
    cw: &mut ClassWriter,
    name: &str,
    desc: &str,
    code: &mut CodeBuilder,
    locals: u16,
) {
    finish_code::<{ 0x0001 | 0x0040 | 0x1000 }>(cw, name, desc, code, locals);
}

/// Emit `ACC_BRIDGE|ACC_SYNTHETIC` methods: each has the supertype's erased descriptor, adapts its
/// arguments (checkcast / unbox / numeric convert), delegates to the concrete override, and adapts
/// the return value back (box / numeric convert). Bridges are straight-line — no frames.
fn emit_bridges(c: &crate::ir::IrClass, cw: &mut ClassWriter) {
    for b in &c.bridges {
        let ep = jvm_tys(&b.erased_params);
        let cp = jvm_tys(&b.concrete_params);
        let er = ir_ty_to_jvm(&b.erased_ret);
        let cr = ir_ty_to_jvm(&b.concrete_ret);
        let erased_desc = method_descriptor(&ep, er);
        // A bridge whose (name, descriptor) already names a REAL method on this class would be a
        // duplicate (`ClassFormatError`) — e.g. an interface getter `getX()T` overridden with the SAME
        // type differs from the impl only by a spurious nullability/representation detail. Skip it; the
        // real method already satisfies the interface. (Real methods are emitted before `emit_bridges`.)
        if cw.has_method(&b.name, &erased_desc) {
            continue;
        }
        let pw: u16 = ep.iter().map(|t| slot_words(*t)).sum();
        let mut code = CodeBuilder::new(1 + pw);
        code.aload(0);
        let mut slot = 1u16;
        for (k, (et, ct)) in ep.iter().zip(&cp).enumerate() {
            load(*et, slot, &mut code);
            slot += slot_words(*et);
            // A boxed value-class param (a generic supertype method `f(Object,…)` delegating to a mangled
            // concrete override taking the underlying): checkcast the incoming `Object` to the boxed `X`,
            // then `unbox-impl` it to the underlying `ct` the target expects.
            if let Some(Some(vc)) = b.unbox_params.get(k) {
                let vc = vc.render();
                let ci = cw.class_ref(&vc);
                code.checkcast(ci);
                let m = cw.methodref(&vc, "unbox-impl", &format!("(){}", type_descriptor(*ct)));
                code.invokevirtual(m, 0, slot_words(*ct) as i32);
            } else if et != ct {
                if et.is_reference() && ct.is_reference() {
                    let ci = cw.class_ref(&ref_internal(*ct));
                    code.checkcast(ci);
                } else if et.is_reference() && ct.is_jvm_scalar() {
                    unbox_prim(cw, &mut code, *ct);
                } else if et.is_jvm_scalar() && ct.is_jvm_scalar() {
                    emit_num_conv(*et, *ct, &mut code);
                }
            }
        }
        let argw: i32 = cp.iter().map(|t| slot_words(*t) as i32).sum();
        // A value-class boxing bridge calls the mangled override (`target_name`) which returns the
        // erased underlying, then boxes the result back to `X` with `X.box-impl`.
        let target = b.target_name.as_deref().unwrap_or(&b.name);
        let owner = c.fq_name();
        let m = cw.methodref(&owner, target, &method_descriptor(&cp, cr));
        code.invokevirtual(m, argw, slot_words(cr) as i32);
        if cr.is_reference() && ref_internal(cr) == "java/lang/Void" && !er.is_reference() {
            // A `Nothing` override may have a `java/lang/Void` descriptor while the value-class
            // supertype bridge returns the unboxed primitive. The target must diverge; if it ever
            // falls through, discard the null-only Void result and throw to keep the bridge verifiable.
            code.pop();
            throw_assertion_error(cw, &mut code);
            finish_bridge(cw, &b.name, &erased_desc, &mut code, 1 + pw);
            continue;
        }
        if b.concrete_ret == Ty::Nothing {
            // Kotlin `Nothing` methods must not fall through. If the concrete descriptor still leaves a
            // physical carrier value, discard it before throwing so the assertion path starts with a clean
            // stack for every bridge return representation.
            if cr == Ty::Nothing {
                code.pop();
            } else {
                discard(cr, &mut code);
            }
            throw_assertion_error(cw, &mut code);
            finish_bridge(cw, &b.name, &erased_desc, &mut code, 1 + pw);
            continue;
        }
        if let Some(owner) = &b.box_ret {
            let owner = owner.render();
            let bi = cw.methodref(
                &owner,
                "box-impl",
                &format!(
                    "({}){}",
                    type_descriptor(cr),
                    type_descriptor(Ty::obj(&owner))
                ),
            );
            code.invokestatic(bi, slot_words(cr) as i32, 1);
        } else if cr != er {
            if er.is_reference() && cr.is_jvm_scalar() {
                box_prim_free(cw, &mut code, cr);
            } else if er.is_jvm_scalar() && cr.is_jvm_scalar() {
                emit_num_conv(cr, er, &mut code);
            } else if cr == Ty::Unit && er.is_reference() {
                // A `Unit`-returning override bridged to a reference-returning supertype method
                // (`B.foo(): Unit` over `A.foo(): Any`): the JVM call is void, so materialize the
                // `kotlin/Unit` singleton the erased bridge must return.
                let f = cw.fieldref("kotlin/Unit", "INSTANCE", "Lkotlin/Unit;");
                code.getstatic(f, 1);
            } else if er.is_reference() && cr.is_reference() && ref_internal(cr) == "java/lang/Void"
            {
                // `Nothing?` has only the value `null`, but its concrete JVM descriptor is
                // `java/lang/Void`. A bridge returning a narrower reference (for example a nullable
                // value class box) must refine the verifier type before `areturn`.
                let ci = cw.class_ref(&ref_internal(er));
                code.checkcast(ci);
            } else if er.is_reference() && !er.is_array() && ref_internal(cr) == "java/lang/Object"
            {
                // Covariant generic DIAMOND: the inherited concrete getter returns the erased
                // `Object` (`val x: T` in a generic base), but an interface in the hierarchy requires
                // a NARROWER reference type (`override val x: String`). This bridge's declared return
                // (`er`) is that narrower type, so the `Object` on the stack must be `checkcast` to it
                // before `areturn` — otherwise the verifier rejects it ("Bad return type"). The usual
                // direction (concrete is a SUBtype of erased) needs no cast; this is the inverse.
                // Restricted to a plain object type (`Ty::Obj`): an array `er` would need a descriptor-
                // form class ref, and that narrowing direction doesn't arise here.
                let ci = cw.class_ref(&ref_internal(er));
                code.checkcast(ci);
            } // reference→reference (concrete is a subtype of erased): no cast needed
        }
        emit_return(er, &mut code);
        finish_bridge(cw, &b.name, &erased_desc, &mut code, 1 + pw);
    }
}

/// Box a primitive on the stack to its wrapper (free-function form for the bridge emitter). A signed
/// primitive boxes via its `java/lang/*` `valueOf`; an UNSIGNED type via its inline-class wrapper's
/// `box-impl` (`kotlin/UInt.box-impl(I)Lkotlin/UInt;`) — both are rows in the one table.
fn box_prim_free(cw: &mut ClassWriter, code: &mut CodeBuilder, t: Ty) {
    let (cls, meth, desc) = match t {
        Ty::Int => ("java/lang/Integer", "valueOf", "(I)Ljava/lang/Integer;"),
        Ty::Long => ("java/lang/Long", "valueOf", "(J)Ljava/lang/Long;"),
        Ty::Double => ("java/lang/Double", "valueOf", "(D)Ljava/lang/Double;"),
        Ty::Float => ("java/lang/Float", "valueOf", "(F)Ljava/lang/Float;"),
        Ty::Boolean => ("java/lang/Boolean", "valueOf", "(Z)Ljava/lang/Boolean;"),
        Ty::Char => ("java/lang/Character", "valueOf", "(C)Ljava/lang/Character;"),
        Ty::Byte => ("java/lang/Byte", "valueOf", "(B)Ljava/lang/Byte;"),
        Ty::Short => ("java/lang/Short", "valueOf", "(S)Ljava/lang/Short;"),
        Ty::UInt => ("kotlin/UInt", "box-impl", "(I)Lkotlin/UInt;"),
        Ty::ULong => ("kotlin/ULong", "box-impl", "(J)Lkotlin/ULong;"),
        _ => return,
    };
    let m = cw.methodref(cls, meth, desc);
    code.invokestatic(m, slot_words(t) as i32, 1);
}

/// Unbox a wrapper on the stack to the primitive `t` (free-function form for the bridge emitter).
fn unbox_prim(cw: &mut ClassWriter, code: &mut CodeBuilder, t: Ty) {
    let (cls, meth, desc) = match t {
        Ty::Int => ("java/lang/Integer", "intValue", "()I"),
        Ty::Long => ("java/lang/Long", "longValue", "()J"),
        Ty::Double => ("java/lang/Double", "doubleValue", "()D"),
        Ty::Float => ("java/lang/Float", "floatValue", "()F"),
        Ty::Boolean => ("java/lang/Boolean", "booleanValue", "()Z"),
        Ty::Char => ("java/lang/Character", "charValue", "()C"),
        Ty::Byte => ("java/lang/Byte", "byteValue", "()B"),
        Ty::Short => ("java/lang/Short", "shortValue", "()S"),
        // An unsigned wrapper unboxes via its inline-class `unbox-impl` (a row, not a special case).
        Ty::UInt => ("kotlin/UInt", "unbox-impl", "()I"),
        Ty::ULong => ("kotlin/ULong", "unbox-impl", "()J"),
        _ => return,
    };
    let ci = cw.class_ref(cls);
    code.checkcast(ci);
    let m = cw.methodref(cls, meth, desc);
    code.invokevirtual(m, 0, slot_words(t) as i32);
}

/// Emit a Kotlin `annotation class` as a JVM ANNOTATION INTERFACE: `ACC_PUBLIC|ACC_INTERFACE|ACC_ABSTRACT|
/// ACC_ANNOTATION`, extending `java/lang/annotation/Annotation`, with one `public abstract` accessor per
/// member (`int x()`, `String s()`) named after the property and returning its type — kotlinc's shape.
/// Members come from `fields`. Instances are built by the synthetic impl ([`emit_annotation_impl_class`]).
fn emit_annotation_class(
    c: &crate::ir::IrClass,
    opts: &EmitOptions,
    class_meta: Option<&KotlinMetadata>,
) -> Vec<u8> {
    let fq_name = c.fq_name();
    let mut cw = new_writer(&fq_name, "java/lang/Object", opts);
    cw.set_access(0x0001 | 0x0200 | 0x0400 | 0x2000); // PUBLIC | INTERFACE | ABSTRACT | ANNOTATION
    cw.add_interface("java/lang/annotation/Annotation");
    for field in &c.fields {
        let ret = ir_ty_to_jvm(&field.ty);
        cw.add_abstract_method(0x0401, &field.name, &format!("(){}", type_descriptor(ret)));
        // PUBLIC|ABSTRACT
    }
    // A RUNTIME-retention Kotlin annotation emits `@java.lang.annotation.Retention(RUNTIME)` so the JVM
    // keeps its USES (`@Anno` in `RuntimeVisibleAnnotations`) visible to reflection.
    let mut meta: Vec<crate::ir::AppliedAnnotation> = Vec::new();
    if c.runtime_retained {
        meta.push(crate::ir::AppliedAnnotation {
            internal: crate::types::type_name("java/lang/annotation/Retention"),
            values: vec![(
                "value".to_string(),
                crate::ir::AnnoValue::Enum(
                    crate::types::type_name("java/lang/annotation/RetentionPolicy"),
                    "RUNTIME".to_string(),
                ),
            )],
        });
    }
    meta.extend(c.applied_annotations.iter().cloned());
    cw.set_runtime_annotations(&meta);
    if let Some(m) = class_meta {
        cw.set_kotlin_metadata(m.k, &m.mv, m.xi, &m.d1, &m.d2);
    }
    cw.finish()
}

/// The boxed-wrapper internal name + a static `hashCode` helper descriptor for a primitive `Ty`, used by
/// the annotation impl's `hashCode`. Returns `(wrapper_internal, hashCode_arg_descriptor)`.
fn prim_wrapper(t: Ty) -> Option<(&'static str, &'static str)> {
    Some(match t {
        Ty::Boolean => ("java/lang/Boolean", "Z"),
        Ty::Byte => ("java/lang/Byte", "B"),
        Ty::Short => ("java/lang/Short", "S"),
        Ty::Char => ("java/lang/Character", "C"),
        Ty::Int => ("java/lang/Integer", "I"),
        Ty::Long => ("java/lang/Long", "J"),
        Ty::Float => ("java/lang/Float", "F"),
        Ty::Double => ("java/lang/Double", "D"),
        _ => return None,
    })
}

/// Java `String.hashCode()` of `s` (the annotation `hashCode` weights each member by `127 *
/// name.hashCode()`, a compile-time constant).
fn java_string_hash(s: &str) -> i32 {
    s.chars()
        .fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(c as i32))
}

/// Emit the synthetic IMPLEMENTATION class for a Kotlin annotation instantiation (`A(args)`): a final
/// class implementing the annotation interface `iface` and the full `java.lang.annotation.Annotation`
/// contract — private final fields, a constructor, per-member accessors (`x()`/`s()`), `annotationType()`,
/// and content-correct `equals`/`hashCode`/`toString` (arrays via `java.util.Arrays`, `float`/`double` via
/// their wrappers' `equals`/`hashCode` for NaN/`-0.0` semantics). `c.fields` are the members in order.
fn emit_annotation_impl_class(c: &crate::ir::IrClass, iface: &str, opts: &EmitOptions) -> Vec<u8> {
    let fq = c.fq_name();
    let members: Vec<(String, Ty)> = c
        .fields
        .iter()
        .map(|f| (f.name.clone(), ir_ty_to_jvm(&f.ty)))
        .collect();
    let mut cw = new_writer(&fq, "java/lang/Object", opts);
    cw.set_access(0x0001 | 0x0010 | 0x1000); // PUBLIC | FINAL | SYNTHETIC
    cw.add_interface(iface);
    for (name, jt) in &members {
        cw.add_field(0x0002 | 0x0010, name, &type_descriptor(*jt)); // PRIVATE | FINAL
    }

    // <init>(members…): super(); store each arg to its field.
    {
        let params_words: u16 = members.iter().map(|(_, jt)| slot_words(*jt)).sum();
        let mut ctor = CodeBuilder::new(1 + params_words);
        ctor.aload(0);
        let obj_init = cw.methodref("java/lang/Object", "<init>", "()V");
        ctor.invokespecial(obj_init, 0, 0);
        let mut slot = 1u16;
        for (name, jt) in &members {
            ctor.aload(0);
            load(*jt, slot, &mut ctor);
            let fref = cw.fieldref(&fq, name, &type_descriptor(*jt));
            ctor.putfield(fref, slot_words(*jt) as i32);
            slot += slot_words(*jt);
        }
        let desc = format!(
            "({})V",
            members
                .iter()
                .map(|(_, jt)| type_descriptor(*jt))
                .collect::<String>()
        );
        ctor.ret_void();
        finish_code::<0x0001>(&mut cw, "<init>", &desc, &mut ctor, 1 + params_words);
    }

    // Per-member accessor `x()T`: return this.x.
    for (name, jt) in &members {
        let mut g = CodeBuilder::new(1);
        g.aload(0);
        let fref = cw.fieldref(&fq, name, &type_descriptor(*jt));
        g.getfield(fref, slot_words(*jt) as i32);
        emit_return(*jt, &mut g);
        finish_code::<0x0011>(
            &mut cw,
            name,
            &format!("(){}", type_descriptor(*jt)),
            &mut g,
            1,
        );
    }

    // annotationType(): return <iface>.class.
    {
        let mut m = CodeBuilder::new(1);
        m.ldc_class(iface, &mut cw);
        m.areturn();
        finish_code::<0x0011>(&mut cw, "annotationType", "()Ljava/lang/Class;", &mut m, 1);
    }

    emit_annotation_equals(&mut cw, &fq, iface, &members);
    emit_annotation_hashcode(&mut cw, &fq, &members);
    emit_annotation_tostring(&mut cw, &fq, iface, &members);
    cw.finish()
}

/// `equals(Object)Z` for an annotation impl: `o` must be an instance of the annotation interface and every
/// member must be equal (arrays compared by content via `Arrays.equals`; `float`/`double` via their
/// wrappers' `equals` so `NaN`==`NaN` and `-0.0`!=`0.0` per the annotation contract; other references via
/// `Object.equals`). One `false` exit label.
fn emit_annotation_equals(cw: &mut ClassWriter, fq: &str, iface: &str, members: &[(String, Ty)]) {
    let mut cb = CodeBuilder::new(2); // this=0, o=1
    cb.ensure_locals(3); // +o-as-iface at local 2
    let lfalse = cb.new_label();
    let icls = cw.class_ref(iface);
    cb.aload(1);
    cb.instance_of(icls);
    cb.ifeq(lfalse);
    cb.aload(1);
    cb.checkcast(icls);
    cb.astore(2);
    for (name, jt) in members {
        let fref = cw.fieldref(fq, name, &type_descriptor(*jt));
        let aref = cw.interface_methodref(iface, name, &format!("(){}", type_descriptor(*jt)));
        let push_this = |cb: &mut CodeBuilder| {
            cb.aload(0);
            cb.getfield(fref, slot_words(*jt) as i32);
        };
        let push_other = |cb: &mut CodeBuilder| {
            cb.aload(2);
            cb.invokeinterface(aref, 0, slot_words(*jt) as i32);
        };
        match jt {
            Ty::Int | Ty::Short | Ty::Byte | Ty::Char | Ty::Boolean => {
                push_this(&mut cb);
                push_other(&mut cb);
                cb.if_icmpne(lfalse);
            }
            Ty::Long => {
                push_this(&mut cb);
                push_other(&mut cb);
                cb.lcmp();
                cb.ifne(lfalse);
            }
            Ty::Float | Ty::Double => {
                let (wrap, pd) = prim_wrapper(*jt).unwrap();
                let valueof = cw.methodref(wrap, "valueOf", &format!("({pd})L{wrap};"));
                push_this(&mut cb);
                cb.invokestatic(valueof, slot_words(*jt) as i32, 1);
                push_other(&mut cb);
                cb.invokestatic(valueof, slot_words(*jt) as i32, 1);
                let eq = cw.methodref(wrap, "equals", "(Ljava/lang/Object;)Z");
                cb.invokevirtual(eq, 1, 1);
                cb.ifeq(lfalse);
            }
            _ if jt.is_array() => {
                let arr_desc = arrays_param_desc(*jt);
                let eq = cw.methodref(
                    "java/util/Arrays",
                    "equals",
                    &format!("({arr_desc}{arr_desc})Z"),
                );
                push_this(&mut cb);
                push_other(&mut cb);
                cb.invokestatic(eq, 2, 1);
                cb.ifeq(lfalse);
            }
            _ => {
                // Reference member (String / enum / nested annotation): Object.equals.
                push_this(&mut cb);
                push_other(&mut cb);
                let eq = cw.methodref("java/lang/Object", "equals", "(Ljava/lang/Object;)Z");
                cb.invokevirtual(eq, 1, 1);
                cb.ifeq(lfalse);
            }
        }
    }
    cb.push_int(1, cw);
    cb.ireturn();
    cb.bind(lfalse);
    let impl_ref = cw.class_ref(fq);
    let obj_ref = cw.class_ref("java/lang/Object");
    cb.add_frame_if_new(
        lfalse,
        vec![VerifType::Object(impl_ref), VerifType::Object(obj_ref)],
        vec![],
    );
    cb.push_int(0, cw);
    cb.ireturn();
    cb.set_needs_stackmap();
    cb.link();
    cw.add_method(0x0011, "equals", "(Ljava/lang/Object;)Z", &cb);
}

/// `Arrays.equals`/`Arrays.hashCode`/`Arrays.toString` parameter descriptor for an array member: a
/// primitive specialized array has its own overload (`[I`), a reference `Array<T>` uses
/// `[Ljava/lang/Object;` (array covariance lets a `String[]`/`Enum[]` flow in). Keyed off the array
/// KIND (its class), not the element — `Array<Int>` is a reference `Integer[]`, not `[I`.
fn arrays_param_desc(array: Ty) -> String {
    if array.is_reference_array() {
        "[Ljava/lang/Object;".to_string()
    } else {
        type_descriptor(array)
    }
}

/// `hashCode()I` for an annotation impl: the contract sum of `(127 * memberName.hashCode()) ^
/// memberValue.hashCode()` over members (arrays via `Arrays.hashCode`, primitives via their wrappers'
/// static `hashCode`). Straight-line (no frames).
fn emit_annotation_hashcode(cw: &mut ClassWriter, fq: &str, members: &[(String, Ty)]) {
    let mut cb = CodeBuilder::new(1);
    cb.push_int(0, cw); // acc
    for (name, jt) in members {
        cb.push_int(127i32.wrapping_mul(java_string_hash(name)), cw);
        // value.hashCode():
        let fref = cw.fieldref(fq, name, &type_descriptor(*jt));
        cb.aload(0);
        cb.getfield(fref, slot_words(*jt) as i32);
        match jt {
            Ty::Int | Ty::Short | Ty::Byte | Ty::Char => { /* int value IS its hashCode */ }
            Ty::Boolean | Ty::Long | Ty::Float | Ty::Double => {
                let (wrap, pd) = prim_wrapper(*jt).unwrap();
                let hc = cw.methodref(wrap, "hashCode", &format!("({pd})I"));
                cb.invokestatic(hc, slot_words(*jt) as i32, 1);
            }
            _ if jt.is_array() => {
                let ad = arrays_param_desc(*jt);
                let hc = cw.methodref("java/util/Arrays", "hashCode", &format!("({ad})I"));
                cb.invokestatic(hc, 1, 1);
            }
            _ => {
                let hc = cw.methodref("java/lang/Object", "hashCode", "()I");
                cb.invokevirtual(hc, 0, 1);
            }
        }
        cb.ixor();
        cb.iadd();
    }
    cb.ireturn();
    finish_code::<0x0011>(cw, "hashCode", "()I", &mut cb, 1);
}

/// `toString()` for an annotation impl: `@<fqName>(m1=v1, m2=v2, …)` built with a `StringBuilder` (arrays
/// rendered via `Arrays.toString`). Straight-line (no frames).
fn emit_annotation_tostring(cw: &mut ClassWriter, fq: &str, iface: &str, members: &[(String, Ty)]) {
    let mut cb = CodeBuilder::new(1);
    let sb = "java/lang/StringBuilder";
    let sb_cls = cw.class_ref(sb);
    cb.new_obj(sb_cls);
    cb.dup();
    let sb_init = cw.methodref(sb, "<init>", "()V");
    cb.invokespecial(sb_init, 0, 0);
    let append_str = cw.methodref(
        sb,
        "append",
        "(Ljava/lang/String;)Ljava/lang/StringBuilder;",
    );
    let append_lit = |cb: &mut CodeBuilder, cw: &mut ClassWriter, s: &str| {
        cb.push_string(s, cw);
        cb.invokevirtual(append_str, 1, 1);
    };
    append_lit(&mut cb, cw, &format!("@{}(", iface.replace('/', ".")));
    for (i, (name, jt)) in members.iter().enumerate() {
        append_lit(
            &mut cb,
            cw,
            &format!("{}{}=", if i == 0 { "" } else { ", " }, name),
        );
        let fref = cw.fieldref(fq, name, &type_descriptor(*jt));
        match jt {
            _ if jt.is_array() => {
                cb.aload(0);
                cb.getfield(fref, 1);
                let ad = arrays_param_desc(*jt);
                let ats = cw.methodref(
                    "java/util/Arrays",
                    "toString",
                    &format!("({ad})Ljava/lang/String;"),
                );
                cb.invokestatic(ats, 1, 1);
                cb.invokevirtual(append_str, 1, 1);
            }
            Ty::Int | Ty::Short | Ty::Byte => {
                cb.aload(0);
                cb.getfield(fref, 1);
                let ap = cw.methodref(sb, "append", "(I)Ljava/lang/StringBuilder;");
                cb.invokevirtual(ap, 1, 1);
            }
            Ty::Char => {
                cb.aload(0);
                cb.getfield(fref, 1);
                let ap = cw.methodref(sb, "append", "(C)Ljava/lang/StringBuilder;");
                cb.invokevirtual(ap, 1, 1);
            }
            Ty::Boolean => {
                cb.aload(0);
                cb.getfield(fref, 1);
                let ap = cw.methodref(sb, "append", "(Z)Ljava/lang/StringBuilder;");
                cb.invokevirtual(ap, 1, 1);
            }
            Ty::Long => {
                cb.aload(0);
                cb.getfield(fref, 2);
                let ap = cw.methodref(sb, "append", "(J)Ljava/lang/StringBuilder;");
                cb.invokevirtual(ap, 2, 1);
            }
            Ty::Float => {
                cb.aload(0);
                cb.getfield(fref, 1);
                let ap = cw.methodref(sb, "append", "(F)Ljava/lang/StringBuilder;");
                cb.invokevirtual(ap, 1, 1);
            }
            Ty::Double => {
                cb.aload(0);
                cb.getfield(fref, 2);
                let ap = cw.methodref(sb, "append", "(D)Ljava/lang/StringBuilder;");
                cb.invokevirtual(ap, 2, 1);
            }
            Ty::String => {
                cb.aload(0);
                cb.getfield(fref, 1);
                cb.invokevirtual(append_str, 1, 1);
            }
            _ => {
                cb.aload(0);
                cb.getfield(fref, 1);
                let ap = cw.methodref(
                    sb,
                    "append",
                    "(Ljava/lang/Object;)Ljava/lang/StringBuilder;",
                );
                cb.invokevirtual(ap, 1, 1);
            }
        }
    }
    append_lit(&mut cb, cw, ")");
    let to_str = cw.methodref(sb, "toString", "()Ljava/lang/String;");
    cb.invokevirtual(to_str, 0, 1);
    cb.areturn();
    finish_code::<0x0011>(cw, "toString", "()Ljava/lang/String;", &mut cb, 1);
}

/// Emit an `interface`: `ACC_PUBLIC|ACC_INTERFACE|ACC_ABSTRACT`, extends `java/lang/Object`. A method
/// with no body is a `public abstract` declaration; a method WITH a body is a Kotlin default method —
/// emitted as a concrete instance method (Code, no `ACC_ABSTRACT`), which the JVM treats as a default
/// method.
fn emit_interface_class(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    facade: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
    class_meta: Option<&KotlinMetadata>,
    extra: &mut Vec<(String, Vec<u8>)>,
) -> Vec<u8> {
    let bodies = env.bodies;
    let fq_name = c.fq_name();
    let mut cw = new_writer(&fq_name, "java/lang/Object", opts);
    cw.set_access(0x0001 | 0x0200 | 0x0400); // PUBLIC | INTERFACE | ABSTRACT
    for itf in c.interfaces.iter_rendered() {
        cw.add_interface(&itf);
    }
    let mut default_impls: Option<ClassWriter> = None;
    for &fid in &c.methods {
        let f = &ir.functions[fid as usize];
        if f.body.is_some() {
            // A default method — concrete instance method on the interface.
            emit_method(ir, fid, &fq_name, facade, &mut cw, !f.is_static, env);
        } else {
            cw.add_abstract_method(0x0001 | 0x0400, &f.name, &ir_method_desc(&f.params, &f.ret));
            // PUBLIC | ABSTRACT
        }
        // An interface method with default parameters gets a STATIC `<name>$default(iface, params…, mask,
        // marker)` (the JVM realization of interface default args) — it applies the defaults then dispatches
        // to the abstract method via `invokeinterface`. kotlinc emits it ON THE INTERFACE (call sites use
        // it) AND a compatibility copy on the `<Iface>$DefaultImpls` holder class (`public final`).
        if let Some(defaults) = ir.param_defaults(fid) {
            emit_default_stub(ir, fid, &fq_name, facade, &mut cw, defaults, env, true);
            let di = default_impls.get_or_insert_with(|| {
                let mut w =
                    new_writer(&format!("{fq_name}$DefaultImpls"), "java/lang/Object", opts);
                w.set_access(0x0011 | 0x0020); // PUBLIC | FINAL | SUPER
                w
            });
            emit_default_stub(ir, fid, &fq_name, facade, di, defaults, env, true);
        }
    }
    if let Some(di) = default_impls {
        extra.push((format!("{fq_name}$DefaultImpls"), di.finish()));
    }
    // A companion `val` on the interface is a `public static final` field ON THE INTERFACE (interface
    // fields are implicitly static final): a `const val` as a `ConstantValue`, a non-const `val`
    // initialized in the interface's `<clinit>`. Read as `getstatic C.X`.
    for s in ir.statics.iter().filter(|s| s.owner_matches(&fq_name)) {
        let desc = ir_type_desc(&s.ty);
        if let Some(cv) = const_value_idx(ir, s.init, &mut cw) {
            cw.add_field_const(0x0019, &s.name, &desc, cv); // PUBLIC | STATIC | FINAL
        } else {
            cw.add_field(0x0019, &s.name, &desc);
        }
    }
    // A `companion object` with methods: a `public static final Companion` field of the synthesized
    // `C$Companion` type, constructed in the interface's `<clinit>` (alongside any non-const statics).
    if let Some(comp_fq) = c.companion_class() {
        cw.add_field(0x0019, "Companion", &format!("L{comp_fq};"));
    }
    let clinit_statics: Vec<&crate::ir::IrStatic> = ir
        .statics
        .iter()
        .filter(|s| s.owner_matches(&fq_name) && !(s.is_const && const_value_idx_peek(ir, s.init)))
        .collect();
    if c.companion_class.is_some() || !clinit_statics.is_empty() {
        let mut e = Emitter {
            ir,
            cw: &mut cw,
            bodies,
            run: env.run,
            owner: fq_name.clone(),
            facade: facade.to_string(),
            slots: HashMap::new(),
            var_types: collect_var_types(ir),
            next_slot: 0,
            ret: Ty::Unit,
            loop_stack: Vec::new(),
            pending_stack: Vec::new(),
        };
        let mut clinit = CodeBuilder::new(0);
        if let Some(comp_fq) = c.companion_class() {
            let comp_desc = format!("L{comp_fq};");
            let ci = e.cw.class_ref(&comp_fq);
            clinit.new_obj(ci);
            clinit.dup();
            // Through the companion's `(DefaultConstructorMarker)` accessor — its real ctor is private.
            clinit.aconst_null();
            let init = e.cw.methodref(
                &comp_fq,
                "<init>",
                "(Lkotlin/jvm/internal/DefaultConstructorMarker;)V",
            );
            clinit.invokespecial(init, 1, 0);
            let fref = e.cw.fieldref(&fq_name, "Companion", &comp_desc);
            clinit.putstatic(fref, 1);
        }
        for s in &clinit_statics {
            e.emit_value(s.init, &mut clinit);
            let jt = ir_ty_to_jvm(&s.ty);
            let fref = e.cw.fieldref(&fq_name, &s.name, &type_descriptor(jt));
            clinit.putstatic(fref, slot_words(jt) as i32);
        }
        clinit.ret_void();
        clinit.ensure_locals(e.next_slot);
        clinit.link();
        e.cw.add_method(0x0008, "<clinit>", "()V", &clinit);
    }
    // An interface is a VIEW of the same `IrClass` every other kind is — compute its `@Metadata` (and
    // therefore its debug tables/annotations) through the shared path, exactly like `emit_class`.
    let computed = (class_meta.is_none() && opts.emit_class_metadata)
        .then(|| build_class_metadata(ir, c, opts))
        .flatten();
    if computed.is_some() {
        attach_synth_debug_tables(c, &mut cw);
        attach_declared_method_debug(ir, c, &mut cw);
        attach_synth_nullability(c, &mut cw);
    }
    if let Some(m) = class_meta.or(computed.as_ref()) {
        cw.set_kotlin_metadata(m.k, &m.mv, m.xi, &m.d1, &m.d2);
    }
    cw.finish()
}

/// Emit an `enum class`: extends `java/lang/Enum`, a private `(String name, int ordinal, …)` ctor →
/// `super(name, ordinal)`, a `public static final` field per entry plus a `$VALUES` array, a
/// `<clinit>` that constructs the entries and fills `$VALUES`, and synthetic `values()`/`valueOf`.
fn emit_enum_class(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    facade: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
) -> Vec<u8> {
    let bodies = env.bodies;
    const ACC_ENUM: u16 = 0x4000;
    const ACC_SYNTHETIC: u16 = 0x1000;
    let fq = c.fq_name();
    let self_desc = format!("L{fq};");
    let arr_desc = format!("[{self_desc}");
    let mut cw = new_writer(&fq, "java/lang/Enum", opts);
    // An enum with an abstract member is `ACC_ABSTRACT`; one with any bodied entry (so a subclass
    // extends it) must not be `final`. A plain enum stays `final`.
    let has_abstract = c
        .methods
        .iter()
        .any(|&fid| ir.functions[fid as usize].body.is_none());
    let has_subclass = c.enum_entries.iter().any(|e| e.subclass.is_some());
    let mut access = 0x0001 | 0x0020 | ACC_ENUM; // PUBLIC | SUPER | ENUM
    if has_abstract {
        access |= 0x0400;
    } // ABSTRACT
    if !has_abstract && !has_subclass {
        access |= 0x0010;
    } // FINAL
    cw.set_access(access);
    // Every enum extends the generic `java.lang.Enum<Self>`, so kotlinc emits a class `Signature`
    // (`Ljava/lang/Enum<LSelf;>;` plus a raw `L<itf>;` for each superinterface). The erased
    // descriptor already names `java/lang/Enum`; the Signature carries the `<Self>` type argument.
    let mut sig = format!("Ljava/lang/Enum<L{fq};>;");
    for itf in c.interfaces.iter_rendered() {
        sig.push('L');
        sig.push_str(&itf);
        sig.push(';');
    }
    cw.set_signature(&sig);
    // Interfaces the enum implements (`enum class E : I`) — without these the JVM rejects an
    // interface-typed call with `IncompatibleClassChangeError`.
    for itf in c.interfaces.iter_rendered() {
        cw.add_interface(&itf);
    }

    let field_tys = field_jvm_tys(&c.fields);
    // (bridges emitted after the methods below — `emit_bridges` references emitted method refs)
    let n_params = c.ctor_param_count as usize;
    let user_tys: Vec<Ty> = field_tys[..n_params].to_vec();
    // Property backing fields are private (kotlinc), reached through the synthesized `getX()`/`setX()`
    // accessors — for both the primary-constructor fields and body member-property fields
    // (`enum class E { A; val x = … }`), initialized in the constructor via `init_body`.
    let enum_field_acc = |f: &IrField| {
        (if f.is_private { 0x0002 } else { 0x0001 }) | if f.is_final { 0x0010 } else { 0 }
    };
    for (f, t) in c.fields[..n_params].iter().zip(&user_tys) {
        cw.add_field(enum_field_acc(f), &f.name, &type_descriptor(*t));
    }
    for (f, t) in c.fields[n_params..].iter().zip(&field_tys[n_params..]) {
        cw.add_field(enum_field_acc(f), &f.name, &type_descriptor(*t));
    }
    // One static-final constant per entry, plus the private `$VALUES` array.
    for entry in &c.enum_entries {
        cw.add_field(0x0001 | 0x0008 | 0x0010 | ACC_ENUM, &entry.name, &self_desc);
        apply_field_annotations(&mut cw, c, &entry.name);
    }
    cw.add_field(
        0x0002 | 0x0008 | 0x0010 | ACC_SYNTHETIC,
        "$VALUES",
        &arr_desc,
    );
    // The `entries` property backing (Kotlin 2.x emits this on EVERY enum): a `private static final`
    // `kotlin/enums/EnumEntries`, initialized in `<clinit>` from `EnumEntriesKt.enumEntries($VALUES)`.
    cw.add_field(
        0x0002 | 0x0008 | 0x0010 | ACC_SYNTHETIC,
        "$ENTRIES",
        "Lkotlin/enums/EnumEntries;",
    );
    // A `@Serializable enum`'s serializer machinery: a `public static final Companion` field + any
    // owner-scoped statics the serialization plugin synthesized (`$cachedSerializer$delegate`), both
    // initialized in `<clinit>` below.
    if let Some(comp_fq) = c.companion_class() {
        cw.add_field(0x0019, "Companion", &format!("L{comp_fq};")); // PUBLIC | STATIC | FINAL
    }
    let owner_statics: Vec<&crate::ir::IrStatic> =
        ir.statics.iter().filter(|s| s.owner_matches(&fq)).collect();
    for s in &owner_statics {
        let acc = if s.visibility.is_private() {
            0x001A // PRIVATE | STATIC | FINAL
        } else {
            0x0019 // PUBLIC | STATIC | FINAL
        };
        cw.add_field(acc, &s.name, &ir_type_desc(&s.ty));
    }

    // Private constructor `(Ljava/lang/String;I<user params>)V` → `super(name, ordinal)` then store the
    // property params / run the body-property initializers. The user params are ALL primary-ctor params
    // (from `ctor_args`) — a `val`/`var` param backs a field, a plain param is an argument only (in scope
    // for a body-property initializer), so `all_param_tys` can be wider than the `n_params` fields.
    let all_param_tys = class_ctor_jvm_tys(c);
    let ctor_params: Vec<Ty> = [Ty::String, Ty::Int]
        .into_iter()
        .chain(all_param_tys.iter().copied())
        .collect();
    let ctor_desc = method_descriptor(&ctor_params, Ty::Unit);
    let ctor_words: u16 = ctor_params.iter().map(|t| slot_words(*t)).sum();
    let mut ctor = CodeBuilder::new(1 + ctor_words);
    ctor.aload(0);
    ctor.aload(1);
    load(Ty::Int, 2, &mut ctor);
    let super_init = cw.methodref("java/lang/Enum", "<init>", "(Ljava/lang/String;I)V");
    ctor.invokespecial(super_init, 2, 0);
    let mut max_locals = 1 + ctor_words;
    // When body-property initializers exist, the lowered `init_body` carries BOTH the property-param→
    // field stores AND the body inits (it set `explicit_param_stores`). Emit it through the standard IR
    // emitter, mapping value ids onto the enum's slot layout — `this` at 0, then EVERY user param at
    // slots 3+ (after the synthetic `name`/`ordinal`), in declaration order. Otherwise hand-store just
    // the property-param fields (a plain param has no field), reading each at its own slot.
    if let Some(init_body) = c.init_body.filter(|_| c.fields.len() > n_params) {
        let mut e = Emitter {
            ir,
            cw: &mut cw,
            bodies,
            run: env.run,
            owner: fq.clone(),
            facade: facade.to_string(),
            slots: HashMap::new(),
            var_types: collect_var_types(ir),
            next_slot: 1 + ctor_words,
            ret: Ty::Unit,
            loop_stack: Vec::new(),
            pending_stack: Vec::new(),
        };
        e.slots.insert(0, (0, Ty::obj(&fq)));
        let mut s = 3u16;
        for (i, t) in all_param_tys.iter().enumerate() {
            e.slots.insert(i as u32 + 1, (s, *t));
            s += slot_words(*t);
        }
        e.emit(init_body, &mut ctor);
        max_locals = max_locals.max(e.next_slot);
    } else {
        let mut slot = 3u16;
        let mut field_i = 0usize;
        for (a, t) in c.ctor_args.iter().zip(&all_param_tys) {
            if a.is_field {
                let name = &c.fields[field_i].name;
                ctor.aload(0);
                load(*t, slot, &mut ctor);
                let fref = cw.fieldref(&fq, name, &type_descriptor(*t));
                ctor.putfield(fref, slot_words(*t) as i32);
                field_i += 1;
            }
            slot += slot_words(*t);
        }
    }
    ctor.ret_void();
    ctor.ensure_locals(max_locals);
    ctor.link();
    // A plain enum's constructor is `private` (matching kotlinc — javap then hides the synthetic
    // `(String,int)` params in its display). A subclassed enum's ctor must be reachable from its entry
    // subclasses' `<init>` (an `invokespecial` from another class): kotlinc keeps it `private` and relies
    // on nestmate access, which krusty doesn't emit, so it stays package-private + synthetic here.
    let base_ctor_acc = if has_subclass { ACC_SYNTHETIC } else { 0x0002 };
    // kotlinc emits a generic `Signature` on the enum ctor listing only the USER params (the synthetic
    // leading `(String, int)` are excluded) — e.g. `()V` for a plain enum, `(I)V` for `E(val n: Int)`.
    // javap reads it to display `Color()` instead of `Color(String, int)`; without it the synthetic
    // params leak into the disassembly (a per-enum divergence from kotlinc).
    let ctor_sig = {
        let mut s = String::from("(");
        for t in &all_param_tys {
            s.push_str(&type_descriptor(*t));
        }
        s.push_str(")V");
        s
    };
    cw.add_method_sig(base_ctor_acc, "<init>", &ctor_desc, &ctor, Some(&ctor_sig));

    // <clinit>: construct each entry, then `$VALUES = $values()` and
    // `$ENTRIES = EnumEntriesKt.enumEntries($VALUES)`. BUILT here but ADDED last (kotlinc orders it
    // after values/valueOf/getEntries/$values); the linked `CodeBuilder` is self-contained.
    let ctor_argw: i32 = ctor_params.iter().map(|t| slot_words(*t) as i32).sum();
    let clinit = {
        let mut e = Emitter {
            ir,
            cw: &mut cw,
            bodies,
            run: env.run,
            owner: fq.clone(),
            facade: facade.to_string(),
            slots: HashMap::new(),
            var_types: collect_var_types(ir),
            next_slot: 0,
            ret: Ty::Unit,
            loop_stack: Vec::new(),
            pending_stack: Vec::new(),
        };
        let mut clinit = CodeBuilder::new(0);
        for (i, entry) in c.enum_entries.iter().enumerate() {
            let args = &entry.args;
            // A branchy entry arg (`X(1 == 1)`) must run on a clean stack — spill all args to temps
            // first, then construct (mirrors the `New` node's spill).
            let spill = args.iter().any(|&a| e.records_frame(a));
            let temps = if spill {
                e.spill_to_temps(args, &mut clinit)
            } else {
                Vec::new()
            };
            // A bodied entry is an instance of its synthesized subclass (`new Enum$ENTRY(...)`); the
            // subclass constructor shares the enum's `(String,int,<user>)V` descriptor.
            let new_class = entry
                .subclass
                .map(TypeName::render)
                .unwrap_or_else(|| fq.clone());
            let cls = e.cw.class_ref(&new_class);
            clinit.new_obj(cls);
            clinit.dup();
            clinit.push_string(&entry.name, e.cw);
            clinit.push_int(i as i32, e.cw);
            if spill {
                for &(slot, t, _) in &temps {
                    load(t, slot, &mut clinit);
                }
                for &(_, _, key) in &temps {
                    e.slots.remove(&key);
                }
            } else {
                for &a in args {
                    e.emit_value(a, &mut clinit);
                }
            }
            let ctor_ref = e.cw.methodref(&new_class, "<init>", &ctor_desc);
            clinit.invokespecial(ctor_ref, ctor_argw, 0);
            let fref = e.cw.fieldref(&fq, &entry.name, &self_desc);
            clinit.putstatic(fref, 1);
        }
        // `$VALUES = $values()` — kotlinc factors the array build into a private `$values()` helper.
        let vfn = e.cw.methodref(&fq, "$values", &format!("(){arr_desc}"));
        clinit.invokestatic(vfn, 0, 1);
        let valref = e.cw.fieldref(&fq, "$VALUES", &arr_desc);
        clinit.putstatic(valref, 1);
        // `$ENTRIES = EnumEntriesKt.enumEntries((Enum[]) $VALUES)`.
        clinit.getstatic(valref, 1);
        let enumarr = e.cw.class_ref("[Ljava/lang/Enum;");
        clinit.checkcast(enumarr);
        let entries_fn = e.cw.methodref(
            "kotlin/enums/EnumEntriesKt",
            "enumEntries",
            "([Ljava/lang/Enum;)Lkotlin/enums/EnumEntries;",
        );
        clinit.invokestatic(entries_fn, 1, 1);
        let entref = e.cw.fieldref(&fq, "$ENTRIES", "Lkotlin/enums/EnumEntries;");
        clinit.putstatic(entref, 1);
        // A `@Serializable enum`'s serializer statics (`$cachedSerializer$delegate`) then its `Companion`
        // — same shape as a plain class's `<clinit>` companion/static init.
        for s in &owner_statics {
            e.emit_value(s.init, &mut clinit);
            let jt = ir_ty_to_jvm(&s.ty);
            let fref = e.cw.fieldref(&fq, &s.name, &type_descriptor(jt));
            clinit.putstatic(fref, slot_words(jt) as i32);
        }
        if let Some(comp_fq) = c.companion_class() {
            let comp_desc = format!("L{comp_fq};");
            let ci = e.cw.class_ref(&comp_fq);
            clinit.new_obj(ci);
            clinit.dup();
            clinit.aconst_null();
            let init = e.cw.methodref(
                &comp_fq,
                "<init>",
                "(Lkotlin/jvm/internal/DefaultConstructorMarker;)V",
            );
            clinit.invokespecial(init, 1, 0);
            let fref = e.cw.fieldref(&fq, "Companion", &comp_desc);
            clinit.putstatic(fref, 1);
        }
        clinit.ret_void();
        clinit.ensure_locals(e.next_slot.max(2));
        clinit.link();
        clinit
    };

    // values(): `$VALUES.clone()` cast back to the array type.
    let mut vals = CodeBuilder::new(0);
    let valref = cw.fieldref(&fq, "$VALUES", &arr_desc);
    vals.getstatic(valref, 1);
    // kotlinc invokes `clone()` via `java/lang/Object` (not the `[LE;` array type).
    let clone_m = cw.methodref("java/lang/Object", "clone", "()Ljava/lang/Object;");
    vals.invokevirtual(clone_m, 0, 1);
    let arr_cls = cw.class_ref(&arr_desc);
    vals.checkcast(arr_cls);
    vals.areturn();
    finish_code::<0x0009>(&mut cw, "values", &format!("(){arr_desc}"), &mut vals, 0);

    // valueOf(String): `Enum.valueOf(E.class, name)` cast to E.
    let mut vof = CodeBuilder::new(1);
    vof.ldc_class(&fq, &mut cw);
    vof.aload(0);
    let veo = cw.methodref(
        "java/lang/Enum",
        "valueOf",
        "(Ljava/lang/Class;Ljava/lang/String;)Ljava/lang/Enum;",
    );
    vof.invokestatic(veo, 2, 1);
    let cc = cw.class_ref(&fq);
    vof.checkcast(cc);
    vof.areturn();
    finish_code::<0x0009>(
        &mut cw,
        "valueOf",
        &format!("(Ljava/lang/String;){self_desc}"),
        &mut vof,
        1,
    );

    // getEntries(): the `entries` property accessor → `return $ENTRIES`. Carries the generic
    // `Signature` `()Lkotlin/enums/EnumEntries<LSelf;>;` kotlinc emits.
    let mut gent = CodeBuilder::new(0);
    let entref = cw.fieldref(&fq, "$ENTRIES", "Lkotlin/enums/EnumEntries;");
    gent.getstatic(entref, 1);
    gent.areturn();
    gent.ensure_locals(0);
    gent.link();
    cw.add_method_sig(
        0x0009,
        "getEntries",
        "()Lkotlin/enums/EnumEntries;",
        &gent,
        Some(&format!("()Lkotlin/enums/EnumEntries<L{fq};>;")),
    );

    for &fid in &c.methods {
        let f = &ir.functions[fid as usize];
        if f.body.is_some() {
            // Honor `is_static` (an extension-synthesized `static` member like serialization's
            // `serializer()` accessor) — emitting it as an instance method breaks an `E.serializer()`
            // static call (`IncompatibleClassChangeError`).
            emit_method(ir, fid, &fq, facade, &mut cw, !f.is_static, env);
        } else {
            // An abstract enum member (`abstract fun t(): String`) — declared `ACC_ABSTRACT`, the
            // entry subclasses override it.
            cw.add_abstract_method(0x0001 | 0x0400, &f.name, &ir_method_desc(&f.params, &f.ret));
        }
    }
    // $values(): build the backing array — `new E[n]` filled with each entry constant (kotlinc factors
    // this out of `<clinit>`). Private static final synthetic, returning `E[]`.
    let mut vbuild = CodeBuilder::new(1);
    vbuild.push_int(c.enum_entries.len() as i32, &mut cw);
    let acls = cw.class_ref(&fq);
    vbuild.anewarray(acls);
    vbuild.astore(0);
    for (i, entry) in c.enum_entries.iter().enumerate() {
        vbuild.aload(0);
        vbuild.push_int(i as i32, &mut cw);
        let fref = cw.fieldref(&fq, &entry.name, &self_desc);
        vbuild.getstatic(fref, 1);
        vbuild.array_store(0x53, 1); // aastore
    }
    vbuild.aload(0);
    vbuild.areturn();
    vbuild.ensure_locals(1);
    vbuild.link();
    cw.add_method(
        0x0002 | 0x0008 | 0x0010 | ACC_SYNTHETIC,
        "$values",
        &format!("(){arr_desc}"),
        &vbuild,
    );

    // <clinit> is added LAST (built earlier), matching kotlinc's member order.
    cw.add_method(0x0008, "<clinit>", "()V", &clinit);

    // Erased bridges for a generic-interface method overridden at the enum level
    // (`enum E : A<String> { …; override fun foo(t: String) }` → bridge `foo(Object)`→`foo(String)`).
    emit_bridges(c, &mut cw);
    // An enum is a VIEW of the same `IrClass` — compute its `@Metadata` (and hence debug tables /
    // annotations) through the shared path, exactly like `emit_class` and `emit_interface_class`.
    if let Some(m) = opts
        .emit_class_metadata
        .then(|| build_class_metadata(ir, c, opts))
        .flatten()
    {
        attach_synth_debug_tables(c, &mut cw);
        attach_declared_method_debug(ir, c, &mut cw);
        attach_synth_nullability(c, &mut cw);
        cw.set_kotlin_metadata(m.k, &m.mv, m.xi, &m.d1, &m.d2);
    }
    cw.finish()
}

/// Emit function `fid` as a method on `owner`. `instance` = an instance method (`this` in slot 0).
#[allow(clippy::too_many_arguments)]
fn emit_method_maybe_rescued(
    ir: &IrFile,
    fid: u32,
    owner: &str,
    facade: &str,
    cw: &mut ClassWriter,
    instance: bool,
    env: &EmitEnv,
    rescued: bool,
) {
    if rescued {
        // A rescued must-inline impl IS emitted despite its `inline_only` mark (see
        // `emit_all_with_class_meta`) — bypass the early return.
        emit_method_inner(ir, fid, owner, facade, cw, instance, env);
    } else {
        emit_method(ir, fid, owner, facade, cw, instance, env);
    }
}

fn emit_method(
    ir: &IrFile,
    fid: u32,
    owner: &str,
    facade: &str,
    cw: &mut ClassWriter,
    instance: bool,
    env: &EmitEnv,
) {
    // An inline-only lambda impl (its body has a non-local `return`) is never a real callable method —
    // it exists only to be spliced via its `inline_body`. Emitting it would produce an invalid, dead
    // method (an `areturn` of the enclosing fn's type from the lambda's signature). Skip it.
    if ir.inline_only_fns.contains(&fid) {
        return;
    }
    emit_method_inner(ir, fid, owner, facade, cw, instance, env);
}

fn emit_method_inner(
    ir: &IrFile,
    fid: u32,
    owner: &str,
    facade: &str,
    cw: &mut ClassWriter,
    instance: bool,
    env: &EmitEnv,
) {
    let bodies = env.bodies;
    let f = &ir.functions[fid as usize];
    let body = f.body.unwrap();
    let param_tys = jvm_tys(&f.params);
    let ret = ir_ty_to_jvm(&f.ret);
    let mut e = Emitter {
        ir,
        cw,
        bodies,
        run: env.run,
        owner: owner.to_string(),
        facade: facade.to_string(),
        slots: HashMap::new(),
        var_types: collect_var_types(ir),
        next_slot: 0,
        ret,
        loop_stack: Vec::new(),
        pending_stack: Vec::new(),
    };
    if instance {
        e.slots.insert(0, (0, Ty::obj(owner)));
        e.next_slot = 1;
    }
    for (i, t) in param_tys.iter().enumerate() {
        let vi = i as u32 + if instance { 1 } else { 0 };
        let slot = e.next_slot;
        e.slots.insert(vi, (slot, *t));
        e.next_slot += slot_words(*t);
    }
    let mut code = CodeBuilder::new(e.next_slot);
    // kotlinc guards each non-null reference parameter of a visible function with
    // `Intrinsics.checkNotNullParameter(param, "name")` at method entry — emit the same.
    let param_checks = f.param_checks.clone();
    for (i, check) in param_checks.iter().enumerate() {
        if let Some(name) = check {
            let vi = i as u32 + if instance { 1 } else { 0 };
            if let Some(&(slot, _)) = e.slots.get(&vi) {
                code.aload(slot);
                code.push_string(name, e.cw);
                let m = e.cw.methodref(
                    "kotlin/jvm/internal/Intrinsics",
                    "checkNotNullParameter",
                    "(Ljava/lang/Object;Ljava/lang/String;)V",
                );
                code.invokestatic(m, 2, 0);
            }
        }
    }
    e.emit(body, &mut code);
    // The implicit `return` for a `Unit` function is dead code when the body already diverges
    // (`fun foo() { throw … }`): an unreachable `return` after `athrow` has no stack-map frame and
    // the verifier rejects it. Skip it exactly when the body can't fall through.
    if ret == Ty::Unit && !e.diverges(body) {
        code.ret_void();
    }
    code.ensure_locals(e.next_slot);
    code.link();
    // Top-level/`static` functions are always `final` (kotlinc emits `public static final`). An
    // instance method of a *final* class (nothing extends it) is also `final` and can never be
    // overridden, so marking it is safe; in an open/extended class we conservatively leave it
    // non-`final` (a method-level `open`/`override` model would refine this).
    let access = if instance {
        // kotlinc keeps an `Object`-override (a data class's toString/hashCode/equals) open even in a
        // final class, so honor `open_methods`; otherwise a method of a final class is itself final.
        let final_class = !ir.classes.iter().any(|o| o.superclass_matches(owner));
        // An interface default method must NOT be `final` (the JVM rejects a final interface method).
        let owner_is_iface = ir
            .classes
            .iter()
            .any(|o| o.fq_name_matches(owner) && o.is_interface);
        let fin = final_class && !ir.open_methods.contains(&fid) && !owner_is_iface;
        // A `private set` setter is `private final` (kotlinc); else `public` (+`final` per above).
        let vis = if ir.private_methods.contains(&fid) {
            0x0002
        } else {
            0x0001
        };
        // A private method is `final` on a CLASS, but a private INTERFACE method must NOT carry `ACC_FINAL`
        // (`ClassFormatError: illegal modifiers 0x12`) — private already makes it non-virtual.
        vis | if fin || (ir.private_methods.contains(&fid) && !owner_is_iface) {
            0x0010
        } else {
            0
        }
    } else {
        // A `static` method is `<vis> static final` (kotlinc) — EXCEPT on an interface, where a `final`
        // static method is illegal (`ClassFormatError`), or a value class's `constructor-impl`/
        // `<name>-impl` delegate members, which kotlinc emits `public static` (non-`final`) and marks via
        // `open_methods`. `box-impl`/`equals-impl0` stay `public static final` (not opened). Visibility
        // derives from the member's own (a private declaration — or a lambda impl, which kotlinc always
        // emits private — is `ACC_PRIVATE`).
        let owner_is_iface = ir
            .classes
            .iter()
            .any(|o| o.fq_name_matches(owner) && o.is_interface);
        let vis = if ir.private_methods.contains(&fid) {
            0x0002
        } else {
            0x0001
        };
        if owner_is_iface || ir.open_methods.contains(&fid) {
            vis | 0x0008 // <vis> | STATIC
        } else {
            vis | 0x0018 // <vis> | STATIC | FINAL
        }
    };
    // A value class's `box-impl`/`unbox-impl` are compiler-manufactured box adapters — kotlinc marks them
    // `ACC_SYNTHETIC`.
    let access = access
        | if ir.synthetic_methods.contains(&fid) {
            0x1000
        } else {
            0
        }
        | if ir.bridge_methods.contains(&fid) {
            0x0040 // ACC_BRIDGE
        } else {
            0
        };
    // A method with own type parameters (`fun <T> …`) → the tparam-based signature; otherwise a method
    // whose concrete param/return type is PARAMETERIZED (`getXs(): List<String>`, `copy(List<String>)`)
    // → its generic signature. `f.params`/`f.ret` are the SOURCE types (retain `<…>` args); `param_tys`/
    // `ret` are erased.
    let signature = ir
        .signatures
        .get(&fid)
        .and_then(|g| jvm_method_signature(g, f))
        .or_else(|| method_parameterized_sig(&f.params, &f.ret));
    let desc = method_descriptor(&param_tys, ret);
    e.cw.add_method_sig(access, &f.name, &desc, &code, signature.as_deref());
    if ir.deprecated_methods.contains(&fid) {
        e.cw.mark_method_deprecated(&f.name, &desc);
    }
}

/// Format a function's backend-agnostic [`crate::ir::IrGenericSig`] into a JVM generic `Signature`
/// (`<T:Ljava/lang/Object;>(TT;)TT;`). `None` if a bound can't be represented yet. Concrete parameter/
/// return descriptors come from the (erased) `IrFunction`; bare type-parameter positions are `T<name>;`.
fn jvm_method_signature(g: &crate::ir::IrGenericSig, f: &crate::ir::IrFunction) -> Option<String> {
    let mut s = jvm_type_params(g)?;
    s.push('(');
    for (i, pt) in g.param_tparams.iter().enumerate() {
        match pt {
            Some(name) => s.push_str(&format!("T{name};")),
            None => s.push_str(&ir_type_desc(&f.params[i])),
        }
    }
    s.push(')');
    match &g.ret_tparam {
        Some(name) => s.push_str(&format!("T{name};")),
        None => s.push_str(&ir_type_desc(&f.ret)),
    }
    Some(s)
}

/// Format a class's generic shape into a JVM class `Signature` (`<T:Ljava/lang/Object;>Ljava/lang/Object;`).
fn jvm_class_signature(g: &crate::ir::IrGenericSig) -> Option<String> {
    let mut s = jvm_type_params(g)?;
    if g.supers.is_empty() {
        // A plain generic class with no (parameterized) supertypes: just extends `Object`.
        s.push_str("Ljava/lang/Object;");
    } else {
        // The parameterized superclass + interfaces (`Ljava/lang/Object;LOperation<Lkotlin/Result<..>;>;`),
        // formatted from the platform-agnostic `Ty`s so a reader recovers a member's concrete generic return.
        for sup in &g.supers {
            s.push_str(&ty_generic_sig(sup)?);
        }
    }
    Some(s)
}

/// A `Ty` as a JVM generic-signature type element: a primitive in a generic position is its BOXED wrapper
/// (`Int` → `Ljava/lang/Integer;`), a reference maps its internal (`kotlin/Any` → `java/lang/Object`) and
/// carries its (recursively formatted) type arguments. `None` for a shape not representable here.
fn ty_generic_sig(t: &Ty) -> Option<String> {
    if let Some(boxed) = t.boxed_ref() {
        // A scalar in a generic position boxes; `boxed_ref` gives its wrapper `Obj` (`Integer`, …).
        return Some(type_descriptor(boxed));
    }
    match t {
        Ty::String => Some("Ljava/lang/String;".to_string()),
        Ty::Unit => Some("Lkotlin/Unit;".to_string()),
        Ty::Obj(internal, args) => {
            let internal = internal.render();
            let jvm = super::jvm_class_map::to_jvm_internal(&internal);
            let mut s = format!("L{jvm}");
            if !args.is_empty() {
                s.push('<');
                for a in args.iter() {
                    s.push_str(&ty_generic_sig(a)?);
                }
                s.push('>');
            }
            s.push(';');
            Some(s)
        }
        _ => None,
    }
}

/// The generic `Signature` element for a parameterized concrete type (`List<String>` →
/// `Ljava/util/List<Ljava/lang/String;>;`); `None` when erasure loses nothing. `T?` unwraps (generics
/// survive nullability). Bare type parameters are handled separately via `field_signatures`.
fn parameterized_sig(ty: &Ty) -> Option<String> {
    let inner = match ty {
        Ty::Nullable(t) => t,
        t => t,
    };
    match inner {
        Ty::Obj(_, args) if !args.is_empty() => {
            let sig = ty_generic_sig(inner)?;
            (sig != ir_type_desc(inner)).then_some(sig)
        }
        _ => None,
    }
}

/// A method's generic `Signature` when a concrete param/return type is parameterized (`getXs()` →
/// `()Ljava/util/List<Ljava/lang/String;>;`); non-generic positions keep their erased descriptor,
/// `None` when none are parameterized.
fn method_parameterized_sig(params: &[Ty], ret: &Ty) -> Option<String> {
    // Runs for every emitted method — bail before building any string when no position can carry one.
    let is_parameterized = |t: &Ty| {
        let inner = match t {
            Ty::Nullable(t) => t,
            t => t,
        };
        matches!(inner, Ty::Obj(_, args) if !args.is_empty())
    };
    if !params
        .iter()
        .chain(std::iter::once(ret))
        .any(is_parameterized)
    {
        return None;
    }
    let mut any = false;
    let mut s = String::from("(");
    for p in params {
        match parameterized_sig(p) {
            Some(ps) => {
                s.push_str(&ps);
                any = true;
            }
            None => s.push_str(&ir_type_desc(p)),
        }
    }
    s.push(')');
    match parameterized_sig(ret) {
        Some(ps) => {
            s.push_str(&ps);
            any = true;
        }
        None => s.push_str(&ir_type_desc(ret)),
    }
    any.then_some(s)
}

/// The primary constructor's generic `Signature` — bare type-parameter params (`(TT;)V`) and
/// parameterized concrete params (`(Ljava/util/List<Ljava/lang/String;>;)V`), others erased; `None` when
/// none need generics. Shared by the pool seeder and the attribute emitter so both produce one string.
fn class_ctor_generic_sig(ir: &IrFile, c: &crate::ir::IrClass, fq_name: &str) -> Option<String> {
    let param_tys = class_ctor_jvm_tys(c);
    let ftp = ir.field_signatures(fq_name);
    let is_field: Vec<bool> = if c.ctor_args.is_empty() {
        vec![true; param_tys.len()]
    } else {
        c.ctor_args.iter().map(|a| a.is_field).collect()
    };
    let mut sig = String::from("(");
    let mut any = false;
    let mut field_i = 0usize;
    for (i, t) in param_tys.iter().enumerate() {
        if is_field.get(i).copied().unwrap_or(true) {
            let f = c.fields.get(field_i);
            let fname = f.map(|f| f.name.as_str()).unwrap_or("");
            if let Some((_, tp)) = ftp.and_then(|ftp| ftp.iter().find(|(fp, _)| fp == fname)) {
                sig.push_str(&format!("T{tp};"));
                any = true;
            } else if let Some(ps) = f.and_then(|f| parameterized_sig(&f.ty)) {
                sig.push_str(&ps);
                any = true;
            } else {
                sig.push_str(&type_descriptor(*t));
            }
            field_i += 1;
        } else {
            sig.push_str(&type_descriptor(*t));
        }
    }
    sig.push_str(")V");
    any.then_some(sig)
}

/// The shared `<T:bound…>` type-parameter DECLARATION section, or `""` when there are no own type
/// parameters (e.g. a generic class's getter `getA()` → `()TA;` USES the class's `A` but declares none).
/// `None` if any bound can't be represented.
fn jvm_type_params(g: &crate::ir::IrGenericSig) -> Option<String> {
    if g.type_params.is_empty() {
        return Some(String::new());
    }
    let mut s = String::from("<");
    for (name, bound) in &g.type_params {
        s.push_str(name);
        s.push(':');
        s.push_str(&jvm_bound_descriptor(bound)?);
    }
    s.push('>');
    Some(s)
}

/// A type-parameter upper bound as a JVM signature element: `kotlin/Any` → `Ljava/lang/Object;`, a
/// primitive → its boxed wrapper (`kotlin/Int` → `Ljava/lang/Integer;`). `None` for anything else.
fn jvm_bound_descriptor(bound: &Ty) -> Option<String> {
    let ty = ir_ty_to_jvm(bound);
    if ty == Ty::obj("kotlin/Any") {
        return Some("Ljava/lang/Object;".to_string());
    }
    if ty.is_jvm_scalar() {
        return ty.nullable_boxed().map(type_descriptor);
    }
    // A reference bound — `T : Foo`, `T : CharSequence` (ir_lower already suppressed parameterized
    // bounds) — emits its erased class descriptor `L<internal>;`, mapping a Kotlin built-in
    // (`kotlin/CharSequence` → `java/lang/CharSequence`) the same way the emitter maps any owner.
    match ty {
        Ty::String => Some("Ljava/lang/String;".to_string()),
        Ty::Obj(n, _) => Some(format!(
            "L{};",
            crate::jvm::jvm_class_map::to_jvm_internal(&n.render())
        )),
        _ => None,
    }
}

/// Emit the JVM `<name>$default(self, params…, mask: int, marker: Object)` synthetic stub for an
/// instance method with default-valued parameters: for each defaulted param, `if ((mask & (1<<i)) != 0)
/// param = <default>;` then tail-call the real method. The default-value exprs reference `self` as value
/// 0. This is the JVM realization of default arguments — the `param_defaults` *meaning* is in the IR.
#[allow(clippy::too_many_arguments)]
fn emit_default_stub(
    ir: &IrFile,
    fid: u32,
    owner: &str,
    facade: &str,
    cw: &mut ClassWriter,
    defaults: &[Option<u32>],
    env: &EmitEnv,
    is_interface: bool,
) {
    let bodies = env.bodies;
    let f = &ir.functions[fid as usize];
    let method_name = f.name.clone();
    // The REAL (base-method) param types unbox every value class. `stub_param_tys` is the `$default`
    // signature, where a nullable-underlying value-class param stays BOXED (kotlinc): the stub takes the
    // value class, `box-impl`s any default-filled value, and `unbox-impl`s before delegating to the base.
    let real_params = jvm_tys(&f.params);
    let boxed: HashMap<usize, Ty> = ir
        .default_stub_boxed_params
        .get(&fid)
        .map(|v| v.iter().copied().collect())
        .unwrap_or_default();
    let stub_param_tys: Vec<Ty> = real_params
        .iter()
        .enumerate()
        .map(|(i, t)| boxed.get(&i).copied().unwrap_or(*t))
        .collect();
    let ret = ir_ty_to_jvm(&f.ret);
    let owner_ty = Ty::obj(owner);

    let mut e = Emitter {
        ir,
        cw,
        bodies,
        run: env.run,
        owner: owner.to_string(),
        facade: facade.to_string(),
        slots: HashMap::new(),
        var_types: collect_var_types(ir),
        next_slot: 0,
        ret,
        loop_stack: Vec::new(),
        pending_stack: Vec::new(),
    };
    // value 0 = self; values 1..=n = the real params; then mask + marker (not value-indexed).
    e.slots.insert(0, (0, owner_ty));
    let mut slot = 1u16;
    let mut param_slots: Vec<(u16, Ty)> = Vec::new();
    for (i, t) in stub_param_tys.iter().enumerate() {
        e.slots.insert((i + 1) as u32, (slot, *t));
        param_slots.push((slot, *t));
        slot += slot_words(*t);
    }
    let mask_slots: Vec<u16> = (0..default_mask_count(real_params.len()))
        .map(|mi| {
            let s = slot;
            e.slots.insert(9_000_001 + mi as u32, (s, Ty::Int)); // register so frames type these slots
            slot += 1;
            s
        })
        .collect();
    e.slots.insert(
        9_000_001 + mask_slots.len() as u32,
        (slot, Ty::obj("java/lang/Object")),
    );
    slot += 1;
    e.next_slot = slot;

    let mut code = CodeBuilder::new(slot);
    emit_default_param_overwrites(
        &mut e,
        &mut code,
        defaults,
        &param_slots,
        &mask_slots,
        &boxed,
    );
    code.aload(0);
    for (i, &(pslot, pty)) in param_slots.iter().enumerate() {
        load(pty, pslot, &mut code);
        // A boxed value-class stub param unboxes to the underlying the base (mangled) method expects.
        if let Some(vc) = boxed.get(&i) {
            emit_unbox_impl(ir, e.cw, vc, &mut code);
        }
    }
    let aw: i32 = real_params.iter().map(|t| slot_words(*t) as i32).sum();
    let desc = method_descriptor(&real_params, ret);
    let is_private = ir.private_methods.contains(&fid);
    if is_interface {
        // The default stub is a STATIC interface method; it dispatches to the real (abstract) member via
        // `invokeinterface` on `$this`.
        let m = e.cw.interface_methodref(owner, &method_name, &desc);
        code.invokeinterface(m, aw, slot_words(ret) as i32);
    } else if is_private {
        // A PRIVATE member is non-virtual — `invokevirtual` on it fails resolution pre-nestmates
        // (class-file major 52); kotlinc dispatches with `invokespecial`.
        let m = e.cw.methodref(owner, &method_name, &desc);
        code.invokespecial(m, aw, slot_words(ret) as i32);
    } else {
        let m = e.cw.methodref(owner, &method_name, &desc);
        code.invokevirtual(m, aw, slot_words(ret) as i32);
    }
    emit_return(ret, &mut code);
    code.ensure_locals(e.next_slot);
    code.link();

    let mut stub_params = vec![owner_ty];
    stub_params.extend(stub_param_tys.iter().copied());
    stub_params.extend(std::iter::repeat_n(
        Ty::Int,
        default_mask_count(real_params.len()),
    ));
    stub_params.push(Ty::obj("java/lang/Object"));
    let desc = method_descriptor(&stub_params, ret);
    e.cw.add_method(
        default_stub_access(ir, fid),
        &format!("{method_name}$default"),
        &desc,
        &code,
    );
}

/// The access flags of a member's `$default` synthetic: kotlinc mirrors the origin's visibility —
/// with PRIVATE demoted to package-private (the stub is invoked from call sites that could not reach the
/// private member itself) — always `| STATIC | SYNTHETIC`. Keyed on the IR's visibility model in ONE
/// place: it currently distinguishes public vs private (`ir.private_methods`); when the IR carries
/// protected/internal, their mappings extend here.
fn default_stub_access(ir: &IrFile, fid: u32) -> u16 {
    let vis = if ir.private_methods.contains(&fid) {
        0x0000 // package-private
    } else {
        0x0001 // ACC_PUBLIC
    };
    vis | 0x1008 // ACC_STATIC | ACC_SYNTHETIC
}

fn emit_default_param_overwrites(
    e: &mut Emitter<'_>,
    code: &mut CodeBuilder,
    defaults: &[Option<u32>],
    param_slots: &[(u16, Ty)],
    mask_slots: &[u16],
    boxed: &HashMap<usize, Ty>,
) {
    for (i, def) in defaults.iter().enumerate().take(param_slots.len()) {
        if let Some(def_expr) = def {
            let (pslot, pty) = param_slots[i];
            code.iload(mask_slots[i / 32]);
            code.push_int(default_mask_bit(i), e.cw);
            code.iand();
            let skip = code.new_label();
            e.frame(skip, vec![], code);
            code.ifeq(skip);
            // The default is computed in the (erased) UNDERLYING form; a slot typed by a nullable-
            // underlying value class boxes it (`box-impl`) so the slot holds the value class.
            e.emit_value(*def_expr, code);
            if let Some(vc) = boxed.get(&i) {
                emit_box_impl(e.ir, e.cw, vc, code);
            }
            store(pty, pslot, code);
            code.bind(skip);
        }
    }
}

fn default_mask_count(param_count: usize) -> usize {
    param_count.div_ceil(32).max(1)
}

fn default_mask_bit(param_index: usize) -> i32 {
    (1u32 << (param_index % 32)) as i32
}

fn full_default_masks(param_count: usize) -> Vec<i32> {
    (0..default_mask_count(param_count))
        .map(|chunk| {
            let start = chunk * 32;
            let end = ((chunk + 1) * 32).min(param_count);
            (start..end).fold(0i32, |mask, i| mask | default_mask_bit(i))
        })
        .collect()
}

/// A value class's (erased) underlying JVM type — its single field's type.
fn vc_underlying_jvm(ir: &IrFile, vc: &Ty) -> Ty {
    vc.obj_internal()
        .and_then(|fq| {
            let fq = fq.render();
            ir.classes.iter().find(|c| c.fq_name_matches(&fq))
        })
        .and_then(|c| c.fields.first())
        .map(|f| ir_ty_to_jvm(&f.ty))
        .unwrap_or(Ty::obj("java/lang/Object"))
}

/// Emit `VC.box-impl(<underlying>)LVC;` (static) — boxes the underlying value on the stack into `VC`.
fn emit_box_impl(ir: &IrFile, cw: &mut ClassWriter, vc: &Ty, code: &mut CodeBuilder) {
    let fq = vc
        .obj_internal()
        .map(|n| n.render())
        .unwrap_or_else(|| "java/lang/Object".to_string());
    let u = vc_underlying_jvm(ir, vc);
    let m = cw.methodref(&fq, "box-impl", &format!("({})L{fq};", type_descriptor(u)));
    code.invokestatic(m, slot_words(u) as i32, 1);
}

/// Emit `VC.unbox-impl()<underlying>` (virtual) — unboxes the `VC` on the stack to its underlying.
fn emit_unbox_impl(ir: &IrFile, cw: &mut ClassWriter, vc: &Ty, code: &mut CodeBuilder) {
    let fq = vc
        .obj_internal()
        .map(|n| n.render())
        .unwrap_or_else(|| "java/lang/Object".to_string());
    let u = vc_underlying_jvm(ir, vc);
    let m = cw.methodref(&fq, "unbox-impl", &format!("(){}", type_descriptor(u)));
    code.invokevirtual(m, 0, slot_words(u) as i32);
}

/// Emit the `foo$default(params…, int mask, Object marker)` synthetic for a TOP-LEVEL facade function
/// (kotlinc's default-argument ABI). Unlike [`emit_default_stub`] (an instance member) there is NO leading
/// `self`: the real parameters occupy value-indices `0..n` (the STATIC layout the defaults were lowered
/// with), and the stub dispatches to the real facade method via `invokestatic`. For each `mask & (1<<i)`
/// bit set, the argument slot is overwritten with `default_i` before the dispatch.
fn emit_facade_default_stub(
    ir: &IrFile,
    fid: u32,
    facade: &str,
    cw: &mut ClassWriter,
    defaults: &[Option<u32>],
    env: &EmitEnv,
    marker: Ty,
) {
    let bodies = env.bodies;
    let f = &ir.functions[fid as usize];
    let method_name = f.name.clone();
    let real_params = jvm_tys(&f.params);
    let ret = ir_ty_to_jvm(&f.ret);

    let mut e = Emitter {
        ir,
        cw,
        bodies,
        run: env.run,
        owner: facade.to_string(),
        facade: facade.to_string(),
        slots: HashMap::new(),
        var_types: collect_var_types(ir),
        next_slot: 0,
        ret,
        loop_stack: Vec::new(),
        pending_stack: Vec::new(),
    };
    // No `self`: value-index `i` = the i-th real parameter (the static layout the defaults were lowered
    // with); then mask + marker (not value-indexed).
    let mut slot = 0u16;
    let mut param_slots: Vec<(u16, Ty)> = Vec::new();
    for (i, t) in real_params.iter().enumerate() {
        e.slots.insert(i as u32, (slot, *t));
        param_slots.push((slot, *t));
        slot += slot_words(*t);
    }
    let mask_slots: Vec<u16> = (0..default_mask_count(real_params.len()))
        .map(|mi| {
            let s = slot;
            e.slots.insert(9_000_001 + mi as u32, (s, Ty::Int)); // register so frames type these slots
            slot += 1;
            s
        })
        .collect();
    e.slots
        .insert(9_000_001 + mask_slots.len() as u32, (slot, marker));
    slot += 1;
    e.next_slot = slot;

    let mut code = CodeBuilder::new(slot);
    emit_default_param_overwrites(
        &mut e,
        &mut code,
        defaults,
        &param_slots,
        &mask_slots,
        &HashMap::new(),
    );
    for &(pslot, pty) in &param_slots {
        load(pty, pslot, &mut code);
    }
    let aw: i32 = real_params.iter().map(|t| slot_words(*t) as i32).sum();
    let desc = method_descriptor(&real_params, ret);
    let m = e.cw.methodref(facade, &method_name, &desc);
    code.invokestatic(m, aw, slot_words(ret) as i32);
    emit_return(ret, &mut code);
    code.ensure_locals(e.next_slot);
    code.link();

    let mut stub_params = real_params.clone();
    stub_params.extend(std::iter::repeat_n(
        Ty::Int,
        default_mask_count(real_params.len()),
    ));
    stub_params.push(marker);
    let desc = method_descriptor(&stub_params, ret);
    e.cw.add_method(
        default_stub_access(ir, fid),
        &format!("{method_name}$default"),
        &desc,
        &code,
    );
}

/// Emit the synthetic `<init>(params…, int mask, DefaultConstructorMarker)` overload for a class whose
/// primary constructor has defaulted parameters. Unlike a `$default` method this is a CONSTRUCTOR: `this`
/// is slot 0, the real parameters follow, then the mask + marker; after overwriting each masked slot with
/// its default it `invokespecial`s the real `<init>`. Access is `PUBLIC | SYNTHETIC` (0x1001), matching
/// kotlinc. The defaults were lowered in the instance frame (`this` = value 0, params = 1..=n).
fn emit_ctor_default_stub(
    ir: &IrFile,
    owner: &str,
    real_params: &[Ty],
    defaults: &[Option<u32>],
    cw: &mut ClassWriter,
    env: &EmitEnv,
) {
    let bodies = env.bodies;
    let n = real_params.len();
    let mut e = Emitter {
        ir,
        cw,
        bodies,
        run: env.run,
        owner: owner.to_string(),
        facade: owner.to_string(),
        slots: HashMap::new(),
        var_types: collect_var_types(ir),
        next_slot: 0,
        ret: Ty::Unit,
        loop_stack: Vec::new(),
        pending_stack: Vec::new(),
    };
    let marker = Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker");
    // `this` at slot 0 = value-index 0; real params at value-index 1..=n.
    e.slots.insert(0, (0, Ty::obj(owner)));
    let mut slot = 1u16;
    let mut param_slots: Vec<(u16, Ty)> = Vec::new();
    for (i, t) in real_params.iter().enumerate() {
        e.slots.insert((i + 1) as u32, (slot, *t));
        param_slots.push((slot, *t));
        slot += slot_words(*t);
    }
    let mask_slots: Vec<u16> = (0..default_mask_count(real_params.len()))
        .map(|mi| {
            let s = slot;
            e.slots.insert(9_000_001 + mi as u32, (s, Ty::Int));
            slot += 1;
            s
        })
        .collect();
    e.slots
        .insert(9_000_001 + mask_slots.len() as u32, (slot, marker));
    slot += 1;
    e.next_slot = slot;

    // The stackmap frame at each mask-branch target: `this` (slot 0) is UNINITIALIZED (the real `<init>`
    // has not run yet), the params keep their types, then the mask ints + marker. Built manually because
    // the frame machinery types slot 0 from `e.slots` as an initialized `Object`, which the verifier rejects.
    let branch_locals: Vec<VerifType> = {
        let mut raw = vec![VerifType::Top; e.next_slot as usize];
        raw[0] = VerifType::UninitializedThis;
        for &(pslot, pty) in &param_slots {
            raw[pslot as usize] = e.verif_single(pty);
        }
        for &mask_slot in &mask_slots {
            raw[mask_slot as usize] = VerifType::Integer;
        }
        raw[slot as usize - 1] = e.verif_single(marker);
        // Collapse the two-slot categories (long/double occupy one verif entry) and trim trailing Top.
        let mut out = Vec::new();
        let mut i = 0;
        while i < raw.len() {
            let wide = matches!(raw[i], VerifType::Long | VerifType::Double);
            out.push(raw[i].clone());
            i += if wide { 2 } else { 1 };
        }
        while out.last() == Some(&VerifType::Top) {
            out.pop();
        }
        out
    };
    let mut code = CodeBuilder::new(slot);
    for (i, def) in defaults.iter().enumerate().take(n) {
        if let Some(def_expr) = def {
            let (pslot, pty) = param_slots[i];
            code.iload(mask_slots[i / 32]);
            code.push_int(default_mask_bit(i), e.cw);
            code.iand();
            let skip = code.new_label();
            code.add_frame_if_new(skip, branch_locals.clone(), vec![]);
            code.ifeq(skip);
            e.emit_value(*def_expr, &mut code);
            store(pty, pslot, &mut code);
            code.bind(skip);
        }
    }
    // `invokespecial <owner>.<init>(realparams)V` — delegate to the real primary constructor.
    code.aload(0);
    for &(pslot, pty) in &param_slots {
        load(pty, pslot, &mut code);
    }
    let init_desc = method_descriptor(real_params, Ty::Unit);
    let aw: i32 = 1 + real_params
        .iter()
        .map(|t| slot_words(*t) as i32)
        .sum::<i32>();
    let m = e.cw.methodref(owner, "<init>", &init_desc);
    code.invokespecial(m, aw, 0);
    code.ret_void();
    code.ensure_locals(e.next_slot);
    code.link();

    let mut stub_params = real_params.to_vec();
    stub_params.extend(std::iter::repeat_n(
        Ty::Int,
        default_mask_count(real_params.len()),
    ));
    stub_params.push(marker);
    let desc = method_descriptor(&stub_params, Ty::Unit);
    e.cw.add_method(0x1001 /* PUBLIC | SYNTHETIC */, "<init>", &desc, &code);
}

/// Emit the PUBLIC|SYNTHETIC accessor `<init>(…args, DefaultConstructorMarker)` for a class whose primary
/// constructor is private (its parameters mention a value class). It delegates straight to the private
/// `<init>` — `this` at slot 0, the real params, then the marker (unused); `invokespecial` the primary,
/// return. Straight-line (no branches ⇒ no StackMapTable). Distinct from the default-arg overload, which
/// carries the extra `int mask` and fills defaults.
fn emit_ctor_marker_accessor(owner: &str, real_params: &[Ty], cw: &mut ClassWriter) {
    let mut slot = 1u16; // slot 0 = `this`
    let mut param_slots: Vec<(u16, Ty)> = Vec::new();
    for t in real_params {
        param_slots.push((slot, *t));
        slot += slot_words(*t);
    }
    let total = slot + 1; // + the marker local
                          // The accessor's OWN descriptor interns before its body's Methodref — kotlinc visits a method's
                          // signature before its code, so the private ctor this delegates to must not claim the earlier slot.
    let mut stub_params = real_params.to_vec();
    stub_params.push(Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker"));
    let desc = method_descriptor(&stub_params, Ty::Unit);
    cw.reserve_descriptor(&desc);
    let mut code = CodeBuilder::new(total);
    code.aload(0);
    for &(pslot, pty) in &param_slots {
        load(pty, pslot, &mut code);
    }
    let init_desc = method_descriptor(real_params, Ty::Unit);
    let aw: i32 = 1 + real_params
        .iter()
        .map(|t| slot_words(*t) as i32)
        .sum::<i32>();
    let m = cw.methodref(owner, "<init>", &init_desc);
    code.invokespecial(m, aw, 0);
    code.ret_void();
    code.ensure_locals(total);
    code.link();

    cw.add_method(0x1001 /* PUBLIC | SYNTHETIC */, "<init>", &desc, &code);
}

struct Emitter<'a> {
    ir: &'a IrFile,
    cw: &'a mut ClassWriter,
    /// The narrow bytecode provider — lets the emitter read a cross-module `inline fun`'s compiled
    /// body (`bodies.body`) to splice it at the call site (the bytecode inliner).
    bodies: &'a dyn MethodBodies,
    /// The per-emit-run accumulators — the deep sites record a used lambda / an emit-or-inline bail
    /// here (formerly thread-locals).
    run: &'a EmitRun,
    owner: String,
    facade: String,
    slots: HashMap<u32, (u16, Ty)>,
    /// Every `Variable` index → its JVM type (file-wide); a `value_ty(GetValue)` fallback for a slot not
    /// yet registered in `slots` (queried before its declaration emits — e.g. an inline result temp).
    var_types: HashMap<u32, Ty>,
    next_slot: u16,
    ret: Ty,
    /// Stack of enclosing loops' `(continue_label, break_label)` — `break`/`continue` target the top.
    /// Stack of enclosing loops: `(continue_label, break_label, source_label)`. A labeled
    /// `break@l`/`continue@l` targets the entry whose `source_label == Some(l)`; an unlabeled one
    /// targets the innermost (top).
    loop_stack: Vec<(Label, Label, Option<String>)>,
    /// Operand-stack verification types sitting BELOW the expression currently being emitted (an
    /// arithmetic LHS held on the stack across a branchy RHS, e.g. a data-class `hashCode` accumulator
    /// `result*31 + <branchy nullable-field hash>`). Prepended to every recorded stack-map frame's stack
    /// so the pending operand is typed through the branch (matching kotlinc), avoiding the spill-to-temp
    /// krusty would otherwise need. Pushed/popped around the branchy RHS in `emit_binop`.
    pending_stack: Vec<VerifType>,
}

/// Parse a method descriptor's parameter types (in order) to `Ty`s.
fn parse_descriptor_params(desc: &str) -> Option<Vec<Ty>> {
    let inner = desc.strip_prefix('(')?.split(')').next()?;
    let b = inner.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        while b.get(i) == Some(&b'[') {
            i += 1;
        }
        match b.get(i)? {
            b'L' => {
                while b.get(i) != Some(&b';') {
                    i += 1;
                }
                i += 1;
            }
            _ => i += 1,
        }
        out.push(crate::jvm::jvm_libraries::desc_to_ty(&inner[start..i]));
    }
    Some(out)
}

impl<'a> Emitter<'a> {
    /// Emit a lambda's `inline_body` (its value-producing form) INLINE at a stdlib-inline-fn splice:
    /// bind its parameter value-indices `0..` to the given JVM slots (captures → caller slots, lambda
    /// params → the on-stack args), then emit the body as a value — leaving the result on the stack. A
    /// user `return` inside the body emits a real `*return` from the enclosing method, i.e. a correct
    /// non-local return (no synthetic-return rewriting needed).
    fn emit_fn_body_inline(
        &mut self,
        inline_body: u32,
        param_slots: &[(u16, Ty)],
        code: &mut CodeBuilder,
    ) {
        let saved_slots = std::mem::take(&mut self.slots);
        for (i, &(slot, ty)) in param_slots.iter().enumerate() {
            self.slots.insert(i as u32, (slot, ty));
        }
        self.emit_value(inline_body, code);
        self.slots = saved_slots;
    }

    /// THE unified host+lambda splice (the merge of the branchy and lambda paths): splice a possibly
    /// BRANCHY host `inline fun` body, replacing each zero-arg lambda-parameter `Function0.invoke` site
    /// with that lambda's body. Handles `require(cond) { msg }` / `check(cond) { msg }` and the like —
    /// where the lambda runs only on a branch. v1: zero-arg (Function0) lambdas with branchless bodies,
    /// at an empty operand-stack baseline. Returns `false` (caller falls back / skips) on any other shape.
    fn try_inline_unified(
        &mut self,
        descriptor: &str,
        args: &[u32],
        body: &crate::jvm::classreader::MethodCode,
        base: u16,
        code: &mut CodeBuilder,
    ) -> bool {
        let Some(params) = parse_descriptor_params(descriptor) else {
            return false;
        };
        if params.len() != args.len() {
            return false;
        }
        let top_local = base + body.max_locals;
        self.next_slot = self.next_slot.max(top_local);
        // Build each lambda argument's pre-relocated body (leaving its boxed result on the stack), and
        // its own (branchy-predicate) frames — resolved to byte offsets within the body, relocated below.
        let mut lam_splices: Vec<crate::jvm::inline::LambdaSplice> = Vec::new();
        let mut lam_frames: Vec<ResolvedFrames> = Vec::new();
        // The deepest operand stack any spliced lambda body reaches — the host's `max_stack` must cover it,
        // since the body is inlined into the host (a deep lambda body, e.g. `123 != intArrayOf() as Any`,
        // would otherwise overflow the host's stack). Propagated to `splice_inline` below.
        let mut lam_max_stack = 0u16;
        for (i, &a) in args.iter().enumerate() {
            let mut scratch = CodeBuilder::new(self.next_slot);
            let (lam_insns, lam_fr) = if let IrExpr::Lambda {
                impl_fn,
                arity,
                captures,
                inline_body,
                ..
            } = self.ir.expr(a).clone()
            {
                let Some(inline_body) = inline_body else {
                    return false;
                };
                let arity = arity as usize;
                let impl_f = &self.ir.functions[impl_fn as usize];
                // The impl method's parameters are `[captures…, lambda_params…]`.
                let Some(n_cap) = impl_f.params.len().checked_sub(arity) else {
                    return false;
                };
                if n_cap != captures.len() {
                    return false;
                }
                let cap_tys = jvm_tys(&impl_f.params[..n_cap]);
                let lam_tys = jvm_tys(&impl_f.params[n_cap..]);
                let impl_ret = ir_ty_to_jvm(&impl_f.ret);
                // Each capture binds to the caller's actual slot (a mutable capture writes through).
                let mut cap_slots: Vec<(u16, Ty)> = Vec::with_capacity(captures.len());
                for (k, &cap) in captures.iter().enumerate() {
                    let IrExpr::GetValue(v) = self.ir.expr(cap) else {
                        return false;
                    };
                    let Some(&(slot, _)) = self.slots.get(v) else {
                        return false;
                    };
                    cap_slots.push((slot, cap_tys[k]));
                }
                // Build the lambda body into a scratch builder. The host left the lambda's `arity`
                // arguments on the stack (as `Object`, the erased `FunctionN.invoke` parameters);
                // unbox a primitive parameter, or `checkcast` a specific reference parameter to its
                // type, then store it (top = last). Then run the body, then box the result to `Object`
                // (matching the replaced `invoke`'s `Object` result).
                scratch.set_stack(arity as u16);
                let mut param_slots: Vec<(u16, Ty)> = cap_slots;
                param_slots.extend(std::iter::repeat_n((0u16, Ty::Error), arity));
                for j in (0..arity).rev() {
                    let jt = lam_tys[j];
                    if jt.is_jvm_scalar() {
                        unbox_prim(self.cw, &mut scratch, jt);
                    } else if let Some(internal) = checkcast_internal(jt) {
                        let ci = self.cw.class_ref(&internal);
                        scratch.checkcast(ci);
                    }
                    let slot = self.next_slot;
                    self.next_slot += slot_words(jt);
                    store(jt, slot, &mut scratch);
                    param_slots[n_cap + j] = (slot, jt);
                }
                self.emit_fn_body_inline(inline_body, &param_slots, &mut scratch);
                if impl_ret.is_jvm_scalar() {
                    box_prim_free(self.cw, &mut scratch, impl_ret);
                }
                scratch.link(); // patch the lambda body's own branch operands before reading its bytes
                let lam_fr = scratch.resolved_frames(); // branchy predicate body → its own frames
                let Some(lam_insns) = crate::jvm::inline::disassemble(&scratch.bytes) else {
                    return false;
                };
                (lam_insns, lam_fr)
            } else if let Some((class, captures)) = self.function_ref_class_and_captures(a) {
                let Some((lam_insns, lam_fr)) =
                    self.emit_function_ref_inline_body(class, &captures, &mut scratch)
                else {
                    return false;
                };
                (lam_insns, lam_fr)
            } else if let Some((class, captures)) = self.property_ref_class_and_captures(a) {
                let Some((lam_insns, lam_fr)) =
                    self.emit_property_ref_inline_body(class, &captures, &mut scratch)
                else {
                    return false;
                };
                (lam_insns, lam_fr)
            } else {
                continue;
            };
            if code.max_locals < scratch.max_locals {
                code.max_locals = scratch.max_locals;
            }
            self.next_slot = self.next_slot.max(scratch.max_locals);
            lam_max_stack = lam_max_stack.max(scratch.max_stack);
            lam_frames.push(lam_fr);
            lam_splices.push(crate::jvm::inline::LambdaSplice {
                param_index: i,
                body: lam_insns,
            });
        }
        if lam_splices.is_empty() {
            return false; // no lambda argument — not this path
        }
        // Probe at offset 0 to learn whether frames are needed (HOST branchy OR any lambda BODY branchy).
        let Some(probe) = crate::jvm::inline::splice_unified(
            body,
            descriptor,
            base,
            &lam_splices,
            0,
            self.cw,
            &HashMap::new(),
        ) else {
            return false;
        };
        // The splice records frames if it has a join, any lambda body has frames, OR the HOST body itself
        // records frames (a loop HOF's loop frames). All of these are bound relative to an empty operand
        // baseline (no caller operand prefix is threaded into them), so a non-empty baseline must bail —
        // `records_frame` makes a parent operand sequence spill earlier operands so we reach here at 0.
        let needs_frames = probe.join_required
            || !probe.frames.is_empty()
            || lam_frames.iter().any(|f| !f.is_empty());
        if needs_frames && code.stack_height() != 0 {
            crate::trace_compiler!(
                "splice",
                "unified BAIL: needs_frames but stack_height={}",
                code.stack_height()
            );
            return false; // frames carry no stack prefix → need an empty baseline
        }
        let ret_words = descriptor_ret_words(descriptor);
        // Emit each NON-lambda argument (the operands the host prologue stores into its parameter slots).
        let mut arg_words = 0i32;
        for (i, &a) in args.iter().enumerate() {
            if matches!(self.ir.expr(a), IrExpr::Lambda { .. })
                || self.function_ref_class_and_captures(a).is_some()
                || self.property_ref_class_and_captures(a).is_some()
            {
                continue;
            }
            self.emit_value(a, code);
            let at = self.value_ty(a);
            if params[i].is_reference() && at.is_jvm_scalar() {
                box_prim_free(self.cw, code, at);
            }
            arg_words += slot_words(params[i]) as i32;
        }
        if !needs_frames {
            // Pure branchless host + lambda: append the bytes, no frames; works at any stack height.
            // The host's stack must cover the host body PLUS the deepest spliced lambda body (a safe upper
            // bound on the real peak) — else a deep lambda body overflows the host's operand stack.
            code.splice_inline(
                &probe.bytes,
                body.max_stack + lam_max_stack,
                top_local,
                arg_words,
                ret_words,
            );
            return true;
        }
        // RE-splice at the real method offset (so any switch in the host/lambda body pads correctly), then
        // bind the relocated HOST frames, the LAMBDA bodies' own frames, the spliced bytes, and the join.
        let splice_start = code.bytes.len();
        let Some(bs) = crate::jvm::inline::splice_unified(
            body,
            descriptor,
            base,
            &lam_splices,
            splice_start,
            self.cw,
            &HashMap::new(),
        ) else {
            return false;
        };
        let prefix = self.verif_locals_upto(base);
        for (abs_off, body_locals, stack) in &bs.frames {
            let mut locals = prefix.clone();
            locals.extend(body_locals.iter().map(vtype_to_verif));
            let st: Vec<VerifType> = stack.iter().map(vtype_to_verif).collect();
            let l = code.new_label();
            code.bind_at(l, *abs_off);
            code.add_frame_if_new(l, locals, st);
        }
        for (k, frames) in lam_frames.iter().enumerate() {
            let host_ctx = bs.lambda_host_locals.get(k).cloned().unwrap_or_default();
            // The lambda body's frames were compiled against an EMPTY operand base; rebase each onto the
            // host operand-stack prefix sitting below the lambda value (e.g. a `map` destination). Empty
            // for `forEach`/`fold`/`takeIf`; `splice_unified` only returns `Some` here for a branchy body.
            let op_prefix: Vec<VerifType> = bs
                .lambda_stack_prefix
                .get(k)
                .and_then(|p| p.as_ref())
                .map(|p| p.iter().map(vtype_to_verif).collect())
                .unwrap_or_default();
            for (fb, locals, stack) in frames {
                let off = bs.lambda_byte_starts[k] + fb;
                let merged = self.merge_lambda_frame_locals(base, top_local, &host_ctx, locals);
                let mut st = op_prefix.clone();
                st.extend(stack.iter().cloned());
                let l = code.new_label();
                code.bind_at(l, off);
                code.add_frame_if_new(l, merged, st);
            }
        }
        // Register the spliced body's relocated exception handlers (try/catch/finally from `use`/
        // `synchronized`/`runCatching`). The handler frames are already bound above (each handler is a
        // StackMapTable target in `bs.frames`); here we add the guarded-range entries to the caller's
        // exception table via labels bound at the absolute spliced offsets.
        for &(start, end, handler, catch_type) in &bs.handlers {
            let (ls, le, lh) = (code.new_label(), code.new_label(), code.new_label());
            code.bind_at(ls, start);
            code.bind_at(le, end);
            code.bind_at(lh, handler);
            code.add_exception(ls, le, lh, catch_type);
        }
        code.set_needs_stackmap();
        // Host stack must cover the host body PLUS the deepest spliced lambda body (safe upper bound).
        code.splice_inline(
            &bs.bytes,
            body.max_stack + lam_max_stack,
            top_local,
            arg_words,
            ret_words,
        );
        if bs.join_required {
            let join = code.new_label();
            code.bind(join);
            let join_stack: Vec<VerifType> = bs.join_stack.iter().map(vtype_to_verif).collect();
            code.add_frame_if_new(join, prefix, join_stack);
        }
        true
    }

    /// Full locals for a frame INSIDE a spliced lambda body: the caller's locals (`0..base`), then the
    /// HOST's live body locals at the invoke (`host_ctx`, slots `base..` — for a loop host the loop
    /// iterator/accumulator, not just params), then the lambda's own slots (`top_local..`) from its
    /// scratch frame. All three are slot-expanded, overlaid, and re-collapsed.
    fn merge_lambda_frame_locals(
        &mut self,
        base: u16,
        top_local: u16,
        host_ctx: &[crate::jvm::inline::VType],
        lam_locals: &[VerifType],
    ) -> Vec<VerifType> {
        let mut slots = self.verif_slots_upto(base); // 0..base caller locals (slot-indexed)
                                                     // The host's live locals at `base..` (slot-indexed), then pad to `top_local` with `Top`.
        let host_collapsed: Vec<VerifType> = host_ctx.iter().map(vtype_to_verif).collect();
        slots.extend(expand_collapsed_locals(&host_collapsed));
        slots.truncate(top_local as usize);
        while slots.len() < top_local as usize {
            slots.push(VerifType::Top);
        }
        // The lambda's own slots (`top_local..`): expand the scratch frame, take from `top_local`.
        for s in expand_collapsed_locals(lam_locals)
            .into_iter()
            .skip(top_local as usize)
        {
            slots.push(s);
        }
        collapse_locals(&slots)
    }

    /// Slot-indexed caller locals for `0..upto` (long/double take two slots; `Top` fills the gaps).
    fn verif_slots_upto(&mut self, upto: u16) -> Vec<VerifType> {
        let mut raw = vec![VerifType::Top; upto as usize];
        let entries: Vec<(u16, Ty)> = self.slots.values().copied().collect();
        for (slot, ty) in entries {
            if (slot as usize) < raw.len() {
                raw[slot as usize] = self.verif_single(ty);
            }
        }
        raw
    }

    fn function_ref_class_and_captures(&self, expr: u32) -> Option<(crate::ir::ClassId, Vec<u32>)> {
        match self.ir.expr(expr) {
            IrExpr::New { class, args, .. }
                if self.ir.classes[*class as usize].func_ref.is_some() =>
            {
                Some((*class, args.clone()))
            }
            IrExpr::StaticInstance { ty, .. }
                if self.ir.classes[*ty as usize].func_ref.is_some() =>
            {
                Some((*ty, Vec::new()))
            }
            _ => None,
        }
    }

    fn property_ref_class_and_captures(&self, expr: u32) -> Option<(crate::ir::ClassId, Vec<u32>)> {
        match self.ir.expr(expr) {
            IrExpr::New { class, args, .. }
                if self.ir.classes[*class as usize].prop_ref.is_some() =>
            {
                Some((*class, args.clone()))
            }
            IrExpr::StaticInstance { ty, .. }
                if self.ir.classes[*ty as usize].prop_ref.is_some() =>
            {
                Some((*ty, Vec::new()))
            }
            _ => None,
        }
    }

    fn emit_function_ref_inline_body(
        &mut self,
        class: crate::ir::ClassId,
        captures: &[u32],
        scratch: &mut CodeBuilder,
    ) -> Option<(Vec<crate::jvm::inline::Insn>, ResolvedFrames)> {
        let fr = self.ir.classes[class as usize].func_ref.clone()?;
        let call_owner = fr.call_owner_or_facade(&self.facade);
        let first_call_arg = match fr.dispatch {
            crate::ir::FrDispatch::VirtualUnbound => 1usize,
            _ => 0usize,
        };
        // `StaticBound`: invoke arg `k` maps to `target_param_tys[k + 1]` (slot 0 is the receiver).
        let target_offset = match fr.dispatch {
            crate::ir::FrDispatch::StaticBound => 1usize,
            _ => 0usize,
        };
        if fr.param_tys.len() != fr.arity as usize {
            return None;
        }
        let param_tys = jvm_tys(&fr.param_tys);
        let target_param_tys = jvm_tys(&fr.target_param_tys);
        let mut param_slots = vec![(0u16, Ty::Error); fr.arity as usize];
        scratch.set_stack(fr.arity as u16);
        for j in (0..fr.arity as usize).rev() {
            let jt = param_tys[j];
            if jt.is_jvm_scalar() {
                unbox_prim(self.cw, scratch, jt);
            } else if let Some(internal) = checkcast_internal(jt) {
                let ci = self.cw.class_ref(&internal);
                scratch.checkcast(ci);
            }
            let slot = self.next_slot;
            self.next_slot += slot_words(jt);
            store(jt, slot, scratch);
            param_slots[j] = (slot, jt);
        }

        match fr.dispatch {
            crate::ir::FrDispatch::Static => {}
            crate::ir::FrDispatch::VirtualBound => {
                let [capture] = captures else { return None };
                self.emit_value(*capture, scratch);
                let owner_ref = self.cw.class_ref(&call_owner);
                scratch.checkcast(owner_ref);
            }
            crate::ir::FrDispatch::VirtualUnbound => {
                let (slot, jt) = *param_slots.first()?;
                load(jt, slot, scratch);
            }
            crate::ir::FrDispatch::StaticBound => {
                // The captured receiver is the first static argument: push it, cast to the receiver type.
                let [capture] = captures else { return None };
                self.emit_value(*capture, scratch);
                if let Some(internal) = target_param_tys
                    .first()
                    .copied()
                    .and_then(checkcast_internal)
                {
                    let cref = self.cw.class_ref(&internal);
                    scratch.checkcast(cref);
                }
            }
        }

        let mut call_desc = String::from("(");
        // `StaticBound` leads the target descriptor with the (already pushed) receiver.
        let mut call_arg_words = if target_offset == 1 {
            let recv_jt = target_param_tys.first().copied().unwrap_or(Ty::Error);
            call_desc.push_str(&type_descriptor(recv_jt));
            slot_words(recv_jt) as i32
        } else {
            0i32
        };
        for (k, (slot, jt)) in param_slots.iter().enumerate().skip(first_call_arg) {
            let target_jt = target_param_tys
                .get(k + target_offset)
                .copied()
                .unwrap_or(*jt);
            load(*jt, *slot, scratch);
            if let Some(vc) = fr.unbox_params.get(k).and_then(|v| v.as_ref()) {
                let locals = self.verif_locals_with(&param_slots);
                let stack_prefix = func_ref_call_stack_prefix(self.cw, &fr.dispatch, &call_owner);
                emit_value_class_unbox_adapter(
                    self.cw,
                    scratch,
                    *vc,
                    target_jt,
                    fr.unbox_param_nullable.get(k).copied().unwrap_or(false),
                    Some(locals),
                    stack_prefix,
                );
            }
            call_desc.push_str(&type_descriptor(target_jt));
            call_arg_words += slot_words(target_jt) as i32;
        }
        call_desc.push(')');
        let ret_jvm = ir_ty_to_jvm(&fr.target_ret_ty);
        let returns_void = matches!(fr.ret_ty, Ty::Unit | Ty::Nothing);
        if returns_void {
            call_desc.push('V');
        } else {
            call_desc.push_str(&type_descriptor(ret_jvm));
        }
        let ret_words = if returns_void {
            0
        } else {
            slot_words(ret_jvm) as i32
        };
        match fr.dispatch {
            crate::ir::FrDispatch::Static | crate::ir::FrDispatch::StaticBound => {
                let m = self.cw.methodref(&call_owner, &fr.call_name, &call_desc);
                scratch.invokestatic(m, call_arg_words, ret_words);
            }
            // A bound mapped-builtin member ref invokes the same physical JVM method a direct call would
            // (`String.get` → `charAt`) — apply the backend's name mapping (see the free-function twin).
            _ if fr.call_interface => {
                let vn = mapped_builtin_virtual_name(&call_owner, &fr.call_name);
                let m = self.cw.interface_methodref(&call_owner, vn, &call_desc);
                scratch.invokeinterface(m, call_arg_words, ret_words);
            }
            _ => {
                let vn = mapped_builtin_virtual_name(&call_owner, &fr.call_name);
                let m = self.cw.methodref(&call_owner, vn, &call_desc);
                scratch.invokevirtual(m, call_arg_words, ret_words);
            }
        }
        if returns_void {
            let unit = self.cw.fieldref("kotlin/Unit", "INSTANCE", "Lkotlin/Unit;");
            scratch.getstatic(unit, 1);
        } else if let Some(owner) = &fr.box_ret {
            let owner = owner.render();
            // A value-class-returning reference: box the erased underlying back to the boxed VC (`X` object)
            // — a VC in a `FunctionN` slot is boxed. See the sibling adapter above.
            let bi = self.cw.methodref(
                &owner,
                "box-impl",
                &format!(
                    "({}){}",
                    type_descriptor(ret_jvm),
                    type_descriptor(Ty::obj(&owner))
                ),
            );
            scratch.invokestatic(bi, slot_words(ret_jvm) as i32, 1);
        } else if ret_jvm.is_jvm_scalar() {
            box_prim_free(self.cw, scratch, ret_jvm);
        }
        scratch.link();
        let frames = scratch.resolved_frames();
        let insns = crate::jvm::inline::disassemble(&scratch.bytes)?;
        Some((insns, frames))
    }

    fn emit_property_ref_inline_body(
        &mut self,
        class: crate::ir::ClassId,
        captures: &[u32],
        scratch: &mut CodeBuilder,
    ) -> Option<(Vec<crate::jvm::inline::Insn>, ResolvedFrames)> {
        let pr = self.ir.classes[class as usize].prop_ref.clone()?;
        let owner = pr.owner_or_facade(&self.facade);
        let prop_jvm = ir_ty_to_jvm(&pr.prop_ty);
        let getter_desc = format!("(){}", type_descriptor(prop_jvm));
        let arity = if pr.bound || pr.static_dispatch { 0 } else { 1 };
        scratch.set_stack(arity);
        if pr.static_dispatch {
            let gref = self.cw.methodref(&owner, &pr.getter_name, &getter_desc);
            scratch.invokestatic(gref, 0, slot_words(prop_jvm) as i32);
        } else {
            if pr.bound {
                let [capture] = captures else { return None };
                self.emit_value(*capture, scratch);
            } else {
                let owner_ref = self.cw.class_ref(&owner);
                scratch.checkcast(owner_ref);
            }
            let gref = self.cw.methodref(&owner, &pr.getter_name, &getter_desc);
            scratch.invokevirtual(gref, 0, slot_words(prop_jvm) as i32);
        }
        if prop_jvm.is_jvm_scalar() {
            box_prim_free(self.cw, scratch, prop_jvm);
        }
        scratch.link();
        let frames = scratch.resolved_frames();
        let insns = crate::jvm::inline::disassemble(&scratch.bytes)?;
        Some((insns, frames))
    }

    /// Attempt to splice a cross-module `inline fun`'s compiled body at the call site (the bytecode
    /// inliner; the callee body comes from [`MethodBodies::body`]). Returns `true` if spliced; `false`
    /// means the caller must report an inline backend gap rather than silently treating this as an
    /// ordinary call-resolution fallback.
    /// The reified type substitution (type-parameter name → JVM internal name) for the value expression
    /// `e` being emitted, from [`IrFile::reified_call_subst`]. Empty for a call that isn't a
    /// `<reified T>` classpath-extension splice — the common case. Fed to `splice_unified` so a
    /// `reifiedOperationMarker`/`T::class` in the spliced body specializes to the concrete type.
    fn reified_type_map(&self, e: u32) -> HashMap<String, String> {
        self.ir
            .reified_call_subst
            .get(&e)
            .map(|subst| {
                subst
                    .iter()
                    .filter_map(|(name, ty)| {
                        // `kotlin_class_internal` (not `obj_internal`): a reified type arg inferred from a
                        // receiver arrives as a bare `Ty::Int`/`Ty::String` variant whose `obj_internal()`
                        // is `None` — the boxed reified array element is `java/lang/Integer` etc.
                        let internal = ty.kotlin_class_internal()?.render();
                        let internal = crate::jvm::jvm_class_map::to_jvm_internal(&internal);
                        Some((name.clone(), internal.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn try_inline_static(
        &mut self,
        owner: &str,
        name: &str,
        descriptor: &str,
        args: &[u32],
        code: &mut CodeBuilder,
    ) -> bool {
        let target = InlineStaticTarget {
            owner,
            name,
            descriptor,
            splice_desc: descriptor,
        };
        self.try_inline_static_as(target, args, code, true, &HashMap::new())
    }

    /// Splice `owner.name` whose REAL (body-fetch) descriptor is `descriptor`, mapping the body's locals
    /// per `splice_desc`. For an ordinary static they are equal; for an INSTANCE inline method spliced
    /// through this path, `splice_desc` PREPENDS the receiver as the first parameter (`this` = local 0)
    /// and `args[0]` is that receiver — so the body's `aload_0`/`aload_1`/… map to receiver/params.
    fn try_inline_static_as(
        &mut self,
        target: InlineStaticTarget<'_>,
        args: &[u32],
        code: &mut CodeBuilder,
        allow_owner_bridge: bool,
        reified: &HashMap<String, String>,
    ) -> bool {
        let InlineStaticTarget {
            owner,
            name,
            descriptor,
            splice_desc,
        } = target;
        let Some(body) = self.bodies.body(owner, name, descriptor) else {
            crate::trace_compiler!("splice", "no body for {owner}.{name}{descriptor}");
            return false;
        };
        if !allow_owner_bridge && owner != methodref_owner(&body, name, descriptor).unwrap_or(owner)
        {
            crate::trace_compiler!(
                "splice",
                "owner-bridge mismatch for {owner}.{name}{descriptor} (real owner {:?})",
                methodref_owner(&body, name, descriptor)
            );
            return false;
        }
        // Splice the body's locals above BOTH the slot allocator's next free slot and the code's
        // high-water mark, so the spliced temporaries can never collide with a caller local (live or
        // reserved-but-unstored).
        let base = self.next_slot.max(code.max_locals);
        // Route (b): a literal lambda argument → splice its body at the host's `FunctionN.invoke` site
        // (the unified host+lambda splice handles both the branchy `require(c){m}` and the branchless
        // `let`/`also`/… shapes).
        let has_lambda_arg = args.iter().any(|&a| {
            matches!(self.ir.expr(a), IrExpr::Lambda { .. })
                || self.function_ref_class_and_captures(a).is_some()
                || self.property_ref_class_and_captures(a).is_some()
        });
        if has_lambda_arg {
            // If the body INVOKES the lambda parameter (`FunctionN.invoke`), splice the lambda body at
            // those sites. If the lambda is used only as a VALUE — passed to a call/constructor, as in the
            // `Continuation(ctx){…}` fake-constructor's `new …$Continuation$1(ctx, resumeWith)` — there is
            // no invoke site to splice into, so fall through to MATERIALIZE the lambda as a `Function1`
            // object (`emit_operands`) and splice the body verbatim (the param slot binds to that object).
            let body_invokes_lambda =
                crate::jvm::inline::disassemble(&body.code).is_some_and(|insns| {
                    !crate::jvm::inline::function_invoke_sites(&insns, &body.source_cp).is_empty()
                });
            if body_invokes_lambda {
                return self.try_inline_unified(descriptor, args, &body, base, code);
            }
        } else if descriptor.contains("Lkotlin/jvm/functions/Function") {
            // A function-typed parameter whose argument isn't a literal lambda (a passed `Function`) is a
            // current inline-splice gap — can't materialize an unknown `Function` value here.
            return false;
        }
        let ret_words = descriptor_ret_words(descriptor);
        let top_local = base + body.max_locals;
        // ONE splicer for every no-lambda body (`splice_unified` subsumes the old branchless + branchy
        // paths). Probe at offset 0 to learn `join_required` (a branchless body has no switch, so its
        // layout is position-independent); a branchy body is then RE-spliced at its real method offset so
        // any `tableswitch`/`lookupswitch` pads correctly.
        let Some(probe) =
            crate::jvm::inline::splice_unified(&body, splice_desc, base, &[], 0, self.cw, reified)
        else {
            crate::trace_compiler!(
                "splice",
                "splice_unified probe failed for {owner}.{name}{descriptor} (splice_desc={splice_desc})"
            );
            return false;
        };
        let arg_words: i32 = args
            .iter()
            .map(|&a| slot_words(self.value_ty(a)) as i32)
            .sum();
        if !probe.join_required {
            // Branchless: append the bytes, no frames. A DIVERGING body (ends in `athrow`, e.g.
            // `error(msg)`) leaves NOTHING on the stack — its post-splice height is the baseline.
            self.emit_operands(args, code);
            let diverges = probe.bytes.last() == Some(&0xbf);
            let ret_words = if diverges { 0 } else { ret_words };
            code.splice_inline(
                &probe.bytes,
                body.max_stack,
                top_local,
                arg_words,
                ret_words,
            );
            return true;
        }
        // Branchy body: needs an empty operand-stack baseline (the relocated frames carry no stack
        // prefix); a sub-expression inline call (non-empty stack) falls back to a normal call.
        if code.stack_height() != 0 {
            return false;
        }
        self.emit_operands(args, code);
        let splice_start = code.bytes.len();
        let Some(bs) = crate::jvm::inline::splice_unified(
            &body,
            splice_desc,
            base,
            &[],
            splice_start,
            self.cw,
            reified,
        ) else {
            return false;
        };
        let prefix = self.verif_locals_upto(base);
        for (abs_off, body_locals, stack) in &bs.frames {
            let mut locals = prefix.clone();
            locals.extend(body_locals.iter().map(vtype_to_verif));
            let st: Vec<VerifType> = stack.iter().map(vtype_to_verif).collect();
            let l = code.new_label();
            code.bind_at(l, *abs_off);
            code.add_frame_if_new(l, locals, st);
        }
        code.set_needs_stackmap();
        code.splice_inline(&bs.bytes, body.max_stack, top_local, arg_words, ret_words);
        // Join frame: the redirected returns land at the continuation right after the spliced body.
        let join = code.new_label();
        code.bind(join);
        let join_stack: Vec<VerifType> = bs.join_stack.iter().map(vtype_to_verif).collect();
        code.add_frame_if_new(join, prefix, join_stack);
        true
    }

    /// Caller-local verification types for slots `0..upto` (collapsing `long`/`double` to one entry),
    /// NOT trimming trailing `Top` — the prefix a spliced branchy body's frames are concatenated onto
    /// (the body's own locals occupy slots `upto..`).
    fn verif_locals_upto(&mut self, upto: u16) -> Vec<VerifType> {
        let mut raw = vec![VerifType::Top; upto as usize];
        let entries: Vec<(u16, Ty)> = self.slots.values().copied().collect();
        for (slot, ty) in entries {
            if (slot as usize) < raw.len() {
                raw[slot as usize] = self.verif_single(ty);
            }
        }
        let mut out = Vec::new();
        let mut i = 0;
        while i < raw.len() {
            let wide = matches!(raw[i], VerifType::Long | VerifType::Double);
            out.push(raw[i].clone());
            i += if wide { 2 } else { 1 };
        }
        out
    }

    fn emit(&mut self, e: u32, code: &mut CodeBuilder) {
        match self.ir.expr(e).clone() {
            IrExpr::Block { stmts, value } => {
                // Scope block-locals: restore the slot *map* after the block (keeping next_slot
                // monotonic) so a local declared here doesn't leak into a later merge-point frame
                // (its slot must read as `Top` once out of scope — else a sibling branch that never
                // initialized it fails verification).
                let saved = self.slots.clone();
                let mut dead = false;
                for s in stmts {
                    // See the value-context `Block` arm: a statement nets zero, so reset the tracked
                    // height afterward to undo an approximate branchy-splice drift.
                    let base = code.stack_height();
                    self.emit(s, code);
                    if self.diverges(s) {
                        dead = true;
                        break;
                    } // rest of the block is unreachable
                    code.set_stack(base.max(0) as u16);
                }
                if !dead {
                    if let Some(v) = value {
                        self.emit_discarding(v, code);
                    }
                }
                self.slots = saved;
            }
            IrExpr::Return(v) => match v {
                Some(v) => {
                    let ret = self.ret;
                    self.emit_value_as(v, &ret, code);
                    // `return <diverging>` (`return throw e`, `return error(..)`): the value already
                    // transferred control (athrow / a `Nothing`-returning call), so the trailing return
                    // opcode is unreachable dead code the verifier rejects (no stack-map frame). Skip it.
                    if !self.diverges(v) {
                        emit_return(self.ret, code);
                    }
                }
                None => code.ret_void(),
            },
            IrExpr::Variable {
                index, ty, init, ..
            } => {
                // Emit the initializer BEFORE allocating the slot, so the variable's slot isn't
                // claimed in StackMapTable frames recorded inside a branchy initializer (where the
                // verifier still sees it as `top`).
                let jt = ir_ty_to_jvm(&ty);
                // Reuse the slot if this value-index is already live with a compatible verification
                // type. A spilled local is declared twice — once by the dispatch loop-top restore,
                // once by its real in-body declaration in a resume state — for the SAME value-index.
                // They must share a slot: then the loop-top restore's assignment covers the fresh path
                // too, so the slot reads as definitely-assigned in later frames. A fresh slot per
                // declaration instead leaves the in-body slot `top` on the fresh edge to a `?: continue`
                // target — a StackMapTable VerifyError (ResAgg getAllResources/getResourceById). Reuse
                // only when the verification types agree: identical, or both reference types (the
                // restore reads an `Object` continuation field and the in-body decl may be a narrower
                // reference — the wider header type still verifies every subtype back-edge). Never
                // reuse across differing primitives (e.g. an `int` slot as a `float` — same width but a
                // different verification category would pin a wrong frame type).
                let is_ref = |t: Ty| matches!(t, Ty::String | Ty::Obj(..)) || t.is_array();
                let reuse = self
                    .slots
                    .get(&index)
                    .copied()
                    .filter(|(_, ejt)| *ejt == jt || (is_ref(*ejt) && is_ref(jt)))
                    .map(|(s, _)| s);
                if let Some(i) = init {
                    self.emit_value(i, code);
                    emit_num_conv(self.value_ty(i), jt, code);
                    let slot = reuse.unwrap_or_else(|| {
                        let s = self.next_slot;
                        self.next_slot += slot_words(jt);
                        s
                    });
                    self.slots.insert(index, (slot, jt));
                    store(jt, slot, code);
                } else {
                    let slot = reuse.unwrap_or_else(|| {
                        let s = self.next_slot;
                        self.next_slot += slot_words(jt);
                        s
                    });
                    self.slots.insert(index, (slot, jt));
                }
            }
            IrExpr::SetValue { var, value } => {
                let Some(&(slot, jt)) = self.slots.get(&var) else {
                    self.run.emit_bail.set(true);
                    return;
                };
                // `i = i + k` / `i = k + i` / `i = i - k` on an `Int` local with a small constant `k`
                // compiles to `iinc slot, k` (kotlinc's form), not load/const/add/store.
                let delta: Option<i32> = if jt == Ty::Int {
                    if let IrExpr::PrimitiveBinOp { op, lhs, rhs } = *self.ir.expr(value) {
                        let cint = |e: u32| match self.ir.expr(e) {
                            IrExpr::Const(IrConst::Int(k)) => Some(*k),
                            _ => None,
                        };
                        let isvar =
                            |e: u32| matches!(self.ir.expr(e), IrExpr::GetValue(v) if *v == var);
                        match op {
                            IrBinOp::Add if isvar(lhs) => cint(rhs),
                            IrBinOp::Add if isvar(rhs) => cint(lhs),
                            IrBinOp::Sub if isvar(lhs) => cint(rhs).map(|k| -k),
                            _ => None,
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
                match delta {
                    Some(d) if (-128..=127).contains(&d) => code.iinc(slot, d as i8),
                    _ => {
                        self.emit_value(value, code);
                        emit_num_conv(self.value_ty(value), jt, code);
                        store(jt, slot, code);
                    }
                }
            }
            IrExpr::SetField {
                receiver,
                class,
                index,
                value,
            } => {
                let c = &self.ir.classes[class as usize];
                let name = c.fields[index as usize].name.clone();
                let fty = c.fields[index as usize].ty.clone();
                let jt = ir_ty_to_jvm(&fty);
                let owner = c.fq_name();
                // A branchy value emits a merge frame; with the receiver already on the stack the
                // verifier sees a non-empty baseline it can't reconcile (krusty's frames carry no stack
                // prefix). Spill the value to a temp first — its branches then run on a clean stack —
                // then load the receiver and the temp. (Plain values keep the direct receiver,value order.)
                if self.records_frame(value) {
                    let temps = self.spill_to_temps(&[value], code);
                    self.emit_value(receiver, code);
                    let (slot, t, key) = temps[0];
                    load(t, slot, code);
                    self.slots.remove(&key);
                } else {
                    self.emit_value(receiver, code);
                    self.emit_value(value, code);
                }
                let fref = self.cw.fieldref(&owner, &name, &type_descriptor(jt));
                code.putfield(fref, slot_words(jt) as i32);
            }
            IrExpr::SetStatic { index, value } => {
                let s = &self.ir.statics[index as usize];
                let jt = ir_ty_to_jvm(&s.ty);
                let name = s.name.clone();
                let is_const = s.is_const;
                let facade = self.facade.clone();
                self.emit_value(value, code);
                // Within the facade write the field directly; from another class go through `setX()` —
                // or, for a PRIVATE top-level property (no public setter), the `access$set<X>$p` bridge.
                let private = self.ir.statics[index as usize].visibility.is_private();
                if self.owner == facade || is_const {
                    let fref = self.cw.fieldref(&facade, &name, &type_descriptor(jt));
                    code.putstatic(fref, slot_words(jt) as i32);
                } else {
                    let sname = if private {
                        format!("access${}$p", property_setter_name(&name))
                    } else {
                        property_setter_name(&name)
                    };
                    let m =
                        self.cw
                            .methodref(&facade, &sname, &format!("({})V", type_descriptor(jt)));
                    code.invokestatic(m, slot_words(jt) as i32, 0);
                }
            }
            IrExpr::While {
                cond,
                body,
                update,
                post_test,
                label,
            } => {
                let start = code.new_label();
                let cont = code.new_label();
                let end = code.new_label();
                self.frame(start, vec![], code);
                code.bind(start);
                // A pre-test loop checks the condition before the body; a `do…while` skips this and
                // tests at the bottom (`cont`), so the body always runs once.
                if !post_test {
                    // Jump out of the loop when the condition is false (fused comparison branch).
                    self.emit_cond_branch(cond, end, false, code);
                }
                // `continue` targets `cont` (run the update / bottom test); `break` targets `end`.
                self.loop_stack.push((cont, end, label.clone()));
                self.emit(body, code);
                // The body block restored the slot map, so framing `cont`/`start` here captures the
                // loop's outer locals — a `continue` jumping in from a deeper scope stays compatible.
                self.frame(cont, vec![], code);
                code.bind(cont);
                // The update is part of the loop, so it keeps the `break`/`continue` scope active — the
                // non-overflowing counted loop puts its `if (i == end) break` here (before the increment)
                // so a `continue` lands on it too, instead of skipping straight to the wrapping `i++`.
                if let Some(u) = update {
                    self.emit(u, code);
                }
                self.loop_stack.pop();
                if post_test {
                    // `do…while`: loop back while the condition holds, then fall through to `end`.
                    self.emit_cond_branch(cond, start, true, code);
                } else {
                    self.frame(start, vec![], code);
                    code.goto(start);
                }
                self.frame(end, vec![], code);
                code.bind(end);
            }
            IrExpr::Break { label } => {
                let (_, end) = self.loop_target(&label);
                code.goto(end);
            }
            IrExpr::Continue { label } => {
                let (cont, _) = self.loop_target(&label);
                code.goto(cont);
            }
            other => {
                self.emit_discarding_node(e, &other, code);
            }
        }
    }

    fn emit_discarding(&mut self, e: u32, code: &mut CodeBuilder) {
        let node = self.ir.expr(e).clone();
        self.emit_discarding_node(e, &node, code);
    }

    fn emit_discarding_node(&mut self, e: u32, node: &IrExpr, code: &mut CodeBuilder) {
        self.emit_value_node(e, node, code);
        // A `Nothing`-returning call leaves a physical `Void` and must terminate the path (it would
        // otherwise fall through with a stray value); the throw replaces the discard.
        if self.terminate_if_nothing_call(node, code) {
            return;
        }
        discard(self.value_ty(e), code);
    }

    fn emit_value(&mut self, e: u32, code: &mut CodeBuilder) {
        let node = self.ir.expr(e).clone();
        self.emit_value_node(e, &node, code);
        self.terminate_if_nothing_call(&node, code);
    }

    /// Emit `e` and then narrow it to the CONSUMPTION type `expected` — the `checkcast` kotlinc inserts
    /// when a value out of an ERASED slot (a type parameter's `Object`, a generic `Array<T>`'s `Object[]`)
    /// flows to a more specific reference (a `return`/argument/receiver of that type). Keyed on the value's
    /// ACTUAL physical type: a concrete source (already the target, or an unrelated concrete type such as a
    /// value class's unboxed underlying) is left alone — the backend owns this erasure decision.
    fn emit_value_as(&mut self, e: u32, expected: &Ty, code: &mut CodeBuilder) {
        self.emit_value(e, code);
        let src = self.value_ty(e);
        self.narrow_on_stack(src, expected, code);
    }

    /// Narrow the value on top of the stack (whose actual type is `src`) to the CONSUMPTION type
    /// `expected` — the `checkcast` kotlinc inserts when an ERASED value (a type parameter's `Object`, a
    /// generic `Array<T>`'s `Object[]`) flows to a more specific reference. Keyed on `src`: a concrete
    /// source (already the target, or an unrelated concrete type such as a value class's unboxed
    /// underlying) is left alone.
    fn narrow_on_stack(&mut self, src: Ty, expected: &Ty, code: &mut CodeBuilder) {
        let s = ir_ty_to_jvm(&src);
        if !jvm_is_erased_top(s) {
            return;
        }
        let exp = ir_ty_to_jvm(expected);
        if !exp.is_reference() || type_descriptor(s) == type_descriptor(exp) {
            return;
        }
        let internal = ref_internal(exp);
        if internal != "java/lang/Object" {
            let ci = self.cw.class_ref(&internal);
            code.checkcast(ci);
        }
    }

    /// A `Nothing`-returning REAL-invoke call (`exit(): Nothing`) physically leaves a `java/lang/Void`
    /// on the stack and falls through — unlike `throw`/`return`, which terminate. kotlinc makes the path
    /// truly diverge: discard the `Void`, then `throw KotlinNothingValueException()`. Mirror that so a
    /// `Nothing` call used in a branch (`if (c) … else exit()`, a diverging `catch`) terminates instead of
    /// leaking a `Void` into the merge/handler frame. Inline-spliced `Nothing` calls (`error(...)`) already
    /// end in `athrow` and are excluded. Returns whether the terminating throw was emitted.
    fn terminate_if_nothing_call(&mut self, node: &IrExpr, code: &mut CodeBuilder) -> bool {
        if !self.is_real_nothing_call(node) {
            return false;
        }
        code.pop();
        let cls = self.cw.class_ref("kotlin/KotlinNothingValueException");
        code.new_obj(cls);
        code.dup();
        let ctor = self
            .cw
            .methodref("kotlin/KotlinNothingValueException", "<init>", "()V");
        code.invokespecial(ctor, 0, 0);
        code.athrow();
        true
    }

    /// A call that physically returns (real `invoke`, leaving a `java/lang/Void`) yet is typed `Nothing`.
    /// Excludes inline-spliced (`error`/`require`) and intrinsic (`External`) callees, which already end
    /// the path in `athrow` and leave nothing to discard.
    fn is_real_nothing_call(&self, node: &IrExpr) -> bool {
        match node {
            IrExpr::MethodCall { class, index, .. } => {
                let fid = self.ir.classes[*class as usize].methods[*index as usize];
                ret_is_nothing(&self.ir.functions[fid as usize].ret)
            }
            IrExpr::Call { callee, .. } => match callee {
                Callee::Local(fid) | Callee::LocalDefault(fid) => {
                    ret_is_nothing(&self.ir.functions[*fid as usize].ret)
                }
                Callee::CrossFile { ret, .. } => ret_is_nothing(ret),
                Callee::Virtual { descriptor, .. } | Callee::Special { descriptor, .. } => {
                    descriptor.ends_with(")Ljava/lang/Void;")
                }
                Callee::CrossFileVirtual { ret, .. } => ret_is_nothing(ret),
                Callee::Static {
                    descriptor, inline, ..
                } => !inline.can_inline() && descriptor.ends_with(")Ljava/lang/Void;"),
                Callee::External(_) => false,
            },
            _ => false,
        }
    }

    fn emit_value_node(&mut self, e: u32, node: &IrExpr, code: &mut CodeBuilder) {
        match node {
            // `break`/`continue` are `Nothing`-typed: in value position (e.g. `x ?: break`) they diverge
            // — emit the jump and push nothing; the consuming branch is dead past this point.
            IrExpr::Break { label } => {
                let (_, end) = self.loop_target(label);
                code.goto(end);
                return;
            }
            IrExpr::Continue { label } => {
                let (cont, _) = self.loop_target(label);
                code.goto(cont);
                return;
            }
            IrExpr::Const(c) => match c {
                IrConst::Boolean(b) => code.push_int(if *b { 1 } else { 0 }, self.cw),
                IrConst::Int(v) => code.push_int(*v, self.cw),
                IrConst::Short(v) => code.push_int(*v as i32, self.cw),
                IrConst::Byte(v) => code.push_int(*v as i32, self.cw),
                IrConst::Char(v) => code.push_int(*v as i32, self.cw),
                IrConst::Long(v) => code.push_long(*v, self.cw),
                IrConst::Double(v) => code.push_double(*v, self.cw),
                IrConst::Float(v) => code.push_float(*v, self.cw),
                IrConst::String(s) => code.push_string(s, self.cw),
                IrConst::Null => code.aconst_null(),
            },
            IrExpr::ClassConst { internal } => {
                let name = internal
                    .as_ref()
                    .map_or_else(|| self.facade.clone(), |name| name.render());
                code.ldc_class(&name, self.cw);
            }
            IrExpr::GetValue(i) => {
                // A slot that was never allocated means the lowering produced malformed IR (e.g. an
                // unsupported suspend shape). Don't panic — flag the file unemittable and skip it.
                let Some(&(slot, jt)) = self.slots.get(i) else {
                    crate::trace_compiler!(
                        "suspend",
                        "EMIT_BAIL GetValue unallocated slot i={i} owner={} known={:?}",
                        self.owner,
                        self.slots.keys().collect::<Vec<_>>()
                    );
                    self.run.emit_bail.set(true);
                    return;
                };
                load(jt, slot, code);
            }
            IrExpr::GetField {
                receiver,
                class,
                index,
            } => {
                let c = &self.ir.classes[*class as usize];
                let name = c.fields[*index as usize].name.clone();
                let fty = c.fields[*index as usize].ty.clone();
                let jt = ir_ty_to_jvm(&fty);
                let owner = c.fq_name();
                let is_lateinit = c.fields[*index as usize].is_lateinit;
                self.emit_value(*receiver, code);
                let fref = self.cw.fieldref(&owner, &name, &type_descriptor(jt));
                code.getfield(fref, slot_words(jt) as i32);
                // A `lateinit var` read throws `UninitializedPropertyAccessException` while the field is
                // still null (kotlinc inserts this at every access): `dup; ifnonnull L; ldc name;
                // invokestatic Intrinsics.throwUninitializedPropertyAccessException; L:`.
                if is_lateinit {
                    code.dup();
                    let lbl = code.new_label();
                    code.ifnonnull(lbl);
                    code.push_string(&name, self.cw);
                    let m = self.cw.methodref(
                        "kotlin/jvm/internal/Intrinsics",
                        "throwUninitializedPropertyAccessException",
                        "(Ljava/lang/String;)V",
                    );
                    code.invokestatic(m, 1, 0);
                    // At the join the field value (non-null on the taken path) is on the stack.
                    let st = self.verif_stack(jt);
                    self.frame(lbl, st, code);
                    code.bind(lbl);
                }
            }
            IrExpr::GetStatic(i) => {
                let s = &self.ir.statics[*i as usize];
                let jt = ir_ty_to_jvm(&s.ty);
                let name = s.name.clone();
                let is_const = s.is_const;
                let facade = self.facade.clone();
                // Within the facade (or a `const val`, which is public) read the field directly; from
                // another class a plain top-level property is private, so go through `getX()` — kotlinc's
                // cross-file property-access compilation.
                let private = self.ir.statics[*i as usize].visibility.is_private();
                if self.owner == facade || is_const {
                    let fref = self.cw.fieldref(&facade, &name, &type_descriptor(jt));
                    code.getstatic(fref, slot_words(jt) as i32);
                } else {
                    // A PRIVATE top-level property has no public getter; cross-class reads inside the
                    // file go through kotlinc's `access$get<X>$p` bridge.
                    let gname = if private {
                        format!("access${}$p", property_getter_name(&name))
                    } else {
                        property_getter_name(&name)
                    };
                    let m =
                        self.cw
                            .methodref(&facade, &gname, &format!("(){}", type_descriptor(jt)));
                    code.invokestatic(m, 0, slot_words(jt) as i32);
                }
            }
            IrExpr::New {
                class,
                args,
                ctor_params,
            } => {
                let c = &self.ir.classes[*class as usize];
                let owner = c.fq_name();
                // The constructor takes only the parameter fields (primary), or a secondary
                // constructor's explicit parameter types; body properties are set inside it.
                let mut field_tys: Vec<Ty> = match ctor_params {
                    Some(ps) => jvm_tys(ps),
                    None => class_ctor_jvm_tys(c),
                };
                // A class whose primary ctor takes a value-class param has a PRIVATE primary + a
                // PUBLIC|SYNTHETIC accessor `(…args, DefaultConstructorMarker)`. Construction from ANOTHER
                // class must route through the accessor (a trailing `null` marker) — the private primary is
                // inaccessible. Same-class construction (a secondary ctor, `box-impl`) keeps the primary.
                let use_accessor = ctor_params.is_none()
                    && self.owner != owner
                    && self.ir.has_value_param_ctor(&owner);
                if use_accessor {
                    field_tys.push(Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker"));
                }
                let args = args.clone();
                let aw: i32 = field_tys.iter().map(|t| slot_words(*t) as i32).sum();
                let desc = method_descriptor(&field_tys, Ty::Unit);
                if args.iter().any(|&a| self.records_frame(a)) {
                    // A branchy argument can't run with `[new, dup]` on the stack — its merge frame
                    // would omit them. Evaluate all args into temps first (clean stack), then build.
                    let temps = self.spill_to_temps(&args, code);
                    let ci = self.cw.class_ref(&owner);
                    code.new_obj(ci);
                    code.dup();
                    for &(slot, t, _) in &temps {
                        load(t, slot, code);
                    }
                    for &(_, _, key) in &temps {
                        self.slots.remove(&key);
                    }
                    if use_accessor {
                        code.aconst_null();
                    }
                    let m = self.cw.methodref(&owner, "<init>", &desc);
                    code.invokespecial(m, aw, 0);
                } else {
                    let ci = self.cw.class_ref(&owner);
                    code.new_obj(ci);
                    code.dup();
                    for &a in &args {
                        self.emit_value(a, code);
                    }
                    if use_accessor {
                        code.aconst_null();
                    }
                    let m = self.cw.methodref(&owner, "<init>", &desc);
                    code.invokespecial(m, aw, 0);
                }
            }
            IrExpr::MethodCall {
                class,
                index,
                receiver,
                args,
            } => {
                let c = &self.ir.classes[*class as usize];
                let fid = c.methods[*index as usize];
                let f = &self.ir.functions[fid as usize];
                let param_tys = jvm_tys(&f.params);
                let ret = ir_ty_to_jvm(&f.ret);
                let name = f.name.clone();
                let owner = c.fq_name();
                let is_iface = c.is_interface;
                if args.iter().any(|a| a.is_none()) {
                    // Some arguments are omitted — invoke the `<name>$default(self, params…, mask, marker)`
                    // stub: receiver, each provided arg (or a zero placeholder for an omitted one with its
                    // mask bit set), the mask, then a null marker. A nullable-underlying value-class param
                    // is BOXED in the stub signature (matching `emit_default_stub`), so a provided arg is
                    // `box-impl`d and the placeholder/descriptor use the boxed type.
                    let boxed: HashMap<usize, Ty> = self
                        .ir
                        .default_stub_boxed_params
                        .get(&fid)
                        .map(|v| v.iter().copied().collect())
                        .unwrap_or_default();
                    let stub_param_tys: Vec<Ty> = param_tys
                        .iter()
                        .enumerate()
                        .map(|(i, t)| boxed.get(&i).copied().unwrap_or(*t))
                        .collect();
                    let args = args.clone();
                    self.emit_value(*receiver, code);
                    let mut masks = vec![0i32; default_mask_count(param_tys.len())];
                    for (i, arg) in args.iter().enumerate() {
                        match arg {
                            Some(a) => {
                                self.emit_value(*a, code);
                                if let Some(vc) = boxed.get(&i) {
                                    emit_box_impl(self.ir, self.cw, vc, code);
                                }
                            }
                            None => {
                                push_zero(stub_param_tys[i], code, self.cw);
                                masks[i / 32] |= default_mask_bit(i);
                            }
                        }
                    }
                    for mask in masks {
                        code.push_int(mask, self.cw);
                    }
                    code.aconst_null();
                    let mut stub_params = vec![Ty::obj(&owner)];
                    stub_params.extend(stub_param_tys.iter().copied());
                    stub_params.extend(std::iter::repeat_n(
                        Ty::Int,
                        default_mask_count(param_tys.len()),
                    ));
                    stub_params.push(Ty::obj("java/lang/Object"));
                    let aw: i32 = stub_params.iter().map(|t| slot_words(*t) as i32).sum();
                    let stub_desc = method_descriptor(&stub_params, ret);
                    let stub_name = format!("{name}$default");
                    // The `$default` stub of an INTERFACE method is a STATIC interface method — referenced
                    // via an `InterfaceMethodref` constant (a plain `Methodref` is an
                    // `IncompatibleClassChangeError`), still invoked with `invokestatic`. (kotlinc ALSO
                    // emits a compatibility copy on `<Iface>$DefaultImpls`; call sites use the interface.)
                    let m = if is_iface {
                        self.cw.interface_methodref(&owner, &stub_name, &stub_desc)
                    } else {
                        self.cw.methodref(&owner, &stub_name, &stub_desc)
                    };
                    code.invokestatic(m, aw, slot_words(ret) as i32);
                    return;
                }
                let call_args: Vec<u32> = args.iter().map(|a| a.unwrap()).collect();
                self.emit_virtual_operands(&owner, *receiver, &call_args, code);
                let aw: i32 = param_tys.iter().map(|t| slot_words(*t) as i32).sum();
                let desc = method_descriptor(&param_tys, ret);
                crate::trace_compiler!(
                    "resolve",
                    "emit MethodCall {}.{} fid={fid} private={} iface={is_iface}",
                    owner,
                    name,
                    self.ir.private_methods.contains(&fid)
                );
                if self.ir.private_methods.contains(&fid) {
                    // A PRIVATE method is non-virtual — `invokespecial` (an interface private method uses an
                    // `InterfaceMethodref`), so it never dispatches to a same-named override.
                    let m = if is_iface {
                        self.cw.interface_methodref(&owner, &name, &desc)
                    } else {
                        self.cw.methodref(&owner, &name, &desc)
                    };
                    code.invokespecial(m, aw, slot_words(ret) as i32);
                } else if is_iface {
                    // Dispatch through an interface — `invokeinterface I.m`.
                    let m = self.cw.interface_methodref(&owner, &name, &desc);
                    code.invokeinterface(m, aw, slot_words(ret) as i32);
                } else {
                    let m = self.cw.methodref(&owner, &name, &desc);
                    code.invokevirtual(m, aw, slot_words(ret) as i32);
                }
            }
            IrExpr::Call {
                callee,
                dispatch_receiver,
                args,
            } => match callee {
                Callee::Local(fid) => {
                    let f = &self.ir.functions[*fid as usize];
                    let param_tys = jvm_tys(&f.params);
                    let ret = ir_ty_to_jvm(&f.ret);
                    // A PRIVATE facade function can't be invoked from another class (a lambda impl on
                    // its enclosing class, a continuation class, any class member) — kotlinc routes
                    // those callers through the `access$<name>` bridge (emitted by `emit_pass` when
                    // referenced; see `facade_access_bridges`).
                    let name = if self.owner != self.facade && self.ir.private_methods.contains(fid)
                    {
                        format!("access${}", f.name)
                    } else {
                        f.name.clone()
                    };
                    let args = args.clone();
                    self.emit_operands(&args, code);
                    let aw: i32 = param_tys.iter().map(|t| slot_words(*t) as i32).sum();
                    let owner = self.facade.clone();
                    let m = self
                        .cw
                        .methodref(&owner, &name, &method_descriptor(&param_tys, ret));
                    code.invokestatic(m, aw, slot_words(ret) as i32);
                }
                Callee::LocalDefault(fid) => {
                    // The `foo$default(realparams, mask..., Object marker)` synthetic on the self facade
                    // (emitted by `emit_facade_default_stub`). Args already include mask words + marker.
                    let f = &self.ir.functions[*fid as usize];
                    let mut param_tys = jvm_tys(&f.params);
                    param_tys.extend(std::iter::repeat_n(
                        Ty::Int,
                        default_mask_count(f.params.len()),
                    ));
                    param_tys.push(Ty::obj("java/lang/Object"));
                    let ret = ir_ty_to_jvm(&f.ret);
                    let name = format!("{}$default", f.name);
                    let args = args.clone();
                    self.emit_operands(&args, code);
                    let aw: i32 = param_tys.iter().map(|t| slot_words(*t) as i32).sum();
                    let owner = self.facade.clone();
                    let m = self
                        .cw
                        .methodref(&owner, &name, &method_descriptor(&param_tys, ret));
                    code.invokestatic(m, aw, slot_words(ret) as i32);
                }
                Callee::External(fq) => self.emit_intrinsic(fq, dispatch_receiver, args, code),
                Callee::CrossFile {
                    facade,
                    name,
                    params,
                    ret,
                } => {
                    // A top-level function from another file → `invokestatic <facade>.<name>(desc)`.
                    let param_tys = jvm_tys(params);
                    let ret = ir_ty_to_jvm(ret);
                    let (facade, name) = (facade.render(), name.clone());
                    let args = args.clone();
                    self.emit_operands(&args, code);
                    let aw: i32 = param_tys.iter().map(|t| slot_words(*t) as i32).sum();
                    let desc = method_descriptor(&param_tys, ret);
                    // A static method declared on an INTERFACE (`@Serializable(with=X) interface I` whose
                    // synthetic `serializer()` is static) needs an InterfaceMethodref constant, even for
                    // `invokestatic` (else `IncompatibleClassChangeError`).
                    let m = if self
                        .ir
                        .classes
                        .iter()
                        .any(|c| c.fq_name_matches(&facade) && c.is_interface)
                    {
                        self.cw.interface_methodref(&facade, &name, &desc)
                    } else {
                        self.cw.methodref(&facade, &name, &desc)
                    };
                    code.invokestatic(m, aw, slot_words(ret) as i32);
                }
                Callee::Static {
                    owner,
                    name,
                    descriptor,
                    inline,
                } => {
                    let (owner, name, descriptor, inline) =
                        (owner.render(), name.clone(), descriptor.clone(), *inline);
                    let args = args.clone();
                    crate::trace_compiler!(
                        "resolve",
                        "emit static {owner}.{name}{descriptor} inline={inline:?}"
                    );
                    // `@InlineOnly`/non-public inline functions must splice. Public inline functions have
                    // callable bytecode, so a failed optional splice can fall back to a real call.
                    if inline.can_inline() {
                        let spliced = if let Some(&recv) = dispatch_receiver.as_ref() {
                            let recv_desc = type_descriptor(self.value_ty(recv));
                            let splice_desc = format!("({}{}", recv_desc, &descriptor[1..]);
                            let mut all = Vec::with_capacity(args.len() + 1);
                            all.push(recv);
                            all.extend(args.iter().copied());
                            let target = InlineStaticTarget {
                                owner: &owner,
                                name: &name,
                                descriptor: &descriptor,
                                splice_desc: &splice_desc,
                            };
                            let reified = self.reified_type_map(e);
                            self.try_inline_static_as(target, &all, code, true, &reified)
                        } else {
                            let has_lambda_arg = args.iter().any(|&a| {
                                matches!(self.ir.expr(a), IrExpr::Lambda { .. })
                                    || self.function_ref_class_and_captures(a).is_some()
                                    || self.property_ref_class_and_captures(a).is_some()
                            });
                            let target = InlineStaticTarget {
                                owner: &owner,
                                name: &name,
                                descriptor: &descriptor,
                                splice_desc: &descriptor,
                            };
                            let reified = self.reified_type_map(e);
                            self.try_inline_static_as(
                                target,
                                &args,
                                code,
                                inline.must_inline() || has_lambda_arg,
                                &reified,
                            )
                        };
                        if spliced {
                            return;
                        }
                        if inline.must_inline() {
                            self.run.set_inline_bail(format!(
                                "inline splice failed for {owner}.{name}{descriptor}"
                            ));
                        }
                    }
                    self.emit_operands(&args, code);
                    let aw: i32 = args
                        .iter()
                        .map(|&a| slot_words(self.value_ty(a)) as i32)
                        .sum();
                    let ret = ty_from_descriptor_ret(&descriptor);
                    // A static method DECLARED ON AN INTERFACE (a Kotlin interface's `foo$default` synthetic,
                    // reached when a call omits an interface-declared default) must be an `InterfaceMethodref`
                    // even for `invokestatic` — else the JVM throws `IncompatibleClassChangeError`. Classes
                    // (stdlib facades, the common case) stay `Methodref`.
                    let m = if self.bodies.owner_is_interface(&owner) {
                        self.cw.interface_methodref(&owner, &name, &descriptor)
                    } else {
                        self.cw.methodref(&owner, &name, &descriptor)
                    };
                    code.invokestatic(m, aw, slot_words(ret) as i32);
                }
                Callee::Virtual {
                    owner,
                    name,
                    descriptor,
                    interface,
                } => {
                    let (owner, name, descriptor, interface) =
                        (owner.render(), name.clone(), descriptor.clone(), *interface);
                    let recv = dispatch_receiver.expect("virtual call needs a receiver");
                    let args = args.clone();
                    if self.emit_primitive_inc_dec_virtual(
                        &owner,
                        &name,
                        &descriptor,
                        recv,
                        &args,
                        code,
                    ) {
                        return;
                    }
                    if self.emit_unsigned_compare_to_virtual(&owner, &name, recv, &args, code) {
                        return;
                    }
                    if is_string_plus_virtual(&owner, &name, &descriptor) && args.len() == 1 {
                        self.emit_string_plus(recv, args[0], code);
                        return;
                    }
                    if let Some((range_internal, ctor_desc, aw, elem)) =
                        range_to_virtual_ctor(&owner, &name, &descriptor)
                            .filter(|_| args.len() == 1)
                    {
                        self.emit_external_new_coerced(
                            range_internal,
                            ctor_desc,
                            &[recv, args[0]],
                            aw,
                            elem,
                            code,
                        );
                        return;
                    }
                    if parse_descriptor_params(&descriptor)
                        .is_some_and(|params| params.len() == args.len() + 1)
                    {
                        let mut physical_args = Vec::with_capacity(args.len() + 1);
                        physical_args.push(recv);
                        physical_args.extend(args.iter().copied());
                        if self.try_inline_static(&owner, &name, &descriptor, &physical_args, code)
                        {
                            return;
                        }
                        self.emit_operands(&physical_args, code);
                        let aw: i32 = physical_args
                            .iter()
                            .map(|&a| slot_words(self.value_ty(a)) as i32)
                            .sum();
                        let ret = ty_from_descriptor_ret(&descriptor);
                        let m = self.cw.methodref(&owner, &name, &descriptor);
                        code.invokestatic(m, aw, slot_words(ret) as i32);
                        return;
                    }
                    self.emit_virtual_operands(&owner, recv, &args, code);
                    let aw: i32 = args
                        .iter()
                        .map(|&a| slot_words(self.value_ty(a)) as i32)
                        .sum();
                    let ret = ty_from_descriptor_ret(&descriptor);
                    let jvm_name = mapped_builtin_virtual_name(&owner, &name);
                    if interface {
                        let m = self.cw.interface_methodref(&owner, jvm_name, &descriptor);
                        code.invokeinterface(m, aw, slot_words(ret) as i32);
                    } else {
                        let m = self.cw.methodref(&owner, jvm_name, &descriptor);
                        code.invokevirtual(m, aw, slot_words(ret) as i32);
                    }
                }
                Callee::CrossFileVirtual {
                    owner,
                    name,
                    params,
                    ret,
                    interface,
                } => {
                    let owner = owner.render();
                    let name = name.clone();
                    let interface = *interface;
                    let param_tys = jvm_tys(params);
                    let ret = ir_ty_to_jvm(ret);
                    let descriptor = method_descriptor(&param_tys, ret);
                    let recv = dispatch_receiver.expect("cross-file virtual call needs a receiver");
                    let args = args.clone();
                    let mut ops = vec![recv];
                    ops.extend(args.iter().copied());
                    self.emit_operands(&ops, code);
                    let aw: i32 = param_tys.iter().map(|t| slot_words(*t) as i32).sum();
                    if interface {
                        let m = self.cw.interface_methodref(&owner, &name, &descriptor);
                        code.invokeinterface(m, aw, slot_words(ret) as i32);
                    } else {
                        let m = self.cw.methodref(&owner, &name, &descriptor);
                        code.invokevirtual(m, aw, slot_words(ret) as i32);
                    }
                }
                Callee::Special {
                    owner,
                    name,
                    descriptor,
                    interface,
                } => {
                    let (owner, name, descriptor, interface) =
                        (owner.render(), name.clone(), descriptor.clone(), *interface);
                    let recv = dispatch_receiver.expect("special call needs a receiver");
                    let args = args.clone();
                    let mut ops = vec![recv];
                    ops.extend(args.iter().copied());
                    self.emit_operands(&ops, code);
                    let aw: i32 = args
                        .iter()
                        .map(|&a| slot_words(self.value_ty(a)) as i32)
                        .sum();
                    let ret = ty_from_descriptor_ret(&descriptor);
                    // A diamond `super.f()` to a superinterface DEFAULT method: `invokespecial` on an
                    // `InterfaceMethodref` (JVM allows a direct-superinterface default this way).
                    let m = if interface {
                        self.cw.interface_methodref(&owner, &name, &descriptor)
                    } else {
                        self.cw.methodref(&owner, &name, &descriptor)
                    };
                    code.invokespecial(m, aw, slot_words(ret) as i32);
                }
            },
            IrExpr::TypeOp {
                op,
                arg,
                type_operand,
            } => {
                // A primitive target of `instanceof`/`checkcast` (`x is Int`) tests the boxed wrapper.
                let jvm_ty = ir_ty_to_jvm(type_operand);
                let internal = if jvm_ty.is_jvm_scalar() {
                    crate::jvm::jvm_class_map::wrapper_internal(jvm_ty)
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| ref_internal(jvm_ty))
                } else {
                    ref_internal(jvm_ty)
                };
                self.emit_value(*arg, code);
                match op {
                    IrTypeOp::InstanceOf => {
                        let ci = self.cw.class_ref(&internal);
                        code.instance_of(ci);
                    }
                    IrTypeOp::NotInstanceOf => {
                        let ci = self.cw.class_ref(&internal);
                        code.instance_of(ci);
                        code.push_int(1, self.cw);
                        code.ixor();
                    }
                    IrTypeOp::Cast => {
                        // The emitter owns erasure: a `checkcast` to `java/lang/Object` (an unbounded `as T`)
                        // is a no-op, and so is one whose target descriptor already equals the value's
                        // actual (physical) descriptor — an erasure-narrowing tag where the value is already
                        // that type (`List<T>` read tagged `List<Int>`). kotlinc emits neither.
                        let redundant =
                            type_descriptor(self.value_ty(*arg)) == type_descriptor(jvm_ty);
                        if internal != "java/lang/Object" && !redundant {
                            let ci = self.cw.class_ref(&internal);
                            code.checkcast(ci);
                        }
                    }
                    IrTypeOp::CastNonNull => {
                        // Null-check (throws on null) then checkcast — matching kotlinc's `as T`.
                        let kotlin_name = match type_operand.non_null() {
                            Ty::Obj(fq_name, _) => fq_name.replace('/', "."),
                            Ty::TyParam(name, _) => name.to_string(),
                            _ => "kotlin.Any".to_string(),
                        };
                        code.dup();
                        code.push_string(
                            &format!("null cannot be cast to non-null type {kotlin_name}"),
                            self.cw,
                        );
                        let m = self.cw.methodref(
                            "kotlin/jvm/internal/Intrinsics",
                            "checkNotNull",
                            "(Ljava/lang/Object;Ljava/lang/String;)V",
                        );
                        code.invokestatic(m, 2, 0);
                        // Erased bound `java/lang/Object` (an `<T : Any>` cast) needs no `checkcast`.
                        if internal != "java/lang/Object" {
                            let ci = self.cw.class_ref(&internal);
                            code.checkcast(ci);
                        }
                    }
                    // Box a primitive into a reference target, unbox a wrapper into a primitive, or
                    // widen/narrow between primitive numeric types (`Int`→`Long`, `Double`→`Int`, …).
                    IrTypeOp::ImplicitCoercion => {
                        let at = self.value_ty(*arg);
                        let target = ir_ty_to_jvm(type_operand);
                        crate::trace_compiler!(
                            "value_classes",
                            "coerce at={at:?} target={target:?} type_operand={type_operand:?}"
                        );
                        if at.is_jvm_scalar() && target.is_reference() {
                            box_prim_free(self.cw, code, at);
                        } else if at.is_reference() && target.is_jvm_scalar() {
                            // The unbox method comes from the SOURCE wrapper on the stack: a boxed UNSIGNED
                            // value is `kotlin/UInt` (unboxed via its inline-class `unbox-impl` row in
                            // `unbox_prim`), while `target` was erased to the signed `Int`. Recover the
                            // unsigned type from `at` so the right `unbox_prim` row is hit.
                            let src = at
                                .obj_internal()
                                .and_then(|n| {
                                    if n.matches("kotlin/UInt") {
                                        Some(Ty::UInt)
                                    } else if n.matches("kotlin/ULong") {
                                        Some(Ty::ULong)
                                    } else {
                                        None
                                    }
                                })
                                .unwrap_or(target);
                            unbox_prim(self.cw, code, src);
                        } else if at.is_jvm_scalar() && target.is_jvm_scalar() && at != target {
                            emit_num_conv(at, target, code);
                        }
                    }
                    IrTypeOp::SafeCast => {}
                }
            }
            IrExpr::PrimitiveBinOp { op, lhs, rhs } => self.emit_binop(*op, *lhs, *rhs, code),
            IrExpr::StringConcat(parts) => {
                let parts = parts.clone();
                if parts.len() == 1 {
                    let p = parts[0];
                    if matches!(self.ir.expr(p), IrExpr::Const(IrConst::String(_))) {
                        // A lone string constant is already a `String`.
                        self.emit_value(p, code);
                    } else {
                        // A single interpolation `"$x"` → `String.valueOf(x)` (kotlinc's form).
                        let pty = self.value_ty(p);
                        self.emit_value(p, code);
                        let m = self
                            .cw
                            .methodref("java/lang/String", "valueOf", valueof_desc(pty));
                        code.invokestatic(m, slot_words(pty) as i32, 1);
                    }
                } else {
                    let sb = self.cw.class_ref("java/lang/StringBuilder");
                    let init = self
                        .cw
                        .methodref("java/lang/StringBuilder", "<init>", "()V");
                    // A branchy part (`"${when{…}}"`) records merge frames that would omit the
                    // StringBuilder on the stack — spill every part to a temp first, then build.
                    if parts.iter().any(|&p| self.records_frame(p)) {
                        let temps = self.spill_to_temps(&parts, code);
                        code.new_obj(sb);
                        code.dup();
                        code.invokespecial(init, 0, 0);
                        for &(slot, t, _) in &temps {
                            load(t, slot, code);
                            self.append_top(t, code);
                        }
                        for &(_, _, key) in &temps {
                            self.slots.remove(&key);
                        }
                    } else {
                        code.new_obj(sb);
                        code.dup();
                        code.invokespecial(init, 0, 0);
                        for &p in &parts {
                            self.append_part(p, code);
                        }
                    }
                    let ts = self.cw.methodref(
                        "java/lang/StringBuilder",
                        "toString",
                        "()Ljava/lang/String;",
                    );
                    code.invokevirtual(ts, 0, 1);
                }
            }
            IrExpr::EnumEntry { class, index } => {
                let c = &self.ir.classes[*class as usize];
                let entry = c.enum_entries[*index as usize].name.clone();
                let fq_name = c.fq_name();
                let desc = format!("L{fq_name};");
                let f = self.cw.fieldref(&fq_name, &entry, &desc);
                code.getstatic(f, 1);
            }
            IrExpr::StaticInstance { owner, ty, field } => {
                let owner_fq = self.ir.classes[*owner as usize].fq_name();
                let ty_fq = self.ir.classes[*ty as usize].fq_name();
                let f = self.cw.fieldref(&owner_fq, field, &format!("L{ty_fq};"));
                code.getstatic(f, 1);
            }
            IrExpr::ExternalStaticInstance { owner, ty, field } => {
                let owner = owner.render();
                let ty = ty.render();
                let f = self.cw.fieldref(&owner, field, &format!("L{ty};"));
                code.getstatic(f, 1);
            }
            IrExpr::ExternalStaticField {
                owner,
                name,
                descriptor,
            } => {
                let owner = owner.render();
                let f = self.cw.fieldref(&owner, name, descriptor);
                let words = if descriptor == "J" || descriptor == "D" {
                    2
                } else {
                    1
                };
                code.getstatic(f, words);
            }
            IrExpr::EnumValues { class } => {
                let fq = self.ir.classes[*class as usize].fq_name();
                let m = self.cw.methodref(&fq, "values", &format!("()[L{fq};"));
                code.invokestatic(m, 0, 1);
            }
            IrExpr::EnumValueOf { class, arg } => {
                let fq = self.ir.classes[*class as usize].fq_name();
                self.emit_value(*arg, code);
                let m = self
                    .cw
                    .methodref(&fq, "valueOf", &format!("(Ljava/lang/String;)L{fq};"));
                code.invokestatic(m, 1, 1);
            }
            IrExpr::When { branches } => self.emit_when(branches, code),
            // Block in value position: run its statements for effect, leave the trailing value on the
            // stack. Scope block-locals (restore the slot map) so they don't leak into outer frames.
            IrExpr::Block { stmts, value } => {
                let saved = self.slots.clone();
                let mut dead = false;
                for s in stmts {
                    // A statement nets zero on the operand stack (its value is stored/discarded). Reset
                    // the tracked height to that baseline afterward: a branchy lambda splice (`takeIf`)
                    // tracks its internal branches only approximately and can leave `cur_stack` drifted
                    // above the real (verified-balanced) height, which would make a LATER branchy splice
                    // in the same block falsely see a non-empty baseline and bail.
                    let base = code.stack_height();
                    self.emit(*s, code);
                    if self.diverges(*s) {
                        dead = true;
                        break;
                    }
                    code.set_stack(base.max(0) as u16);
                }
                if !dead {
                    if let Some(v) = value {
                        self.emit_value(*v, code);
                    }
                }
                self.slots = saved;
            }
            IrExpr::Lambda {
                impl_fn,
                arity,
                captures,
                sam,
                ..
            } => {
                // This lambda becomes a REAL closure (`invokedynamic` referencing its impl method) — record
                // it so the dead-lambda pass keeps the impl. An INLINED lambda never reaches this arm.
                self.run.used_lambdas.borrow_mut().insert(*impl_fn);
                let f = &self.ir.functions[*impl_fn as usize];
                let impl_name = f.name.clone();
                let impl_params = jvm_tys(&f.params);
                let impl_ret = ir_ty_to_jvm(&f.ret);
                // The impl method's parameters are the captured variables (bound at the call site)
                // followed by the lambda's own parameters. Only the latter form the SAM/instantiated
                // method types; the captures parameterize the `invokedynamic` itself.
                let n_cap = impl_params.len() - *arity as usize;
                let (cap_tys, lam_tys) = impl_params.split_at(n_cap);
                let impl_desc = method_descriptor(&impl_params, impl_ret);
                // For a Kotlin lambda the target is `FunctionN.invoke` (samMethodType erased to
                // `(Object,…)Object`, instantiatedMethodType the boxed actuals); for a user SAM
                // conversion the target is the interface's single method, whose descriptor is the
                // lambda's concrete signature (no erasure/boxing).
                let (iface, sam_method, sam_desc, inst_desc) = match sam {
                    Some((iface, method)) => {
                        // `samMethodType` is the INTERFACE method's (erased) descriptor — NOT the
                        // lambda's — so a SAM with parameters (or a generic SAM erased to `Object`)
                        // matches the abstract method the metafactory must implement.
                        // `instantiatedMethodType` is the impl's actual lambda signature; the
                        // metafactory inserts the bridge between them.
                        let inst_desc = method_descriptor(lam_tys, impl_ret);
                        let sam_desc = self
                            .ir
                            .classes
                            .iter()
                            .find(|c| c.fq_name_matches(iface))
                            .and_then(|c| {
                                c.methods
                                    .iter()
                                    .map(|&m| &self.ir.functions[m as usize])
                                    .find(|f| f.name == *method)
                            })
                            .map(|f| ir_method_desc(&f.params, &f.ret))
                            .unwrap_or_else(|| inst_desc.clone());
                        (iface.clone(), method.clone(), sam_desc, inst_desc)
                    }
                    None => {
                        let iface = format!("kotlin/jvm/functions/Function{arity}");
                        let inst_params: Vec<String> =
                            lam_tys.iter().map(|t| boxed_descriptor(*t)).collect();
                        let inst_desc =
                            format!("({}){}", inst_params.concat(), boxed_descriptor(impl_ret));
                        (
                            iface,
                            "invoke".to_string(),
                            sam_descriptor(*arity),
                            inst_desc,
                        )
                    }
                };
                // The impl method lives on whichever class owns it (a class-member lambda's impl is a
                // method of the enclosing class, so it can access that class's privates); top-level
                // lambdas keep theirs on the file facade.
                let impl_owner = self
                    .ir
                    .classes
                    .iter()
                    .find(|c| c.methods.contains(impl_fn))
                    .map(|c| c.fq_name())
                    .unwrap_or_else(|| self.facade.clone());
                let meta = self.cw.method_handle_static(
                    "java/lang/invoke/LambdaMetafactory",
                    "metafactory",
                    LMF_METAFACTORY_DESC,
                );
                let sam_mt = self.cw.method_type(&sam_desc);
                let impl_mh = self
                    .cw
                    .method_handle_static(&impl_owner, &impl_name, &impl_desc);
                let inst_mt = self.cw.method_type(&inst_desc);
                let bsm = self.cw.add_bootstrap(meta, vec![sam_mt, impl_mh, inst_mt]);
                // The `invokedynamic` takes the captured values and yields the interface instance.
                let cap_descs: String = cap_tys.iter().map(|t| type_descriptor(*t)).collect();
                let indy =
                    self.cw
                        .invoke_dynamic(bsm, &sam_method, &format!("({cap_descs})L{iface};"));
                let cap_words: i32 = cap_tys.iter().map(|t| slot_words(*t) as i32).sum();
                for &c in captures {
                    self.emit_value(c, code);
                }
                code.invokedynamic(indy, cap_words, 1);
            }
            IrExpr::UnitInstance => {
                let f = self.cw.fieldref("kotlin/Unit", "INSTANCE", "Lkotlin/Unit;");
                code.getstatic(f, 1);
            }
            IrExpr::CurrentContinuation => {
                // The CPS pass (`jvm/suspend.rs`) rewrites every `CurrentContinuation` to a `GetValue` of
                // the continuation slot before emit; reaching here means it was emitted outside a suspend
                // function, which the front end forbids.
                unreachable!("CurrentContinuation must be resolved by the CPS pass before emit")
            }
            IrExpr::NotNullAssert { operand } => {
                self.emit_value(*operand, code);
                code.dup();
                let m = self.cw.methodref(
                    "kotlin/jvm/internal/Intrinsics",
                    "checkNotNull",
                    "(Ljava/lang/Object;)V",
                );
                code.invokestatic(m, 1, 0);
            }
            IrExpr::LateinitCheck { operand, name } => {
                // A `lateinit var` local read: throw `UninitializedPropertyAccessException` while the slot
                // is still null. Same guard as the member-field lateinit read (`dup; ifnonnull L; ldc
                // name; invokestatic throwUninitializedPropertyAccessException; L:`).
                self.emit_value(*operand, code);
                code.dup();
                let lbl = code.new_label();
                code.ifnonnull(lbl);
                code.push_string(name, self.cw);
                let m = self.cw.methodref(
                    "kotlin/jvm/internal/Intrinsics",
                    "throwUninitializedPropertyAccessException",
                    "(Ljava/lang/String;)V",
                );
                code.invokestatic(m, 1, 0);
                // `value_ty` already yields the JVM type of the operand (a reference here); the surviving
                // (non-null) value is on the stack at the branch target.
                let jt = self.value_ty(*operand);
                let st = self.verif_stack(jt);
                self.frame(lbl, st, code);
                code.bind(lbl);
            }
            IrExpr::Throw { operand } => {
                self.emit_value(*operand, code);
                code.athrow();
            }
            // `return v` in value position (`x ?: return v`): emit the return; control transfers away, so
            // (like `throw`) nothing is left for the surrounding merge.
            IrExpr::Return(ret_val) => match ret_val {
                Some(rv) => {
                    let ret = self.ret;
                    self.emit_value_as(*rv, &ret, code);
                    if !self.diverges(*rv) {
                        emit_return(self.ret, code);
                    }
                }
                None => code.ret_void(),
            },
            IrExpr::Vararg {
                array_type,
                elements,
            } => {
                let et = array_jvm_element(array_type);
                let elements = elements.clone();
                code.push_int(elements.len() as i32, self.cw);
                if et.is_jvm_scalar() {
                    code.newarray(prim_newarray_atype(et));
                } else {
                    let ci = self.cw.class_ref(&ref_internal(et));
                    code.anewarray(ci);
                }
                let (op, w) = array_store_op(et);
                // A boxed-primitive element array (`arrayOf(1,2,3)` → `Integer[]`): box each primitive
                // value before the `aastore` (mirrors `kotlin/Array.set`).
                let box_elem = boxed_prim_of(et);
                for (i, &el) in elements.iter().enumerate() {
                    code.dup();
                    code.push_int(i as i32, self.cw);
                    self.emit_value(el, code);
                    if let Some(p) = box_elem {
                        box_prim_free(self.cw, code, p);
                    }
                    code.array_store(op, w);
                }
            }
            IrExpr::NewArray { array_type, size } => {
                let et = array_jvm_element(array_type);
                self.emit_value(*size, code);
                if et.is_jvm_scalar() {
                    code.newarray(prim_newarray_atype(et));
                } else {
                    // Peel a nullable element's `?`: `Array<Int?>` = `Integer[]`, so the `anewarray` class
                    // is `java/lang/Integer` (the `?` only tells `Array.get`/`.set` to keep it boxed).
                    let ci = self.cw.class_ref(&ref_internal(et.non_null()));
                    code.anewarray(ci);
                }
            }
            IrExpr::Try {
                body,
                catches,
                finally,
                result,
            } => {
                let catches = catches.clone();
                let result = result.clone();
                self.emit_try(*body, &catches, *finally, &result, code);
            }
            IrExpr::NewExternal {
                internal,
                ctor_desc,
                args,
            } => {
                let owner = internal.render();
                let desc = ctor_desc.clone();
                let args = args.clone();
                // Arguments were coerced to the constructor's parameter types in lowering, so each
                // argument's `value_ty` is its parameter — the descriptor's argument-word count.
                let aw: i32 = args
                    .iter()
                    .map(|&a| slot_words(self.value_ty(a)) as i32)
                    .sum();
                self.emit_external_new(&owner, &desc, &args, aw, code);
            }
            IrExpr::NewCrossFile {
                internal,
                params,
                args,
            } => {
                let owner = internal.render();
                let param_tys = jvm_tys(params);
                let desc = method_descriptor(&param_tys, Ty::Unit);
                let aw: i32 = param_tys.iter().map(|t| slot_words(*t) as i32).sum();
                let args = args.clone();
                if args.iter().any(|&a| self.records_frame(a)) {
                    let temps = self.spill_to_temps(&args, code);
                    let ci = self.cw.class_ref(&owner);
                    code.new_obj(ci);
                    code.dup();
                    for &(slot, t, _) in &temps {
                        load(t, slot, code);
                    }
                    for &(_, _, key) in &temps {
                        self.slots.remove(&key);
                    }
                    let m = self.cw.methodref(&owner, "<init>", &desc);
                    code.invokespecial(m, aw, 0);
                } else {
                    let ci = self.cw.class_ref(&owner);
                    code.new_obj(ci);
                    code.dup();
                    for &a in &args {
                        self.emit_value(a, code);
                    }
                    let m = self.cw.methodref(&owner, "<init>", &desc);
                    code.invokespecial(m, aw, 0);
                }
            }
            IrExpr::RefNew { elem, init } => {
                let (cls, fdesc) = ref_class(elem);
                let ew = slot_words(ir_ty_to_jvm(elem)) as i32;
                // A branchy initializer can't run with `[holder, holder]` on the stack — spill it.
                if self.records_frame(*init) {
                    let temps = self.spill_to_temps(&[*init], code);
                    let ci = self.cw.class_ref(cls);
                    code.new_obj(ci);
                    code.dup();
                    let m = self.cw.methodref(cls, "<init>", "()V");
                    code.invokespecial(m, 0, 0);
                    code.dup();
                    for &(slot, t, _) in &temps {
                        load(t, slot, code);
                    }
                    for &(_, _, key) in &temps {
                        self.slots.remove(&key);
                    }
                } else {
                    let ci = self.cw.class_ref(cls);
                    code.new_obj(ci);
                    code.dup();
                    let m = self.cw.methodref(cls, "<init>", "()V");
                    code.invokespecial(m, 0, 0);
                    code.dup();
                    self.emit_value(*init, code);
                }
                let f = self.cw.fieldref(cls, "element", fdesc);
                code.putfield(f, ew);
            }
            IrExpr::RefGet { holder, elem } => {
                self.emit_value(*holder, code);
                let (cls, fdesc) = ref_class(elem);
                let f = self.cw.fieldref(cls, "element", fdesc);
                let ejvm = ir_ty_to_jvm(elem);
                code.getfield(f, slot_words(ejvm) as i32);
                // An `ObjectRef.element` is typed `Object`; narrow to the boxed value's reference type.
                if ejvm.is_reference() && ref_internal(ejvm) != "java/lang/Object" {
                    let cc = self.cw.class_ref(&ref_internal(ejvm));
                    code.checkcast(cc);
                }
            }
            IrExpr::RefSet {
                holder,
                elem,
                value,
            } => {
                self.emit_value(*holder, code);
                self.emit_value(*value, code);
                let (cls, fdesc) = ref_class(elem);
                let f = self.cw.fieldref(cls, "element", fdesc);
                code.putfield(f, slot_words(ir_ty_to_jvm(elem)) as i32);
            }
            IrExpr::InvokeFunction { func, args, ret } => {
                let n = args.len();
                if args.iter().any(|&a| self.records_frame(a)) {
                    // A branchy argument can't run with the function value on the stack — its merge
                    // frame would omit it. Evaluate the function + args into temps first (in order),
                    // then load and box.
                    let mut all = vec![*func];
                    all.extend(args.iter().copied());
                    let temps = self.spill_to_temps(&all, code);
                    load(temps[0].1, temps[0].0, code);
                    for &(slot, t, _) in &temps[1..] {
                        load(t, slot, code);
                        box_prim_free(self.cw, code, t);
                    }
                    for &(_, _, key) in &temps {
                        self.slots.remove(&key);
                    }
                } else {
                    self.emit_value(*func, code);
                    for &arg in args {
                        self.emit_value(arg, code);
                        let at = self.value_ty(arg);
                        box_prim_free(self.cw, code, at); // box a primitive arg to its wrapper (an Object)
                    }
                }
                let iface = format!("kotlin/jvm/functions/Function{n}");
                let m = self
                    .cw
                    .interface_methodref(&iface, "invoke", &sam_descriptor(n as u8));
                code.invokeinterface(m, n as i32, 1);
                // The interface returns `Object`; cast/unbox to the function's declared return type.
                let rt = ir_ty_to_jvm(ret);
                match rt {
                    Ty::Int
                    | Ty::Long
                    | Ty::Double
                    | Ty::Float
                    | Ty::Boolean
                    | Ty::Char
                    | Ty::Byte
                    | Ty::Short => unbox_prim(self.cw, code, rt),
                    Ty::Unit | Ty::Nothing => code.pop(),
                    Ty::String => {
                        let ci = self.cw.class_ref("java/lang/String");
                        code.checkcast(ci);
                    }
                    _ if rt.is_array() => {
                        let ci = self.cw.class_ref(&type_descriptor(rt));
                        code.checkcast(ci);
                    }
                    Ty::Obj(internal, _) => {
                        let ci = self.cw.class_ref(&internal.render());
                        code.checkcast(ci);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn emit_intrinsic(
        &mut self,
        fq: &str,
        recv: &Option<u32>,
        args: &[u32],
        code: &mut CodeBuilder,
    ) {
        match fq {
            // Static numeric helpers used by synthesized data-class equals/hashCode.
            "java/lang/Double.hashCode"
            | "java/lang/Long.hashCode"
            | "java/lang/Float.hashCode"
            | "java/lang/Boolean.hashCode"
            | "java/lang/Integer.hashCode"
            | "java/lang/Short.hashCode"
            | "java/lang/Byte.hashCode"
            | "java/lang/Character.hashCode"
            | "java/util/Objects.hashCode" => {
                self.emit_value(args[0], code);
                let (cls, d) = match fq {
                    "java/lang/Double.hashCode" => ("java/lang/Double", "(D)I"),
                    "java/lang/Long.hashCode" => ("java/lang/Long", "(J)I"),
                    "java/lang/Float.hashCode" => ("java/lang/Float", "(F)I"),
                    "java/lang/Boolean.hashCode" => ("java/lang/Boolean", "(Z)I"),
                    "java/lang/Integer.hashCode" => ("java/lang/Integer", "(I)I"),
                    "java/lang/Short.hashCode" => ("java/lang/Short", "(S)I"),
                    "java/lang/Byte.hashCode" => ("java/lang/Byte", "(B)I"),
                    "java/lang/Character.hashCode" => ("java/lang/Character", "(C)I"),
                    _ => ("java/util/Objects", "(Ljava/lang/Object;)I"),
                };
                let aw = slot_words(self.value_ty(args[0])) as i32;
                let m = self.cw.methodref(cls, "hashCode", d);
                code.invokestatic(m, aw, 1);
            }
            "java/lang/Double.compare" | "java/lang/Float.compare" => {
                self.emit_value(args[0], code);
                self.emit_value(args[1], code);
                let (cls, d, aw) = if fq == "java/lang/Double.compare" {
                    ("java/lang/Double", "(DD)I", 4)
                } else {
                    ("java/lang/Float", "(FF)I", 2)
                };
                let m = self.cw.methodref(cls, "compare", d);
                code.invokestatic(m, aw, 1);
            }
            "kotlin/String.plus" => {
                let recv = recv.unwrap();
                self.emit_string_plus(recv, args[0], code);
            }
            // `e.ordinal` / `e.name` on an enum value → `Enum.ordinal()I` / `Enum.name()String`.
            "java/lang/Enum.ordinal" => {
                self.emit_value(recv.unwrap(), code);
                let m = self.cw.methodref("java/lang/Enum", "ordinal", "()I");
                code.invokevirtual(m, 0, 1);
            }
            "java/lang/Enum.name" => {
                self.emit_value(recv.unwrap(), code);
                let m = self
                    .cw
                    .methodref("java/lang/Enum", "name", "()Ljava/lang/String;");
                code.invokevirtual(m, 0, 1);
            }
            // `s.length` → `String.length()`.
            "kotlin/String.length" => {
                self.emit_value(recv.unwrap(), code);
                let m = self.cw.methodref("java/lang/String", "length", "()I");
                code.invokevirtual(m, 0, 1);
            }
            "kotlin/String.hashCode" => {
                self.emit_value(recv.unwrap(), code);
                let m = self.cw.methodref("java/lang/String", "hashCode", "()I");
                code.invokevirtual(m, 0, 1);
            }
            // `s[i]` → `String.charAt(i)`.
            "kotlin/String.get" => {
                self.emit_value(recv.unwrap(), code);
                self.emit_value(args[0], code);
                let m = self.cw.methodref("java/lang/String", "charAt", "(I)C");
                code.invokevirtual(m, 1, 1);
            }
            // Array operations: the JVM platform realizes them with native array instructions; the
            // element type comes from the receiver's IR type (`kotlin/Array.get/set/size`) or from
            // the per-element constructor name (`kotlin/IntArray.<init>`).
            "kotlin/Array.get" => {
                let arr = recv.unwrap();
                let elem = self.array_elem(arr);
                self.emit_value(arr, code);
                self.emit_value(args[0], code);
                let (op, w) = array_load_op(elem);
                code.array_load(op, w);
                // A boxed primitive array (`Array<Int>` = `Integer[]`): `a[i]` is an unboxed `Int`, so
                // unbox the loaded wrapper. (`value_ty` for this call reports the same primitive.)
                if let Some(p) = boxed_prim_of(elem) {
                    unbox_prim(self.cw, code, p);
                }
            }
            "kotlin/Array.set" => {
                let arr = recv.unwrap();
                let elem = self.array_elem(arr);
                self.emit_value(arr, code);
                self.emit_value(args[0], code);
                self.emit_value(args[1], code);
                // Boxed primitive array: box the primitive value before the `aastore`.
                if let Some(p) = boxed_prim_of(elem) {
                    box_prim_free(self.cw, code, p);
                }
                let (op, w) = array_store_op(elem);
                code.array_store(op, w);
            }
            "kotlin/Array.size" => {
                self.emit_value(recv.unwrap(), code);
                code.arraylength();
            }
            _ if prim_array_elem_ty(fq).is_some() => {
                self.emit_value(args[0], code);
                let elem = prim_array_atype(fq);
                code.newarray(elem);
            }
            // `x.toString()` → `String.valueOf(x)` (the right primitive/Object overload).
            "kotlin/Any.toString" => {
                let r = recv.unwrap();
                let ty = self.value_ty(r);
                self.emit_value(r, code);
                let desc = match ty {
                    Ty::Int | Ty::Short | Ty::Byte => "(I)Ljava/lang/String;",
                    Ty::Long => "(J)Ljava/lang/String;",
                    Ty::Boolean => "(Z)Ljava/lang/String;",
                    Ty::Char => "(C)Ljava/lang/String;",
                    Ty::Double => "(D)Ljava/lang/String;",
                    Ty::Float => "(F)Ljava/lang/String;",
                    _ => "(Ljava/lang/Object;)Ljava/lang/String;",
                };
                let m = self.cw.methodref("java/lang/String", "valueOf", desc);
                code.invokestatic(m, slot_words(ty) as i32, 1);
            }
            "kotlin/Any.hashCode" => {
                let r = recv.unwrap();
                let ty = self.value_ty(r);
                self.emit_value(r, code);
                match ty {
                    // A primitive hashes via its wrapper's static `hashCode`.
                    Ty::Int | Ty::Short | Ty::Byte | Ty::Char => {}
                    Ty::Long => {
                        let m = self.cw.methodref("java/lang/Long", "hashCode", "(J)I");
                        code.invokestatic(m, 2, 1);
                    }
                    Ty::Boolean => {
                        let m = self.cw.methodref("java/lang/Boolean", "hashCode", "(Z)I");
                        code.invokestatic(m, 1, 1);
                    }
                    Ty::Double => {
                        let m = self.cw.methodref("java/lang/Double", "hashCode", "(D)I");
                        code.invokestatic(m, 2, 1);
                    }
                    Ty::Float => {
                        let m = self.cw.methodref("java/lang/Float", "hashCode", "(F)I");
                        code.invokestatic(m, 1, 1);
                    }
                    // Kotlin `Any?.hashCode()` is null-safe: a null reference hashes to 0. `Objects`
                    // preserves virtual hash dispatch for non-null references and handles null.
                    _ => {
                        let m = self.cw.methodref(
                            "java/util/Objects",
                            "hashCode",
                            "(Ljava/lang/Object;)I",
                        );
                        code.invokestatic(m, 1, 1);
                    }
                }
            }
            _ => {}
        }
    }

    fn append(&mut self, e: u32, code: &mut CodeBuilder) {
        let ty = self.value_ty(e);
        self.emit_value(e, code);
        self.append_top(ty, code);
    }

    fn emit_string_plus(&mut self, recv: u32, arg: u32, code: &mut CodeBuilder) {
        let sb = self.cw.class_ref("java/lang/StringBuilder");
        // A branchy operand (`when`/`try`) can't be emitted with the `StringBuilder` on the stack — its
        // merge frames would omit it. Spill such operands to temps first.
        if self.records_frame(recv) || self.records_frame(arg) {
            let temps = self.spill_to_temps(&[recv, arg], code);
            code.new_obj(sb);
            code.dup();
            let init = self
                .cw
                .methodref("java/lang/StringBuilder", "<init>", "()V");
            code.invokespecial(init, 0, 0);
            for &(slot, t, _) in &temps {
                load(t, slot, code);
                self.append_top(t, code);
            }
            for &(_, _, key) in &temps {
                self.slots.remove(&key);
            }
        } else {
            code.new_obj(sb);
            code.dup();
            let init = self
                .cw
                .methodref("java/lang/StringBuilder", "<init>", "()V");
            code.invokespecial(init, 0, 0);
            self.append(recv, code);
            self.append(arg, code);
        }
        let ts = self.cw.methodref(
            "java/lang/StringBuilder",
            "toString",
            "()Ljava/lang/String;",
        );
        code.invokevirtual(ts, 0, 1);
    }

    /// Append one string-template part to the `StringBuilder` beneath it. A single-character string
    /// constant appends as a `char` (kotlinc emits `append(C)` with the char constant, not `append(String)`).
    fn append_part(&mut self, p: u32, code: &mut CodeBuilder) {
        let single_char = if let IrExpr::Const(IrConst::String(s)) = self.ir.expr(p) {
            if s.chars().count() == 1 {
                s.chars().next()
            } else {
                None
            }
        } else {
            None
        };
        if let Some(c) = single_char {
            code.push_int(c as i32, self.cw);
            self.append_top(Ty::Char, code);
        } else {
            self.append(p, code);
        }
    }

    /// Append a value already on the operand stack (of type `ty`) to a `StringBuilder` beneath it.
    fn append_top(&mut self, ty: Ty, code: &mut CodeBuilder) {
        // A `String` value reaches here either as `Ty::String` or as `Ty::Obj("java/lang/String")` —
        // the latter when its type was parsed from a method-return descriptor (e.g. a classpath call
        // or the data-class `Arrays.toString(field)` wrapper). Both must pick the `append(String)`
        // overload kotlinc uses, not the less-specific `append(Object)`.
        let is_string = matches!(ty, Ty::String)
            || matches!(ty, Ty::Obj(n, _) if n == "java/lang/String" || n == "kotlin/String");
        let desc = match ty {
            _ if is_string => "(Ljava/lang/String;)Ljava/lang/StringBuilder;",
            Ty::Int | Ty::Short | Ty::Byte => "(I)Ljava/lang/StringBuilder;",
            Ty::Long => "(J)Ljava/lang/StringBuilder;",
            Ty::Boolean => "(Z)Ljava/lang/StringBuilder;",
            Ty::Char => "(C)Ljava/lang/StringBuilder;",
            Ty::Double => "(D)Ljava/lang/StringBuilder;",
            Ty::Float => "(F)Ljava/lang/StringBuilder;",
            _ => "(Ljava/lang/Object;)Ljava/lang/StringBuilder;",
        };
        let m = self.cw.methodref("java/lang/StringBuilder", "append", desc);
        code.invokevirtual(m, slot_words(ty) as i32, 1);
    }

    /// Whether emitting `e` as a value records a StackMapTable frame (a primitive comparison, a
    /// `when`, or a `while` — anywhere in its subtree). Such an expression can't be emitted while
    /// other operands sit on the stack (its merge frames would omit them); callers spill first.
    /// Whether an operand held on the stack BELOW `e` must be spilled to a temp instead
    /// (`pending_stack` must not be used across it): a `try` in the subtree — the JVM clears the
    /// operand stack on handler entry, so a held value would be lost (and its handler frame mistyped) —
    /// or an inlinable call, whose splice expects the stack baseline its frames were recorded at.
    /// Conservative: the spill path is always correct, only byte-parity with kotlinc is deferred.
    fn must_spill_across(&self, e: u32) -> bool {
        match self.ir.expr(e) {
            IrExpr::Try { .. } => true,
            IrExpr::Call {
                callee: Callee::Static { inline, .. },
                ..
            } if inline.can_inline() => true,
            _ => {
                let mut spill = false;
                crate::ir::for_each_child(&self.ir.exprs, e, &mut |c| {
                    spill = spill || self.must_spill_across(c);
                });
                spill
            }
        }
    }

    fn records_frame(&self, e: u32) -> bool {
        use IrBinOp::*;
        match self.ir.expr(e) {
            IrExpr::When { .. } | IrExpr::While { .. } | IrExpr::Try { .. } => true,
            // The multi-part `StringConcat` itself spills branchy parts internally, so as a whole it
            // leaves only its `String` result — but a parent operand sequence still must treat it as
            // frame-recording if any part does (it builds the StringBuilder mid-stack otherwise).
            IrExpr::StringConcat(parts) => parts.iter().any(|&p| self.records_frame(p)),
            IrExpr::PrimitiveBinOp { op, lhs, rhs } => {
                (matches!(op, Lt | Le | Gt | Ge | Eq | Ne) && self.value_ty(*lhs).is_jvm_scalar())
                    // `===`/`!==` always emits a branch+merge frame — the `if_acmp*` path (references)
                    // and the value-compare path it remaps to for primitives both do.
                    || matches!(op, RefEq | RefNe)
                    // `x == null`/`x != null` emits an `ifnull`/`ifnonnull` branch+merge frame.
                    || (matches!(op, Eq | Ne)
                        && (matches!(self.ir.expr(*lhs), IrExpr::Const(IrConst::Null))
                            || matches!(self.ir.expr(*rhs), IrExpr::Const(IrConst::Null))))
                    || self.records_frame(*lhs) || self.records_frame(*rhs)
            }
            IrExpr::Call {
                callee,
                dispatch_receiver,
                args,
            } => {
                // An inline call whose SPLICED body records StackMapTable frames — a branchy lambda body,
                // or a branchy host body (a loop HOF like `map`/`filter`, or an `@InlineOnly` `require`/
                // `check`) — records frames at THIS position. So a parent operand sequence must spill the
                // earlier operands to temps (keeping the splice at an empty baseline), exactly as for
                // `when`/`try`. Without this, an inline HOF used as a non-first operand
                // (`sb.append(xs.map { … }))`) would splice at a non-empty baseline and bail to a real call.
                let splice_records = match callee {
                    Callee::Static {
                        owner,
                        name,
                        descriptor,
                        inline,
                    } if inline.can_inline() => {
                        args.iter().any(|&a| {
                            matches!(self.ir.expr(a),
                                IrExpr::Lambda { inline_body: Some(b), .. } if self.records_frame(*b))
                        }) || self
                            .bodies
                            .body(&owner.render(), name, descriptor)
                            .and_then(|b| crate::jvm::inline::disassemble(&b.code))
                            .is_some_and(|ins| {
                                ins.iter()
                                    .any(|i| !matches!(i, crate::jvm::inline::Insn::Plain { .. }))
                            })
                    }
                    _ => false,
                };
                splice_records
                    || dispatch_receiver.map_or(false, |r| self.records_frame(r))
                    || args.iter().any(|&a| self.records_frame(a))
            }
            IrExpr::MethodCall { receiver, args, .. } => {
                self.records_frame(*receiver)
                    || args
                        .iter()
                        .any(|a| a.map_or(false, |x| self.records_frame(x)))
            }
            IrExpr::New { args, .. } => args.iter().any(|&a| self.records_frame(a)),
            IrExpr::GetField { receiver, .. } => self.records_frame(*receiver),
            IrExpr::SetField {
                receiver, value, ..
            } => self.records_frame(*receiver) || self.records_frame(*value),
            IrExpr::SetValue { value, .. } | IrExpr::SetStatic { value, .. } => {
                self.records_frame(*value)
            }
            IrExpr::TypeOp { arg, .. } | IrExpr::EnumValueOf { arg, .. } => {
                self.records_frame(*arg)
            }
            IrExpr::NotNullAssert { operand } => self.records_frame(*operand),
            // A `lateinit` read emits an `ifnonnull` merge frame, so a parent must spill other operands
            // first (else the frame at the join would omit them).
            IrExpr::LateinitCheck { .. } => true,
            IrExpr::NewExternal { args, .. } => args.iter().any(|&a| self.records_frame(a)),
            IrExpr::NewCrossFile { args, .. } => args.iter().any(|&a| self.records_frame(a)),
            IrExpr::RefGet { holder, .. } => self.records_frame(*holder),
            IrExpr::RefSet { holder, value, .. } => {
                self.records_frame(*holder) || self.records_frame(*value)
            }
            IrExpr::RefNew { init, .. } => self.records_frame(*init),
            IrExpr::Throw { operand } => self.records_frame(*operand),
            IrExpr::Vararg { elements, .. } => elements.iter().any(|&a| self.records_frame(a)),
            IrExpr::NewArray { size, .. } => self.records_frame(*size),
            IrExpr::Return(v) => v.map_or(false, |x| self.records_frame(x)),
            IrExpr::Variable { init, .. } => init.map_or(false, |i| self.records_frame(i)),
            IrExpr::Block { stmts, value } => {
                stmts.iter().any(|&s| self.records_frame(s))
                    || value.map_or(false, |v| self.records_frame(v))
            }
            _ => false, // Const, GetValue, GetStatic, EnumEntry, EnumValues — no frames
        }
    }

    /// Push `ops` onto the stack in order. If any op after the first records a frame (so an earlier
    /// op would be live on the stack across that frame), evaluate all ops into temps first, then load
    /// them — keeping the stack empty while each frame-recording op runs.
    fn emit_operands(&mut self, ops: &[u32], code: &mut CodeBuilder) {
        if ops.iter().skip(1).any(|&o| self.records_frame(o)) {
            let temps = self.spill_to_temps(ops, code);
            for &(slot, t, _) in &temps {
                load(t, slot, code);
            }
            for &(_, _, key) in &temps {
                self.slots.remove(&key);
            }
        } else {
            for &o in ops {
                self.emit_value(o, code);
            }
        }
    }

    fn emit_virtual_operands(
        &mut self,
        owner: &str,
        recv: u32,
        args: &[u32],
        code: &mut CodeBuilder,
    ) {
        let recv_ty = self.value_ty(recv);
        let box_recv_as = wrapper_owner_primitive(owner).filter(|_| recv_ty.is_jvm_scalar());
        // A member call on a value whose static type is `owner` but whose ERASED physical type is a top
        // (`Object`) needs the `checkcast owner` kotlinc inserts before the dispatch verifies.
        let narrow_recv = |e: &mut Self, src: Ty, code: &mut CodeBuilder| {
            if box_recv_as.is_none() {
                e.narrow_on_stack(src, &Ty::obj(owner), code);
            }
        };
        if args.iter().any(|&o| self.records_frame(o)) {
            let mut ops = vec![recv];
            ops.extend(args.iter().copied());
            let temps = self.spill_to_temps(&ops, code);
            for (i, &(slot, t, _)) in temps.iter().enumerate() {
                load(t, slot, code);
                if i == 0 {
                    if let Some(box_ty) = box_recv_as {
                        box_prim_free(self.cw, code, box_ty);
                    } else {
                        narrow_recv(self, t, code);
                    }
                }
            }
            for &(_, _, key) in &temps {
                self.slots.remove(&key);
            }
        } else {
            self.emit_value(recv, code);
            if let Some(box_ty) = box_recv_as {
                box_prim_free(self.cw, code, box_ty);
            } else {
                narrow_recv(self, recv_ty, code);
            }
            for &arg in args {
                self.emit_value(arg, code);
            }
        }
    }

    fn emit_external_new(
        &mut self,
        owner: &str,
        desc: &str,
        args: &[u32],
        aw: i32,
        code: &mut CodeBuilder,
    ) {
        if args.iter().any(|&a| self.records_frame(a)) {
            // A branchy argument can't run with `[new, dup]` on the stack (its merge frame would omit
            // them) — evaluate args into temps first, then build.
            let temps = self.spill_to_temps(args, code);
            let ci = self.cw.class_ref(owner);
            code.new_obj(ci);
            code.dup();
            for &(slot, t, _) in &temps {
                load(t, slot, code);
            }
            for &(_, _, key) in &temps {
                self.slots.remove(&key);
            }
            let m = self.cw.methodref(owner, "<init>", desc);
            code.invokespecial(m, aw, 0);
        } else {
            let ci = self.cw.class_ref(owner);
            code.new_obj(ci);
            code.dup();
            for &a in args {
                self.emit_value(a, code);
            }
            let m = self.cw.methodref(owner, "<init>", desc);
            code.invokespecial(m, aw, 0);
        }
    }

    fn emit_external_new_coerced(
        &mut self,
        owner: &str,
        desc: &str,
        args: &[u32],
        aw: i32,
        target: Ty,
        code: &mut CodeBuilder,
    ) {
        let emit_arg = |this: &mut Self, arg: u32, code: &mut CodeBuilder| {
            let from = this.value_ty(arg);
            this.emit_value(arg, code);
            emit_num_conv(from, target, code);
        };
        if args.iter().any(|&a| self.records_frame(a)) {
            let temps = self.spill_to_temps(args, code);
            let ci = self.cw.class_ref(owner);
            code.new_obj(ci);
            code.dup();
            for &(slot, t, _) in &temps {
                load(t, slot, code);
                emit_num_conv(t, target, code);
            }
            for &(_, _, key) in &temps {
                self.slots.remove(&key);
            }
            let m = self.cw.methodref(owner, "<init>", desc);
            code.invokespecial(m, aw, 0);
        } else {
            let ci = self.cw.class_ref(owner);
            code.new_obj(ci);
            code.dup();
            for &arg in args {
                emit_arg(self, arg, code);
            }
            let m = self.cw.methodref(owner, "<init>", desc);
            code.invokespecial(m, aw, 0);
        }
    }

    fn emit_primitive_inc_dec_virtual(
        &mut self,
        owner: &str,
        name: &str,
        descriptor: &str,
        recv: u32,
        args: &[u32],
        code: &mut CodeBuilder,
    ) -> bool {
        if !args.is_empty() || !matches!(name, "inc" | "dec") {
            return false;
        }
        let Some(owner_prim) = wrapper_owner_primitive(owner) else {
            return false;
        };
        let recv_ty = self.value_ty(recv);
        let source_prim = if recv_ty.is_jvm_scalar() {
            recv_ty
        } else {
            owner_prim
        };
        let ret = ty_from_descriptor_ret(descriptor);
        self.emit_value(recv, code);
        if !recv_ty.is_jvm_scalar() {
            unbox_prim(self.cw, code, owner_prim);
        }
        match owner_prim {
            Ty::Long => {
                code.push_long(1, self.cw);
                if name == "inc" {
                    code.ladd();
                } else {
                    code.lsub();
                }
            }
            Ty::Float => {
                code.push_float(1.0, self.cw);
                if name == "inc" {
                    code.fadd();
                } else {
                    code.fsub();
                }
            }
            Ty::Double => {
                code.push_double(1.0, self.cw);
                if name == "inc" {
                    code.dadd();
                } else {
                    code.dsub();
                }
            }
            _ => {
                code.push_int(1, self.cw);
                if name == "inc" {
                    code.iadd();
                } else {
                    code.isub();
                }
            }
        }
        let arithmetic_ty = owner_prim.int_arithmetic_repr();
        emit_num_conv(arithmetic_ty, source_prim, code);
        emit_num_conv(source_prim, ret, code);
        true
    }

    fn emit_unsigned_compare_to_virtual(
        &mut self,
        owner: &str,
        name: &str,
        recv: u32,
        args: &[u32],
        code: &mut CodeBuilder,
    ) -> bool {
        if name != "compareTo" || args.len() != 1 {
            return false;
        }
        let (logical, jdk_owner, prim_desc, repr) = match owner {
            "kotlin/UInt" => (Ty::UInt, "java/lang/Integer", "I", Ty::Int),
            "kotlin/ULong" => (Ty::ULong, "java/lang/Long", "J", Ty::Long),
            _ => return false,
        };
        self.emit_unsigned_operand(recv, logical, repr, code);
        self.emit_unsigned_operand(args[0], logical, repr, code);
        let m = self.cw.methodref(
            jdk_owner,
            "compareUnsigned",
            &format!("({prim_desc}{prim_desc})I"),
        );
        code.invokestatic(m, (slot_words(repr) * 2) as i32, 1);
        true
    }

    fn emit_unsigned_operand(&mut self, expr: u32, logical: Ty, repr: Ty, code: &mut CodeBuilder) {
        let from = self.value_ty(expr);
        self.emit_value(expr, code);
        if from.is_reference() {
            let (owner, desc) = match logical {
                Ty::UInt => ("kotlin/UInt", "()I"),
                Ty::ULong => ("kotlin/ULong", "()J"),
                _ => return,
            };
            let cls = self.cw.class_ref(owner);
            code.checkcast(cls);
            let m = self.cw.methodref(owner, "unbox-impl", desc);
            code.invokevirtual(m, 0, slot_words(repr) as i32);
        } else {
            emit_num_conv(from, repr, code);
        }
    }

    /// Evaluate each of `ops` into a fresh temp slot, in order. Each temp is registered in `self.slots`
    /// (so a *later* op's frames see the earlier temps as live, not `Top`); the caller loads them and
    /// then removes them (they're dead once loaded). Returns `(slot, ty, slots-key)` per op.
    fn spill_to_temps(&mut self, ops: &[u32], code: &mut CodeBuilder) -> Vec<(u16, Ty, u32)> {
        let mut temps = Vec::new();
        for &o in ops {
            self.emit_value(o, code);
            let t = self.value_ty(o);
            let slot = self.next_slot;
            self.next_slot += slot_words(t);
            store(t, slot, code);
            let key = 2_000_000 + slot as u32;
            self.slots.insert(key, (slot, t));
            temps.push((slot, t, key));
        }
        temps
    }

    fn emit_binop(&mut self, op: IrBinOp, lhs: u32, rhs: u32, code: &mut CodeBuilder) {
        use IrBinOp::*;
        let lt = self.value_ty(lhs);
        match op {
            Add | Sub | Mul | Div | Rem => {
                // A branchy RHS (`result*31 + <nullable-field hashCode ternary>`): keep the numeric LHS on
                // the operand stack across the RHS's branch — matching kotlinc — by typing it into the
                // RHS's stack-map frames via `pending_stack`, instead of spilling it to a temp. The LHS of
                // arithmetic is always a numeric scalar, so the pending verif type interns no `Class`. A
                // non-branchy RHS (or a branchy LHS) keeps the ordinary `emit_operands` path (spill only if
                // needed) — bytecode unchanged for the common case. NOT applicable when the RHS can enter
                // an exception handler or splice inline bytecode (`must_spill_across`): a handler CLEARS
                // the operand stack (the held LHS would be lost and its handler frame mistyped), and a
                // splice expects its recorded baseline.
                if self.records_frame(rhs)
                    && !self.records_frame(lhs)
                    && !self.must_spill_across(rhs)
                {
                    self.emit_value(lhs, code);
                    let lv = self.verif_single(lt);
                    self.pending_stack.push(lv);
                    self.emit_value(rhs, code);
                    self.pending_stack.pop();
                } else {
                    self.emit_operands(&[lhs, rhs], code);
                }
                match lt {
                    Ty::Long => match op {
                        Add => code.ladd(),
                        Sub => code.lsub(),
                        Mul => code.lmul(),
                        Div => code.ldiv(),
                        Rem => code.lrem(),
                        _ => unreachable!(),
                    },
                    Ty::Double => match op {
                        Add => code.dadd(),
                        Sub => code.dsub(),
                        Mul => code.dmul(),
                        Div => code.ddiv(),
                        Rem => code.drem(),
                        _ => unreachable!(),
                    },
                    Ty::Float => match op {
                        Add => code.fadd(),
                        Sub => code.fsub(),
                        Mul => code.fmul(),
                        Div => code.fdiv(),
                        Rem => code.frem(),
                        _ => unreachable!(),
                    },
                    _ => match op {
                        Add => code.iadd(),
                        Sub => code.isub(),
                        Mul => code.imul(),
                        Div => code.idiv(),
                        Rem => code.irem(),
                        _ => unreachable!(),
                    },
                }
            }
            And | Or => {
                // Evaluate lhs, hold it in a temp while rhs is emitted (rhs may record frames that
                // must see the temp as live), then combine. The temp is dead afterwards, so remove it
                // from the slot map so it doesn't leak into later merge frames (next_slot stays
                // monotonic — no reuse). Without this, a `false`/`else` path that never assigned the
                // temp reaches a merge whose frame claims it's defined → VerifyError.
                self.emit_value(lhs, code);
                let tmp = self.next_slot;
                self.next_slot += 1;
                let key = 1_000_000 + tmp as u32;
                self.slots.insert(key, (tmp, Ty::Boolean));
                code.istore(tmp);
                self.emit_value(rhs, code);
                code.iload(tmp);
                if op == And {
                    code.iand()
                } else {
                    code.ior()
                }
                self.slots.remove(&key);
            }
            BitAnd | BitOr | BitXor => {
                self.emit_operands(&[lhs, rhs], code);
                match lt {
                    Ty::Long => match op {
                        BitAnd => code.land(),
                        BitOr => code.lor(),
                        BitXor => code.lxor(),
                        _ => unreachable!(),
                    },
                    _ => match op {
                        BitAnd => code.iand(),
                        BitOr => code.ior(),
                        BitXor => code.ixor(),
                        _ => unreachable!(),
                    },
                }
            }
            Shl | Shr | Ushr => {
                self.emit_operands(&[lhs, rhs], code); // shift amount is an `Int`
                match lt {
                    Ty::Long => match op {
                        Shl => code.lshl(),
                        Shr => code.lshr(),
                        Ushr => code.lushr(),
                        _ => unreachable!(),
                    },
                    _ => match op {
                        Shl => code.ishl(),
                        Shr => code.ishr(),
                        Ushr => code.iushr(),
                        _ => unreachable!(),
                    },
                }
            }
            Lt | Le | Gt | Ge | Eq | Ne | RefEq | RefNe => self.emit_compare(op, lhs, rhs, code),
        }
    }

    fn emit_compare(&mut self, op: IrBinOp, lhs: u32, rhs: u32, code: &mut CodeBuilder) {
        let lt = self.value_ty(lhs);
        // Referential identity (`===`/`!==`) on *reference* operands: compare the two object refs
        // directly with `if_acmp*` (never the structural `Intrinsics.areEqual` the `Eq`/`Ne` reference
        // path uses below). On *primitive* operands Kotlin's `===` is just value `==`, so those fall
        // through to the ordinary numeric comparison after remapping to `Eq`/`Ne`.
        if matches!(op, IrBinOp::RefEq | IrBinOp::RefNe)
            && lt.is_reference()
            && self.value_ty(rhs).is_reference()
        {
            self.emit_operands(&[lhs, rhs], code);
            let t = code.new_label();
            let end = code.new_label();
            self.frame(t, vec![], code);
            if op == IrBinOp::RefEq {
                code.if_acmpeq(t)
            } else {
                code.if_acmpne(t)
            }
            code.push_int(0, self.cw);
            self.frame(end, vec![VerifType::Integer], code);
            code.goto(end);
            code.bind(t);
            code.push_int(1, self.cw);
            code.bind(end);
            return;
        }
        let op = match op {
            IrBinOp::RefEq => IrBinOp::Eq,
            IrBinOp::RefNe => IrBinOp::Ne,
            o => o,
        };
        // `x == null` / `x != null`: compare against null directly with `ifnull`/`ifnonnull` (kotlinc's
        // bytecode), regardless of the operand's static value type. `Intrinsics.areEqual` below is only
        // for two reference operands neither of which is the `null` literal — and a plain `if_icmp*` on
        // a reference (what the numeric path would emit) is only accepted by the verifier when no
        // stackmap frame pins the operand types, so it must not be relied on.
        let lhs_null = matches!(self.ir.expr(lhs), IrExpr::Const(IrConst::Null));
        let rhs_null = matches!(self.ir.expr(rhs), IrExpr::Const(IrConst::Null));
        if matches!(op, IrBinOp::Eq | IrBinOp::Ne) && (lhs_null || rhs_null) {
            let operand = if lhs_null { rhs } else { lhs };
            self.emit_value(operand, code);
            let t = code.new_label();
            let end = code.new_label();
            self.frame(t, vec![], code);
            if op == IrBinOp::Eq {
                code.ifnull(t)
            } else {
                code.ifnonnull(t)
            }
            code.push_int(0, self.cw);
            self.frame(end, vec![VerifType::Integer], code);
            code.goto(end);
            code.bind(t);
            code.push_int(1, self.cw);
            code.bind(end);
            return;
        }
        // Kotlin `==`/`!=` on reference operands is structural (`a?.equals(b)`), realized by the
        // null-safe `kotlin/jvm/internal/Intrinsics.areEqual` — the exact helper kotlinc's JVM backend
        // emits (`intrinsics/Equals.kt`), so the bytecode matches. Primitives keep the
        // `if_icmp*`/3-way-compare path below.
        if matches!(op, IrBinOp::Eq | IrBinOp::Ne)
            && lt.is_reference()
            && self.value_ty(rhs).is_reference()
        {
            // Spill if rhs is branchy (`x == when{…}`) so lhs isn't live across its merge frames.
            self.emit_operands(&[lhs, rhs], code);
            let m = self.cw.methodref(
                "kotlin/jvm/internal/Intrinsics",
                "areEqual",
                "(Ljava/lang/Object;Ljava/lang/Object;)Z",
            );
            code.invokestatic(m, 2, 1);
            if op == IrBinOp::Ne {
                code.push_int(1, self.cw);
                code.ixor();
            }
            return;
        }
        self.emit_operands(&[lhs, rhs], code);
        // Long/Double/Float compare to a 3-way result, then test against 0 with `if_icmp*`. For float
        // types `>`/`>=` use the `*l` variant (NaN → -1) and `<`/`<=` the `*g` variant (NaN → +1), so a
        // NaN operand makes the comparison false either way — matching kotlinc.
        let nan_l = matches!(op, IrBinOp::Gt | IrBinOp::Ge);
        match lt {
            Ty::Long => {
                code.lcmp();
                code.push_int(0, self.cw);
            }
            Ty::Double => {
                if nan_l {
                    code.dcmpl();
                } else {
                    code.dcmpg();
                }
                code.push_int(0, self.cw);
            }
            Ty::Float => {
                if nan_l {
                    code.fcmpl();
                } else {
                    code.fcmpg();
                }
                code.push_int(0, self.cw);
            }
            _ => {}
        }
        let t = code.new_label();
        let end = code.new_label();
        self.frame(t, vec![], code);
        match op {
            IrBinOp::Lt => code.if_icmplt(t),
            IrBinOp::Le => code.if_icmple(t),
            IrBinOp::Gt => code.if_icmpgt(t),
            IrBinOp::Ge => code.if_icmpge(t),
            IrBinOp::Eq => code.if_icmpeq(t),
            IrBinOp::Ne => code.if_icmpne(t),
            _ => unreachable!(),
        }
        // The `if_icmp*` popped both operands — this is the height on BOTH merge paths (the `t`
        // branch and the fall-through). The 0/1 booleans below each leave exactly one value, so the
        // tracker must be reset to this height at `bind(t)`; otherwise the linear counter carries the
        // fall-through's `push 0` past the `goto`, drifting `cur_stack` +1 (harmless for max_stack, but
        // it makes `stack_height()` over-report, which the branchy-inline baseline check relies on).
        let merged = code.stack_height().max(0) as u16;
        code.push_int(0, self.cw);
        self.frame(end, vec![VerifType::Integer], code);
        code.goto(end);
        code.bind(t);
        code.set_stack(merged);
        code.push_int(1, self.cw);
        code.bind(end);
    }

    /// Emit a conditional jump to `target`, taken exactly when `cond` evaluates to `jump_when_true`.
    /// When `cond` is a primitive/reference comparison it is FUSED into the branch (`if_icmpge`,
    /// `ifnull`, `if_acmpeq`, `lcmp;ifge`, …) instead of materializing a 0/1 boolean and testing it
    /// with `ifeq`/`ifne` — the bytecode kotlinc emits for every `if`/`while`/`for` over a comparison.
    fn emit_cond_branch(
        &mut self,
        cond: u32,
        target: Label,
        jump_when_true: bool,
        code: &mut CodeBuilder,
    ) {
        // A constant condition folds: `while (true)` (a `Boolean(true)` pre-test, jump-out-when-false)
        // emits NO branch — a spurious `ifeq end` to the method end leaves a branch target with no
        // stack-map frame. An always-taken branch becomes an unconditional `goto`.
        if let IrExpr::Const(IrConst::Boolean(b)) = *self.ir.expr(cond) {
            // Frame the target regardless (callers — `when`/loop emission — rely on the branch target
            // having a stack-map frame), but only emit the jump when the constant actually takes it.
            self.frame(target, vec![], code);
            if b == jump_when_true {
                code.goto(target);
            }
            return;
        }
        if let IrExpr::PrimitiveBinOp { op, lhs, rhs } = *self.ir.expr(cond) {
            use IrBinOp::*;
            if matches!(op, Lt | Le | Gt | Ge | Eq | Ne | RefEq | RefNe) {
                self.emit_compare_branch(op, lhs, rhs, target, jump_when_true, code);
                return;
            }
        }
        // Fuse `x is T` / `x !is T` (a reference target) into `instanceof; if{ne,eq}` — no 0/1 boolean is
        // materialized (kotlinc's shape, e.g. a data class `equals`' `instanceof; ifne <ok>`).
        let inst_fuse = if let IrExpr::TypeOp {
            op: to,
            arg,
            type_operand,
        } = self.ir.expr(cond)
        {
            if matches!(to, IrTypeOp::InstanceOf | IrTypeOp::NotInstanceOf) {
                let jvm_ty = ir_ty_to_jvm(type_operand);
                (!jvm_ty.is_jvm_scalar()).then(|| (*to, *arg, ref_internal(jvm_ty)))
            } else {
                None
            }
        } else {
            None
        };
        if let Some((to, arg, internal)) = inst_fuse {
            self.emit_value(arg, code);
            let ci = self.cw.class_ref(&internal);
            code.instance_of(ci);
            self.frame(target, vec![], code);
            // Stack holds 1 iff `arg instanceof T`. The condition is true on `instanceof` for `InstanceOf`
            // and on `!instanceof` for `NotInstanceOf`; jump when the condition equals `jump_when_true`.
            let jump_on_instance = if matches!(to, IrTypeOp::InstanceOf) {
                jump_when_true
            } else {
                !jump_when_true
            };
            if jump_on_instance {
                code.ifne(target);
            } else {
                code.ifeq(target);
            }
            return;
        }
        self.emit_value(cond, code);
        self.frame(target, vec![], code);
        if jump_when_true {
            code.ifne(target);
        } else {
            code.ifeq(target);
        }
    }

    /// Emit the comparison `lhs <op> rhs` directly as a single conditional jump to `target`, taken when
    /// the comparison's result equals `jt` — no 0/1 boolean is materialized. Mirrors `emit_compare`'s
    /// operand/3-way/null/ref handling but ends in one fused branch with the right polarity.
    fn emit_compare_branch(
        &mut self,
        op: IrBinOp,
        lhs: u32,
        rhs: u32,
        target: Label,
        jt: bool,
        code: &mut CodeBuilder,
    ) {
        use IrBinOp::*;
        let lt = self.value_ty(lhs);
        // `x == null` / `x != null` / `x === null` / `x !== null` → single-operand `ifnull`/`ifnonnull`
        // (kotlinc's form), NOT `aconst_null; if_acmp*`. Computed up front so the referential-identity
        // path below doesn't claim a null comparison (a `null` literal's type is a reference).
        let lhs_null = matches!(self.ir.expr(lhs), IrExpr::Const(IrConst::Null));
        let rhs_null = matches!(self.ir.expr(rhs), IrExpr::Const(IrConst::Null));
        // Referential identity (`===`/`!==`) on two non-null references → `if_acmpeq`/`if_acmpne`.
        if matches!(op, RefEq | RefNe)
            && lt.is_reference()
            && self.value_ty(rhs).is_reference()
            && !lhs_null
            && !rhs_null
        {
            self.emit_operands(&[lhs, rhs], code);
            self.frame(target, vec![], code);
            if (op == RefEq) == jt {
                code.if_acmpeq(target);
            } else {
                code.if_acmpne(target);
            }
            return;
        }
        let op = match op {
            RefEq => Eq,
            RefNe => Ne,
            o => o,
        };
        if matches!(op, Eq | Ne) && (lhs_null || rhs_null) {
            let operand = if lhs_null { rhs } else { lhs };
            self.emit_value(operand, code);
            self.frame(target, vec![], code);
            if (op == Eq) == jt {
                code.ifnull(target);
            } else {
                code.ifnonnull(target);
            }
            return;
        }
        // Reference structural `==`/`!=` → `Intrinsics.areEqual` then test the `Z` result.
        if matches!(op, Eq | Ne) && lt.is_reference() && self.value_ty(rhs).is_reference() {
            self.emit_operands(&[lhs, rhs], code);
            let m = self.cw.methodref(
                "kotlin/jvm/internal/Intrinsics",
                "areEqual",
                "(Ljava/lang/Object;Ljava/lang/Object;)Z",
            );
            code.invokestatic(m, 2, 1);
            self.frame(target, vec![], code);
            if (op == Eq) == jt {
                code.ifne(target); // areEqual true ⇒ equal
            } else {
                code.ifeq(target);
            }
            return;
        }
        // Numeric. A comparison against the integer literal `0` uses the single-operand compare-to-zero
        // branch (`ifeq`/`iflt`/… — kotlinc's form), saving the `iconst_0`. Only the int category; the
        // others compare 3-way through `lcmp`/`dcmp*`/`fcmp*`, which already tests the result vs 0.
        let int_cat = !matches!(lt, Ty::Long | Ty::Double | Ty::Float);
        let zero = |e: u32| matches!(self.ir.expr(e), IrExpr::Const(IrConst::Int(0)));
        if int_cat && zero(rhs) {
            self.emit_value(lhs, code);
            self.frame(target, vec![], code);
            self.cmp0_branch(op, jt, target, code);
            return;
        }
        if int_cat && zero(lhs) {
            self.emit_value(rhs, code);
            self.frame(target, vec![], code);
            self.cmp0_branch(swap_cmp(op), jt, target, code);
            return;
        }
        // int-category fuses to `if_icmp*`; Long/Double/Float → 3-way compare then single-operand `if*`.
        self.emit_operands(&[lhs, rhs], code);
        // `>`/`>=` use the `*l` float-compare variant, `<`/`<=` the `*g` — so NaN yields false (kotlinc).
        let nan_l = matches!(op, Gt | Ge);
        match lt {
            Ty::Long => code.lcmp(),
            Ty::Double => {
                if nan_l {
                    code.dcmpl()
                } else {
                    code.dcmpg()
                }
            }
            Ty::Float => {
                if nan_l {
                    code.fcmpl()
                } else {
                    code.fcmpg()
                }
            }
            _ => {}
        }
        self.frame(target, vec![], code);
        if !int_cat {
            self.cmp0_branch(op, jt, target, code);
        } else {
            match (op, jt) {
                (Lt, true) => code.if_icmplt(target),
                (Lt, false) => code.if_icmpge(target),
                (Le, true) => code.if_icmple(target),
                (Le, false) => code.if_icmpgt(target),
                (Gt, true) => code.if_icmpgt(target),
                (Gt, false) => code.if_icmple(target),
                (Ge, true) => code.if_icmpge(target),
                (Ge, false) => code.if_icmplt(target),
                (Eq, true) => code.if_icmpeq(target),
                (Eq, false) => code.if_icmpne(target),
                (Ne, true) => code.if_icmpne(target),
                (Ne, false) => code.if_icmpeq(target),
                _ => unreachable!(),
            }
        }
    }

    /// A single-operand compare-to-zero branch (`ifeq`/`ifne`/`iflt`/`ifle`/`ifgt`/`ifge`) to `target`,
    /// taken when `(value <op> 0) == jt`. Used for `x <op> 0` and for the 3-way `lcmp`/`dcmp*`/`fcmp*`
    /// result tested against 0.
    fn cmp0_branch(&self, op: IrBinOp, jt: bool, target: Label, code: &mut CodeBuilder) {
        use IrBinOp::*;
        match (op, jt) {
            (Lt, true) => code.iflt(target),
            (Lt, false) => code.ifge(target),
            (Le, true) => code.ifle(target),
            (Le, false) => code.ifgt(target),
            (Gt, true) => code.ifgt(target),
            (Gt, false) => code.ifle(target),
            (Ge, true) => code.ifge(target),
            (Ge, false) => code.iflt(target),
            (Eq, true) => code.ifeq(target),
            (Eq, false) => code.ifne(target),
            (Ne, true) => code.ifne(target),
            (Ne, false) => code.ifeq(target),
            _ => unreachable!(),
        }
    }

    fn emit_when(&mut self, branches: &[(Option<u32>, u32)], code: &mut CodeBuilder) {
        let end = code.new_label();
        // The operand-stack height BEFORE any branch (the conditions consume their own operands). Each
        // subsequent branch is reached by a JUMP from the previous condition, so it starts at THIS height,
        // not the height the previous branch left after pushing its value (the linear counter carries the
        // prior branch's value across `bind(next)`); reset it so a branch body emits on the right baseline
        // (else e.g. an inline HOF splice in the SECOND branch sees a phantom operand and bails).
        let entry_height = code.stack_height().max(0) as u16;
        let has_else = branches.iter().any(|(c, _)| c.is_none());
        // A `when` with no `else`, or one whose value is `Unit`, is a statement: branch values are
        // discarded and nothing reaches the operand stack at `end`.
        let is_stmt = !has_else || self.value_ty_of_when(branches) == Ty::Unit;
        let result_stack = if is_stmt {
            vec![]
        } else {
            self.verif_stack(self.value_ty_of_when(branches))
        };
        // `end` is reachable if any branch falls through to it (i.e. doesn't return/throw). A
        // no-`else` statement always has the implicit no-match fallthrough.
        let mut end_reachable = !has_else;
        for (cond, body) in branches {
            match cond {
                Some(c) => {
                    // Skip to the next branch when this condition is false (fused comparison branch).
                    let next = code.new_label();
                    self.emit_cond_branch(*c, next, false, code);
                    self.emit_value(*body, code);
                    if !self.diverges(*body) {
                        // A diverging branch (e.g. an inlined `error(...)`) left nothing and ended in
                        // `athrow` — don't discard (nothing to pop) and don't jump to `end`.
                        if is_stmt {
                            discard(self.value_ty(*body), code);
                        }
                        // Only a falling-through branch jumps to (and needs a frame at) `end`.
                        self.frame(end, result_stack.clone(), code);
                        code.goto(end);
                        end_reachable = true;
                    }
                    code.bind(next);
                    // `next` is reached only via the conditional jump above, where the stack is back at the
                    // pre-branch baseline — reset the linear counter (the just-emitted branch body left its
                    // value on the counter, but not on this control path).
                    code.set_stack(entry_height);
                }
                None => {
                    self.emit_value(*body, code);
                    if !self.diverges(*body) {
                        if is_stmt {
                            discard(self.value_ty(*body), code);
                        }
                        end_reachable = true;
                    }
                    // The else is last — it falls through to `end` (no goto needed).
                }
            }
        }
        // Frame `end` only when it's actually reachable; if every branch diverges, `end` is dead
        // (no jump targets it) and a frame there would be "Expecting a stack map frame".
        if end_reachable {
            self.frame(end, result_stack, code);
        }
        code.bind(end);
    }

    /// `try { body } catch (v: E) { … } …` (no `finally`). The body value (and each catch value) is
    /// stored into a result temp, then loaded at the merge — mirroring kotlinc. The protected region
    /// `[start, end)` covers the body+store; each catch is an exception-table handler whose frame has
    /// the caught exception on the stack and the pre-`try` locals (the result temp/catch var read as
    /// `top` there, since an exception may occur before they are assigned).
    fn emit_try(
        &mut self,
        body: u32,
        catches: &[crate::ir::IrCatch],
        finally: Option<u32>,
        result: &Ty,
        code: &mut CodeBuilder,
    ) {
        let rt = ir_ty_to_jvm(result);
        let is_stmt = matches!(rt, Ty::Unit | Ty::Nothing);
        let result_slot = if is_stmt {
            None
        } else {
            let s = self.next_slot;
            self.next_slot += slot_words(rt);
            Some(s)
        };
        const RESULT_KEY: u32 = 3_000_000;
        // A `finally` that diverges (`finally { throw }`) never falls through to `after`.
        let fin_diverges = finally.map_or(false, |f| self.diverges(f));

        let start = code.new_label();
        let end = code.new_label();
        let after = code.new_label();

        code.bind(start);
        let body_diverges = self.diverges(body);
        if is_stmt || body_diverges {
            // Statement, or a diverging body (`throw`/`return`): no value reaches the result temp.
            self.emit(body, code);
        } else {
            self.emit_value(body, code);
            store(rt, result_slot.unwrap(), code);
        }
        code.bind(end);
        let mut after_reachable = false;
        if !body_diverges {
            if let Some(f) = finally {
                self.emit(f, code);
            } // `finally` inlined on the normal path
            if !fin_diverges {
                code.goto(after);
                after_reachable = true;
            }
        }

        // The `finally` catch-all must protect the body and each catch BODY, but NOT the inlined finally
        // code (normal-path, per-catch, or its own) — otherwise an exception thrown inside an inlined
        // finally re-enters the handler and the finally runs twice. Collect each catch body's range
        // (`[cbody_start, cbody_end)`, ending before that catch's inlined finally).
        let mut fin_ranges: Vec<(Label, Label)> = vec![(start, end)];
        for c in catches {
            let handler = code.new_label();
            code.bind(handler);
            let exc_internal = c.exc_internal.render();
            let exc_ci = self.cw.class_ref(&exc_internal);
            // Handler entry: the exception is the sole stack value; locals are the pre-`try` state.
            self.frame(handler, vec![VerifType::Object(exc_ci)], code);
            let exc_ty = Ty::obj(&exc_internal);
            let cslot = self.next_slot;
            self.next_slot += 1;
            self.slots.insert(c.var, (cslot, exc_ty));
            store(exc_ty, cslot, code);
            let cbody_start = code.new_label();
            code.bind(cbody_start);
            let cbody_diverges = self.diverges(c.body);
            if is_stmt || cbody_diverges {
                self.emit(c.body, code);
            } else {
                self.emit_value(c.body, code);
                store(rt, result_slot.unwrap(), code);
            }
            self.slots.remove(&c.var);
            // The catch body is protected by the finally handler (a throw in a catch runs the finally),
            // but the catch's own inlined finally (below) is not.
            let cbody_end = code.new_label();
            code.bind(cbody_end);
            if finally.is_some() {
                fin_ranges.push((cbody_start, cbody_end));
            }
            if !cbody_diverges {
                if let Some(f) = finally {
                    self.emit(f, code);
                } // `finally` inlined after the catch
                if !fin_diverges {
                    code.goto(after);
                    after_reachable = true;
                }
            }
            code.add_exception(start, end, handler, exc_ci);
        }

        // `finally` catch-all: any exception not handled above (in the body or a catch body) runs the
        // `finally` then re-throws. It protects only the body + catch bodies (`fin_ranges`), NOT the
        // inlined finally code — which lies past those ranges, so it doesn't re-catch itself.
        if let Some(f) = finally {
            let fin_handler = code.new_label();
            code.bind(fin_handler);
            let thr_ci = self.cw.class_ref("java/lang/Throwable");
            self.frame(fin_handler, vec![VerifType::Object(thr_ci)], code);
            let thr_ty = Ty::obj("java/lang/Throwable");
            let tslot = self.next_slot;
            self.next_slot += 1;
            store(thr_ty, tslot, code);
            // The caught exception is LIVE in `tslot` across the whole inlined `finally` (it is re-raised
            // after it). Register it so any StackMapTable frame recorded WHILE emitting the finally —
            // e.g. a `finally` that itself contains a `try`/`catch` — lists `tslot` as an initialized
            // local; otherwise the trailing `aload tslot; athrow` reads a slot the verifier sees as `top`.
            // Keyed by the slot number (unique, and disjoint from small value indices) so nested catch-all
            // handlers each register their own live exception.
            let thr_key = 4_000_000 + tslot as u32;
            self.slots.insert(thr_key, (tslot, thr_ty));
            self.emit(f, code);
            self.slots.remove(&thr_key);
            // Re-raise the caught exception after the `finally` — unless the `finally` itself transfers
            // control (`finally { return … }` / `finally { throw … }`), in which case the rethrow is
            // unreachable and emitting it would leave a dead instruction without a stackmap frame.
            if !fin_diverges {
                load(thr_ty, tslot, code);
                code.athrow();
            }
            // `catch_type` 0 = catch-all (any throwable), matching kotlinc's `finally` table entry.
            for (rs, re) in fin_ranges {
                code.add_exception(rs, re, fin_handler, 0);
            }
        }

        if after_reachable {
            if let Some(slot) = result_slot {
                self.slots.insert(RESULT_KEY, (slot, rt));
            }
            self.frame(after, vec![], code);
            code.bind(after);
            if let Some(slot) = result_slot {
                load(rt, slot, code);
                self.slots.remove(&RESULT_KEY);
            }
        } else {
            // Every path diverges — `after` is dead; bind it so any stray reference resolves, but emit
            // no frame (nothing reaches it) and leave no value (the `try` is `Nothing`-typed).
            code.bind(after);
        }
    }

    /// Whether emitting `e` as a value always transfers control away (returns/throws), so control
    /// Resolve a `break`/`continue` target to `(continue_label, break_label)`. `None` → the innermost
    /// loop; `Some(l)` → the nearest enclosing loop carrying `l@`. Falls back to the innermost if the
    /// label isn't found (a compilable program always has the labeled loop in scope).
    fn loop_target(&self, label: &Option<String>) -> (Label, Label) {
        let entry = match label {
            Some(l) => self
                .loop_stack
                .iter()
                .rev()
                .find(|(_, _, sl)| sl.as_deref() == Some(l.as_str()))
                .or_else(|| self.loop_stack.last()),
            None => self.loop_stack.last(),
        };
        let (cont, end, _) = entry.expect("break/continue outside loop");
        (*cont, *end)
    }

    /// never falls through past it. Used to suppress dead `goto`s and unreachable merge frames.
    fn diverges(&self, e: u32) -> bool {
        match self.ir.expr(e) {
            IrExpr::Return(_)
            | IrExpr::Throw { .. }
            | IrExpr::Break { .. }
            | IrExpr::Continue { .. } => true,
            IrExpr::Block { stmts, value } => match value {
                Some(v) => self.diverges(*v),
                None => stmts.last().map_or(false, |s| self.diverges(*s)),
            },
            IrExpr::When { branches } => {
                branches.iter().any(|(c, _)| c.is_none())
                    && branches.iter().all(|(_, b)| self.diverges(*b))
            }
            // A `try` diverges if its `finally` diverges, or if the body and every catch diverge (no
            // path falls through to the merge).
            IrExpr::Try {
                body,
                catches,
                finally,
                ..
            } => {
                finally.map_or(false, |f| self.diverges(f))
                    || (self.diverges(*body) && catches.iter().all(|c| self.diverges(c.body)))
            }
            // A `Nothing`-typed call never returns — an inlined `error(...)`/`throw`-helper diverges via
            // `athrow`, so the branch it ends doesn't fall through to the merge.
            IrExpr::Call { .. } | IrExpr::MethodCall { .. } => self.value_ty(e) == Ty::Nothing,
            _ => false,
        }
    }

    /// The element `Ty` of an array-typed IR expression.
    fn array_elem(&self, e: u32) -> Ty {
        self.value_ty(e).array_elem().unwrap_or(Ty::Error)
    }

    fn value_ty_of_when(&self, branches: &[(Option<u32>, u32)]) -> Ty {
        // No `else` → the `when` is a Unit statement.
        if !branches.iter().any(|(c, _)| c.is_none()) {
            return Ty::Unit;
        }
        // The value type comes from a branch that *falls through* — a diverging branch (`else ->
        // return …`/`throw`) contributes nothing to the merge, so its `Unit`/`Nothing` must not make
        // the whole `when` look like a statement.
        let last = branches
            .iter()
            .rev()
            .find(|(_, b)| !self.diverges(*b))
            .map(|(_, b)| self.value_ty(*b))
            .unwrap_or(Ty::Unit);
        // A `null`/`Nothing` branch carries no concrete type and would verify-type the merge stack as
        // `top`; use a concrete fall-through branch type instead (`null` is assignable to any reference).
        if matches!(last, Ty::Null | Ty::Nothing | Ty::Error) {
            for (_, b) in branches {
                if self.diverges(*b) {
                    continue;
                }
                let t = self.value_ty(*b);
                if !matches!(t, Ty::Null | Ty::Nothing | Ty::Error) {
                    return t;
                }
            }
        }
        // When the falling-through branches are references of DIFFERENT classes (`if (c) Foo() else Bar()`,
        // joined by the checker to `Any`), the merge-point stack type must be a common supertype — krusty
        // uses `Object`. Each branch value is a subtype, so the merge frame (`Object`) verifies; the last
        // branch's own (more specific) class would mismatch the other predecessor's value (a VerifyError).
        if last.is_reference() {
            // Compare by the JVM internal name (`String` and `Obj("java/lang/String")` are the same type
            // but distinct `Ty` values), so only a genuinely differing class triggers the `Object` merge.
            let internal = |t: &Ty| -> Option<String> {
                match t {
                    Ty::String => Some("java/lang/String".to_string()),
                    _ if t.is_array() => Some(type_descriptor(*t)),
                    Ty::Obj(n, _) => Some(n.to_string()),
                    _ => None,
                }
            };
            let mut names = branches
                .iter()
                .filter(|(_, b)| !self.diverges(*b))
                .map(|(_, b)| self.value_ty(*b))
                .filter(|t| !matches!(t, Ty::Null | Ty::Nothing | Ty::Error))
                .filter_map(|t| internal(&t));
            if let Some(first) = names.next() {
                if names.any(|n| n != first) {
                    return Ty::obj("kotlin/Any");
                }
            }
        }
        last
    }

    fn frame(&mut self, label: Label, stack: Vec<VerifType>, code: &mut CodeBuilder) {
        let locals = self.verif_locals();
        // Prepend any operand held on the stack below the current expression (an arithmetic LHS across a
        // branchy RHS), so the frame types the full operand stack the verifier sees.
        let stack = if self.pending_stack.is_empty() {
            stack
        } else {
            let mut full = self.pending_stack.clone();
            full.extend(stack);
            full
        };
        code.add_frame_if_new(label, locals, stack);
    }

    fn verif_locals(&mut self) -> Vec<VerifType> {
        self.verif_locals_with(&[])
    }

    fn verif_locals_with(&mut self, extra: &[(u16, Ty)]) -> Vec<VerifType> {
        let max = self.next_slot as usize;
        let mut raw = vec![VerifType::Top; max];
        let entries: Vec<(u16, Ty)> = self.slots.values().copied().collect();
        for (slot, ty) in entries {
            if (slot as usize) < raw.len() {
                raw[slot as usize] = self.verif_single(ty);
            }
        }
        for (slot, ty) in extra.iter().copied() {
            if (slot as usize) < raw.len() {
                raw[slot as usize] = self.verif_single(ty);
            }
        }
        let mut out = Vec::new();
        let mut i = 0;
        while i < raw.len() {
            let wide = matches!(raw[i], VerifType::Long | VerifType::Double);
            out.push(raw[i].clone());
            i += if wide { 2 } else { 1 };
        }
        while out.last() == Some(&VerifType::Top) {
            out.pop();
        }
        out
    }

    fn verif_single(&mut self, ty: Ty) -> VerifType {
        // Object types are recorded by NAME (`VerifType::ObjectName`), NOT interned here — the class is
        // interned only when a WRITTEN StackMapTable frame lists it (`build_stackmap`). A frame that
        // compresses to `same_frame` drops its locals, so a class appearing only in dropped frames (e.g.
        // a `copy$default` mask-branch param) never enters the pool — matching kotlinc, no orphan.
        match ty {
            t if is_jvm_int_category(t) => VerifType::Integer,
            Ty::Long => VerifType::Long,
            Ty::Double => VerifType::Double,
            Ty::Float => VerifType::Float,
            Ty::String => VerifType::ObjectName("java/lang/String".to_string()),
            // An array's verification type is an `Object` whose class name is its descriptor (`[I`).
            t if t.is_array() => VerifType::ObjectName(type_descriptor(ty)),
            Ty::Obj(n, _) => VerifType::ObjectName(n.render()),
            _ => VerifType::Top,
        }
    }

    fn verif_stack(&mut self, ty: Ty) -> Vec<VerifType> {
        match ty {
            Ty::Unit | Ty::Nothing | Ty::Error => vec![],
            _ => vec![self.verif_single(ty)],
        }
    }

    fn value_ty(&self, e: u32) -> Ty {
        match self.ir.expr(e) {
            IrExpr::StringConcat(_) => Ty::String,
            // A class literal `T::class` is a `java/lang/Class` constant — a reference, so `==`/`!=` on
            // two class literals routes to reference equality, not the primitive `if_icmpeq`.
            IrExpr::ClassConst { .. } => Ty::obj("java/lang/Class"),
            IrExpr::Const(c) => match c {
                IrConst::Boolean(_) => Ty::Boolean,
                IrConst::Int(_) => Ty::Int,
                IrConst::Long(_) => Ty::Long,
                IrConst::Double(_) => Ty::Double,
                IrConst::Float(_) => Ty::Float,
                IrConst::Char(_) => Ty::Char,
                IrConst::String(_) => Ty::String,
                IrConst::Short(_) => Ty::Short,
                IrConst::Byte(_) => Ty::Byte,
                IrConst::Null => Ty::Null,
            },
            IrExpr::GetValue(i) => self
                .slots
                .get(i)
                .map(|(_, t)| *t)
                .or_else(|| self.var_types.get(i).copied())
                .unwrap_or(Ty::Error),
            IrExpr::GetField { class, index, .. } => {
                ir_ty_to_jvm(&self.ir.classes[*class as usize].fields[*index as usize].ty)
            }
            IrExpr::GetStatic(i) => ir_ty_to_jvm(&self.ir.statics[*i as usize].ty),
            IrExpr::New { class, .. } => Ty::obj(&self.ir.classes[*class as usize].fq_name()),
            IrExpr::MethodCall { class, index, .. } => {
                let fid = self.ir.classes[*class as usize].methods[*index as usize];
                call_ret_ty(&self.ir.functions[fid as usize].ret)
            }
            IrExpr::Call {
                callee,
                dispatch_receiver,
                ..
            } => match callee {
                Callee::Local(fid) | Callee::LocalDefault(fid) => {
                    call_ret_ty(&self.ir.functions[*fid as usize].ret)
                }
                Callee::CrossFile { ret, .. } => call_ret_ty(ret),
                // Array `get` returns the receiver's element; an array `<init>` returns the array type.
                Callee::External(fq) if fq == "kotlin/Array.get" => dispatch_receiver
                    .map(|r| {
                        // A boxed primitive array yields the UNBOXED primitive (`a[i]: Int`).
                        let e = self.array_elem(r);
                        boxed_prim_of(e).unwrap_or(e)
                    })
                    .unwrap_or(Ty::Error),
                Callee::External(fq) if prim_array_elem_ty(fq).is_some() => {
                    Ty::array(prim_array_elem_ty(fq).unwrap())
                }
                Callee::External(fq) => intrinsic_ret(fq),
                Callee::Static { descriptor, .. }
                | Callee::Virtual { descriptor, .. }
                | Callee::Special { descriptor, .. } => {
                    // A kotlin `Nothing` return is a `java/lang/Void` JVM descriptor — report it as
                    // `Nothing` so a diverging (inlined `error(...)`) call is treated as never returning
                    // (no value, no dead epilogue after the spliced `athrow`).
                    if descriptor.ends_with(")Ljava/lang/Void;") {
                        Ty::Nothing
                    } else {
                        ty_from_descriptor_ret(descriptor)
                    }
                }
                Callee::CrossFileVirtual { ret, .. } => call_ret_ty(ret),
            },
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
                // An arithmetic/bitwise op leaves a PRIMITIVE on the stack — the emitter unboxes each
                // operand first. So the result type is the UNBOXED primitive of the lhs, even when the lhs
                // value is a boxed wrapper (`it + 100` where `it` is an `Integer` from a `Map` get). Using
                // the boxed `value_ty(lhs)` here made a caller (e.g. the safe-call/elvis boxing coercion)
                // believe the result was already a reference and skip its `valueOf` → an `int`/`Integer`
                // stackmap mismatch once the masking spill was removed.
                _ => {
                    let t = self.value_ty(*lhs);
                    boxed_prim_of(t).unwrap_or(t)
                }
            },
            IrExpr::When { branches } => self.value_ty_of_when(branches),
            IrExpr::EnumEntry { class, .. } | IrExpr::EnumValueOf { class, .. } => {
                Ty::obj(&self.ir.classes[*class as usize].fq_name())
            }
            IrExpr::StaticInstance { ty, .. } => Ty::obj(&self.ir.classes[*ty as usize].fq_name()),
            IrExpr::ExternalStaticInstance { ty, .. } => Ty::obj(&ty.render()),
            IrExpr::ExternalStaticField { descriptor, .. } => {
                // The static field's JVM type, from its descriptor (an object `L…;` for an `object`'s
                // INSTANCE; primitives for the rare const-field case).
                match descriptor.as_str() {
                    "J" => Ty::Long,
                    "D" => Ty::Double,
                    "I" => Ty::Int,
                    "Z" => Ty::Boolean,
                    d => d
                        .strip_prefix('L')
                        .and_then(|s| s.strip_suffix(';'))
                        .map(Ty::obj)
                        .unwrap_or(Ty::obj("java/lang/Object")),
                }
            }
            IrExpr::RefNew { elem, .. } => Ty::obj(ref_class(elem).0),
            IrExpr::RefGet { elem, .. } => ir_ty_to_jvm(elem),
            IrExpr::RefSet { .. } => Ty::Unit,
            IrExpr::EnumValues { class } => {
                Ty::array(Ty::obj(&self.ir.classes[*class as usize].fq_name()))
            }
            IrExpr::Block { value, .. } => value.map(|v| self.value_ty(v)).unwrap_or(Ty::Unit),
            IrExpr::TypeOp {
                op, type_operand, ..
            } => match op {
                IrTypeOp::InstanceOf | IrTypeOp::NotInstanceOf => Ty::Boolean,
                _ => ir_ty_to_jvm(type_operand),
            },
            IrExpr::Lambda { arity, .. } => {
                Ty::obj(&format!("kotlin/jvm/functions/Function{arity}"))
            }
            IrExpr::InvokeFunction { ret, .. } => ir_ty_to_jvm(ret),
            IrExpr::NotNullAssert { operand } => self.value_ty(*operand),
            IrExpr::LateinitCheck { operand, .. } => self.value_ty(*operand),
            IrExpr::NewExternal { internal, .. } => Ty::obj(&internal.render()),
            IrExpr::NewCrossFile { internal, .. } => Ty::obj(&internal.render()),
            IrExpr::Throw { .. } | IrExpr::Break { .. } | IrExpr::Continue { .. } => Ty::Nothing,
            IrExpr::Vararg { array_type, .. } => ir_ty_to_jvm(array_type),
            IrExpr::NewArray { array_type, .. } => ir_ty_to_jvm(array_type),
            IrExpr::UnitInstance => Ty::obj("kotlin/Unit"),
            IrExpr::CurrentContinuation => Ty::obj("kotlin/coroutines/Continuation"),
            IrExpr::Try { result, .. } => ir_ty_to_jvm(result),
            _ => Ty::Error,
        }
    }
}

/// The `LambdaMetafactory.metafactory` bootstrap-method descriptor (the standard non-altmetafactory form).
const LMF_METAFACTORY_DESC: &str = "(Ljava/lang/invoke/MethodHandles$Lookup;Ljava/lang/String;\
Ljava/lang/invoke/MethodType;Ljava/lang/invoke/MethodType;Ljava/lang/invoke/MethodHandle;\
Ljava/lang/invoke/MethodType;)Ljava/lang/invoke/CallSite;";

/// A JVM method descriptor `(p1p2…)R` from parameter/return `Ty`s.
/// The erased SAM descriptor `(Ljava/lang/Object;…)Ljava/lang/Object;` for `FunctionN.invoke`.
fn sam_descriptor(arity: u8) -> String {
    let mut s = String::from("(");
    for _ in 0..arity {
        s.push_str("Ljava/lang/Object;");
    }
    s.push_str(")Ljava/lang/Object;");
    s
}

/// The boxed (wrapper) descriptor for a `Ty` — primitives map to their wrapper, references unchanged.
fn boxed_descriptor(t: Ty) -> String {
    match crate::jvm::jvm_class_map::wrapper_internal(t) {
        Some(w) => format!("L{w};"),
        None => type_descriptor(t),
    }
}

/// JVM internal name for a reference `Ty`, for `instanceof`/`checkcast`.
/// Convert the numeric primitive on top of the stack from `from` to `to` (JVM `i2l`/`i2d`/…).
/// Byte/Short/Char live in the `int` stack category; widening goes via that category, and a
/// Byte/Short/Char target is narrowed from `int` last.
/// Parse the return type of a JVM method descriptor (`(…)Lfoo/Bar;` → `Obj("foo/Bar")`) into a `Ty`.
fn ty_from_descriptor_ret(desc: &str) -> Ty {
    let ret = desc.rsplit(')').next().unwrap_or("V");
    ty_from_field_descriptor(ret)
}

fn descriptor_ret_words(desc: &str) -> i32 {
    // A genuinely `void` (`)V`) method leaves nothing on the stack; `ty_from_descriptor_ret` maps `V` to
    // `Unit` (a 1-word value) for type flow elsewhere.
    if desc.ends_with(")V") {
        0
    } else {
        slot_words(ty_from_descriptor_ret(desc)) as i32
    }
}

/// Parse a single JVM field/type descriptor into a `Ty`.
fn ty_from_field_descriptor(d: &str) -> Ty {
    match d.as_bytes().first() {
        Some(b'I') => Ty::Int,
        Some(b'J') => Ty::Long,
        Some(b'Z') => Ty::Boolean,
        Some(b'B') => Ty::Byte,
        Some(b'C') => Ty::Char,
        Some(b'S') => Ty::Short,
        Some(b'F') => Ty::Float,
        Some(b'D') => Ty::Double,
        Some(b'V') => Ty::Unit,
        Some(b'L') => Ty::obj(
            d.strip_prefix('L')
                .and_then(|s| s.strip_suffix(';'))
                .unwrap_or(d),
        ),
        Some(b'[') => Ty::array(ty_from_field_descriptor(&d[1..])),
        _ => Ty::Error,
    }
}

fn emit_num_conv(from: Ty, to: Ty, code: &mut CodeBuilder) {
    use Ty::*;
    if from == to {
        return;
    }
    let wide = |t: Ty| match t {
        Byte | Short | Char | Int => Int,
        o => o,
    };
    match (wide(from), wide(to)) {
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
        _ => {} // same wide category (e.g. Byte→Int): the value is already correct on the stack
    }
    match to {
        Byte => code.i2b(),
        Short => code.i2s(),
        Char => code.i2c(),
        _ => {}
    }
}

fn ref_internal(t: Ty) -> String {
    match t {
        Ty::String => "java/lang/String".to_string(),
        // An array's reference identity is its descriptor (`[I`, `[Ljava/lang/String;`) — checked before
        // the `Obj` arm since arrays are now `Obj("kotlin/Array")`/`Obj("kotlin/IntArray")` too.
        t if t.is_array() => type_descriptor(t),
        // Erase a Kotlin built-in name (`kotlin/collections/MutableList`) to its JVM identity here at the
        // bytecode boundary, so `instanceof`/`checkcast`/method-owner refs never leak a Kotlin-only name.
        Ty::Obj(n, _) => crate::jvm::jvm_class_map::to_jvm_internal(&n.render()).to_string(),
        // A function type's reference identity is its `kotlin/jvm/functions/FunctionN` interface, so
        // `x is Function1<*, *>` / `x as (A) -> B` test/cast against that class, not `Object`.
        Ty::Fun(_) => t
            .function_interface_internal()
            .unwrap_or("java/lang/Object")
            .to_string(),
        _ => "java/lang/Object".to_string(),
    }
}

fn intrinsic_ret(fq: &str) -> Ty {
    match fq {
        "kotlin/String.plus" | "kotlin/Any.toString" => Ty::String,
        "kotlin/Any.hashCode" => Ty::Int,
        "kotlin/String.length" | "kotlin/Array.size" | "java/lang/Enum.ordinal" => Ty::Int,
        "kotlin/String.get" => Ty::Char,
        "kotlin/Array.set" => Ty::Unit,
        "java/lang/Enum.name" => Ty::String,
        f if f.ends_with(".hashCode") || f.ends_with(".compare") => Ty::Int,
        _ => Ty::Error,
    }
}

/// `newarray` atype for a `kotlin/<Prim>Array.<init>` intrinsic.
fn prim_array_atype(fq: &str) -> u8 {
    match prim_array_elem_ty(fq) {
        Some(Ty::Boolean) => 4,
        Some(Ty::Char) => 5,
        Some(Ty::Float) => 6,
        Some(Ty::Double) => 7,
        Some(Ty::Byte) => 8,
        Some(Ty::Short) => 9,
        Some(Ty::Long) => 11,
        _ => 10, // Int (the only remaining primitive-array element)
    }
}

/// Element `Ty` for a `kotlin/<Prim>Array.<init>` intrinsic FqName — `None` for any other call.
/// Matches the full FqName exactly (not a suffix) so a user class named `…Array` can't be mistaken
/// for a primitive-array constructor.
fn prim_array_elem_ty(fq: &str) -> Option<Ty> {
    Some(match fq {
        "kotlin/IntArray.<init>" => Ty::Int,
        "kotlin/LongArray.<init>" => Ty::Long,
        "kotlin/DoubleArray.<init>" => Ty::Double,
        "kotlin/FloatArray.<init>" => Ty::Float,
        "kotlin/BooleanArray.<init>" => Ty::Boolean,
        "kotlin/CharArray.<init>" => Ty::Char,
        "kotlin/ByteArray.<init>" => Ty::Byte,
        "kotlin/ShortArray.<init>" => Ty::Short,
        _ => return None,
    })
}

/// `(opcode, value-words)` for an array element load (`Xaload`).
/// If `t` is the boxed-reference form of a primitive (the element of a `Array<Int>` etc., carried as
/// `Obj("kotlin/Int")`), the underlying primitive `Ty`. Used to insert box/unbox at the boxed-array
/// element boundary (`a[i]` yields an unboxed `Int`; `a[i] = v` boxes the `Int`).
fn boxed_prim_of(t: Ty) -> Option<Ty> {
    match t {
        Ty::Obj(n, _) if n.matches("kotlin/Int") => Some(Ty::Int),
        Ty::Obj(n, _) if n.matches("kotlin/Long") => Some(Ty::Long),
        Ty::Obj(n, _) if n.matches("kotlin/Short") => Some(Ty::Short),
        Ty::Obj(n, _) if n.matches("kotlin/Byte") => Some(Ty::Byte),
        Ty::Obj(n, _) if n.matches("kotlin/Double") => Some(Ty::Double),
        Ty::Obj(n, _) if n.matches("kotlin/Float") => Some(Ty::Float),
        Ty::Obj(n, _) if n.matches("kotlin/Boolean") => Some(Ty::Boolean),
        Ty::Obj(n, _) if n.matches("kotlin/Char") => Some(Ty::Char),
        _ => None,
    }
}

fn array_load_op(elem: Ty) -> (u8, i32) {
    match elem {
        // Unsigned arrays are the unboxed underlying primitive array (`UIntArray` = `[I`,
        // `ULongArray` = `[J`), so they load with `iaload`/`laload`.
        Ty::Int | Ty::UInt => (0x2e, 1),
        Ty::Long | Ty::ULong => (0x2f, 2),
        Ty::Float => (0x30, 1),
        Ty::Double => (0x31, 2),
        Ty::Boolean | Ty::Byte => (0x33, 1),
        Ty::Char => (0x34, 1),
        Ty::Short => (0x35, 1),
        _ => (0x32, 1), // aaload
    }
}

/// `(opcode, value-words)` for an array element store (`Xastore`).
/// Push the zero value of `t` (the placeholder for an omitted `$default` argument; the stub overwrites
/// it when the mask bit is set).
fn push_zero(t: Ty, code: &mut CodeBuilder, cw: &mut ClassWriter) {
    match t {
        Ty::Long => code.lconst_0(),
        Ty::Double => code.dconst_0(),
        Ty::Float => code.fconst_0(),
        t if is_jvm_int_category(t) => code.push_int(0, cw),
        _ => code.aconst_null(),
    }
}

fn is_jvm_int_category(t: Ty) -> bool {
    matches!(t, Ty::Int | Ty::Boolean | Ty::Byte | Ty::Short | Ty::Char)
}

fn array_store_op(elem: Ty) -> (u8, i32) {
    match elem {
        // Unsigned arrays store into the unboxed underlying primitive array (`[I`/`[J`).
        Ty::Int | Ty::UInt => (0x4f, 1),
        Ty::Long | Ty::ULong => (0x50, 2),
        Ty::Float => (0x51, 1),
        Ty::Double => (0x52, 2),
        Ty::Boolean | Ty::Byte => (0x54, 1),
        Ty::Char => (0x55, 1),
        Ty::Short => (0x56, 1),
        _ => (0x53, 1), // aastore
    }
}

/// `newarray` atype for a primitive element (JVMS Table 6.5.newarray-A).
fn prim_newarray_atype(elem: Ty) -> u8 {
    match elem {
        Ty::Boolean => 4,
        Ty::Char => 5,
        Ty::Float => 6,
        Ty::Double => 7,
        Ty::Byte => 8,
        Ty::Short => 9,
        Ty::Long => 11,
        _ => 10, // int
    }
}

/// Normalize a call's return JVM-type: a Kotlin `Nothing` is carried as an object whose JVM mapping is
/// `java/lang/Void` (the descriptor the front end emits for it). Collapse that to the `Ty::Nothing`
/// bottom variant so `diverges`/`value_ty_of_when` see the call never returns — a `Static`/`Virtual`
/// callee already gets this from its `)Ljava/lang/Void;` descriptor; a `Local`/`CrossFile`/method callee
/// reads the IR `ret` directly and needs the same normalization (else a `Nothing`-returning call's value
/// is wrongly merged/popped, e.g. an `exit()` branch of an `if` ⇒ inconsistent stackmap frames).
/// Whether an IR return type is the NON-nullable bottom type `Nothing` (so a call to it never returns and
/// must be terminated). A `Nothing?` return is NULLABLE — it can yield `null` (`fun f(): Nothing? { … return
/// null … }`) — and must NOT be treated as diverging; the JVM descriptor erases the `?` (both are `Void`),
/// so the nullability is checked on the IR type before it is erased by `ir_ty_to_jvm`.
fn ret_is_nothing(ret: &Ty) -> bool {
    !ret.is_nullable() && norm_nothing(ir_ty_to_jvm(ret)) == Ty::Nothing
}

/// The JVM `Ty` a call to a function with IR return `ret` leaves on the stack: the `Ty::Nothing` bottom
/// for a NON-nullable `Nothing` return (no value — the call diverges), else the erased reference/value
/// type. A `Nothing?` return is a real nullable reference (`Void`, 1 slot) that yields `null`, so it must
/// NOT collapse to `Nothing` (that would mis-size discards and mis-flag it as diverging).
fn call_ret_ty(ret: &Ty) -> Ty {
    if ret_is_nothing(ret) {
        Ty::Nothing
    } else {
        ir_ty_to_jvm(ret)
    }
}

fn norm_nothing(t: Ty) -> Ty {
    match &t {
        Ty::Obj(n, _)
            if crate::jvm::jvm_class_map::type_name_maps_to_jvm_internal(*n, "java/lang/Void") =>
        {
            Ty::Nothing
        }
        _ => t,
    }
}

pub fn ir_ty_to_jvm(t: &Ty) -> Ty {
    // A nullable PRIMITIVE is a JVM reference — its boxed wrapper (`Int?` → `java/lang/Integer`, a
    // 1-slot reference), NOT the unboxed scalar. Map it before peeling `?`, so descriptors, slots and
    // stackmap frames all see the reference. A nullable REFERENCE keeps its descriptor (peel below).
    if let Ty::Nullable(inner) = t {
        if **inner == Ty::Nothing {
            return Ty::obj("kotlin/Any");
        }
        if **inner == Ty::Unit {
            return Ty::obj("kotlin/Unit");
        }
        if let Some(boxed) = inner.boxed_ref() {
            // `boxed_ref` already picks the right wrapper — `java/lang/Integer` for `Int?`, the inline-class
            // `kotlin/UInt` for `UInt?` — so do NOT re-map through `ir_ty_to_jvm` (which would erase the
            // unsigned wrapper to `Integer`).
            return boxed;
        }
    }
    // Nullability is otherwise erased at the JVM-type level (a nullable reference keeps its descriptor),
    // so peel the `?` first.
    match t.non_null() {
        Ty::Unit => Ty::Unit,
        Ty::Nothing => Ty::Nothing,
        // Bare scalar/`String` variants are already JVM types — pass through. (Front-end/`ir_lower` types
        // can arrive either as these variants or as their `Obj("kotlin/…")` spelling; both must map here.)
        Ty::Int => Ty::Int,
        Ty::Long => Ty::Long,
        Ty::Short => Ty::Short,
        Ty::Byte => Ty::Byte,
        Ty::Boolean => Ty::Boolean,
        Ty::Char => Ty::Char,
        Ty::Double => Ty::Double,
        Ty::Float => Ty::Float,
        Ty::String => Ty::String,
        // Unsigned scalars are inline classes over the signed primitive; unboxed they ARE that primitive
        // (`UInt` = `int`, `ULong` = `long`) — same JVM slots and `istore`/`iload`/arithmetic. Unsigned
        // semantics live in the intrinsic calls (`Integer.compareUnsigned`, …) ir_lower already inserted.
        Ty::UInt => Ty::Int,
        Ty::ULong => Ty::Long,
        Ty::Obj(fq_name, type_args) => match () {
            _ if fq_name.matches("kotlin/Int") => Ty::Int,
            _ if fq_name.matches("kotlin/Long") => Ty::Long,
            _ if fq_name.matches("kotlin/Short") => Ty::Short,
            _ if fq_name.matches("kotlin/Byte") => Ty::Byte,
            _ if fq_name.matches("kotlin/Boolean") => Ty::Boolean,
            _ if fq_name.matches("kotlin/Char") => Ty::Char,
            _ if fq_name.matches("kotlin/Double") => Ty::Double,
            _ if fq_name.matches("kotlin/Float") => Ty::Float,
            _ if fq_name.matches("kotlin/String") => Ty::String,
            // Arrays are regular class types the JVM backend lowers to JVM array types here.
            _ if fq_name.matches("kotlin/IntArray") => Ty::array(Ty::Int),
            _ if fq_name.matches("kotlin/LongArray") => Ty::array(Ty::Long),
            _ if fq_name.matches("kotlin/DoubleArray") => Ty::array(Ty::Double),
            _ if fq_name.matches("kotlin/FloatArray") => Ty::array(Ty::Float),
            _ if fq_name.matches("kotlin/BooleanArray") => Ty::array(Ty::Boolean),
            _ if fq_name.matches("kotlin/CharArray") => Ty::array(Ty::Char),
            _ if fq_name.matches("kotlin/ByteArray") => Ty::array(Ty::Byte),
            _ if fq_name.matches("kotlin/ShortArray") => Ty::array(Ty::Short),
            // Unsigned arrays are `inline class`es over the signed primitive array; at the JVM level they
            // ARE that array (`UIntArray` = `[I`). The unsigned element semantics are a source/checker
            // concern already resolved before emit, so collapse to the physical signed array here.
            _ if fq_name.matches("kotlin/UIntArray") => Ty::array(Ty::Int),
            _ if fq_name.matches("kotlin/ULongArray") => Ty::array(Ty::Long),
            // A `kotlin/Array<T>` is a JVM reference array: a primitive element `T` is BOXED
            // (`Array<Int>` = `[Ljava/lang/Integer;`, distinct from the unboxed `IntArray` = `[I`).
            _ if fq_name.matches("kotlin/Array") => Ty::array(
                type_args
                    .first()
                    .map(|e| {
                        let et = ir_ty_to_jvm(e);
                        let boxed = et.boxed_ref().unwrap_or(et);
                        // Keep a NULLABLE element's `?`: `Array<Int?>` = `Integer[]` whose `get` yields the
                        // BOXED element (it can be `null`), UNLIKE `Array<Int>` whose `get` unboxes.
                        // `boxed_prim_of` returns `None` for a `Nullable(..)`, so the emitter's `Array.get`
                        // keeps it boxed and `.set` skips the extra box — matching the value the front end
                        // supplies (boxed for a nullable element, unboxed for a non-null one).
                        if e.is_nullable() {
                            Ty::nullable(boxed)
                        } else {
                            boxed
                        }
                    })
                    .unwrap_or(Ty::obj("java/lang/Object")),
            ),
            _ => Ty::obj(&fq_name.render()),
        },
        // The JVM representation of a function type is `kotlin/jvm/functions/FunctionN`. A `suspend`
        // function type carries a trailing `Continuation` parameter, so its arity is one greater.
        Ty::Fun(s) => Ty::obj(&format!(
            "kotlin/jvm/functions/Function{}",
            s.params.len() + usize::from(s.suspend)
        )),
        // JVM erasure of a type parameter: collapse `T` to its declared upper bound (which itself
        // erases to `java/lang/Object` for an `Any` bound). This is the ONE place `T` becomes a
        // concrete JVM type.
        Ty::TyParam(_, bound) => ir_ty_to_jvm(bound),
        _ => Ty::Error,
    }
}

pub(crate) fn jvm_tys(tys: &[Ty]) -> Vec<Ty> {
    tys.iter()
        .map(|t| match ir_ty_to_jvm(t) {
            Ty::Nothing => Ty::obj("kotlin/Any"),
            other => other,
        })
        .collect()
}

/// Whether a JVM type is an ERASED TOP reference — the `java/lang/Object` a type parameter erases to, or
/// an `Object[]` a generic `Array<T>` erases to (recursively). A value of this type is a candidate for the
/// narrowing `checkcast` at a consumption site; a concrete type (`String`, `Integer`, `IntArray`, a value
/// class) is not.
fn jvm_is_erased_top(t: Ty) -> bool {
    match t.obj_internal() {
        Some(n) if n.matches("java/lang/Object") || n.matches("kotlin/Any") => true,
        _ => t.array_elem().is_some_and(jvm_is_erased_top),
    }
}

fn ir_type_desc(t: &Ty) -> String {
    type_descriptor(ir_ty_to_jvm(t))
}

fn ir_method_desc(params: &[Ty], ret: &Ty) -> String {
    method_descriptor(&jvm_tys(params), ir_ty_to_jvm(ret))
}

fn field_jvm_tys(fields: &[IrField]) -> Vec<Ty> {
    fields.iter().map(|f| ir_ty_to_jvm(&f.ty)).collect()
}

fn ctor_arg_jvm_tys(args: &[IrCtorArg]) -> Vec<Ty> {
    args.iter().map(|a| ir_ty_to_jvm(&a.ty)).collect()
}

fn class_ctor_jvm_tys(c: &IrClass) -> Vec<Ty> {
    if c.ctor_args.is_empty() {
        field_jvm_tys(&c.fields[..c.ctor_param_count as usize])
    } else {
        ctor_arg_jvm_tys(&c.ctor_args)
    }
}

/// The JVM element type of an array given its whole array type. `ir_ty_to_jvm` already maps
/// `kotlin/Array<Int>` → `[Ljava/lang/Integer;` (boxed) and `kotlin/IntArray` → `[I` (primitive), so the
/// boxed-vs-primitive distinction is carried by the type — no flag needed.
fn array_jvm_element(array_type: &Ty) -> Ty {
    ir_ty_to_jvm(array_type)
        .array_elem()
        .unwrap_or_else(|| Ty::obj("java/lang/Object"))
}

/// Swap the operands of a comparison operator (`a < b` ≡ `b > a`) — used to normalize `0 <op> x` into
/// `x <swapped-op> 0` so the single-operand compare-to-zero branch applies.
fn swap_cmp(op: IrBinOp) -> IrBinOp {
    use IrBinOp::*;
    match op {
        Lt => Gt,
        Le => Ge,
        Gt => Lt,
        Ge => Le,
        o => o,
    }
}

/// The `String.valueOf` overload descriptor for a single interpolated value's type (`"$x"`).
fn valueof_desc(t: Ty) -> &'static str {
    match t {
        Ty::Int | Ty::Short | Ty::Byte => "(I)Ljava/lang/String;",
        Ty::Long => "(J)Ljava/lang/String;",
        Ty::Float => "(F)Ljava/lang/String;",
        Ty::Double => "(D)Ljava/lang/String;",
        Ty::Boolean => "(Z)Ljava/lang/String;",
        Ty::Char => "(C)Ljava/lang/String;",
        _ => "(Ljava/lang/Object;)Ljava/lang/String;",
    }
}

/// `true` if a lowered IR type is a nullable reference (`String?` etc.).
fn ir_ty_nullable(t: &Ty) -> bool {
    t.is_nullable()
}

fn slot_words(t: Ty) -> u16 {
    match t {
        // `ULong` is a `long` on the JVM — two words, like `Long`/`Double` (`UInt` is one, like `Int`).
        Ty::Long | Ty::Double | Ty::ULong => 2,
        Ty::Unit | Ty::Nothing => 0,
        _ => 1,
    }
}

fn load(t: Ty, slot: u16, code: &mut CodeBuilder) {
    match t {
        Ty::Long => code.lload(slot),
        Ty::Double => code.dload(slot),
        Ty::Float => code.fload(slot),
        t if is_jvm_int_category(t) => code.iload(slot),
        _ => code.aload(slot),
    }
}

fn store(t: Ty, slot: u16, code: &mut CodeBuilder) {
    match t {
        Ty::Long => code.lstore(slot),
        Ty::Double => code.dstore(slot),
        Ty::Float => code.fstore(slot),
        t if is_jvm_int_category(t) => code.istore(slot),
        _ => code.astore(slot),
    }
}

fn emit_return(t: Ty, code: &mut CodeBuilder) {
    match t {
        Ty::Long => code.lreturn(),
        Ty::Double => code.dreturn(),
        Ty::Float => code.freturn(),
        t if is_jvm_int_category(t) => code.ireturn(),
        Ty::Unit | Ty::Nothing => code.ret_void(),
        _ => code.areturn(),
    }
}

fn discard(t: Ty, code: &mut CodeBuilder) {
    match slot_words(t) {
        2 => code.pop2(),
        1 => code.pop(),
        _ => {}
    }
}

fn mapped_builtin_virtual_name<'a>(owner: &str, name: &'a str) -> &'a str {
    match (owner, name) {
        ("java/lang/CharSequence", "get") => "charAt",
        ("java/lang/String", "get") | ("kotlin/String", "get") => "charAt",
        ("java/lang/StringBuilder", "get") | ("kotlin/text/StringBuilder", "get") => "charAt",
        (
            "kotlin/ranges/IntRange" | "kotlin/ranges/LongRange" | "kotlin/ranges/CharRange",
            "start",
        ) => "getFirst",
        (
            "kotlin/ranges/IntRange" | "kotlin/ranges/LongRange" | "kotlin/ranges/CharRange",
            "endInclusive",
        ) => "getLast",
        ("java/util/Map" | "kotlin/collections/Map" | "kotlin/collections/MutableMap", "keys") => {
            "keySet"
        }
        (
            "java/util/Map" | "kotlin/collections/Map" | "kotlin/collections/MutableMap",
            "entries",
        ) => "entrySet",
        (
            "kotlin/reflect/KCallable"
            | "kotlin/reflect/KProperty"
            | "kotlin/reflect/KProperty0"
            | "kotlin/reflect/KProperty1"
            | "kotlin/reflect/KMutableProperty0"
            | "kotlin/reflect/KMutableProperty1",
            "name",
        ) => "getName",
        ("java/lang/Number", "toByte") => "byteValue",
        ("java/lang/Number", "toShort") => "shortValue",
        ("java/lang/Number", "toInt") => "intValue",
        ("java/lang/Number", "toLong") => "longValue",
        ("java/lang/Number", "toFloat") => "floatValue",
        ("java/lang/Number", "toDouble") => "doubleValue",
        _ => name,
    }
}

fn is_string_plus_virtual(owner: &str, name: &str, descriptor: &str) -> bool {
    matches!(owner, "java/lang/String" | "kotlin/String")
        && name == "plus"
        && descriptor == "(Ljava/lang/Object;)Ljava/lang/String;"
}

fn range_to_virtual_ctor(
    owner: &str,
    name: &str,
    descriptor: &str,
) -> Option<(&'static str, &'static str, i32, Ty)> {
    if name != "rangeTo" {
        return None;
    }
    Some(match (owner, descriptor) {
        (
            "java/lang/Byte" | "kotlin/Byte" | "java/lang/Short" | "kotlin/Short"
            | "java/lang/Integer" | "kotlin/Int",
            "(B)Lkotlin/ranges/IntRange;"
            | "(S)Lkotlin/ranges/IntRange;"
            | "(I)Lkotlin/ranges/IntRange;",
        ) => ("kotlin/ranges/IntRange", "(II)V", 2, Ty::Int),
        (
            "java/lang/Byte" | "kotlin/Byte" | "java/lang/Short" | "kotlin/Short"
            | "java/lang/Integer" | "kotlin/Int" | "java/lang/Long" | "kotlin/Long",
            "(B)Lkotlin/ranges/LongRange;"
            | "(S)Lkotlin/ranges/LongRange;"
            | "(I)Lkotlin/ranges/LongRange;"
            | "(J)Lkotlin/ranges/LongRange;",
        ) => ("kotlin/ranges/LongRange", "(JJ)V", 4, Ty::Long),
        ("java/lang/Character" | "kotlin/Char", "(C)Lkotlin/ranges/CharRange;") => {
            ("kotlin/ranges/CharRange", "(CC)V", 2, Ty::Char)
        }
        _ => return None,
    })
}

fn wrapper_owner_primitive(owner: &str) -> Option<Ty> {
    Some(match owner {
        "java/lang/Integer" | "kotlin/Int" => Ty::Int,
        "java/lang/Long" | "kotlin/Long" => Ty::Long,
        "java/lang/Double" | "kotlin/Double" => Ty::Double,
        "java/lang/Float" | "kotlin/Float" => Ty::Float,
        "java/lang/Boolean" | "kotlin/Boolean" => Ty::Boolean,
        "java/lang/Character" | "kotlin/Char" => Ty::Char,
        "java/lang/Byte" | "kotlin/Byte" => Ty::Byte,
        "java/lang/Short" | "kotlin/Short" => Ty::Short,
        _ => return None,
    })
}

fn methodref_owner<'a>(body: &'a MethodCode, name: &str, descriptor: &str) -> Option<&'a str> {
    fn utf8(cp: &[C], idx: u16) -> Option<&str> {
        match cp.get(idx as usize)? {
            C::Utf8(s) => Some(s.as_str()),
            _ => None,
        }
    }
    fn class_name(cp: &[C], idx: u16) -> Option<&str> {
        match cp.get(idx as usize)? {
            C::Class(name_idx) => utf8(cp, *name_idx),
            _ => None,
        }
    }
    fn name_and_desc(cp: &[C], idx: u16) -> Option<(&str, &str)> {
        match cp.get(idx as usize)? {
            C::NameAndType(name_idx, desc_idx) => {
                Some((utf8(cp, *name_idx)?, utf8(cp, *desc_idx)?))
            }
            _ => None,
        }
    }

    body.source_cp.iter().find_map(|entry| {
        let C::Methodref(class_idx, nt_idx) = entry else {
            return None;
        };
        let (n, d) = name_and_desc(&body.source_cp, *nt_idx)?;
        (n == name && d == descriptor).then(|| class_name(&body.source_cp, *class_idx))?
    })
}

#[cfg(test)]
mod fail_soft_tests {
    use super::*;
    use crate::ir::{IrExpr, IrFile, IrFunction};
    use crate::jvm::classreader::MethodCode;
    use crate::jvm::inline::MethodBodies;
    use crate::types::Ty;

    struct NoBodies;
    impl MethodBodies for NoBodies {
        fn body(&self, _o: &str, _n: &str, _d: &str) -> Option<MethodCode> {
            None
        }
    }

    // A `GetValue` of a value slot that was never allocated is malformed IR (e.g. an unsupported
    // suspend shape the lowering should have bailed on). The emitter must SKIP the file
    // (`emit_all` -> `None`), never panic — a compiler must not crash on its own IR.
    #[test]
    fn getvalue_of_unallocated_slot_skips_not_panics() {
        let mut ir = IrFile::default();
        let body = ir.add_expr(IrExpr::GetValue(99));
        ir.add_fun(IrFunction {
            name: "box".into(),
            params: vec![],
            ret: Ty::Unit,
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![],
        });
        assert!(emit_all(&ir, "TestKt", &NoBodies, None).is_none());
    }
}
