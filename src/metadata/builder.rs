//! Build the `@kotlin.Metadata` `d1` protobuf payload + `d2` string table for a file facade with
//! top-level functions. Schema/field numbers per `core/metadata/src/metadata.proto`; builtin type
//! names use `predefinedIndex` into `JvmNameResolverBase.PREDEFINED_STRINGS` (see METADATA_NOTES.md).

use std::collections::HashMap;

use crate::metadata::protobuf::Pb;
use crate::types::Ty;

/// One top-level function to describe in the package metadata.
pub struct FnMeta {
    pub name: String,
    pub params: Vec<(String, Ty)>,
    pub ret: Ty,
    /// Extension-receiver type (`Function.receiver_type` = 5), `Some` for an extension function. Recorded
    /// SEPARATELY from `params` (the LOGICAL value params, receiver excluded), so a reader recovers the
    /// extension's true source arity — `fun T.f(a)` is one value param, not two. `None` for a plain fn.
    pub receiver: Option<Ty>,
    /// Per-parameter: `Some(receiver_ty)` when the parameter is a RECEIVER function type `Recv.(…) -> R`
    /// (its `Ty` erases to `kotlin/FunctionN`). Emits the `@kotlin.ExtensionFunctionType` type-annotation
    /// plus the receiver as the function type's first type ARGUMENT, so a reader recognizes a lambda
    /// passed to this parameter binds `this` to `receiver_ty`. Parallel to `params`; empty = none.
    pub param_fun_recvs: Vec<Option<Ty>>,
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
    /// `inline fun` — sets `Function.flags` `IS_INLINE` (bit 10) so a reader resolves the function
    /// as inline (splice candidate), not a plain callable.
    pub inline: bool,
    /// Declared type parameters in order `(name, reified)` — emitted as the `Function.type_parameter`
    /// table (field 4); their indices are the `Type.type_parameter` ids used by generic
    /// receiver/parameter/return types and `is`-conclusions in the contract.
    pub type_params: Vec<(String, bool)>,
    /// The function's decoded contract, emitted as `Function.contract` (field 32) so a separate
    /// compilation applies its effects at call sites. `None` when the function declares none.
    pub contract: Option<std::sync::Arc<crate::contracts::Contract>>,
}

/// `ValueParameter.flags` bit for `DECLARES_DEFAULT_VALUE` (bit 1; `HAS_ANNOTATIONS` is bit 0).
const DECLARES_DEFAULT_VALUE_BIT: u64 = 1 << 1;

/// `predefinedIndex` of a builtin type's fq-name in `PREDEFINED_STRINGS`.
fn builtin_index(t: Ty) -> Option<u64> {
    Some(match t {
        Ty::Unit => 2,
        Ty::Double => 6,
        Ty::Int => 8,
        Ty::Long => 9,
        Ty::Boolean => 11,
        Ty::String => 14,
        _ => return None,
    })
}

/// Accumulates d2 strings + the parallel `StringTableTypes.Record` list, deduping builtin entries.
#[derive(Default)]
struct StringTable {
    strings: Vec<String>,
    records: Vec<Pb>, // one Record per string index
    builtin_dedup: HashMap<u64, u32>,
}

impl StringTable {
    /// Intern a local (source) string; returns its index. (No dedup — names are distinct in v0.)
    fn local(&mut self, s: &str) -> u32 {
        let i = self.strings.len() as u32;
        self.strings.push(s.to_string());
        self.records.push(Pb::new()); // empty Record => use d2 string verbatim
        i
    }

    /// Intern a builtin fq-name via predefinedIndex; deduped. The d2 slot is empty (`""`).
    fn builtin(&mut self, predefined: u64) -> u32 {
        if let Some(&i) = self.builtin_dedup.get(&predefined) {
            return i;
        }
        let i = self.strings.len() as u32;
        self.strings.push(String::new());
        let mut r = Pb::new();
        r.field_varint(2, predefined); // Record.predefined_index = 2
        self.records.push(r);
        self.builtin_dedup.insert(predefined, i);
        i
    }

    /// A class id from a type descriptor `Lpkg/Name;` via operation `DESC_TO_CLASS_ID` (Record.f3=2).
    fn class_id_from_desc(&mut self, descriptor: &str) -> u32 {
        let i = self.strings.len() as u32;
        self.strings.push(descriptor.to_string());
        let mut r = Pb::new();
        r.field_varint(3, 2); // operation = DESC_TO_CLASS_ID
        self.records.push(r);
        i
    }

