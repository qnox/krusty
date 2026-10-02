//! The order kotlinc visits a class's members when it serializes `@Metadata`: the source
//! declarations in declaration order (properties, functions, type aliases and enum entries
//! interleaved), then the members the compiler generates for a data or value class, then the
//! members a compiler plugin generates, then the members forwarding to interface delegates
//! (functions, then properties, each sorted by name). Strings enter `d2` in this order, and each
//! protobuf list keeps it.

use std::ops::Range;

use crate::ir::{
    IrClass, IrFile, IrGeneratedFunctionMetadataScope, IrGeneratedMemberPublication, IrMemberKind,
};
use crate::metadata::builder::TypeAliasMeta;
use crate::metadata::class_builder::{ClassMemberOrder, FnMeta, PropMeta};
use crate::types::{wk, Ty, TypeName};
use std::cmp::Ordering;

/// Indices into a class's metadata function and property lists of members the compiler generates.
pub(super) struct MemberRanges {
    /// A data or value class's generated functions, following the declared ones.
    pub synthesized: Range<usize>,
    /// Forwarders to interface delegates, following the plugin-generated functions.
    pub delegation_functions: Range<usize>,
    /// Delegation properties' positions, already in their metadata order.
    pub delegation_properties: Vec<usize>,
}

/// A class's forwarders to interface delegates that its metadata records, in JVM method order.
pub(super) fn delegation_functions(ir: &IrFile, c: &IrClass) -> Vec<u32> {
    c.methods
        .iter()
        .copied()
        .filter(|function| ir.is_interface_delegation_function(*function))
        .collect()
}

/// Sort delegation functions as kotlinc's `FirCallableDeclarationComparator` orders them: by name,
/// extension receiver, return type, then value parameters (count, then types in order).
pub(super) fn sort_delegation_functions(functions: &mut [FnMeta]) {
    functions.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| receiver_cmp(left.receiver, right.receiver))
            .then_with(|| type_cmp(left.ret, right.ret))
            .then_with(|| {
                let left = &left.params[left.context_count..];
                let right = &right.params[right.context_count..];
                left.len().cmp(&right.len()).then_with(|| {
                    left.iter()
                        .zip(right)
                        .map(|((_, left), (_, right))| type_cmp(*left, *right))
                        .find(|order| order.is_ne())
                        .unwrap_or(Ordering::Equal)
                })
            })
    });
}

fn receiver_cmp(left: Option<Ty>, right: Option<Ty>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => type_cmp(left, right),
        (left, right) => left.is_some().cmp(&right.is_some()),
    }
}

/// kotlinc's type order for declaration sorting: classifier types by class id, then their
/// arguments, then nullability; type parameters and other shapes after them.
fn type_cmp(left: Ty, right: Ty) -> Ordering {
    fn classifier(ty: Ty) -> Option<TypeName> {
        match ty.non_null() {
            Ty::Obj(name, _) => Some(name),
            Ty::Unit => Some(wk::unit()),
            Ty::Nothing => Some(wk::nothing()),
            _ => None,
        }
    }
    let rank = |ty: Ty| match ty.non_null() {
        _ if classifier(ty).is_some() => 0,
        Ty::TyParam(..) => 1,
        _ => 2,
    };
    rank(left)
        .cmp(&rank(right))
        .then_with(|| match (classifier(left), classifier(right)) {
            (Some(left), Some(right)) => left.path_cmp(right),
            _ => Ordering::Equal,
        })
        .then_with(|| match (left.non_null(), right.non_null()) {
            (Ty::Obj(_, left), Ty::Obj(_, right)) => left.len().cmp(&right.len()).then_with(|| {
                left.iter()
                    .zip(right)
                    .map(|(left, right)| type_cmp(*left, *right))
                    .find(|order| order.is_ne())
                    .unwrap_or(Ordering::Equal)
            }),
            _ => Ordering::Equal,
        })
        .then_with(|| left.is_nullable().cmp(&right.is_nullable()))
}

/// Put the properties forwarding to an interface delegate in kotlinc's metadata order (by name,
/// extension receiver, then type) within the positions they occupy (`source_orders` is permuted alongside), and return those
/// positions.
pub(super) fn sort_delegation_properties(
    properties: &mut Vec<PropMeta>,
    source_orders: &mut [u32],
) -> Vec<usize> {
    let positions = (0..properties.len())
        .filter(|index| properties[*index].modifiers.member_kind == IrMemberKind::Delegation)
        .collect::<Vec<_>>();
    let mut sorted = positions.clone();
    sorted.sort_by(|left, right| {
        let (left, right) = (&properties[*left], &properties[*right]);
        left.name
            .cmp(&right.name)
            .then_with(|| receiver_cmp(left.receiver, right.receiver))
            .then_with(|| type_cmp(left.ty, right.ty))
    });
    let mut taken = std::mem::take(properties)
        .into_iter()
        .zip(source_orders.iter().copied())
        .map(Some)
        .collect::<Vec<_>>();
    let mut delegation = sorted.into_iter();
    for (index, order_slot) in source_orders.iter_mut().enumerate().take(taken.len()) {
        let source = if positions.contains(&index) {
            delegation
                .next()
                .expect("one sorted delegation property per position")
        } else {
            index
        };
        let (property, order) = taken[source].take().expect("each property moves once");
        properties.push(property);
        *order_slot = order;
    }
    positions
}

/// `declared_fids` are the declared functions, which open the metadata function list; the
/// `synthesized` indices of that list follow them (a data or value class's generated members), the
/// plugin-generated functions follow those, and the delegation members close the list.
pub(super) fn member_order(
    ir: &IrFile,
    c: &IrClass,
    prop_source_orders: &[u32],
    declared_fids: &[u32],
    ranges: MemberRanges,
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
            .filter(|(index, _)| !ranges.delegation_properties.contains(index))
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
    let inferred_method_count = ranges.synthesized.end;
    let synthesized: Vec<_> = ranges
        .synthesized
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
    let delegation: Vec<_> = ranges
        .delegation_functions
        .map(ClassMemberOrder::Function)
        .chain(
            ranges
                .delegation_properties
                .into_iter()
                .map(ClassMemberOrder::Property),
        )
        .map(&mut generated)
        .collect();
    ordered.extend(delegation);
    ordered.sort_by_key(|(order, _)| *order);
    ordered.into_iter().map(|(_, member)| member).collect()
}
