//! The synthetic implementation class of a Kotlin annotation instantiation (`A(args)`), with the
//! `java.lang.annotation.Annotation` contract: accessors, `annotationType()`, `equals`, `hashCode`
//! and `toString`.

use super::metadata_policy::annotation_impl_carries_nullability;
use super::{
    arrays_param_desc, constructor_defaults, emit_return, finish_code, jvm_declared_ty, load,
    new_writer, slot_words, EmitEnv, EmitOptions,
};
use crate::ir::IrFile;
use crate::jvm::classfile::{ClassWriter, CodeBuilder};
use crate::jvm::names::type_descriptor;
use crate::types::Ty;

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

/// Emit the synthetic IMPLEMENTATION class for a Kotlin annotation instantiation (`A(args)`): a final
/// class implementing the annotation interface `iface` and the full `java.lang.annotation.Annotation`
/// contract — private final fields, a constructor, per-member accessors (`x()`/`s()`), `annotationType()`,
/// and content-correct `equals`/`hashCode`/`toString` (arrays via `java.util.Arrays`, `float`/`double` via
/// their wrappers' `equals`/`hashCode` for NaN/`-0.0` semantics). `c.fields` are the members in order.
pub(super) fn emit_annotation_impl_class(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    iface: &str,
    facade: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
) -> Vec<u8> {
    let fq = c.fq_name();
    let members: Vec<(String, Ty)> = c
        .fields
        .iter()
        .map(|f| (f.name.clone(), jvm_declared_ty(&f.ty)))
        .collect();
    let mut cw = new_writer(&fq, "java/lang/Object", opts);
    cw.set_access(0x0001 | 0x0010 | 0x0020 | 0x1000); // PUBLIC | FINAL | SUPER | SYNTHETIC
    cw.add_interface(iface);
    for (name, jt) in &members {
        // SYNTHETIC: nothing in source declares these — the class is generated for an annotation
        // instantiation, and kotlinc marks its fields and member accessors so tooling skips them.
        // The constructor and the `Object` overrides are NOT marked, which is kotlinc's split.
        cw.add_field(0x0002 | 0x0010 | 0x1000, name, &type_descriptor(*jt)); // PRIVATE|FINAL|SYNTHETIC
    }

    // <init>(members…): super(); store each arg to its field.
    {
        let params_words: u16 = members.iter().map(|(_, jt)| slot_words(*jt)).sum();
        let mut ctor = CodeBuilder::new(1 + params_words);
        // Every REFERENCE member is guarded at entry, before `super()` — kotlinc's shape, and the
        // same `Intrinsics.checkNotNullParameter` any non-null parameter gets. An annotation member
        // is never nullable (the JVM annotation format has no null), so every non-primitive one
        // takes the guard, in declaration order.
        let mut guard_slot = 1u16;
        for (name, jt) in &members {
            if jt.is_reference() {
                ctor.aload(guard_slot);
                ctor.push_string(name, &mut cw);
                let check = cw.methodref(
                    "kotlin/jvm/internal/Intrinsics",
                    "checkNotNullParameter",
                    "(Ljava/lang/Object;Ljava/lang/String;)V",
                );
                ctor.invokestatic(check, 2, 0);
            }
            guard_slot += slot_words(*jt);
        }
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
        let mut locals = vec![("this".to_string(), format!("L{fq};"), 0)];
        let mut debug_slot = 1u16;
        for (name, jt) in &members {
            locals.push((name.clone(), type_descriptor(*jt), debug_slot));
            debug_slot += slot_words(*jt);
        }
        cw.set_method_debug("<init>", &desc, None, &locals);
        // A reference member is non-null (the JVM annotation format has no null), so kotlinc stamps
        // the synthesized `@NotNull` on each such parameter — the same annotation any non-null
        // parameter gets, and what a Java caller reads to know the contract. Kotlin 2.4.20 stamps
        // none anywhere in this synthetic class.
        if annotation_impl_carries_nullability() {
            let notnull = "Lorg/jetbrains/annotations/NotNull;";
            let param_nullability: Vec<Option<&str>> = members
                .iter()
                .map(|(_, jt)| jt.is_reference().then_some(notnull))
                .collect();
            cw.set_method_nullability("<init>", &desc, None, &param_nullability);
        }
        // A default on any annotation member (`annotation class C(val i: Int = 1)`) → the same synthetic
        // `<init>(members…, int mask, DefaultConstructorMarker)` overload an ordinary class gets. The impl
        // class is what `C()` actually constructs, so without it a call omitting a default targets a
        // constructor nothing emits (`NoSuchMethodError`). kotlinc emits it on the impl class too.
        if let Some(defaults) = ir.class_ctor_defaults(&fq) {
            let param_tys: Vec<Ty> = members.iter().map(|(_, jt)| *jt).collect();
            // An annotation class's members carry no declaration annotations of their own.
            constructor_defaults::emit_ctor_default_stub(
                ir, &fq, facade, &param_tys, defaults, false, &mut cw, env,
            );
        }
    }

    // Per-member accessor `x()T`: return this.x.
    for (name, jt) in &members {
        let mut g = CodeBuilder::new(1);
        g.aload(0);
        let fref = cw.fieldref(&fq, name, &type_descriptor(*jt));
        g.getfield(fref, slot_words(*jt) as i32);
        emit_return(*jt, &mut g);
        // PUBLIC | FINAL | SYNTHETIC — see the field flags above.
        let accessor_desc = format!("(){}", type_descriptor(*jt));
        finish_code::<0x1011>(&mut cw, name, &accessor_desc, &mut g, 1);
        // kotlinc names `this` in every member's `LocalVariableTable`, generated class or not.
        cw.set_method_debug(
            name,
            &accessor_desc,
            None,
            &[("this".to_string(), format!("L{fq};"), 0)],
        );
    }

    emit_annotation_equals(env, &mut cw, &fq, iface, &members);
    emit_annotation_hashcode(ir, &mut cw, &fq, &members);
    emit_annotation_tostring(&mut cw, &fq, iface, &members);
    // annotationType(): return <iface>.class. LAST, after the `Object` overrides — kotlinc's member
    // order, and the method table is part of the class file, so emitting it beside the member
    // accessors diverged from the reference on every annotation that is instantiated.
    {
        let mut m = CodeBuilder::new(1);
        m.ldc_class(iface, &mut cw);
        m.areturn();
        // SYNTHETIC like the accessors: `annotationType()` is the `Annotation` contract, not a
        // source declaration.
        finish_code::<0x1011>(&mut cw, "annotationType", "()Ljava/lang/Class;", &mut m, 1);
        cw.set_method_debug(
            "annotationType",
            "()Ljava/lang/Class;",
            None,
            &[("this".to_string(), format!("L{fq};"), 0)],
        );
    }
    cw.finish()
}

