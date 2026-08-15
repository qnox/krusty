//! Build the `@kotlin.Metadata` `d1` protobuf payload + `d2` string table for a file facade with
//! top-level functions. Schema/field numbers per `core/metadata/src/metadata.proto`; builtin type
//! names use `predefinedIndex` into `JvmNameResolverBase.PREDEFINED_STRINGS` (see METADATA_NOTES.md).

use crate::metadata::type_encoder::{
    encode_metadata_type_parameter, encode_type, encode_type_parameter,
    semantic_named_type_parameters, type_parameters, MetadataTypeParameter, StringTable,
    TypeParameters,
};
use crate::metadata::{property_flags, protobuf::Pb};
use crate::types::Ty;

/// One top-level function to describe in the package metadata.
pub struct FnMeta {
    pub name: String,
    pub params: Vec<(String, Ty)>,
    pub ret: Ty,
    /// Position of the declaration in the FILE (see [`PropMeta::decl_order`]).
    pub decl_order: usize,
    /// Argument-less BINARY/RUNTIME-retained annotations applied to the function, recorded as
    /// `Function.annotation` (field 12) `Annotation { id }` entries — how a separate compilation reads
    /// resolution-affecting markers like `@kotlin.internal.LowPriorityInOverloadResolution` back from
    /// the classpath. Also sets `Function.flags` `HAS_ANNOTATIONS` (bit 0). SOURCE-retained annotations
    /// never appear here (kotlinc drops them from metadata too).
    pub annotations: Vec<crate::types::TypeName>,
    /// Extension-receiver type (`Function.receiver_type` = 5), `Some` for an extension function. Recorded
    /// SEPARATELY from `params` (the LOGICAL value params, receiver excluded), so a reader recovers the
    /// extension's true source arity — `fun T.f(a)` is one value param, not two. `None` for a plain fn.
    pub receiver: Option<Ty>,
    /// Per-parameter `DECLARES_DEFAULT_VALUE` flags (parallel to `params`; empty = none default). Sets
    /// `ValueParameter.flags` bit 1 so a cross-module caller may OMIT a defaulted argument (the reader's
    /// `metadata_param_defaults` recovers it). A short/empty vec leaves the remaining params required.
    pub param_defaults: Vec<bool>,
    /// `suspend fun` — sets `Function.flags` `IS_SUSPEND` (bit 13). Its `params`/`ret` are the LOGICAL
    /// signature (no `Continuation`, the source return), exactly as kotlinc records in `@Metadata`.
    pub suspend: bool,
    /// The PHYSICAL JVM method descriptor (`(…,Lkotlin/coroutines/Continuation;)Ljava/lang/Object;` for
    /// a suspend fn) recorded as the `JvmMethodSignature` extension, so a kotlinc reader maps the
    /// metadata function to its bytecode method. `None` omits the extension.
    pub jvm_desc: Option<String>,
    /// The PHYSICAL JVM method NAME (`JvmMethodSignature.name`, f1) when it differs from the Kotlin
    /// one — a value-class-parametered function's mangled `taggedOnly-rnqsQGE`. `None` ⇒ omitted
    /// (the name defaults to the function's), kotlinc's usual shape. Only read when `jvm_desc` is
    /// recorded.
    pub jvm_name: Option<String>,
    /// `inline fun` — sets `Function.flags` `IS_INLINE` (bit 10) so a reader resolves the function
    /// as inline (splice candidate), not a plain callable.
    pub inline: bool,
    /// Declared type parameters in order `(name, reified)` — emitted as the `Function.type_parameter`
    /// table (field 4); their indices are the `Type.type_parameter` ids used by generic
    /// receiver/parameter/return types and `is`-conclusions in the contract.
    pub type_params: Vec<(String, bool)>,
    /// Semantic identities parallel to `type_params`.
    pub semantic_type_params: Vec<String>,
    /// Declared upper bounds, parallel to `type_params`.
    pub type_param_bounds: Vec<Vec<Ty>>,
    /// The function's decoded contract, emitted as `Function.contract` (field 32) so a separate
    /// compilation applies its effects at call sites. `None` when the function declares none.
    pub contract: Option<std::sync::Arc<crate::contracts::Contract>>,
    /// Number of LEADING entries in `params` that are context parameters (`context(a: A) fun f()`).
    /// kotlinc lowers them to leading value parameters; in metadata they ride
    /// `Function.context_parameter` (field 13) so a caller fills them implicitly from the
    /// enclosing context instead of positionally.
    pub context_count: usize,
    /// Index into `params` of a `vararg` parameter. Emits `ValueParameter.vararg_element_type`
    /// (field 4) carrying the ELEMENT type next to the declared array type — the only place
    /// vararg-ness survives (`ACC_VARARGS` is not part of `@Metadata`), so a reader admits
    /// element-form and spread arguments instead of demanding one literal array.
    pub vararg_index: Option<usize>,
    /// Declaration visibility — `Function.flags` visibility bits (INTERNAL=0, PRIVATE=1,
    /// PUBLIC=3); an `internal fun`'s flags word differs from the omitted public-final default (6),
    /// so it is written explicitly and a consuming module enforces the boundary.
    pub visibility: crate::types::Visibility,
}

