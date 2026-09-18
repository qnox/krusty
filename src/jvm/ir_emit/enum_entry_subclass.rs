//! The subclass an enum entry with a body becomes.
//!
//! `Enum$ENTRY extends Enum`: a package-private `final` class whose one constructor delegates to
//! the enum's, plus the entry's overriding methods. It has no fields of its own — the overrides
//! read the enum's through the inherited `this`.

use super::*;

/// Emit a synthesized enum-entry subclass (`Enum$ENTRY extends Enum`) for an entry with a body: a
/// package-private `final` class with one constructor `(String name, int ordinal, <user fields>)V`
/// that delegates to the enum's `(String,int,<user>)V` constructor, plus the entry's overriding
/// methods. It has no fields of its own — overrides read the enum's fields via the inherited `this`.
pub(super) fn emit_enum_entry_subclass(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    facade: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
    user_tys: &[Ty],
) -> Vec<u8> {
    let superclass = c.superclass();
    let fq_name = c.fq_name();
    let signature_formatter = JvmSignatureFormatter::new(ir, env);
    let mut cw = new_writer(&fq_name, &superclass, opts);
    cw.set_access(0x0010 | 0x0020); // FINAL | SUPER (package-private)

    // Entry-body PROPERTIES are private backing fields (read via synthesized getters, like kotlinc).
    for field in c.fields.iter() {
        let acc = 0x0002 | if field.is_final() { 0x0010 } else { 0 };
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
        let mut e = Emitter::new(ir, &mut cw, env, &fq_name, facade, Ty::Unit, [init_body]);
        e.next_slot = 1 + ctor_words;
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
    if let Some(defaults) = ir
        .class_ctor_defaults(&superclass)
        .filter(|defaults| defaults.iter().any(Option::is_some))
    {
        emit_ctor_default_stub_with_prefix(
            ir,
            &fq_name,
            facade,
            &[Ty::String, Ty::Int],
            0,
            &user_jvm,
            defaults,
            false,
            0x1000,
            &mut cw,
            env,
        );
    }

    // The overriding methods + synthesized property getters.
    emit_declared_property_accessors(
        ir,
        c,
        &mut cw,
        &PropertyAccessorEmit {
            fq_name: &fq_name,
            facade,
            formatter: &signature_formatter,
            param_assertions: opts.param_assertions,
            env,
        },
    );
    let markers = property_annotation_marker_fids(ir, c);
    for &fid in &c.methods {
        if markers.contains(&fid) || standalone_method_is_elided(ir, fid, env) {
            continue; // already emitted beside its property's accessors
        }
        // Lambda implementation helpers reparented into an enum-entry subclass remain static; only
        // source member overrides consume an instance receiver. The ordinary class/enum writers
        // already honor this IR bit, and entry subclasses must use the same rule.
        let function = &ir.functions[fid as usize];
        emit_method(ir, fid, &fq_name, facade, &mut cw, !function.is_static, env);
        if ir.function_reference_access_bridges.contains(&fid) {
            super::access_bridges::emit_function_reference_access_bridge(
                ir, fid, &fq_name, &mut cw, false,
            );
        }
        if let Some(defaults) = ir.param_defaults(fid) {
            if function.is_static {
                emit_facade_default_stub(
                    ir,
                    fid,
                    &fq_name,
                    &mut cw,
                    defaults,
                    env,
                    Ty::obj("java/lang/Object"),
                );
            } else {
                emit_default_stub(ir, fid, &fq_name, facade, &mut cw, defaults, env, false);
            }
        }
    }
    // Entry-body override edges are checked and frozen in Pass 1 like ordinary class overrides.
    // Their anonymous subclass still needs the JVM descriptor adapters derived from those edges
    // (for example `apply(Object)` forwarding to `apply(String)`).
    emit_bridges(ir, c, &mut cw);
    cw.finish()
}