    fn serialize_types(&self) -> Pb {
        crate::metadata::serialize_string_table_types(&self.records)
    }
}

fn type_pb(st: &mut StringTable, t: Ty) -> Pb {
    type_pb_generic(st, t, &HashMap::new())
}

/// A `Type` message for `t`, resolving type-parameter NAMES to their `Function.type_parameter`
/// table ids via `tps`. Handles generic class arguments (`Type.argument` = 2), nullability
/// (`Type.nullable` = 3), class types (`Type.class_name` = 6), and type parameters
/// (`Type.type_parameter` = 7).
fn type_pb_generic(st: &mut StringTable, t: Ty, tps: &HashMap<&str, u64>) -> Pb {
    let mut p = Pb::new();
    let (nullable, base) = match t {
        Ty::Nullable(inner) => (true, *inner),
        _ => (false, t),
    };
    for a in base.type_args() {
        let mut arg = Pb::new();
        let at = type_pb_generic(st, *a, tps);
        arg.field_message(2, &at); // Argument.type = 2
        p.repeated_message(2, &arg); // Type.argument = 2
    }
    if nullable {
        p.field_varint(3, 1); // Type.nullable = 3
    }
    match base {
        Ty::TyParam(ref name, _) => {
            // Type.type_parameter = 7 — the id is the parameter's index in the function's table;
            // Type.type_parameter_name = 9 carries the name for by-name readers (kotlinc emits
            // both forms depending on context; krusty's reader accepts either).
            p.field_varint(7, tps.get(name).copied().unwrap_or(0));
            p.field_varint(9, st.local(name) as u64);
        }
        _ => {
            let class_name = match base {
                Ty::Obj(internal, _) => st.class_id_from_desc(&format!("L{internal};")),
                _ => st.builtin(builtin_index(base).unwrap_or(0)), // 0 = kotlin/Any on erroring code
            };
            p.field_varint(6, class_name as u64); // Type.class_name = 6
        }
    }
    p
}

/// Serialize a decoded contract as a `Contract` message (`repeated Effect effect` = 1), the exact
/// mirror of the reader in `src/jvm/metadata.rs`. `tps` maps the function's type-parameter names
/// to their table ids (for an `is R` conclusion).
fn contract_pb(
    st: &mut StringTable,
    contract: &crate::contracts::Contract,
    tps: &HashMap<&str, u64>,
) -> Pb {
    let mut p = Pb::new();
    for e in &contract.effects {
        p.repeated_message(1, &effect_pb(st, e, tps));
    }
    p
}

