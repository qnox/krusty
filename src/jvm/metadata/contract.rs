//! Contract decoding (`Function.contract`, field 32).

use super::*;

// Proto (`core/metadata/src/metadata.proto`): `Contract.effect` = 1 (repeated `Effect`).
// `Effect.effect_type` = 1: RETURNS_CONSTANT = 0 / CALLS = 1 / RETURNS_NOT_NULL = 2 /
// RETURNS_RESULT_OF = 3 — there is NO conditional type: `conclusion_of_conditional_effect` = 3
// being present (with `condition_kind` = 5 absent/CONCLUSION_CONDITION = 0) makes the whole
// message `<returns-effect> implies <conclusion>`. `Effect.kind` = 4: `InvocationKind`
// AT_MOST_ONCE = 0 / EXACTLY_ONCE = 1 / AT_LEAST_ONCE = 2 (the enum order is NOT the Kotlin
// declaration order — verified against kotlin-stdlib's `run`, which is EXACTLY_ONCE = 1).
// `Expression.flags` = 1 (bit 0 = negated, bit 1 = null-check predicate),
// `value_parameter_reference` = 2 (0 = extension receiver, else the 1-based value-parameter
// index), `constant_value` = 3 (TRUE = 0 / FALSE = 1 / NULL = 2), `is_instance_type` = 4
// (inline `Type`), `and_argument` = 6 / `or_argument` = 7 (repeated `Expression`; the FIRST
// operand of the formula is embedded inline in the parent when it is primitive).

/// Decode a `Contract` message body into the shared contract IR. `tparams` maps the function's
/// type-parameter ids to names (for an `is R` conclusion over a generic parameter).
pub(super) fn decode_contract(
    body: &[u8],
    records: &[Rec],
    d2: &[String],
    tparams: &HashMap<u64, String>,
    type_table: Option<&[u8]>,
) -> Option<crate::contracts::Contract> {
    let mut pb = Pb::new(body);
    let mut effects = Vec::new();
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (1, 2) => {
                let n = pb.varint()? as usize;
                effects.push(decode_effect(
                    pb.bytes(n)?,
                    records,
                    d2,
                    tparams,
                    type_table,
                )?);
            }
            (_, w) => pb.skip(w)?,
        }
    }
    (!effects.is_empty()).then_some(crate::contracts::Contract { effects })
}

fn decode_effect(
    body: &[u8],
    records: &[Rec],
    d2: &[String],
    tparams: &HashMap<u64, String>,
    type_table: Option<&[u8]>,
) -> Option<crate::contracts::Effect> {
    use crate::contracts::{Effect, InvocationKind};
    let mut pb = Pb::new(body);
    let mut effect_type = 0u64;
    let mut args: Vec<Vec<u8>> = Vec::new();
    let mut conclusion: Option<Vec<u8>> = None;
    let mut kind = 0u64;
    let mut condition_kind = 0u64;
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (1, 0) => effect_type = pb.varint()?,
            (2, 2) => {
                let n = pb.varint()? as usize;
                args.push(pb.bytes(n)?.to_vec());
            }
            (3, 2) => {
                let n = pb.varint()? as usize;
                conclusion = Some(pb.bytes(n)?.to_vec());
            }
            (4, 0) => kind = pb.varint()?,
            (5, 0) => condition_kind = pb.varint()?,
            (_, w) => pb.skip(w)?,
        }
    }
    let base = match effect_type {
        // RETURNS_CONSTANT — returns()/returns(c) (the constant is the argument, if any).
        0 => Effect::Returns(returns_constant(args.first())),
        // CALLS — callsInPlace(param, kind).
        1 => Effect::CallsInPlace {
            param: crate::contracts::ParamRef::from_wire(expression_param_ref(args.first()?)?),
            kind: InvocationKind::from_wire(kind),
        },
        // RETURNS_NOT_NULL — returnsNotNull().
        2 => Effect::Returns(crate::contracts::ReturnsValue::NotNull),
        // RETURNS_RESULT_OF — not modeled.
        _ => return None,
    };
    // `conclusion_of_conditional_effect` with the default CONCLUSION_CONDITION kind turns the
    // returns-effect into `<returns> implies <conclusion>`. (RETURNS_CONDITION / HOLDSIN forms
    // are not modeled.)
    if condition_kind == 0 {
        if let Some(cb) = conclusion {
            if let Effect::Returns(returns) = base {
                return Some(Effect::ConditionalReturns {
                    returns,
                    conclusion: decode_expression(&cb, records, d2, tparams, type_table)?,
                });
            }
        }
    }
    Some(base)
}