/// `equals(Object)Z` for an annotation impl: `o` must be an instance of the annotation interface and every
/// member must be equal (arrays compared by content via `Arrays.equals`; `float`/`double` via their
/// wrappers' `equals` so `NaN`==`NaN` and `-0.0`!=`0.0` per the annotation contract; other references via
/// `Object.equals`). One `false` exit label.
fn emit_annotation_equals(
    env: &EmitEnv,
    cw: &mut ClassWriter,
    fq: &str,
    iface: &str,
    members: &[(String, Ty)],
) {
    // kotlinc's shape. Three things differ from the obvious encoding, all visible in the class file:
    //
    // - Every check returns EARLY (`ifne L; iconst_0; ireturn; L:`) instead of branching to one
    //   shared exit label, so the body carries one frame per member.
    // - BOTH sides are read through the annotation INTERFACE (`aload_0; checkcast I;
    //   invokeinterface I.m()`), this object's own side included — never `getfield`. An annotation's
    //   contract is its interface, which a proxy or another implementation satisfies too.
    // - The comparison is per type: `if_icmpeq` for the int-likes, `lcmp`, `Float`/`Double.compare`
    //   (not the wrapper's `equals`), `if_acmpeq` for an ENUM member, `Arrays.equals` for an array,
    //   and `Intrinsics.areEqual` for every other reference.
    let mut cb = CodeBuilder::new(2); // this=0, o=1
    cb.ensure_locals(3); // +o-as-iface at local 2
    let icls = cw.class_ref(iface);

    let typed = cb.new_label();
    cb.aload(1);
    cb.instance_of(icls);
    cb.ifne(typed);
    cb.push_int(0, cw);
    cb.ireturn();
    cb.bind(typed);
    cb.aload(1);
    cb.checkcast(icls);
    cb.astore(2);
    for (name, jt) in members {
        let aref = cw.interface_methodref(iface, name, &format!("(){}", type_descriptor(*jt)));
        let next = cb.new_label();
        cb.aload(0);
        cb.checkcast(icls);
        cb.invokeinterface(aref, 0, slot_words(*jt) as i32);
        cb.aload(2);
        cb.invokeinterface(aref, 0, slot_words(*jt) as i32);
        match *jt {
            Ty::Int | Ty::Short | Ty::Byte | Ty::Char | Ty::Boolean => cb.if_icmpeq(next),
            Ty::Long => {
                cb.lcmp();
                cb.ifeq(next);
            }
            Ty::Float | Ty::Double => {
                let (wrap, pd) = prim_wrapper(*jt).unwrap();
                let compare = cw.methodref(wrap, "compare", &format!("({pd}{pd})I"));
                cb.invokestatic(compare, 2 * slot_words(*jt) as i32, 1);
                cb.ifeq(next);
            }
            _ if jt.is_array() => {
                let arr_desc = arrays_param_desc(*jt);
                let eq = cw.methodref(
                    "java/util/Arrays",
                    "equals",
                    &format!("({arr_desc}{arr_desc})Z"),
                );
                cb.invokestatic(eq, 2, 1);
                cb.ifne(next);
            }
            // An ENUM constant is a singleton, so kotlinc compares one by IDENTITY. Asked of the
            // symbol source rather than of this file's classes, so a classpath enum answers the same
            // as a source-declared one.
            other
                if other.obj_internal().is_some_and(|internal| {
                    env.signature_symbols
                        .classifier(internal)
                        .is_some_and(|classifier| classifier.is_enum())
                }) =>
            {
                cb.if_acmpeq(next)
            }
            _ => {
                let eq = cw.methodref(
                    "kotlin/jvm/internal/Intrinsics",
                    "areEqual",
                    "(Ljava/lang/Object;Ljava/lang/Object;)Z",
                );
                cb.invokestatic(eq, 2, 1);
                cb.ifne(next);
            }
        }
        cb.push_int(0, cw);
        cb.ireturn();
        cb.bind(next);
    }
    cb.push_int(1, cw);
    cb.ireturn();
    cb.link();
    let locals = [
        ("this".to_string(), format!("L{fq};"), 0u16),
        ("other".to_string(), "Ljava/lang/Object;".to_string(), 1),
    ];
    // Before the method, so the local names precede the class constants the frame computation
    // interns — kotlinc's writer visits the locals first. See `reserve_method_lvt`.
    cw.reserve_method_lvt(&locals);
    cw.add_method(0x0011, "equals", "(Ljava/lang/Object;)Z", &cb);
    cw.set_method_debug("equals", "(Ljava/lang/Object;)Z", None, &locals);
    // `equals(Object?)` accepts null and answers false, so its parameter is `@Nullable` — kotlinc
    // stamps it, and a Java caller reads the contract from it.
    if annotation_impl_carries_nullability() {
        cw.set_method_nullability(
            "equals",
            "(Ljava/lang/Object;)Z",
            None,
            &[Some("Lorg/jetbrains/annotations/Nullable;")],
        );
    }
}