/// `ValueParameter.flags` bit for `DECLARES_DEFAULT_VALUE` (bit 1; `HAS_ANNOTATIONS` is bit 0).
const DECLARES_DEFAULT_VALUE_BIT: u64 = 1 << 1;

fn type_pb(st: &mut StringTable, t: Ty) -> Pb {
    encode_type(st, t, &TypeParameters::new())
        .unwrap_or_else(|error| panic!("invalid emitted metadata type: {error}"))
}

/// A `Type` message for `t`, resolving type-parameter NAMES to their `Function.type_parameter`
/// table ids via `tps`. Handles generic class arguments (`Type.argument` = 2), nullability
/// (`Type.nullable` = 3), class types (`Type.class_name` = 6), and type parameters
/// (`Type.type_parameter` = 7).
fn type_pb_generic(st: &mut StringTable, t: Ty, tps: &TypeParameters) -> Pb {
    encode_type(st, t, tps).unwrap_or_else(|error| panic!("invalid emitted metadata type: {error}"))
}

/// Serialize a decoded contract as a `Contract` message (`repeated Effect effect` = 1), the exact
/// mirror of the reader in `src/jvm/metadata.rs`. `tps` maps the function's type-parameter names
/// to their table ids (for an `is R` conclusion).
fn contract_pb(
    st: &mut StringTable,
    contract: &crate::contracts::Contract,
    tps: &TypeParameters,
) -> Pb {
    let mut p = Pb::new();
    for e in &contract.effects {
        p.repeated_message(1, &effect_pb(st, e, tps));
    }
    p
}

fn effect_pb(st: &mut StringTable, e: &crate::contracts::Effect, tps: &TypeParameters) -> Pb {
    use crate::contracts::Effect;
    let mut p = Pb::new();
    match e {
        Effect::CallsInPlace { param, kind } => {
            p.field_varint(1, 1); // Effect.effect_type = CALLS
            let arg = expression_param_ref_pb(*param);
            p.repeated_message(2, &arg); // Effect.effect_constructor_argument
                                         // `Unknown` has no wire form — emit OMITS the kind.
            if let Some(k) = kind.to_wire() {
                p.field_varint(4, k); // Effect.kind
            }
        }
        Effect::Returns(rv) => write_returns_effect(&mut p, st, rv, None, tps),
        Effect::ConditionalReturns {
            returns,
            conclusion,
        } => write_returns_effect(&mut p, st, returns, Some(conclusion), tps),
    }
    p
}

fn write_returns_effect(
    p: &mut Pb,
    st: &mut StringTable,
    rv: &crate::contracts::ReturnsValue,
    conclusion: Option<&crate::contracts::Condition>,
    tps: &TypeParameters,
) {
    use crate::contracts::ReturnsValue;
    match rv {
        ReturnsValue::NotNull => {
            p.field_varint(1, 2); // Effect.effect_type = RETURNS_NOT_NULL
        }
        rv => {
            p.field_varint(1, 0); // Effect.effect_type = RETURNS_CONSTANT
            let constant = match rv {
                ReturnsValue::Bool(b) => Some(if *b { 0 } else { 1 }),
                ReturnsValue::Null => Some(2),
                _ => None,
            };
            if let Some(cv) = constant {
                let mut arg = Pb::new();
                arg.field_varint(3, cv); // Expression.constant_value
                p.repeated_message(2, &arg);
            }
        }
    }
    if let Some(c) = conclusion {
        let cb = condition_pb(st, c, tps);
        p.field_message(3, &cb); // Effect.conclusion_of_conditional_effect
    }
}

