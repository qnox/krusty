//! Build the `@kotlin.Metadata` `d1` protobuf payload + `d2` string table for a file facade with
//! top-level functions. Schema/field numbers per `core/metadata/src/metadata.proto`; builtin type
//! names use `predefinedIndex` into `JvmNameResolverBase.PREDEFINED_STRINGS` (see METADATA_NOTES.md).

use crate::metadata::type_encoder::{
    encode_declared_type, encode_metadata_type_parameter, encode_type, encode_type_parameter,
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
    /// BINARY/RUNTIME-retained annotations applied to the function, including their frontend-checked
    /// element values. These become `Function.annotation` (field 12) records; SOURCE annotations never
    /// enter this list.
    pub annotations: Vec<crate::ir::AppliedAnnotation>,
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
    /// `operator fun` — sets `Function.flags` `IS_OPERATOR` (bit 8). The declaration flag exists only
    /// in metadata; omitting it makes a consuming Kotlin module reject indexed/call conventions even
    /// though the facade's JVM method is present.
    pub operator: bool,
    /// `infix fun` — sets `Function.flags` `IS_INFIX` (bit 9). Same metadata-only story as
    /// `operator`: without it a consumer rejects the `a then b` call form.
    pub infix: bool,
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
    /// How SOURCE spelled each declared type above. A `typealias` named in the return type, a
    /// parameter, the receiver, or a type-parameter bound is recorded as `Type.abbreviated_type`
    /// (field 13) next to its expanded classifier — `Ty` is expanded and cannot carry it. Default
    /// (everything empty) for a declaration that names no alias, which is nearly all of them.
    pub spellings: crate::spelling::DeclaredSpellings,
    /// User annotations on each value parameter (`fun f(@Mark a: Int)`), parallel to `params`. A short
    /// or empty vec leaves the remaining parameters unannotated.
    pub param_annotations: Vec<Vec<crate::ir::AppliedAnnotation>>,
}

/// `ValueParameter.flags` bit for `DECLARES_DEFAULT_VALUE` (bit 1; `HAS_ANNOTATIONS` is bit 0).
const DECLARES_DEFAULT_VALUE_BIT: u64 = 1 << 1;

/// A `Type` message for `t`, resolving type-parameter NAMES to their `Function.type_parameter`
/// table ids via `tps`. Handles generic class arguments (`Type.argument` = 2), nullability
/// (`Type.nullable` = 3), class types (`Type.class_name` = 6), and type parameters
/// (`Type.type_parameter` = 7).
fn type_pb_generic(st: &mut StringTable, t: Ty, tps: &TypeParameters) -> Pb {
    encode_type(st, t, tps).unwrap_or_else(|error| panic!("invalid emitted metadata type: {error}"))
}