fn effect_pb(st: &mut StringTable, e: &crate::contracts::Effect, tps: &HashMap<&str, u64>) -> Pb {
    use crate::contracts::{Effect, InvocationKind};
    let mut p = Pb::new();
    match e {
        Effect::CallsInPlace { param, kind } => {
            p.field_varint(1, 1); // Effect.effect_type = CALLS
            let arg = expression_param_ref_pb(*param);
            p.repeated_message(2, &arg); // Effect.effect_constructor_argument
            let kind = match kind {
                InvocationKind::AtMostOnce => Some(0),
                InvocationKind::ExactlyOnce => Some(1),
                InvocationKind::AtLeastOnce => Some(2),
                InvocationKind::Unknown => None,
            };
            if let Some(k) = kind {
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
    tps: &HashMap<&str, u64>,
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

/// An `Expression` naming a parameter: `value_parameter_reference` (0 = receiver, n = 1-based
/// value-parameter index — the wire convention).
fn expression_param_ref_pb(param: crate::contracts::ParamRef) -> Pb {
    let mut p = Pb::new();
    let vpr = match param {
        crate::contracts::ParamRef::Receiver => 0,
        crate::contracts::ParamRef::Param(i) => (i + 1) as u64,
    };
    p.field_varint(2, vpr);
    p
}

fn condition_pb(
    st: &mut StringTable,
    c: &crate::contracts::Condition,
    tps: &HashMap<&str, u64>,
) -> Pb {
    use crate::contracts::{Condition, ConditionType};
    let mut p = Pb::new();
    match c {
        Condition::IsNull { param, negated } => {
            p.field_varint(1, 2 | u64::from(*negated)); // flags: null-check predicate | negated
            let vpr = vpr_of(*param);
            p.field_varint(2, vpr);
        }
        Condition::IsType { param, ty, negated } => {
            if *negated {
                p.field_varint(1, 1);
            }
            let vpr = vpr_of(*param);
            p.field_varint(2, vpr);
            let ty = match ty {
                ConditionType::Metadata(ty) => *ty,
                // Source references are resolved to semantic types before emission
                // (`facade_package_metadata`); an unresolved one degrades to `kotlin/Any`.
                ConditionType::Source(_) => Ty::obj("kotlin/Any"),
            };
            let it = type_pb_generic(st, ty, tps);
            p.field_message(4, &it); // Expression.is_instance_type
        }
        Condition::BoolParam(param) => {
            let vpr = vpr_of(*param);
            p.field_varint(2, vpr);
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

fn vpr_of(param: crate::contracts::ParamRef) -> u64 {
    match param {
        crate::contracts::ParamRef::Receiver => 0,
        crate::contracts::ParamRef::Param(i) => (i + 1) as u64,
    }
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

/// A `Type` for a RECEIVER function-type parameter (`Recv.(…) -> R`, erased to `fun_class` =
/// `kotlin/FunctionN`): records `recv` as the function type's FIRST type ARGUMENT (`Type.argument` = 1,
/// each `Argument.type` = 2) and tags it with the `@kotlin.ExtensionFunctionType` type annotation
/// (`Type.annotation` = 100, a registered extension; `Annotation.id` = 1 → the annotation class). A reader
/// recovers the receiver from argument[0] and the receiver-ness from the annotation, exactly as kotlinc
/// emits for a `Recv.() -> R` parameter.
fn type_pb_recv_fun(st: &mut StringTable, fun_class: Ty, recv: Ty) -> Pb {
    let mut p = type_pb(st, fun_class); // Type.class_name = kotlin/FunctionN
    let recv_ty = type_pb(st, recv);
    let mut arg = Pb::new();
    arg.field_message(2, &recv_ty); // Argument.type = 2 (projection INV omitted)
    p.repeated_message(2, &arg); // Type.argument = 2
    let ext_id = st.class_id_from_desc("Lkotlin/ExtensionFunctionType;");
    let mut anno = Pb::new();
    anno.field_varint(1, ext_id as u64); // Annotation.id = 1
    p.field_message(100, &anno); // Type.annotation = 100 (extension)
    p
}

fn function_pb(st: &mut StringTable, f: &FnMeta) -> Pb {
    let mut p = Pb::new();
    // Function.flags = 9 — emitted only when non-default (`6` = public final is the proto default).
    // Bit 13 = IS_SUSPEND, bit 10 = IS_INLINE; the visibility/modality bits (`0x06`) ride along so
    // the reader keeps public-final. kotlinc orders `flags` before `name`.
    let flags = 0x06u64 | (u64::from(f.suspend) << 13) | (u64::from(f.inline) << 10);
    if flags != 0x06 {
        p.field_varint(9, flags);
    }
    p.field_varint(2, st.local(&f.name) as u64); // Function.name = 2
                                                 // The function's type-parameter table (Function.type_parameter = 4): indices are the
                                                 // `Type.type_parameter` ids generic types and contract conclusions reference.
    let tps: HashMap<&str, u64> = f
        .type_params
        .iter()
        .enumerate()
        .map(|(i, (n, _))| (n.as_str(), i as u64))
        .collect();
    let ret = type_pb_generic(st, f.ret, &tps);
    p.field_message(3, &ret); // Function.return_type = 3
    for (id, (tp_name, reified)) in f.type_params.iter().enumerate() {
        let mut tp = Pb::new();
        tp.field_varint(1, id as u64); // TypeParameter.id = 1
        tp.field_varint(2, st.local(tp_name) as u64); // TypeParameter.name = 2
        if *reified {
            tp.field_varint(3, 1); // TypeParameter.reified = 3
        }
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
        let ty = match f.param_fun_recvs.get(i).and_then(|o| *o) {
            Some(recv) => type_pb_recv_fun(st, *pty, recv),
            None => type_pb_generic(st, *pty, &tps),
        };
        vp.field_message(3, &ty); // ValueParameter.type = 3
        p.repeated_message(6, &vp); // Function.value_parameter = 6
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
    /// Extension-receiver type (`Property.receiver_type` = 10), `Some` for an extension property —
    /// the same separation from the accessor's JVM parameters as [`FnMeta::receiver`].
    pub receiver: Option<Ty>,
    pub getter: (String, String),
    pub setter: Option<(String, String)>,
}

/// `Package` property flags kotlinc emits for top-level `val`/`var` (public, with accessors).
const PKG_VAL_FLAGS: u64 = 8710;
const PKG_VAR_FLAGS: u64 = 1798;

fn jvm_method_sig(st: &mut StringTable, name: &str, desc: &str) -> Pb {
    let mut p = Pb::new();
    p.field_varint(1, st.local(name) as u64); // JvmMethodSignature.name = 1
    p.field_varint(2, st.local(desc) as u64); // JvmMethodSignature.desc = 2
    p
}

fn property_pb(st: &mut StringTable, m: &PropMeta) -> Pb {
    let mut p = Pb::new();
    p.field_varint(2, st.local(&m.name) as u64); // Property.name = 2
    let ret = type_pb(st, m.ty);
    p.field_message(3, &ret); // Property.return_type = 3
    if let Some(recv) = m.receiver {
        let rt = type_pb(st, recv);
        p.field_message(5, &rt); // Property.receiver_type = 5 (extension properties only)
    }
    p.field_varint(
        11,
        if m.is_var {
            PKG_VAR_FLAGS
        } else {
            PKG_VAL_FLAGS
        },
    ); // flags
    let mut jvm = Pb::new();
    jvm.field_message(1, &Pb::new()); // field (empty → derived)
    let getter = jvm_method_sig(st, &m.getter.0, &m.getter.1);
    jvm.field_message(3, &getter);
    if let Some((sn, sd)) = &m.setter {
        let setter = jvm_method_sig(st, sn, sd);
        jvm.field_message(4, &setter);
    }
    p.field_message(100, &jvm); // JvmProtoBuf.propertySignature = 100
    p
}

/// Build `(d1 bytes, d2 strings)` for a file facade. `d1 = delimited(StringTableTypes) + Package`.
pub fn build_package(funcs: &[FnMeta], props: &[PropMeta]) -> (Vec<u8>, Vec<String>) {
    let mut st = StringTable::default();
    let mut package = Pb::new();
    for f in funcs {
        let fp = function_pb(&mut st, f);
        package.repeated_message(3, &fp); // Package.function = 3
    }
    for m in props {
        let pp = property_pb(&mut st, m);
        package.repeated_message(4, &pp); // Package.property = 4
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
    (bytes, st.strings)
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
                name: "f".into(),
                params: vec![("a".into(), Ty::Int)],
                ret: Ty::Int,
                receiver: None,
                param_fun_recvs: Vec::new(),
                param_defaults: Vec::new(),
                suspend: false,
                jvm_desc: None,
                contract: None,
                inline: false,
                type_params: Vec::new(),
            }],
            &[],
        );
        assert_eq!(d2, vec!["f".to_string(), "".to_string(), "a".to_string()]);
        assert_eq!(d1, REF, "\n got: {:02x?}\n ref: {:02x?}", d1, REF);
    }

    // An EXTENSION property's receiver must survive a write→read round trip through krusty's own
    // metadata reader (`Property.receiver_type = 5`): the accessor's JVM descriptor cannot mark
    // receiver-ness, so a separate compilation resolves `"s".doubled` from this record alone.
    #[test]
    fn extension_property_receiver_round_trips() {
        let (d1, d2) = build_package(
            &[],
            &[PropMeta {
                name: "doubled".into(),
                ty: Ty::String,
                is_var: false,
                receiver: Some(Ty::String),
                getter: (
                    "getDoubled".into(),
                    "(Ljava/lang/String;)Ljava/lang/String;".into(),
                ),
                setter: None,
            }],
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
                name: "validate".into(),
                params: vec![("value".into(), Ty::obj("kotlin/Any"))],
                ret: Ty::Boolean,
                receiver: Some(Ty::obj_args(
                    "Refinement",
                    &[Ty::ty_param("T", bound), Ty::ty_param("R", bound)],
                )),
                param_fun_recvs: Vec::new(),
                param_defaults: Vec::new(),
                suspend: false,
                jvm_desc: Some("(LRefinement;Ljava/lang/Object;)Z".into()),
                inline: true,
                type_params: vec![("T".into(), false), ("R".into(), true)],
                contract: Some(std::sync::Arc::new(contract.clone())),
            }],
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
                name: "g".into(),
                params: vec![("x".into(), Ty::Int)],
                ret: Ty::Int,
                receiver: None,
                param_fun_recvs: Vec::new(),
                param_defaults: Vec::new(),
                suspend: false,
                jvm_desc: None,
                contract: None,
                inline: false,
                type_params: Vec::new(),
            }],
            &[],
        );
        assert_eq!(d2.iter().filter(|s| s.is_empty()).count(), 1);
    }
}