/// An `Expression` naming a parameter: `value_parameter_reference` (the wire convention lives
/// on [`crate::contracts::ParamRef`]).
fn expression_param_ref_pb(param: crate::contracts::ParamRef) -> Pb {
    let mut p = Pb::new();
    p.field_varint(2, param.to_wire());
    p
}

fn condition_pb(st: &mut StringTable, c: &crate::contracts::Condition, tps: &TypeParameters) -> Pb {
    use crate::contracts::{Condition, ConditionType};
    let mut p = Pb::new();
    match c {
        Condition::IsNull { param, negated } => {
            p.field_varint(1, 2 | u64::from(*negated)); // flags: null-check predicate | negated
            p.field_varint(2, param.to_wire());
        }
        Condition::IsType { param, ty, negated } => {
            if *negated {
                p.field_varint(1, 1);
            }
            p.field_varint(2, param.to_wire());
            let ConditionType::Metadata(ty) = ty else {
                panic!("unresolved source type reached metadata contract emission");
            };
            let it = type_pb_generic(st, *ty, tps);
            p.field_message(4, &it); // Expression.is_instance_type
        }
        Condition::BoolParam(param) => {
            p.field_varint(2, param.to_wire());
        }
        Condition::Const(b) => {
            p.field_varint(3, if *b { 0 } else { 1 }); // Expression.constant_value TRUE/FALSE
        }
        Condition::And(..) | Condition::Or(..) => {
            // Flatten the formula; every operand rides the repeated field (the reader handles
            // both the embedded-first-operand optimization and this plain form).
            let and = matches!(c, Condition::And(..));
            let field = if and { 6 } else { 7 };
            let mut flat = Vec::new();
            flatten_condition(c, and, &mut flat);
            for operand in flat {
                let ob = condition_pb(st, operand, tps);
                p.repeated_message(field, &ob);
            }
        }
    }
    p
}

fn flatten_condition<'a>(
    c: &'a crate::contracts::Condition,
    and: bool,
    out: &mut Vec<&'a crate::contracts::Condition>,
) {
    use crate::contracts::Condition;
    match (and, c) {
        (true, Condition::And(l, r)) => {
            flatten_condition(l, true, out);
            flatten_condition(r, true, out);
        }
        (false, Condition::Or(l, r)) => {
            flatten_condition(l, false, out);
            flatten_condition(r, false, out);
        }
        _ => out.push(c),
    }
}