/// `hashCode()I` for an annotation impl: the contract sum of `(127 * memberName.hashCode()) ^
/// memberValue.hashCode()` over members (arrays via `Arrays.hashCode`, primitives via their wrappers'
/// static `hashCode`). Straight-line (no frames).
fn emit_annotation_hashcode(ir: &IrFile, cw: &mut ClassWriter, fq: &str, members: &[(String, Ty)]) {
    // kotlinc's shape, instruction for instruction. Two things are deliberate rather than
    // incidental, because both are visible in the class file even though neither changes the value:
    //
    // - The member-name weight is COMPUTED (`ldc "v"; String.hashCode(); bipush 127; imul`), not
    //   folded into a constant. Folding it produced the same number and a different method body.
    // - Every PRIMITIVE goes through its wrapper's static `hashCode` — including `int`, whose value
    //   already IS its hash. kotlinc emits `Integer.hashCode(I)` there regardless.
    //
    // The accumulator lives in local 1: each member xors its weighted name hash with its value hash,
    // adds that into the accumulator (from the second member on) and stores it back.
    let accumulates = members.len() > 1;
    let mut cb = CodeBuilder::new(if accumulates { 2 } else { 1 });
    let string_hash = cw.methodref("java/lang/String", "hashCode", "()I");
    for (index, (name, jt)) in members.iter().enumerate() {
        cb.push_string(name, cw);
        cb.invokevirtual(string_hash, 0, 1);
        cb.push_int(127, cw);
        cb.imul();
        let fref = cw.fieldref(fq, name, &type_descriptor(*jt));
        cb.aload(0);
        cb.getfield(fref, slot_words(*jt) as i32);
        match *jt {
            _ if jt.is_array() => {
                let ad = arrays_param_desc(*jt);
                let hc = cw.methodref("java/util/Arrays", "hashCode", &format!("({ad})I"));
                cb.invokestatic(hc, 1, 1);
            }
            other => match prim_wrapper(other) {
                Some((wrap, pd)) => {
                    let hc = cw.methodref(wrap, "hashCode", &format!("({pd})I"));
                    cb.invokestatic(hc, slot_words(other) as i32, 1);
                }
                None => {
                    // The DECLARED class owns the call (`E.hashCode`, `String.hashCode`) — kotlinc
                    // resolves it against the member's static type, not `Object`. An INTERFACE-typed
                    // member (a nested annotation) cannot: `invokevirtual` on an interface type is
                    // illegal, so kotlinc falls back to `Object.hashCode` there, and so does this.
                    let owner = other
                        .obj_internal()
                        .map(|internal| {
                            crate::jvm::names::classfile_internal_name(&internal.render())
                        })
                        .filter(|internal| {
                            !ir.classes.iter().any(|class| {
                                class.fq_name() == *internal
                                    // An annotation class IS emitted as an interface.
                                    && (class.is_interface || class.is_annotation)
                            })
                        })
                        .unwrap_or_else(|| "java/lang/Object".to_string());
                    let hc = cw.methodref(&owner, "hashCode", "()I");
                    cb.invokevirtual(hc, 0, 1);
                }
            },
        }
        cb.ixor();
        if index > 0 {
            cb.iadd();
        }
        // The accumulator local exists only when there is something to accumulate: a SINGLE-member
        // annotation leaves its one value on the stack and returns it, which is kotlinc's shape.
        if accumulates {
            cb.istore(1);
            cb.iload(1);
        }
    }
    // An annotation with no members hashes to 0.
    if members.is_empty() {
        cb.push_int(0, cw);
    }
    cb.ireturn();
    let max_locals = if accumulates { 2 } else { 1 };
    finish_code::<0x0011>(cw, "hashCode", "()I", &mut cb, max_locals);
}

