//! The order kotlinc visits a class's members when it serializes `@Metadata`: the source
//! declarations in declaration order (properties, functions, type aliases and enum entries
//! interleaved), then the members the compiler generates for a data or value class, then the
//! members a compiler plugin generates. Strings enter `d2` in this order, and each protobuf list
//! keeps it.

use std::ops::Range;

use crate::ir::{IrClass, IrFile, IrGeneratedFunctionMetadataScope, IrGeneratedMemberPublication};
use crate::metadata::builder::TypeAliasMeta;
use crate::metadata::class_builder::ClassMemberOrder;

/// `declared_fids` are the declared functions, which open the metadata function list; the
/// `synthesized` indices of that list follow them (a data or value class's generated members), and
/// the plugin-generated functions follow those.
pub(super) fn member_order(
    ir: &IrFile,
    c: &IrClass,
    prop_source_orders: &[u32],
    declared_fids: &[u32],
    synthesized: Range<usize>,
    generated_publication: Option<&IrGeneratedMemberPublication>,
    type_aliases: &[TypeAliasMeta],
) -> Vec<ClassMemberOrder> {
    let exclusive = matches!(
        generated_publication.map(|publication| publication.metadata_scope),
        Some(IrGeneratedFunctionMetadataScope::Exclusive)
    );
    let mut ordered = Vec::new();
    ordered.extend(
        prop_source_orders
            .iter()
            .copied()
            .enumerate()
            .map(|(index, order)| (order, ClassMemberOrder::Property(index))),
    );
    if !exclusive {
        ordered.extend(declared_fids.iter().enumerate().map(|(index, fid)| {
            (
                ir.fn_source_order.get(fid).copied().unwrap_or(u32::MAX),
                ClassMemberOrder::Function(index),
            )
        }));
    }
    ordered.extend(type_aliases.iter().enumerate().map(|(index, alias)| {
        (
            u32::try_from(alias.decl_order).unwrap_or(u32::MAX),
            ClassMemberOrder::TypeAlias(index),
        )
    }));
    ordered.extend(
        c.enum_entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.source_order, ClassMemberOrder::EnumEntry(index))),
    );
    let mut generated_order = if exclusive {
        0
    } else {
        ordered
            .iter()
            .map(|(order, _)| *order)
            .filter(|order| *order != u32::MAX)
            .max()
            .map_or(0, |order| order.saturating_add(1))
    };
    let mut generated = |member| {
        let order = generated_order;
        generated_order = generated_order.saturating_add(1);
        (order, member)
    };
    let inferred_method_count = synthesized.end;
    let synthesized: Vec<_> = synthesized
        .map(|index| generated(ClassMemberOrder::Function(index)))
        .collect();
    ordered.extend(synthesized);
    if let Some(publication) = generated_publication {
        let plugin: Vec<_> = publication
            .functions
            .iter()
            .filter(|member| member.metadata.is_some())
            .enumerate()
            .map(|(index, _)| generated(ClassMemberOrder::Function(inferred_method_count + index)))
            .collect();
        ordered.extend(plugin);
    }
    ordered.sort_by_key(|(order, _)| *order);
    ordered.into_iter().map(|(_, member)| member).collect()
}