fn function_pb(st: &mut StringTable, f: &FnMeta) -> Pb {
    let mut p = Pb::new();
    // Function.flags = 9 — emitted only when non-default (`6` = public final is the proto default).
    // Bit 13 = IS_SUSPEND, bit 10 = IS_INLINE; the visibility bits ride along (final modality = 0).
    // The field is elided at the public-final default (6) — an `internal fun`'s 0 is explicit.
    let vis: u64 = match f.visibility {
        crate::types::Visibility::Internal => 0,
        crate::types::Visibility::Private => 1,
        crate::types::Visibility::Protected => 2,
        crate::types::Visibility::Public => 3,
    };
    let flags = u64::from(!f.annotations.is_empty())
        | (vis << 1)
        | (u64::from(f.suspend) << 13)
        | (u64::from(f.inline) << 10);
    p.field_varint(2, st.local(&f.name) as u64); // Function.name = 2
                                                 // The function's type-parameter table (Function.type_parameter = 4): indices are the
                                                 // `Type.type_parameter` ids generic types and contract conclusions reference.
    assert_eq!(
        f.semantic_type_params.len(),
        f.type_params.len(),
        "metadata function type parameters require semantic identities"
    );
    let semantic_names = f
        .semantic_type_params
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let tps = semantic_named_type_parameters(
        f.type_params.iter().map(|(name, _)| name.as_str()),
        semantic_names.into_iter(),
    );
    let ret = type_pb_generic(st, f.ret, &tps);
    p.field_message(3, &ret); // Function.return_type = 3
    for (id, (tp_name, reified)) in f.type_params.iter().enumerate() {
        let tp = encode_metadata_type_parameter(
            st,
            id,
            &MetadataTypeParameter {
                name: tp_name.clone(),
                reified: *reified,
                variance: crate::types::TypeVariance::Invariant,
                upper_bounds: f.type_param_bounds.get(id).cloned().unwrap_or_default(),
            },
            &tps,
        )
        .unwrap_or_else(|error| panic!("invalid emitted metadata type parameter: {error}"));
        p.repeated_message(4, &tp); // Function.type_parameter = 4
    }
    // Function.receiver_type = 5 (extension functions only) — between return_type and value_parameter,
    // matching kotlinc's ascending field order. Its presence marks the function an extension.
    if let Some(recv) = f.receiver {
        let rt = type_pb_generic(st, recv, &tps);
        p.field_message(5, &rt);
    }
    for (i, (pname, pty)) in f.params.iter().enumerate() {
        let mut vp = Pb::new();
        // ValueParameter.flags = 1 (before name, matching kotlinc's field order): bit 1 =
        // DECLARES_DEFAULT_VALUE, set when this parameter has a default so a caller may omit it.
        if f.param_defaults.get(i).copied().unwrap_or(false) {
            vp.field_varint(1, DECLARES_DEFAULT_VALUE_BIT);
        }
        vp.field_varint(2, st.local(pname) as u64); // ValueParameter.name = 2
        let ty = type_pb_generic(st, *pty, &tps);
        vp.field_message(3, &ty); // ValueParameter.type = 3
                                  // A `vararg` parameter records its ELEMENT type as `vararg_element_type` (field 4) —
                                  // kotlinc's declared type stays the array.
        if f.vararg_index == Some(i) {
            let elem = pty
                .array_elem()
                .or_else(|| pty.type_args().first().copied());
            if let Some(elem) = elem {
                let et = type_pb_generic(st, elem, &tps);
                vp.field_message(4, &et); // ValueParameter.vararg_element_type = 4
            }
        }
        if i < f.context_count {
            // Leading context parameters → Function.context_parameter = 13 (filled implicitly
            // by callers), NOT the positional value_parameter list.
            p.repeated_message(13, &vp);
        } else {
            p.repeated_message(6, &vp); // Function.value_parameter = 6
        }
    }
    // Function.flags = 9 — in kotlinc's ASCENDING field order (after name/types/params), emitted
    // only when non-default (`6` = public final is the proto default; an `internal fun`'s 0 is
    // explicit). Bit 13 = IS_SUSPEND, bit 10 = IS_INLINE; visibility bits ride along.
    if flags != 0x06 {
        p.field_varint(9, flags);
    }
    // Applied annotations (Function.annotation = 12): `Annotation.id` (field 1) referencing the class
    // through the string table's DESC_TO_CLASS_ID form, exactly as kotlinc records e.g.
    // `@LowPriorityInOverloadResolution`.
    for &annotation in &f.annotations {
        let mut ab = Pb::new();
        ab.field_varint(1, u64::from(st.class_id(annotation)));
        p.repeated_message(12, &ab);
    }
    // The declared contract (Function.contract = 32) — `returns(…) implies …` / `callsInPlace`
    // effects a separate compilation applies at call sites.
    if let Some(contract) = &f.contract {
        let cb = contract_pb(st, contract, &tps);
        p.field_message(32, &cb);
    }
    // JvmProtoBuf.methodSignature extension (Function field 100): only the descriptor (field 2) — the
    // name defaults to the function's, exactly as kotlinc emits for a top-level function.
    if let Some(desc) = &f.jvm_desc {
        let mut sig = Pb::new();
        if let Some(name) = &f.jvm_name {
            sig.field_varint(1, st.local(name) as u64); // JvmMethodSignature.name = 1 (mangled)
        }
        sig.field_varint(2, st.local(desc) as u64); // JvmMethodSignature.desc = 2
        p.field_message(100, &sig); // Function.methodSignature = 100
    }
    p
}