/// [`type_pb_generic`] for a DECLARED type, carrying how source spelled it so a `typealias`
/// becomes `Type.abbreviated_type` (field 13).
fn type_pb_declared(
    st: &mut StringTable,
    t: Ty,
    spelled: &crate::spelling::Spelled,
    tps: &TypeParameters,
) -> Pb {
    encode_declared_type(st, t, spelled, tps)
        .unwrap_or_else(|error| panic!("invalid emitted metadata type: {error}"))
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

fn zigzag_i64(value: i64) -> u64 {
    ((value as u64) << 1) ^ ((value >> 63) as u64)
}

fn annotation_value_pb(st: &mut StringTable, value: &crate::ir::AnnoValue) -> Pb {
    use crate::ir::{AnnoValue, IrConst};
    let mut out = Pb::new();
    match value {
        AnnoValue::Const(constant) => match constant {
            IrConst::Byte(value) => {
                out.field_varint(1, 0);
                out.field_varint(2, zigzag_i64(i64::from(*value)));
            }
            IrConst::Char(value) => {
                out.field_varint(1, 1);
                out.field_varint(2, zigzag_i64(i64::from(*value)));
            }
            IrConst::Short(value) => {
                out.field_varint(1, 2);
                out.field_varint(2, zigzag_i64(i64::from(*value)));
            }
            IrConst::Int(value) => {
                out.field_varint(1, 3);
                out.field_varint(2, zigzag_i64(i64::from(*value)));
            }
            IrConst::Long(value) => {
                out.field_varint(1, 4);
                out.field_varint(2, zigzag_i64(*value));
            }
            IrConst::Float(value) => {
                out.field_varint(1, 5);
                out.field_fixed32(3, value.to_bits());
            }
            IrConst::Double(value) => {
                out.field_varint(1, 6);
                out.field_fixed64(4, value.to_bits());
            }
            IrConst::Boolean(value) => {
                out.field_varint(1, 7);
                out.field_varint(2, u64::from(*value));
            }
            IrConst::String(value) => {
                out.field_varint(1, 8);
                out.field_varint(5, u64::from(st.local(&value.to_lossy())));
            }
            IrConst::Null => panic!("null is not a valid metadata annotation value"),
        },
        AnnoValue::Class(internal) => {
            out.field_varint(1, 9);
            out.field_varint(6, u64::from(st.class_id(*internal)));
        }
        AnnoValue::Enum(internal, constant) => {
            out.field_varint(1, 10);
            out.field_varint(6, u64::from(st.class_id(*internal)));
            out.field_varint(7, u64::from(st.local(constant)));
        }
        AnnoValue::Annotation(annotation) => {
            out.field_varint(1, 11);
            out.field_message(8, &annotation_pb(st, annotation));
        }
        AnnoValue::Array(values) => {
            out.field_varint(1, 12);
            for value in values {
                out.repeated_message(9, &annotation_value_pb(st, value));
            }
        }
    }
    out
}

pub(crate) fn annotation_pb(st: &mut StringTable, annotation: &crate::ir::AppliedAnnotation) -> Pb {
    let mut out = Pb::new();
    out.field_varint(1, u64::from(st.class_id(annotation.internal)));
    for (name, value) in &annotation.values {
        let mut argument = Pb::new();
        argument.field_varint(1, u64::from(st.local(name)));
        argument.field_message(2, &annotation_value_pb(st, value));
        out.repeated_message(2, &argument);
    }
    out
}

fn function_pb(st: &mut StringTable, f: &FnMeta) -> Pb {
    let mut p = Pb::new();
    // Function.flags = 9 — emitted only when non-default (`6` = public final is the proto default).
    // Bit 13 = IS_SUSPEND, bit 10 = IS_INLINE, bit 9 = IS_INFIX, bit 8 = IS_OPERATOR; the
    // visibility bits ride along (final modality = 0).
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
        | (u64::from(f.inline) << 10)
        | (u64::from(f.infix) << 9)
        | (u64::from(f.operator) << 8);
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
    let ret = type_pb_declared(st, f.ret, &f.spellings.ret, &tps);
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
                upper_bound_spellings: f
                    .spellings
                    .type_param_bounds
                    .get(id)
                    .cloned()
                    .unwrap_or_default(),
            },
            &tps,
        )
        .unwrap_or_else(|error| panic!("invalid emitted metadata type parameter: {error}"));
        p.repeated_message(4, &tp); // Function.type_parameter = 4
    }
    // Function.receiver_type = 5 (extension functions only) — between return_type and value_parameter,
    // matching kotlinc's ascending field order. Its presence marks the function an extension.
    if let Some(recv) = f.receiver {
        let rt = type_pb_declared(st, recv, &f.spellings.receiver, &tps);
        p.field_message(5, &rt);
    }
    for (i, (pname, pty)) in f.params.iter().enumerate() {
        let mut vp = Pb::new();
        let annotations = f.param_annotations.get(i).map(Vec::as_slice).unwrap_or(&[]);
        // ValueParameter.flags = 1 (before name, matching kotlinc's field order): bit 1 =
        // DECLARES_DEFAULT_VALUE, set when this parameter has a default so a caller may omit it;
        // bit 0 = HAS_ANNOTATIONS, set when the `annotation` records below are written.
        let flags = if f.param_defaults.get(i).copied().unwrap_or(false) {
            DECLARES_DEFAULT_VALUE_BIT
        } else {
            0
        } | if crate::metadata::class_builder::records_annotations(annotations) {
            crate::metadata::class_builder::HAS_ANNOTATIONS
        } else {
            0
        };
        if flags != 0 {
            vp.field_varint(1, flags);
        }
        vp.field_varint(2, st.local(pname) as u64); // ValueParameter.name = 2
                                                    // A `vararg` parameter is SPELLED as its element (`vararg xs: Cargo`) but RECORDED as the
                                                    // array, so the element's spelling has to be lifted under the array rather than applied to
                                                    // it — otherwise the record claims `Array` itself was written as the alias.
        let (declared_ty, declared_spelling) = if f.vararg_index == Some(i) {
            (
                super::vararg_recorded_type(*pty),
                f.spellings.param(i).as_array_element(),
            )
        } else {
            (*pty, f.spellings.param(i).clone())
        };
        let ty = type_pb_declared(st, declared_ty, &declared_spelling, &tps);
        vp.field_message(3, &ty); // ValueParameter.type = 3
                                  // A `vararg` parameter records its ELEMENT type as `vararg_element_type` (field 4) —
                                  // kotlinc's declared type stays the array.
        if f.vararg_index == Some(i) {
            let elem = pty
                .array_elem()
                .or_else(|| pty.type_args().first().copied());
            if let Some(elem) = elem {
                let et = type_pb_declared(st, elem, f.spellings.param(i), &tps);
                vp.field_message(4, &et); // ValueParameter.vararg_element_type = 4
            }
        }
        crate::metadata::class_builder::append_param_annotations(st, &mut vp, annotations);
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
    // The `JvmMethodSignature` extension (f100) INTERNS its strings before the annotations even
    // though it SERIALIZES after them — kotlinc's serializer writes the extension first, so an
    // ANNOTATED suspend function's CPS descriptor precedes `Lp/Mark;` in d2. Interning it early only
    // when annotations exist leaves every unannotated function's string table exactly where it was.
    let mut signature = (!f.annotations.is_empty())
        .then(|| method_signature_pb(st, f))
        .flatten();
    // Applied annotations (Function.annotation = 12): `Annotation.id` (field 1) referencing the class
    // through the string table's DESC_TO_CLASS_ID form, exactly as kotlinc records e.g.
    // `@LowPriorityInOverloadResolution`.
    for annotation in &f.annotations {
        p.repeated_message(12, &annotation_pb(st, annotation));
    }
    // The declared contract (Function.contract = 32) — `returns(…) implies …` / `callsInPlace`
    // effects a separate compilation applies at call sites.
    if let Some(contract) = &f.contract {
        let cb = contract_pb(st, contract, &tps);
        p.field_message(32, &cb);
    }
    // JvmProtoBuf.methodSignature extension (Function field 100): only the descriptor (field 2) — the
    // name defaults to the function's, exactly as kotlinc emits for a top-level function.
    signature = signature.or_else(|| method_signature_pb(st, f));
    if let Some(sig) = &signature {
        p.field_message(100, sig); // Function.methodSignature = 100
    }
    p
}