/// The `returns(…)` constant of a RETURNS_CONSTANT effect: an `Expression` whose `constant_value`
/// is the returned literal; no argument at all spells `returns()`.
fn returns_constant(arg: Option<&Vec<u8>>) -> crate::contracts::ReturnsValue {
    use crate::contracts::ReturnsValue;
    let Some(body) = arg else {
        return ReturnsValue::Any;
    };
    let mut pb = Pb::new(body);
    let mut constant = None;
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (3, 0) => constant = pb.varint(),
            (_, w) => {
                if pb.skip(w).is_none() {
                    break;
                }
            }
        }
    }
    match constant {
        Some(0) => ReturnsValue::Bool(true),
        Some(1) => ReturnsValue::Bool(false),
        Some(2) => ReturnsValue::Null,
        _ => ReturnsValue::Any,
    }
}

/// The `value_parameter_reference` of an `Expression` body, when present.
fn expression_param_ref(body: &[u8]) -> Option<u64> {
    let mut pb = Pb::new(body);
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (2, 0) => return pb.varint(),
            (_, w) => pb.skip(w)?,
        }
    }
    None
}

/// Decode an `Expression` in conclusion position into a [`crate::contracts::Condition`]. A
/// boolean formula embeds its FIRST operand inline in this message when it is primitive, with
/// the rest in `and_argument` (field 6) / `or_argument` (field 7).
fn decode_expression(
    body: &[u8],
    records: &[Rec],
    d2: &[String],
    tparams: &HashMap<u64, String>,
    type_table: Option<&[u8]>,
) -> Option<crate::contracts::Condition> {
    use crate::contracts::{Condition, ConditionType};
    let mut pb = Pb::new(body);
    let mut flags = 0u64;
    let mut vpr = None;
    let mut constant = None;
    let mut instance_body = None;
    let mut instance_id = None;
    let mut ands: Vec<Condition> = Vec::new();
    let mut ors: Vec<Condition> = Vec::new();
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (1, 0) => flags = pb.varint()?,
            (2, 0) => vpr = Some(pb.varint()?),
            (3, 0) => constant = Some(pb.varint()?),
            (4, 2) => {
                let n = pb.varint()? as usize;
                instance_body = Some(pb.bytes(n)?.to_vec());
            }
            (5, 0) => instance_id = Some(pb.varint()?),
            (6, 2) => {
                let n = pb.varint()? as usize;
                ands.push(decode_expression(
                    pb.bytes(n)?,
                    records,
                    d2,
                    tparams,
                    type_table,
                )?);
            }
            (7, 2) => {
                let n = pb.varint()? as usize;
                ors.push(decode_expression(
                    pb.bytes(n)?,
                    records,
                    d2,
                    tparams,
                    type_table,
                )?);
            }
            (_, w) => pb.skip(w)?,
        }
    }
    // The primitive condition embedded inline in this message, if any.
    let negated = flags & 1 != 0;
    let null_check = flags & 2 != 0;
    let self_cond = if null_check {
        Some(Condition::IsNull {
            param: crate::contracts::ParamRef::from_wire(vpr?),
            negated,
        })
    } else if instance_body.is_some() || instance_id.is_some() {
        // `is_instance_type` may INLINE the `Type` (field 4) or reference the function's
        // `TypeTable` by id (field 5) — kotlinc writes the table form.
        let (tb, table_nullable) = match (instance_body, instance_id) {
            (Some(tb), _) => (tb, false),
            (None, Some(id)) => {
                let (tb, nullable) = type_table_entry(type_table?, id as usize)?;
                (tb.to_vec(), nullable)
            }
            _ => return None,
        };
        let ty = decode_metadata_type(
            &tb,
            type_table,
            records,
            d2,
            tparams,
            &HashMap::new(),
            table_nullable,
            0,
        )?;
        Some(Condition::IsType {
            param: crate::contracts::ParamRef::from_wire(vpr?),
            ty: ConditionType::Metadata(ty),
            negated,
        })
    } else {
        match constant {
            Some(0) => Some(Condition::Const(true)),
            Some(1) => Some(Condition::Const(false)),
            _ => vpr.map(|v| Condition::BoolParam(crate::contracts::ParamRef::from_wire(v))),
        }
    };
    fn fold(
        cs: Vec<Condition>,
        mk: fn(Box<Condition>, Box<Condition>) -> Condition,
    ) -> Option<Condition> {
        let mut it = cs.into_iter();
        let first = it.next()?;
        Some(it.fold(first, |acc, c| mk(Box::new(acc), Box::new(c))))
    }
    if !ands.is_empty() {
        return fold(self_cond.into_iter().chain(ands).collect(), Condition::And);
    }
    if !ors.is_empty() {
        return fold(self_cond.into_iter().chain(ors).collect(), Condition::Or);
    }
    self_cond
}
