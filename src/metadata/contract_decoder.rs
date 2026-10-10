//! Target-neutral decoder for Kotlin metadata contract messages.

use crate::contracts::{
    Condition, ConditionType, Contract, Effect, InvocationKind, ParamRef, ReturnsValue,
};
use crate::metadata::decode::{field, require_wire, Cursor, PackageFragmentDecodeError};
use crate::types::Ty;

/// The two protobuf representations of an `Expression.is_instance_type`.
pub(crate) enum ContractTypeRef<'a> {
    Inline(&'a [u8]),
    Table(u64),
}

/// Decode one `Contract` message. Type decoding stays with the metadata adapter because an inline
/// type and a type-table index need that declaration's own string/type-parameter tables.
pub(crate) fn decode_contract(
    body: &[u8],
    resolve_type: &mut dyn FnMut(ContractTypeRef<'_>) -> Result<Ty, PackageFragmentDecodeError>,
) -> Result<Option<Contract>, PackageFragmentDecodeError> {
    let mut cursor = Cursor::new(body, 0);
    let mut effects = Vec::new();
    while !cursor.at_end() {
        let (number, wire) = field(&mut cursor, "contract")?;
        match number {
            1 => {
                require_wire(&cursor, wire, 2, "contract effect")?;
                let effect = cursor.length_delimited("contract effect")?.0;
                effects.push(decode_effect(effect, resolve_type)?);
            }
            _ => cursor.skip(wire, "contract")?,
        }
    }
    Ok((!effects.is_empty()).then_some(Contract { effects }))
}

fn decode_effect(
    body: &[u8],
    resolve_type: &mut dyn FnMut(ContractTypeRef<'_>) -> Result<Ty, PackageFragmentDecodeError>,
) -> Result<Effect, PackageFragmentDecodeError> {
    let mut cursor = Cursor::new(body, 0);
    let mut effect_type = None;
    let mut arguments = Vec::new();
    let mut conclusion = None;
    let mut invocation_kind = None;
    let mut condition_kind = None;
    while !cursor.at_end() {
        let (number, wire) = field(&mut cursor, "contract effect")?;
        match number {
            1 => set_varint_once(&mut cursor, wire, &mut effect_type, "contract effect type")?,
            2 => {
                require_wire(&cursor, wire, 2, "contract effect argument")?;
                arguments.push(cursor.length_delimited("contract effect argument")?.0);
            }
            3 => {
                require_wire(&cursor, wire, 2, "contract effect conclusion")?;
                let body = cursor.length_delimited("contract effect conclusion")?.0;
                set_once(&cursor, &mut conclusion, body, "contract effect conclusion")?;
            }
            4 => set_varint_once(
                &mut cursor,
                wire,
                &mut invocation_kind,
                "contract invocation kind",
            )?,
            5 => set_varint_once(
                &mut cursor,
                wire,
                &mut condition_kind,
                "contract condition kind",
            )?,
            _ => cursor.skip(wire, "contract effect")?,
        }
    }
    if condition_kind.unwrap_or(0) != 0 {
        return Err(cursor.error(format!(
            "unsupported contract condition kind {}",
            condition_kind.unwrap_or(0)
        )));
    }
    let effect = match effect_type.unwrap_or(0) {
        0 => {
            if invocation_kind.is_some() {
                return Err(cursor.error("returns effect carries an invocation kind"));
            }
            if arguments.len() > 1 {
                return Err(cursor.error("returns effect has more than one constant"));
            }
            Effect::Returns(decode_returns_value(arguments.first().copied())?)
        }
        1 => {
            let invocation_kind = invocation_kind.unwrap_or(0);
            if invocation_kind > 2 {
                return Err(cursor.error(format!(
                    "unsupported contract invocation kind {invocation_kind}"
                )));
            }
            let [argument] = arguments.as_slice() else {
                return Err(cursor.error("calls-in-place effect must name exactly one parameter"));
            };
            Effect::CallsInPlace {
                param: ParamRef::from_wire(decode_parameter_reference(argument)?),
                kind: InvocationKind::from_wire(invocation_kind),
            }
        }
        2 => {
            if invocation_kind.is_some() {
                return Err(cursor.error("returns-not-null effect carries an invocation kind"));
            }
            if !arguments.is_empty() {
                return Err(cursor.error("returns-not-null effect cannot carry arguments"));
            }
            Effect::Returns(ReturnsValue::NotNull)
        }
        value => return Err(cursor.error(format!("unsupported contract effect type {value}"))),
    };
    match conclusion {
        None => Ok(effect),
        Some(conclusion) => match effect {
            Effect::Returns(returns) => Ok(Effect::ConditionalReturns {
                returns,
                conclusion: decode_expression(conclusion, resolve_type)?,
            }),
            Effect::CallsInPlace { .. } | Effect::ConditionalReturns { .. } => {
                Err(cursor.error("only a returns effect may have a conclusion"))
            }
        },
    }
}

fn decode_returns_value(body: Option<&[u8]>) -> Result<ReturnsValue, PackageFragmentDecodeError> {
    let Some(body) = body else {
        return Ok(ReturnsValue::Any);
    };
    let mut cursor = Cursor::new(body, 0);
    let mut constant = None;
    while !cursor.at_end() {
        let (number, wire) = field(&mut cursor, "contract returns constant")?;
        match number {
            3 => set_varint_once(
                &mut cursor,
                wire,
                &mut constant,
                "contract returns constant",
            )?,
            _ => cursor.skip(wire, "contract returns constant")?,
        }
    }
    match constant {
        None => Err(cursor.error("returns effect argument has no constant")),
        Some(0) => Ok(ReturnsValue::Bool(true)),
        Some(1) => Ok(ReturnsValue::Bool(false)),
        Some(2) => Ok(ReturnsValue::Null),
        Some(value) => Err(cursor.error(format!("unsupported contract constant {value}"))),
    }
}

fn decode_parameter_reference(body: &[u8]) -> Result<u64, PackageFragmentDecodeError> {
    let mut cursor = Cursor::new(body, 0);
    let mut parameter = None;
    while !cursor.at_end() {
        let (number, wire) = field(&mut cursor, "contract parameter expression")?;
        match number {
            2 => set_varint_once(
                &mut cursor,
                wire,
                &mut parameter,
                "contract parameter reference",
            )?,
            _ => cursor.skip(wire, "contract parameter expression")?,
        }
    }
    parameter.ok_or_else(|| cursor.error("contract effect argument has no parameter reference"))
}

fn decode_expression(
    body: &[u8],
    resolve_type: &mut dyn FnMut(ContractTypeRef<'_>) -> Result<Ty, PackageFragmentDecodeError>,
) -> Result<Condition, PackageFragmentDecodeError> {
    let mut cursor = Cursor::new(body, 0);
    let mut flags = None;
    let mut parameter = None;
    let mut constant = None;
    let mut instance_type = None;
    let mut ands = Vec::new();
    let mut ors = Vec::new();
    while !cursor.at_end() {
        let (number, wire) = field(&mut cursor, "contract expression")?;
        match number {
            1 => set_varint_once(&mut cursor, wire, &mut flags, "contract expression flags")?,
            2 => set_varint_once(
                &mut cursor,
                wire,
                &mut parameter,
                "contract parameter reference",
            )?,
            3 => set_varint_once(
                &mut cursor,
                wire,
                &mut constant,
                "contract expression constant",
            )?,
            4 => {
                require_wire(&cursor, wire, 2, "contract is-instance type")?;
                let ty = ContractTypeRef::Inline(
                    cursor.length_delimited("contract is-instance type")?.0,
                );
                set_once(&cursor, &mut instance_type, ty, "contract is-instance type")?;
            }
            5 => {
                let id = read_varint(&mut cursor, wire, "contract is-instance type id")?;
                set_once(
                    &cursor,
                    &mut instance_type,
                    ContractTypeRef::Table(id),
                    "contract is-instance type",
                )?;
            }
            6 => {
                require_wire(&cursor, wire, 2, "contract and-expression")?;
                let body = cursor.length_delimited("contract and-expression")?.0;
                ands.push(decode_expression(body, resolve_type)?);
            }
            7 => {
                require_wire(&cursor, wire, 2, "contract or-expression")?;
                let body = cursor.length_delimited("contract or-expression")?.0;
                ors.push(decode_expression(body, resolve_type)?);
            }
            _ => cursor.skip(wire, "contract expression")?,
        }
    }
    if !ands.is_empty() && !ors.is_empty() {
        return Err(cursor.error("contract expression mixes conjunction and disjunction operands"));
    }
    let flags = flags.unwrap_or(0);
    if flags & !0b11 != 0 {
        return Err(cursor.error(format!("unsupported contract expression flags {flags}")));
    }
    let negated = flags & 1 != 0;
    let null_check = flags & 2 != 0;
    let predicate_kinds = usize::from(null_check)
        + usize::from(instance_type.is_some())
        + usize::from(constant.is_some());
    if predicate_kinds > 1 {
        return Err(cursor.error("contract expression has conflicting predicate forms"));
    }
    if constant.is_some() && parameter.is_some() {
        return Err(cursor.error("contract constant also names a parameter"));
    }
    if negated && (constant.is_some() || parameter.is_none()) {
        return Err(cursor.error("unsupported negated contract expression"));
    }
    let primitive = if null_check {
        Some(Condition::IsNull {
            param: ParamRef::from_wire(required_parameter(&cursor, parameter)?),
            negated,
        })
    } else if let Some(reference) = instance_type {
        let ty = resolve_type(reference)?;
        Some(Condition::IsType {
            param: ParamRef::from_wire(required_parameter(&cursor, parameter)?),
            ty: ConditionType::Metadata(ty),
            negated,
        })
    } else if let Some(value) = constant {
        Some(match value {
            0 => Condition::Const(true),
            1 => Condition::Const(false),
            value => {
                return Err(
                    cursor.error(format!("unsupported contract expression constant {value}"))
                )
            }
        })
    } else {
        parameter.map(|value| Condition::BoolParam {
            param: ParamRef::from_wire(value),
            negated,
        })
    };
    let conjunction = !ands.is_empty();
    let operands = if conjunction { ands } else { ors };
    let mut operands = primitive.into_iter().chain(operands);
    let first = operands
        .next()
        .ok_or_else(|| cursor.error("contract expression has no condition"))?;
    let constructor = if conjunction {
        Condition::And as fn(Box<Condition>, Box<Condition>) -> Condition
    } else {
        Condition::Or as fn(Box<Condition>, Box<Condition>) -> Condition
    };
    Ok(operands.fold(first, |left, right| {
        constructor(Box::new(left), Box::new(right))
    }))
}

fn required_parameter(
    cursor: &Cursor<'_>,
    parameter: Option<u64>,
) -> Result<u64, PackageFragmentDecodeError> {
    parameter.ok_or_else(|| cursor.error("contract predicate has no parameter reference"))
}

fn read_varint(
    cursor: &mut Cursor<'_>,
    wire: u64,
    context: &str,
) -> Result<u64, PackageFragmentDecodeError> {
    require_wire(cursor, wire, 0, context)?;
    cursor.varint(context)
}

fn set_varint_once(
    cursor: &mut Cursor<'_>,
    wire: u64,
    slot: &mut Option<u64>,
    context: &str,
) -> Result<(), PackageFragmentDecodeError> {
    let value = read_varint(cursor, wire, context)?;
    set_once(cursor, slot, value, context)
}

fn set_once<T>(
    cursor: &Cursor<'_>,
    slot: &mut Option<T>,
    value: T,
    context: &str,
) -> Result<(), PackageFragmentDecodeError> {
    if slot.replace(value).is_some() {
        Err(cursor.error(format!("duplicate {context}")))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn varint(out: &mut Vec<u8>, mut value: u64) {
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            out.push(if value == 0 { byte } else { byte | 0x80 });
            if value == 0 {
                return;
            }
        }
    }

    fn int_field(out: &mut Vec<u8>, number: u64, value: u64) {
        varint(out, number << 3);
        varint(out, value);
    }

    fn message_field(out: &mut Vec<u8>, number: u64, body: &[u8]) {
        varint(out, number << 3 | 2);
        varint(out, body.len() as u64);
        out.extend_from_slice(body);
    }

    fn contract_with(effect: &[u8]) -> Vec<u8> {
        let mut contract = Vec::new();
        message_field(&mut contract, 1, effect);
        contract
    }

    #[test]
    fn an_unsupported_effect_is_an_explicit_metadata_error() {
        let mut effect = Vec::new();
        int_field(&mut effect, 1, 3);
        let error = decode_contract(&contract_with(&effect), &mut |_| unreachable!())
            .expect_err("RETURNS_RESULT_OF is not modeled");
        assert_eq!(error.into_parts().1, "unsupported contract effect type 3");
    }

    #[test]
    fn a_negated_boolean_parameter_keeps_its_negation() {
        // `returns() implies !actual`, kotlin.test's `assertFalse`.
        let mut expression = Vec::new();
        int_field(&mut expression, 1, 1);
        int_field(&mut expression, 2, 1);
        let mut effect = Vec::new();
        message_field(&mut effect, 3, &expression);
        let contract = decode_contract(&contract_with(&effect), &mut |_| unreachable!())
            .expect("a negated boolean parameter is a modeled conclusion")
            .expect("the contract has an effect");
        assert_eq!(
            contract.effects,
            vec![Effect::ConditionalReturns {
                returns: ReturnsValue::Any,
                conclusion: Condition::BoolParam {
                    param: ParamRef::Param(0),
                    negated: true,
                },
            }]
        );
    }

    #[test]
    fn an_unknown_invocation_kind_is_not_silently_weakened() {
        let mut parameter = Vec::new();
        int_field(&mut parameter, 2, 1);
        let mut effect = Vec::new();
        int_field(&mut effect, 1, 1);
        message_field(&mut effect, 2, &parameter);
        int_field(&mut effect, 4, 7);
        let error = decode_contract(&contract_with(&effect), &mut |_| unreachable!())
            .expect_err("an unknown wire enum must not become a weaker contract");
        assert_eq!(
            error.into_parts().1,
            "unsupported contract invocation kind 7"
        );
    }
}