/// `toString()` for an annotation impl: `@<fqName>(m1=v1, m2=v2, …)` built with a `StringBuilder` (arrays
/// rendered via `Arrays.toString`). Straight-line (no frames).
fn emit_annotation_tostring(cw: &mut ClassWriter, fq: &str, iface: &str, members: &[(String, Ty)]) {
    let mut cb = CodeBuilder::new(1);
    // A MEMBERLESS annotation renders to a constant, so kotlinc emits no `StringBuilder` at all.
    if members.is_empty() {
        cb.push_string(&format!("@{}()", iface.replace('/', ".")), cw);
        cb.areturn();
        finish_code::<0x0011>(cw, "toString", "()Ljava/lang/String;", &mut cb, 1);
        cw.set_method_debug(
            "toString",
            "()Ljava/lang/String;",
            None,
            &[("this".to_string(), format!("L{fq};"), 0)],
        );
        return;
    }
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
    for (i, (name, jt)) in members.iter().enumerate() {
        // Adjacent literals are ONE `ldc`: the class prefix runs into the first member's name
        // (`"@Mk(v="`), and each later member's separator into its own (`", s="`). kotlinc builds the
        // constant that way, so emitting `"@Mk("` and `"v="` as two appends diverged on every
        // annotation that is instantiated.
        append_lit(
            &mut cb,
            cw,
            &if i == 0 {
                format!("@{}({name}=", iface.replace('/', "."))
            } else {
                format!(", {name}=")
            },
        );
        let fref = cw.fieldref(fq, name, &type_descriptor(*jt));
        match *jt {
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
    // The closing paren is a CHAR append, not a one-character String: kotlinc emits
    // `bipush 41; append(C)`.
    cb.push_int(b')' as i32, cw);
    let append_char = cw.methodref(sb, "append", "(C)Ljava/lang/StringBuilder;");
    cb.invokevirtual(append_char, 1, 1);
    let to_str = cw.methodref(sb, "toString", "()Ljava/lang/String;");
    cb.invokevirtual(to_str, 0, 1);
    cb.areturn();
    finish_code::<0x0011>(cw, "toString", "()Ljava/lang/String;", &mut cb, 1);
    cw.set_method_debug(
        "toString",
        "()Ljava/lang/String;",
        None,
        &[("this".to_string(), format!("L{fq};"), 0)],
    );
    // `toString()` returns a non-null String, and kotlinc stamps the synthesized `@NotNull` on it.
    // The member ACCESSORS carry none, even the reference-typed ones — measured, not assumed.
    if annotation_impl_carries_nullability() {
        cw.set_method_nullability(
            "toString",
            "()Ljava/lang/String;",
            Some("Lorg/jetbrains/annotations/NotNull;"),
            &[],
        );
    }
}
