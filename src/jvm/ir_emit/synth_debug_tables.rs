//! Debug tables for primary constructors and synthesized class members.

use super::*;

type VcDebugMethod = (String, String, Vec<(String, String, u16)>);

pub(super) fn attach_synth_debug_tables(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    cw: &mut ClassWriter,
    param_assertions: bool,
    // The primary constructor method emission actually produced and the source-mapped body offset
    // it reached after any parameter guards. Neither the physical descriptor nor the bytecode
    // position may be reconstructed later from fields or semantic constructor arguments.
    primary_ctor_debug: Option<(&str, u16)>,
    // Extra ctor LineNumberTable entries (body-property initializers + the trailing `return`), with
    // their real pcs captured during emission. Empty ⇒ the ctor gets kotlinc's single entry.
    ctor_lines: &[(u16, u32)],
    init_locals: &[(u16, u16, u16, String, String)],
) {
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
    // `aload <slot>` byte length: 1 (aload_0..3), 2 (aload u1), or 4 (wide aload u2). Synthesized
    // setter debug still uses this until those accessors carry their own emission provenance too.
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
    // A data class's `copy` parameters are exactly its property-backed constructor parameters.
    // This is not the primary constructor's physical descriptor (plain parameters may also exist
    // there), so keep the two identities deliberately separate.
    // Primary constructor: `this` + one local per ctor parameter (a property-backed param). An
    // `enum class`'s ctor is `(String name, int ordinal, …declared params)`: kotlinc prepends the two
    // synthetic `Enum` parameters and names them `$enum$name` / `$enum$ordinal` in the LVT.
    let is_enum = c.is_enum;
    let mut ctor_locals = vec![("this".to_string(), this_desc.clone(), 0u16)];
    let mut slot = 1u16;
    if is_enum {
        ctor_locals.push((
            "$enum$name".to_string(),
            "Ljava/lang/String;".to_string(),
            slot,
        ));
        ctor_locals.push(("$enum$ordinal".to_string(), "I".to_string(), slot + 1));
        slot += 2;
    }
    // Before Kotlin 2.4.20 an anonymous context parameter has no LVT row; since then its generated
    // reflection/assertion label names the physical constructor local too.
    let constructor_locals = crate::jvm::parameter_names::constructor_local_variables(ir, c);
    for (argument, name) in c.ctor_args.iter().zip(constructor_locals) {
        if let Some(name) = name {
            ctor_locals.push((name, local_variable_desc(argument.ty), slot));
        }
        slot += slot_size(argument.ty);
    }
    let this_only = [("this".to_string(), this_desc.clone(), 0u16)];
    // kotlinc maps the `super()` call to where the DECLARATION starts — annotations included — and
    // the ctor's trailing `return` (pushed into `ctor_lines` by the emitter) back to the class
    // HEADER line. The two coincide unless an annotation sits on its own line above the header.
    let ctor_start_line = if c.decl_start_line == 0 {
        line
    } else {
        c.decl_start_line
    };
    if let Some((ctor_desc, ctor_pc)) = primary_ctor_debug {
        cw.reserve_ranged_local_names(init_locals);
        cw.set_method_debug(
            "<init>",
            ctor_desc,
            Some((ctor_pc, ctor_start_line)),
            &ctor_locals,
        );
        cw.prepend_ranged_locals("<init>", ctor_desc, init_locals);
        if !ctor_lines.is_empty() {
            let mut entries = vec![(ctor_pc, ctor_start_line)];
            entries.extend_from_slice(ctor_lines);
            // kotlinc never emits two consecutive entries for the same line — a run of stores on the
            // class-declaration line (a single-line `class C(val a: Int)`) collapses to one entry.
            entries.dedup_by_key(|(_, l)| *l);
            cw.set_method_lines("<init>", ctor_desc, &entries);
        }
    }
    // A marker accessor gets the same locals as the primary constructor plus its synthetic marker.
    if has_ctor_marker_accessor(ir, c) {
        const MARKER: &str = "Lkotlin/jvm/internal/DefaultConstructorMarker;";
        let mut acc_locals = ctor_locals.clone();
        let marker_slot = c
            .fields
            .iter()
            .take(c.ctor_param_count as usize)
            .map(|f| slot_size(f.ty))
            .sum::<u16>()
            + 1;
        acc_locals.push((
            "$constructor_marker".to_string(),
            MARKER.to_string(),
            marker_slot,
        ));
        let acc_desc = format!("({}{MARKER})V", ctor_field_descs(c));
        cw.set_method_debug("<init>", &acc_desc, None, &acc_locals);
    }
    // Synthesized property setters use the declaration's recorded nullability policy. This is
    // independent of the constructor's exact `IrCtorArg.check` facts above: a value class may omit
    // its private-constructor guard while its public mutable-property setter still requires one.
    let is_nonnull_ref =
        |name: &str, ty: Ty| -> bool { is_nonnull_reference_field(ir, &c.fq_name(), name, ty) };
    // Property accessors: getter has only `this`; a `var` setter also has its value parameter (named
    // `<set-?>` by kotlinc), guarded when the property type is a non-null reference.
    for (field_index, f) in c.fields.iter().enumerate() {
        // An accessor represented as a real function carries its own debug contract. Decide the
        // getter and setter independently: a `var` can declare one and retain the synthesized other.
        // The plugin-generated `descriptor` getter intentionally has NO line table, which this
        // class-level synthesis would otherwise overwrite with the declaration line.
        let declared_property = c
            .properties
            .iter()
            .find(|property| property.backing_field == Some(field_index as u32));
        // A CTOR-parameter property's accessors sit on the class-declaration line; a BODY property's
        // sit on its own `val`/`var` line.
        let pline = ir
            .prop_decl_lines
            .get(&(c.fq_name_id(), f.name.clone()))
            .copied()
            .filter(|&l| l != 0)
            .unwrap_or(line);
        let (g, s) = accessor_jvm_names(c, &f.name);
        if declared_property.is_none_or(|property| property.getter.is_none()) {
            cw.set_method_debug(
                &g,
                &format!("(){}", desc(f.ty)),
                Some((0, pline)),
                &this_only,
            );
        }
        if !f.is_final() && declared_property.is_none_or(|property| property.setter.is_none()) {
            let pd = desc(f.ty);
            // The setter's value param is always slot 1 (`this`=0): guard = `aload_1`(1) + the
            // `<set-?>` String's real ldc width + invokestatic(3).
            let set_pc = if param_assertions && is_nonnull_ref(&f.name, f.ty) {
                aload_len(1) + cw.string_ldc_len("<set-?>").unwrap_or(2) + 3
            } else {
                0
            };
            cw.set_method_debug(
                &s,
                &format!("({pd})V"),
                Some((set_pc, pline)),
                &[
                    ("this".to_string(), this_desc.clone(), 0),
                    ("<set-?>".to_string(), pd, 1),
                ],
            );
        }
    }
    // HOISTED companion properties: no companion field, but the delegating accessors get the same
    // debug shape kotlinc gives ordinary accessors (getter: `this` only; a `var` setter also has
    // its `<set-?>` value parameter, guarded when the property type is a non-null reference).
    // A `@JvmField` property has NO accessors — nothing to describe.
    for (property_index, property) in c.properties.iter().enumerate() {
        if property.backing_field.is_some()
            || static_fields::jvm_field_static_for(ir, c, property_index)
        {
            continue;
        }
        let Some(hoisted) = static_fields::hoisted_static_for(ir, c, property_index) else {
            continue;
        };
        let pline = if property.decl_line != 0 {
            property.decl_line
        } else {
            line
        };
        let pd = crate::jvm::names::type_descriptor(jvm_declared_ty(&hoisted.ty));
        let (g, s) = accessor_jvm_names(c, &property.name);
        cw.set_method_debug(&g, &format!("(){pd}"), Some((0, pline)), &this_only);
        if hoisted.is_var {
            let set_pc = if param_assertions && is_nonnull_ref(&property.name, hoisted.ty) {
                aload_len(1) + cw.string_ldc_len("<set-?>").unwrap_or(2) + 3
            } else {
                0
            };
            cw.set_method_debug(
                &s,
                &format!("({pd})V"),
                Some((set_pc, pline)),
                &[
                    ("this".to_string(), this_desc.clone(), 0),
                    ("<set-?>".to_string(), pd.clone(), 1),
                ],
            );
        }
    }
    // A companion OUTER's `access$…$cp` bridges: kotlinc maps each to the CLASS declaration line
    // (getter bridges carry only the LineNumberTable; the setter bridge also names its `<set-?>`
    // value parameter).
    for s in ir
        .statics
        .iter()
        .enumerate()
        .filter(|(index, s)| {
            ir.is_jvm_companion_hoisted_static(*index as u32)
                && !ir.is_jvm_field_static(*index as u32)
                && s.owner_matches(&c.fq_name())
        })
        .map(|(_, s)| s)
    {
        let pd = crate::jvm::names::type_descriptor(jvm_declared_ty(&s.ty));
        let getter_bridge = format!("access${}$cp", crate::names::property_getter_name(&s.name));
        cw.set_method_debug(&getter_bridge, &format!("(){pd}"), Some((0, line)), &[]);
        if s.is_var {
            let setter_bridge =
                format!("access${}$cp", crate::names::property_setter_name(&s.name));
            cw.set_method_debug(
                &setter_bridge,
                &format!("({pd})V"),
                Some((0, line)),
                &[("<set-?>".to_string(), pd.clone(), 0)],
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
                        (
                            crate::jvm::parameter_names::value_class_equals_operand(1).to_string(),
                            u.clone(),
                            0,
                        ),
                        (
                            crate::jvm::parameter_names::value_class_equals_operand(2).to_string(),
                            u.clone(),
                            w,
                        ),
                    ],
                ),
            ];
            for (name, d, locals) in &vc_methods {
                cw.set_method_debug(name, d, None, locals);
            }
        }
    }
}
