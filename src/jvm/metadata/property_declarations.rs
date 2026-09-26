//! Decoding of Kotlin `Property` metadata records into [`MetaProp`] declarations: flags, types,
//! context parameters, and the JVM field and accessors the `JvmPropertySignature` names, completed
//! with Kotlin's default mapping where the signature omits them.

use super::*;

/// Decode every `Property` (`prop_field`: 10 in a `Class`, 4 in a `Package`) of this metadata message
/// into [`MetaProp`]s — the property analogue of [`decode_functions`]. Carries the REAL getter/setter
/// JVM names from the `JvmPropertySignature`, so a resolver reads the accessor instead of guessing `getX`.
pub(super) fn decode_properties(
    ctx: &MetaCtx,
    prop_field: u64,
    class_tparams: &[(u64, String)],
    class_tparam_bounds: &[Vec<Ty>],
) -> MetadataResult<Vec<MetaProp>> {
    let inline_underlying_property_name_id = inline_underlying_property_name_id(ctx.msg);
    let declared_classifier = |ty: Ty| match ty.non_null() {
        Ty::Obj(internal, _) => Some(internal),
        _ => None,
    };
    let mut out = Vec::new();
    let records = ctx.records;
    let d2 = ctx.d2;
    let mut type_table = None;
    let mut props: Vec<&[u8]> = Vec::new();
    let mut pb = Pb::new(ctx.msg);
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (f, 2) if f == prop_field => {
                let Some(n) = pb.varint() else { break };
                let Some(b) = pb.bytes(n as usize) else { break };
                props.push(b);
            }
            (30, 2) => {
                let Some(n) = pb.varint() else { break };
                let Some(b) = pb.bytes(n as usize) else { break };
                type_table = Some(b);
            }
            (_, w) => {
                if pb.skip(w).is_none() {
                    break;
                }
            }
        }
    }
    let type_body_of_id = |tid: u64| type_table_entry(type_table?, tid as usize);
    let type_of_id = |tid: u64| -> Option<TypeName> {
        let (tb, _) = type_body_of_id(tid)?;
        let cn = parse_type_class_name(tb)?;
        resolve_class_name(records, d2, cn as usize).map(|name| type_name(&name))
    };
    let type_id_nullable = |tid: u64| -> bool {
        type_table
            .and_then(|table| type_table_entry(table, tid as usize))
            .is_some_and(|(body, table_nullable)| table_nullable || parse_type_nullable(body))
    };
    // Current `Property.flags` is field 11. Its shared declaration prefix is HAS_ANNOTATIONS(0) ·
    // VISIBILITY(1..3) · MODALITY(4..5) · MEMBER_KIND(6..7), so property-specific IS_VAR and IS_CONST
    // live at bits 8 and 11. Older metadata may instead carry `old_flags` in field 1, whose shorter
    // layout puts those facts at bits 6 and 9. Decode the two words independently and prefer field 11
    // regardless of wire order; collapsing them into one mutable word would let a reordered legacy
    // field override the authoritative modern value. The shared modern constants also drive both writers.
    const LEGACY_IS_VAR: u64 = 1 << 6;
    const LEGACY_IS_CONST: u64 = 1 << 9;
    for prop in props {
        let mut p = Pb::new(prop);
        let mut name_id = None;
        let mut ret = None;
        let mut ret_nullable = false;
        let mut ret_body = None;
        let mut legacy_flags = None;
        let mut modern_flags = None;
        let mut sig = ParsedJvmPropertySignature::default();
        let mut receiver_class = None;
        let mut receiver_body = None;
        let mut receiver_nullable = false;
        let mut type_params = Vec::new();
        let mut context_params = Vec::new();
        let mut setter_value_parameter = None;
        let mut context_receiver_bodies = Vec::new();
        let mut context_receiver_type_ids = Vec::new();
        while !p.at_end() {
            let Some(tag) = p.varint() else { break };
            match (tag >> 3, tag & 7) {
                (1, 0) => legacy_flags = p.varint(),
                (11, 0) => modern_flags = p.varint(),
                (2, 0) => name_id = p.varint(),
                (3, 2) => {
                    let Some(n) = p.varint() else { break };
                    let Some(tb) = p.bytes(n as usize) else { break };
                    ret_nullable = parse_type_nullable(tb);
                    ret_body = Some(tb);
                    ret = parse_type_class_name(tb)
                        .and_then(|cn| resolve_class_name(records, d2, cn as usize))
                        .map(|name| type_name(&name));
                }
                (4, 2) => {
                    let Some(n) = p.varint() else { break };
                    let Some(body) = p.bytes(n as usize) else {
                        break;
                    };
                    type_params.push(parse_type_param(body)?);
                }
                (17, 2) => {
                    let Some(n) = p.varint() else { break };
                    let Some(body) = p.bytes(n as usize) else {
                        break;
                    };
                    context_params.push(parse_value_parameter(body)?);
                }
                (12, 2) => {
                    let Some(n) = p.varint() else { break };
                    let Some(body) = p.bytes(n as usize) else {
                        break;
                    };
                    context_receiver_bodies.push(body.to_vec());
                }
                (13, 0) => {
                    if let Some(type_id) = p.varint() {
                        context_receiver_type_ids.push(type_id);
                    }
                }
                (13, 2) => {
                    let Some(n) = p.varint() else { break };
                    let Some(ids) = p.bytes(n as usize) else {
                        break;
                    };
                    context_receiver_type_ids
                        .extend(packed_varints(ids).ok_or(MetadataDecodeError::MalformedWire)?);
                }
                (9, 0) => {
                    if let Some(tid) = p.varint() {
                        ret = type_of_id(tid);
                        ret_nullable = type_id_nullable(tid);
                        ret_body = type_body_of_id(tid).map(|(body, _)| body);
                    }
                }
                // `Property.receiver_type` (field 5, inline `Type`) / `receiver_type_id` (field 10) —
                // PRESENCE marks an EXTENSION property; recover the receiver's class name.
                (5, 2) => {
                    let Some(n) = p.varint() else { break };
                    let Some(tb) = p.bytes(n as usize) else { break };
                    receiver_body = Some(tb);
                    receiver_nullable = parse_type_nullable(tb);
                    receiver_class = parse_type_class_name(tb)
                        .and_then(|cn| resolve_class_name(records, d2, cn as usize))
                        .map(|name| type_name(&name));
                }
                (6, 2) => {
                    let Some(n) = p.varint() else { break };
                    let Some(body) = p.bytes(n as usize) else {
                        break;
                    };
                    setter_value_parameter = Some(parse_value_parameter(body)?);
                }
                (10, 0) => {
                    if let Some(tid) = p.varint() {
                        receiver_class = type_of_id(tid);
                        if let Some((body, table_nullable)) = type_body_of_id(tid) {
                            receiver_body = Some(body);
                            receiver_nullable = table_nullable || parse_type_nullable(body);
                        }
                    }
                }
                (100, 2) => {
                    let Some(n) = p.varint() else { break };
                    let Some(ext) = p.bytes(n as usize) else {
                        break;
                    };
                    sig = parse_jvm_property_signature(ext);
                }
                (_, w) => {
                    if p.skip(w).is_none() {
                        break;
                    }
                }
            }
        }
        let Some(name_id) = name_id else { continue };
        let Some(name) = resolve_string(records, d2, name_id as usize) else {
            continue;
        };
        let ParsedJvmPropertySignature {
            field: field_signature,
            getter: getter_signature,
            setter: setter_signature,
        } = sig;
        let setter_parameter_name = setter_value_parameter
            .and_then(|parameter| resolve_string(records, d2, parameter.name_id as usize));
        let (flags, is_var_bit, is_const_bit) = modern_flags.map_or_else(
            || {
                legacy_flags.map_or(
                    (
                        crate::metadata::property_flags::DEFAULT,
                        crate::metadata::property_flags::IS_VAR,
                        crate::metadata::property_flags::IS_CONST,
                    ),
                    |flags| (flags, LEGACY_IS_VAR, LEGACY_IS_CONST),
                )
            },
            |flags| {
                (
                    flags,
                    crate::metadata::property_flags::IS_VAR,
                    crate::metadata::property_flags::IS_CONST,
                )
            },
        );
        let is_var = setter_signature.is_some() || flags & is_var_bit != 0;
        let generic_sig = build_property_generic_sig(
            ParsedPropertySignature {
                inherited: class_tparams,
                inherited_bounds: class_tparam_bounds,
                type_params: &type_params,
                context_params: &context_params,
                context_receiver_bodies: &context_receiver_bodies,
                context_receiver_type_ids: &context_receiver_type_ids,
                return_body: ret_body,
                return_nullable: ret_nullable,
                receiver_body,
                receiver_nullable,
            },
            records,
            d2,
            type_table,
        );
        if let Some(signature) = &generic_sig {
            ret = signature.ret.non_null().obj_internal();
            ret_nullable = signature.ret.is_nullable();
            if receiver_body.is_some() {
                receiver_class = signature
                    .receiver
                    .and_then(|ty| ty.non_null().obj_internal());
            }
        }
        let context_names = if context_params.is_empty() {
            vec![String::new(); context_receiver_bodies.len() + context_receiver_type_ids.len()]
        } else {
            context_params
                .iter()
                .map(|parameter| {
                    match resolve_string(records, d2, parameter.name_id as usize).as_deref() {
                        Some("<unused var>") => "_".to_owned(),
                        Some(name) => name.to_owned(),
                        None => String::new(),
                    }
                })
                .collect()
        };
        let context_parameter_kinds = if context_params.is_empty() {
            vec![crate::types::ContextParameterKind::LegacyReceiver; context_names.len()]
        } else {
            context_names
                .iter()
                .map(|name| {
                    if name == "_" {
                        crate::types::ContextParameterKind::Anonymous
                    } else {
                        crate::types::ContextParameterKind::Named
                    }
                })
                .collect()
        };
        let decoded_context_params = context_names
            .into_iter()
            .zip(
                generic_sig
                    .as_ref()
                    .into_iter()
                    .flat_map(|signature| signature.params.iter().copied()),
            )
            .map(|(name, ty)| MetaValueParam {
                ty: declared_classifier(ty),
                name,
                flags: MvpFlags::default()
                    .with_nullable(ty.is_nullable())
                    .with_has_type_facts(true),
                recv_fun_receiver: None,
            })
            .collect::<Vec<_>>();
        // `JvmPropertySignature` and each nested `JvmMethodSignature` field are optional when the
        // physical accessor follows Kotlin's default mapping. Complete that metadata declaration
        // here, while its receiver/return types and flags are still together. Downstream symbol
        // sources may verify the resulting handle against bytecode, but must not guess it by name.
        let accessor_types = generic_sig.as_ref().map(|signature| {
            let mut getter_params = signature.params.clone();
            if let Some(receiver) = signature.receiver {
                getter_params.push(receiver);
            }
            (getter_params, signature.ret)
        });
        let default_getter_desc = accessor_types
            .as_ref()
            .map(|(params, ty)| method_descriptor(params, *ty));
        let default_setter_desc = accessor_types.as_ref().map(|(params, ty)| {
            let mut params = params.clone();
            params.push(*ty);
            method_descriptor(&params, Ty::Unit)
        });
        let materialize_accessor =
            |signature: Option<ParsedJvmSignature>,
             default_name: String,
             default_desc: Option<String>| {
                let name = signature
                    .and_then(|signature| signature.name_id)
                    .and_then(|id| resolve_string(records, d2, id as usize))
                    .unwrap_or(default_name);
                let desc = signature
                    .and_then(|signature| signature.desc_id)
                    .and_then(|id| resolve_string(records, d2, id as usize))
                    .or(default_desc)?;
                Some(MetaJvmMethodSig { name, desc })
            };
        let getter = materialize_accessor(
            getter_signature,
            crate::names::property_getter_name(&name),
            default_getter_desc,
        );
        let setter = is_var
            .then(|| {
                materialize_accessor(
                    setter_signature,
                    crate::names::property_setter_name(&name),
                    default_setter_desc,
                )
            })
            .flatten();
        // The backing field, named and typed by default unless the signature says otherwise. Only
        // a `const val` consumer reads it: its `ConstantValue` is the declaration's constant.
        let field = {
            let name = field_signature
                .and_then(|signature| signature.name_id)
                .and_then(|id| resolve_string(records, d2, id as usize))
                .unwrap_or_else(|| name.clone());
            field_signature
                .and_then(|signature| signature.desc_id)
                .and_then(|id| resolve_string(records, d2, id as usize))
                .or_else(|| {
                    accessor_types
                        .as_ref()
                        .map(|(_, ty)| crate::jvm::names::type_descriptor(*ty))
                })
                .map(|desc| MetaJvmFieldSig { name, desc })
        };
        out.push(MetaProp {
            name,
            ret_class: ret,
            ret_nullable,
            generic_sig,
            context_params: decoded_context_params,
            context_parameter_kinds,
            getter,
            setter,
            field,
            setter_parameter_name,
            visibility: crate::types::Visibility::from_metadata(flags_visibility(flags)),
            return_value_status: modern_flags.map_or_else(Default::default, |flags| {
                crate::types::ReturnValueStatus::from_metadata(
                    (flags >> crate::metadata::property_flags::RETURN_VALUE_STATUS_SHIFT) & 0x3,
                )
            }),
            is_const: flags & is_const_bit != 0,
            is_abstract: (flags >> 4) & 0x3 == 2,
            is_var,
            is_inline_underlying: inline_underlying_property_name_id == Some(name_id),
            receiver_class,
            is_extension: receiver_body.is_some(),
            is_companion_block_member: receiver_body.is_none()
                && modern_flags.is_some_and(|flags| {
                    flags & crate::metadata::property_flags::IS_COMPANION != 0
                }),
        });
    }
    Ok(out)
}