/// A top-level property for the package metadata (`Package.property` = field 4).
pub struct PropMeta {
    pub name: String,
    pub ty: Ty,
    pub is_var: bool,
    /// The property's declared type parameters (`val <T> C<T>.value: T`). Their indices are the
    /// `Type.type_parameter` ids used by the receiver and return type.
    pub type_params: Vec<String>,
    /// Extension-receiver type (`Property.receiver_type` = 10), `Some` for an extension property —
    /// the same separation from the accessor's JVM parameters as [`FnMeta::receiver`].
    pub receiver: Option<Ty>,
    pub getter: (String, String),
    pub setter: Option<(String, String)>,
    /// A `const val`: kotlinc sets the CONST flag bit and records a field-only
    /// `JvmPropertySignature` (no accessor exists — reads inline the `ConstantValue`).
    pub is_const: bool,
    /// The initializer is a COMPILE-TIME CONSTANT (a non-null literal) — kotlinc's `hasConstant`
    /// property flag. A constructor-call or `null` initializer does not qualify; `is_const` implies it.
    pub has_constant: bool,
    /// Position of the declaration in the FILE (across kinds) — kotlinc interns package-member
    /// strings in SOURCE DECLARATION order even though the proto groups functions before
    /// properties, so the d2 indices only match when the string table is built in this order.
    pub decl_order: usize,
    /// Declaration visibility — `Property.flags` visibility bits (INTERNAL=0, PRIVATE=1, PUBLIC=3).
    /// An `internal val`'s public JVM getter must not leak the property: a consuming module reads
    /// the metadata visibility (kotlinc: `internal val` flags `8704` vs public `8710`).
    pub visibility: crate::types::Visibility,
}

/// A public top-level typealias declaration in package metadata.
pub struct TypeAliasMeta {
    pub name: String,
    pub target: Ty,
    /// Declared visibility — `TypeAlias.flags` (f1) carries it; elided at the public default so an
    /// `internal typealias` writes an explicit 0 and stays module-bound for consumers.
    pub visibility: crate::types::Visibility,
}

fn type_alias_pb(st: &mut StringTable, alias: &TypeAliasMeta) -> Pb {
    let mut p = Pb::new();
    let vis: u64 = match alias.visibility {
        crate::types::Visibility::Internal => 0,
        crate::types::Visibility::Private => 1,
        crate::types::Visibility::Protected => 2,
        crate::types::Visibility::Public => 3,
    };
    if vis != 3 {
        p.field_varint(1, vis << 1); // TypeAlias.flags = 1 (elided at the public default 6)
    }
    p.field_varint(2, st.local(&alias.name) as u64); // TypeAlias.name = 2
    let target = type_pb(st, alias.target);
    p.field_message(4, &target); // TypeAlias.underlying_type = 4
    p.field_message(6, &target); // TypeAlias.expanded_type = 6
    p
}

/// Package properties use the same schema word as class properties. A `var` adds mutability and a
/// setter; a `val` gains `HAS_CONSTANT` only when its initializer is a compile-time constant
/// (kotlinc: `val counter = 7` yes; a constructor call or the `null` literal no).
const PKG_VAL_FLAGS: u64 = property_flags::DEFAULT;
const PKG_VAR_FLAGS: u64 =
    property_flags::DEFAULT | property_flags::IS_VAR | property_flags::HAS_SETTER;

fn jvm_method_sig(st: &mut StringTable, name: &str, desc: &str) -> Pb {
    let mut p = Pb::new();
    p.field_varint(1, st.local(name) as u64); // JvmMethodSignature.name = 1
    p.field_varint(2, st.local(desc) as u64); // JvmMethodSignature.desc = 2
    p
}