/// The function's `JvmMethodSignature` message, or `None` when it records no physical handle.
fn method_signature_pb(st: &mut StringTable, f: &FnMeta) -> Option<Pb> {
    let desc = f.jvm_desc.as_ref()?;
    let mut sig = Pb::new();
    if let Some(name) = &f.jvm_name {
        sig.field_varint(1, st.local(name) as u64); // JvmMethodSignature.name = 1 (mangled)
    }
    sig.field_varint(2, st.local(desc) as u64); // JvmMethodSignature.desc = 2
    Some(sig)
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
    /// How SOURCE spelled the declared type and receiver — see [`FnMeta::spellings`].
    pub spellings: crate::spelling::DeclaredSpellings,
    /// Whether the property owns a BACKING FIELD. A computed property (`val d get() = 5L`) and
    /// every extension property have none, and kotlinc then omits the `JvmPropertySignature.field`
    /// entry entirely rather than recording an empty (derived) one.
    pub has_backing_field: bool,
    /// Whether the getter is DECLARED rather than compiler-default. kotlinc records
    /// `Property.getter_flags` (f7) = 70 — public·final·`isNotDefault` — for a declared getter and
    /// omits the field for a default one.
    pub has_declared_getter: bool,
}

/// A public top-level typealias declaration in package metadata.
pub struct TypeAliasMeta {
    pub name: String,
    /// The alias's own type-parameter names, in declaration order — `TypeAlias.typeParameter`.
    pub formals: Vec<String>,
    /// The target applied to its own arguments, with [`Self::formals`] as `Ty::TyParam`
    /// (`typealias Box<S, A> = PBox<S, S, A, A>`). Frontend resolution must supply this exact
    /// semantic type; emission never retries with a bare classifier.
    pub expansion: Ty,
    /// Declared visibility — `TypeAlias.flags` (f1) carries it; elided at the public default so an
    /// `internal typealias` writes an explicit 0 and stays module-bound for consumers.
    pub visibility: crate::types::Visibility,
    /// How the right-hand side was SPELLED, which decides `underlying_type` (f4) and abbreviates
    /// `expanded_type` (f6) — see [`crate::spelling`].
    pub expansion_spelling: crate::spelling::Spelled,
    /// Position of the declaration in the FILE (see [`PropMeta::decl_order`]). kotlinc interns
    /// package-member strings in SOURCE DECLARATION order across kinds, so a `typealias` declared
    /// above a function interns before it even though `Package.type_alias` (f5) is written last.
    pub decl_order: usize,
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
                                                     // The alias's own parameters, and the target applied to its arguments over them. Without both,
                                                     // a consumer reading this record back sees only the target CLASSIFIER and cannot place a use
                                                     // site's arguments — `Box<String, Int>` would become `PBox<String, Int>`, an arity that type
                                                     // does not have. kotlinc references an alias's own parameters BY NAME, which is what
                                                     // `NAMED_TYPE_PARAMETER` selects in the shared encoder.
    let tps: TypeParameters = alias
        .formals
        .iter()
        .enumerate()
        .map(|(index, formal)| {
            (
                formal.clone(),
                index as u64 | crate::metadata::type_encoder::NAMED_TYPE_PARAMETER,
            )
        })
        .collect();
    for (index, formal) in alias.formals.iter().enumerate() {
        p.repeated_message(3, &encode_type_parameter(st, index, formal, false));
        // TypeAlias.type_parameter = 3
    }
    // `underlying_type` (f4) is the right-hand side AS WRITTEN and `expanded_type` (f6) is that
    // side fully expanded. They coincide only when the right-hand side names a CLASS. When it names
    // another alias (`typealias Chain = Cargo`), kotlinc writes f4 as a bare
    // `Type{type_alias_name = Cargo}` — no `class_name` at all — and gives f6 the expanded class
    // with `Cargo` as its `abbreviated_type`.
    let underlying = crate::metadata::type_encoder::encode_spelled_type(
        st,
        alias.expansion,
        &alias.expansion_spelling,
        &tps,
    )
    .unwrap_or_else(|error| panic!("invalid emitted metadata type: {error}"));
    p.field_message(4, &underlying); // TypeAlias.underlying_type = 4
    let expanded = type_pb_declared(st, alias.expansion, &alias.expansion_spelling, &tps);
    p.field_message(6, &expanded); // TypeAlias.expanded_type = 6
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
    let ret = type_pb_declared(st, m.ty, &m.spellings.ret, &tps);
    p.field_message(3, &ret); // Property.return_type = 3
    for (id, name) in m.type_params.iter().enumerate() {
        let tp = encode_type_parameter(st, id, name, false);
        p.repeated_message(4, &tp); // Property.type_parameter = 4
    }
    if let Some(recv) = m.receiver {
        let rt = type_pb_declared(st, recv, &m.spellings.receiver, &tps);
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
    // Property.getter_flags = 7 — emitted only for a DECLARED getter (`isNotDefault`), where it
    // reads 70 = public·final·isNotDefault. A compiler-default getter omits the field.
    if m.has_declared_getter {
        p.field_varint(7, property_flags::DECLARED_ACCESSOR);
    }
    let mut jvm = Pb::new();
    // `field` (empty → derived) only when a backing field EXISTS: a computed or extension property
    // has none, and kotlinc omits the entry rather than recording an empty one.
    if m.has_backing_field {
        jvm.field_message(1, &Pb::new());
    }
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
    module_name: Option<&str>,
) -> (Vec<u8>, Vec<String>) {
    let mut st = StringTable::default();
    let mut package = Pb::new();
    // STRINGS INTERN IN SOURCE DECLARATION ORDER across kinds (a `const val` before a `fun`
    // interns first), while the proto still writes functions (f3) before properties (f4) — so the
    // d2 indices match kotlinc's. Build each sub-message in declaration order, emit by field.
    // Ordering within one `decl_order` slot: a `typealias` carries the index of the declaration it
    // PRECEDES (it is not itself an entry in the file's declaration arena), so on a tie it interns
    // first — which is what source order means.
    #[derive(PartialEq, Eq, PartialOrd, Ord)]
    enum Kind {
        Alias,
        Function,
        Property,
    }
    let mut build_order: Vec<(usize, Kind, usize)> = funcs
        .iter()
        .enumerate()
        .map(|(index, f)| (f.decl_order, Kind::Function, index))
        .chain(
            props
                .iter()
                .enumerate()
                .map(|(index, m)| (m.decl_order, Kind::Property, index)),
        )
        .chain(
            aliases
                .iter()
                .enumerate()
                .map(|(index, a)| (a.decl_order, Kind::Alias, index)),
        )
        .collect();
    build_order.sort();
    let mut fn_pbs: Vec<Option<Pb>> = (0..funcs.len()).map(|_| None).collect();
    let mut prop_pbs: Vec<Option<Pb>> = (0..props.len()).map(|_| None).collect();
    let mut alias_pbs: Vec<Option<Pb>> = (0..aliases.len()).map(|_| None).collect();
    for (_, kind, index) in build_order {
        match kind {
            Kind::Function => fn_pbs[index] = Some(function_pb(&mut st, &funcs[index])),
            Kind::Property => prop_pbs[index] = Some(property_pb(&mut st, &props[index])),
            Kind::Alias => alias_pbs[index] = Some(type_alias_pb(&mut st, &aliases[index])),
        }
    }
    for fp in fn_pbs.iter().flatten() {
        package.repeated_message(3, fp); // Package.function = 3
    }
    for pp in prop_pbs.iter().flatten() {
        package.repeated_message(4, pp); // Package.property = 4
    }
    for ap in alias_pbs.iter().flatten() {
        package.repeated_message(5, ap); // Package.type_alias = 5
    }
    // The `-module-name` value → `JvmProtoBuf.packageModuleName` (f101). kotlinc interns it LAST
    // (the end of d2) and omits the field for the default module `main` (callers pass `None` then).
    if let Some(module) = module_name {
        let module_idx = st.local(module);
        package.field_varint(101, module_idx as u64);
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
                spellings: crate::spelling::DeclaredSpellings::default(),
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
                operator: false,
                infix: false,
                type_params: Vec::new(),
                semantic_type_params: Vec::new(),
                type_param_bounds: Vec::new(),
                context_count: 0,
                vararg_index: None,
                visibility: crate::types::Visibility::Public,
                param_annotations: Vec::new(),
            }],
            &[],
            &[],
            None,
        );
        assert_eq!(d2, vec!["f".to_string(), "".to_string(), "a".to_string()]);
        assert_eq!(d1, REF, "\n got: {:02x?}\n ref: {:02x?}", d1, REF);
    }

    /// Exact bytes kotlinc 2.4.10 emits for `fun f(a: Int): Int = a` under `-module-name mymod`:
    /// the same package payload plus `packageModuleName` (f101) = the module name, interned last.
    const REF_MODULE_NAME: &[u8] = &[
        0x00, 0x0a, 0x0a, 0x00, 0x0a, 0x02, 0x10, 0x08, 0x0a, 0x02, 0x08, 0x02, 0x1a, 0x0e, 0x10,
        0x00, 0x1a, 0x02, 0x30, 0x01, 0x32, 0x06, 0x10, 0x02, 0x1a, 0x02, 0x30, 0x01, 0xa8, 0x06,
        0x03,
    ];

    #[test]
    fn package_module_name_metadata_byte_matches_kotlinc() {
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
                operator: false,
                infix: false,
                type_params: Vec::new(),
                semantic_type_params: Vec::new(),
                type_param_bounds: Vec::new(),
                context_count: 0,
                vararg_index: None,
                visibility: crate::types::Visibility::Public,
                spellings: crate::spelling::DeclaredSpellings::default(),
                param_annotations: Vec::new(),
            }],
            &[],
            &[],
            Some("mymod"),
        );
        assert_eq!(
            d2,
            vec![
                "f".to_string(),
                "".to_string(),
                "a".to_string(),
                "mymod".to_string()
            ]
        );
        assert_eq!(
            d1, REF_MODULE_NAME,
            "\n got: {:02x?}\n ref: {:02x?}",
            d1, REF_MODULE_NAME
        );
    }

    #[test]
    fn package_property_constant_flag_follows_checked_initializer_fact() {
        fn property(has_constant: bool) -> PropMeta {
            PropMeta {
                spellings: crate::spelling::DeclaredSpellings::default(),
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
                has_backing_field: true,
                has_declared_getter: false,
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
                spellings: crate::spelling::DeclaredSpellings::default(),
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
                has_backing_field: false,
                has_declared_getter: true,
                has_constant: false,
                decl_order: 0,
            }],
            &[],
            None,
        );
        let d1s: String = d1.iter().map(|&b| b as char).collect();
        let meta =
            crate::jvm::metadata::decode_metadata(&[d1s], &d2, Some(2), "dep/Lib1Kt", None, &[]);
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
                spellings: crate::spelling::DeclaredSpellings::default(),
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
                has_backing_field: false,
                has_declared_getter: true,
                decl_order: 0,
            }],
            &[],
            None,
        );
        let d1s: String = d1.iter().map(|&b| b as char).collect();
        let meta =
            crate::jvm::metadata::decode_metadata(&[d1s], &d2, Some(2), "dep/LibKt", None, &[]);
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
                spellings: crate::spelling::DeclaredSpellings::default(),
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
                operator: false,
                infix: false,
                type_params: vec![("T".into(), false), ("R".into(), true)],
                semantic_type_params: vec!["T".into(), "R".into()],
                type_param_bounds: Vec::new(),
                contract: Some(std::sync::Arc::new(contract.clone())),
                context_count: 0,
                vararg_index: None,
                visibility: crate::types::Visibility::Public,
                param_annotations: Vec::new(),
            }],
            &[],
            &[],
            None,
        );
        let d1s: String = d1.iter().map(|&b| b as char).collect();
        let meta =
            crate::jvm::metadata::decode_metadata(&[d1s], &d2, Some(2), "dep/LibKt", None, &[]);
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
                spellings: crate::spelling::DeclaredSpellings::default(),
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
                operator: false,
                infix: false,
                type_params: Vec::new(),
                semantic_type_params: Vec::new(),
                type_param_bounds: Vec::new(),
                context_count: 0,
                vararg_index: None,
                visibility: crate::types::Visibility::Public,
                param_annotations: Vec::new(),
            }],
            &[],
            &[],
            None,
        );
        assert_eq!(d2.iter().filter(|s| s.is_empty()).count(), 1);
    }
}