fn property_pb(st: &mut StringTable, m: &PropMeta) -> Pb {
    let mut p = Pb::new();
    p.field_varint(2, st.local(&m.name) as u64); // Property.name = 2
    let tps = type_parameters(m.type_params.iter().map(String::as_str));
    let ret = type_pb_generic(st, m.ty, &tps);
    p.field_message(3, &ret); // Property.return_type = 3
    for (id, name) in m.type_params.iter().enumerate() {
        let tp = encode_type_parameter(st, id, name, false);
        p.repeated_message(4, &tp); // Property.type_parameter = 4
    }
    if let Some(recv) = m.receiver {
        let rt = type_pb_generic(st, recv, &tps);
        p.field_message(5, &rt); // Property.receiver_type = 5 (extension properties only)
    }
    let vis: u64 = match m.visibility {
        crate::types::Visibility::Internal => 0,
        crate::types::Visibility::Private => 1,
        crate::types::Visibility::Protected => 2,
        crate::types::Visibility::Public => 3,
    };
    let base = if m.is_var {
        PKG_VAR_FLAGS
    } else {
        PKG_VAL_FLAGS
            | if m.has_constant || m.is_const {
                property_flags::HAS_CONSTANT
            } else {
                0
            }
    };
    // A `const val` sets the CONST flag bit (kotlinc: public const `10758` = `8710 | 2048`).
    let const_bit = if m.is_const { 1 << 11 } else { 0 };
    let pflags = (base & !property_flags::VISIBILITY_MASK) | (vis << 1) | const_bit;
    // protobuf omits an optional field at its declared default — a plain `public val` with a
    // non-constant initializer records NO flags word, exactly like a class property.
    if pflags != property_flags::DEFAULT {
        p.field_varint(11, pflags); // Property.flags = 11
    }
    let mut jvm = Pb::new();
    jvm.field_message(1, &Pb::new()); // field (empty → derived)
                                      // A `const val` has NO accessor — reads inline the `ConstantValue`; kotlinc records the field
                                      // entry alone.
    if !m.is_const {
        let getter = jvm_method_sig(st, &m.getter.0, &m.getter.1);
        jvm.field_message(3, &getter);
        if let Some((sn, sd)) = &m.setter {
            let setter = jvm_method_sig(st, sn, sd);
            jvm.field_message(4, &setter);
        }
    }
    p.field_message(100, &jvm); // JvmProtoBuf.propertySignature = 100
    p
}

/// Build `(d1 bytes, d2 strings)` for a file facade. `d1 = delimited(StringTableTypes) + Package`.
pub fn build_package(
    funcs: &[FnMeta],
    props: &[PropMeta],
    aliases: &[TypeAliasMeta],
) -> (Vec<u8>, Vec<String>) {
    let mut st = StringTable::default();
    let mut package = Pb::new();
    // STRINGS INTERN IN SOURCE DECLARATION ORDER across kinds (a `const val` before a `fun`
    // interns first), while the proto still writes functions (f3) before properties (f4) — so the
    // d2 indices match kotlinc's. Build each sub-message in declaration order, emit by field.
    let mut build_order: Vec<(usize, bool, usize)> = funcs
        .iter()
        .enumerate()
        .map(|(index, f)| (f.decl_order, false, index))
        .chain(
            props
                .iter()
                .enumerate()
                .map(|(index, m)| (m.decl_order, true, index)),
        )
        .collect();
    build_order.sort();
    let mut fn_pbs: Vec<Option<Pb>> = (0..funcs.len()).map(|_| None).collect();
    let mut prop_pbs: Vec<Option<Pb>> = (0..props.len()).map(|_| None).collect();
    for (_, is_prop, index) in build_order {
        if is_prop {
            prop_pbs[index] = Some(property_pb(&mut st, &props[index]));
        } else {
            fn_pbs[index] = Some(function_pb(&mut st, &funcs[index]));
        }
    }
    for fp in fn_pbs.iter().flatten() {
        package.repeated_message(3, fp); // Package.function = 3
    }
    for pp in prop_pbs.iter().flatten() {
        package.repeated_message(4, pp); // Package.property = 4
    }
    for alias in aliases {
        let alias = type_alias_pb(&mut st, alias);
        package.repeated_message(5, &alias); // Package.type_alias = 5
    }
    let stt = st.serialize_types();

    // Empirically required leading byte (kotlinc emits it and reads its own output): the metadata
    // payload begins with 0x00 before the delimited StringTableTypes. (Confirmed via the round-trip
    // test — without it kotlinc reports "unresolved reference" for the functions.)
    let mut bytes = vec![0x00u8];
    let mut d1 = Pb::new();
    d1.varint(stt.as_bytes().len() as u64); // writeDelimitedTo: length prefix
    bytes.extend_from_slice(&d1.into_bytes());
    bytes.extend_from_slice(stt.as_bytes());
    bytes.extend_from_slice(package.as_bytes());
    (bytes, st.into_strings())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference bytes kotlinc 1.9.24 emits for `fun f(a: Int): Int = a` (METADATA_NOTES.md),
    /// minus the leading `0x00` (an artifact — kotlinc's own reader does parseDelimitedFrom first,
    /// so a real length-0 prefix would make it unreadable). We must match the rest exactly.
    /// Exact bytes kotlinc 1.9.24 emits for `fun f(a: Int): Int = a` (incl. the leading 0x00).
    const REF: &[u8] = &[
        0x00, 0x08, 0x0a, 0x00, 0x0a, 0x02, 0x10, 0x08, 0x0a, 0x00, 0x1a, 0x0e, 0x10, 0x00, 0x1a,
        0x02, 0x30, 0x01, 0x32, 0x06, 0x10, 0x02, 0x1a, 0x02, 0x30, 0x01,
    ];

    #[test]
    fn matches_kotlinc_reference_for_f_int_int() {
        let (d1, d2) = build_package(
            &[FnMeta {
                annotations: Vec::new(),
                decl_order: 0,
                jvm_name: None,
                name: "f".into(),
                params: vec![("a".into(), Ty::Int)],
                ret: Ty::Int,
                receiver: None,
                param_defaults: Vec::new(),
                suspend: false,
                jvm_desc: None,
                contract: None,
                inline: false,
                type_params: Vec::new(),
                semantic_type_params: Vec::new(),
                type_param_bounds: Vec::new(),
                context_count: 0,
                vararg_index: None,
                visibility: crate::types::Visibility::Public,
            }],
            &[],
            &[],
        );
        assert_eq!(d2, vec!["f".to_string(), "".to_string(), "a".to_string()]);
        assert_eq!(d1, REF, "\n got: {:02x?}\n ref: {:02x?}", d1, REF);
    }

    #[test]
    fn package_property_constant_flag_follows_checked_initializer_fact() {
        fn property(has_constant: bool) -> PropMeta {
            PropMeta {
                visibility: crate::types::Visibility::Public,
                name: "answer".into(),
                ty: Ty::Int,
                is_var: false,
                type_params: Vec::new(),
                receiver: None,
                getter: ("getAnswer".into(), "()I".into()),
                setter: None,
                is_const: false,
                has_constant,
                decl_order: 0,
            }
        }

        let mut strings = StringTable::default();
        let plain = property_pb(&mut strings, &property(false));
        assert!(
            !plain.as_bytes().contains(&0x58),
            "a plain val must omit Property.flags at the protobuf default"
        );

        let mut strings = StringTable::default();
        let constant = property_pb(&mut strings, &property(true));
        assert!(
            constant
                .as_bytes()
                .windows(3)
                .any(|bytes| bytes == [0x58, 0x86, 0x44]),
            "a literal-backed val must record kotlinc flags 8710"
        );
    }

    // An EXTENSION property's receiver must survive a write→read round trip through krusty's own
    // metadata reader (`Property.receiver_type = 5`): the accessor's JVM descriptor cannot mark
    // receiver-ness, so a separate compilation resolves `"s".doubled` from this record alone.
    #[test]
    fn extension_property_receiver_round_trips() {
        let (d1, d2) = build_package(
            &[],
            &[PropMeta {
                visibility: crate::types::Visibility::Public,
                name: "doubled".into(),
                ty: Ty::String,
                is_var: false,
                type_params: Vec::new(),
                receiver: Some(Ty::String),
                getter: (
                    "getDoubled".into(),
                    "(Ljava/lang/String;)Ljava/lang/String;".into(),
                ),
                setter: None,
                is_const: false,
                has_constant: false,
                decl_order: 0,
            }],
            &[],
        );
        let d1s: String = d1.iter().map(|&b| b as char).collect();
        let meta = crate::jvm::metadata::decode_metadata(&[d1s], &d2, Some(2), "dep/Lib1Kt", &[]);
        let props = meta.package_properties;
        assert_eq!(props.len(), 1);
        assert_eq!(props[0].name, "doubled");
        assert!(
            props[0]
                .receiver_class
                .is_some_and(|r| r.matches("kotlin/String")),
            "receiver_class = {:?}",
            props[0].receiver_class
        );
        assert_eq!(
            props[0].getter.as_ref().map(|g| g.name.as_str()),
            Some("getDoubled")
        );
    }

    #[test]
    fn generic_mutable_extension_property_round_trips() {
        let t = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
        let receiver = Ty::obj_args("sample/C", &[t]);
        let (d1, d2) = build_package(
            &[],
            &[PropMeta {
                visibility: crate::types::Visibility::Public,
                name: "live".into(),
                ty: t,
                is_var: true,
                type_params: vec!["T".into()],
                receiver: Some(receiver),
                getter: ("getLive".into(), "(Lsample/C;)Ljava/lang/Object;".into()),
                setter: Some(("setLive".into(), "(Lsample/C;Ljava/lang/Object;)V".into())),
                is_const: false,
                has_constant: false,
                decl_order: 0,
            }],
            &[],
        );
        let d1s: String = d1.iter().map(|&b| b as char).collect();
        let meta = crate::jvm::metadata::decode_metadata(&[d1s], &d2, Some(2), "dep/LibKt", &[]);
        let property = &meta.package_properties[0];
        let signature = property
            .generic_sig
            .as_ref()
            .expect("generic property signature");
        assert_eq!(signature.formals, ["T"]);
        assert_eq!(signature.receiver, Some(receiver));
        assert_eq!(signature.ret, t);
        assert_eq!(
            property.getter.as_ref().map(|it| it.name.as_str()),
            Some("getLive")
        );
        assert_eq!(
            property.setter.as_ref().map(|it| it.name.as_str()),
            Some("setLive")
        );
    }

    // A declared contract must survive a write→read round trip through krusty's own metadata
    // reader (`Function.contract` = 32), including a type-parameter is-conclusion (`value is R`)
    // and the inline/type-parameter tables it depends on.
    #[test]
    fn contract_round_trips_through_metadata() {
        use crate::contracts::{
            Condition, ConditionType, Contract, Effect, ParamRef, ReturnsValue,
        };
        let bound = Ty::nullable(Ty::obj("kotlin/Any"));
        let contract = Contract {
            effects: vec![Effect::ConditionalReturns {
                returns: ReturnsValue::Any,
                conclusion: Condition::IsType {
                    param: ParamRef::Param(0),
                    ty: ConditionType::Metadata(Ty::ty_param("R", bound)),
                    negated: false,
                },
            }],
        };
        let (d1, d2) = build_package(
            &[FnMeta {
                annotations: Vec::new(),
                decl_order: 0,
                jvm_name: None,
                name: "validate".into(),
                params: vec![("value".into(), Ty::obj("kotlin/Any"))],
                ret: Ty::Boolean,
                receiver: Some(Ty::obj_args(
                    "Refinement",
                    &[Ty::ty_param("T", bound), Ty::ty_param("R", bound)],
                )),
                param_defaults: Vec::new(),
                suspend: false,
                jvm_desc: Some("(LRefinement;Ljava/lang/Object;)Z".into()),
                inline: true,
                type_params: vec![("T".into(), false), ("R".into(), true)],
                semantic_type_params: vec!["T".into(), "R".into()],
                type_param_bounds: Vec::new(),
                contract: Some(std::sync::Arc::new(contract.clone())),
                context_count: 0,
                vararg_index: None,
                visibility: crate::types::Visibility::Public,
            }],
            &[],
            &[],
        );
        let d1s: String = d1.iter().map(|&b| b as char).collect();
        let meta = crate::jvm::metadata::decode_metadata(&[d1s], &d2, Some(2), "dep/LibKt", &[]);
        let mf = meta
            .package_functions
            .iter()
            .find(|f| f.kotlin_name == "validate")
            .expect("validate in package functions");
        assert!(mf.is_inline());
        assert_eq!(mf.contract.as_deref(), Some(&contract));
    }

    #[test]
    fn dedups_builtin_types() {
        // return Int + param Int must share one string-table entry (index 1).
        let (_d1, d2) = build_package(
            &[FnMeta {
                annotations: Vec::new(),
                decl_order: 0,
                jvm_name: None,
                name: "g".into(),
                params: vec![("x".into(), Ty::Int)],
                ret: Ty::Int,
                receiver: None,
                param_defaults: Vec::new(),
                suspend: false,
                jvm_desc: None,
                contract: None,
                inline: false,
                type_params: Vec::new(),
                semantic_type_params: Vec::new(),
                type_param_bounds: Vec::new(),
                context_count: 0,
                vararg_index: None,
                visibility: crate::types::Visibility::Public,
            }],
            &[],
            &[],
        );
        assert_eq!(d2.iter().filter(|s| s.is_empty()).count(), 1);
    }
}
