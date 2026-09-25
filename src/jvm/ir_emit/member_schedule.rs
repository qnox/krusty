//! Source-owned class-member scheduling for JVM emission.

use super::secondary_constructor::defer_serialization_constructor;
use crate::ir::{IrClass, IrFile, IrProperty, IrSecondaryCtor};

/// One entry of a file facade's method table.
#[derive(Clone, Copy)]
pub(super) enum FacadeMember {
    Function(u32),
    /// The generated accessors of the facade-owned static at this index.
    PropertyAccessors(u32),
}

/// Order a file facade's methods as kotlinc's `FileClassLowering` collects them: the file's
/// declarations in source order, with each top-level property's accessors at the property's
/// position. Functions without a source declaration (lifted lambdas and local functions) keep their
/// registration order after the declared members.
pub(super) fn facade_source_ordered_members(
    ir: &IrFile,
    functions: impl Iterator<Item = u32>,
) -> Vec<FacadeMember> {
    let mut ordered: Vec<(u32, FacadeMember)> = functions
        .map(|function| {
            let order = ir
                .fn_source_order
                .get(&function)
                .copied()
                .unwrap_or(u32::MAX);
            (order, FacadeMember::Function(function))
        })
        .collect();
    ordered.extend(
        ir.statics
            .iter()
            .enumerate()
            .filter(|(_, property)| property.is_facade_owned())
            .map(|(index, property)| {
                (
                    property.source_order,
                    FacadeMember::PropertyAccessors(index as u32),
                )
            }),
    );
    ordered.sort_by_key(|(order, _)| *order);
    let mut members: Vec<FacadeMember> = ordered.into_iter().map(|(_, member)| member).collect();
    let mut functions: Vec<u32> = members
        .iter()
        .filter_map(|member| match member {
            FacadeMember::Function(function) => Some(*function),
            FacadeMember::PropertyAccessors(_) => None,
        })
        .collect();
    order_lifted_functions(ir, &mut functions);
    let mut reordered = functions.into_iter();
    for member in &mut members {
        if let FacadeMember::Function(function) = member {
            *function = reordered.next().expect("one function per function slot");
        }
    }
    members
}

pub(super) enum SourceOrderedMember<'a> {
    Property(&'a IrProperty),
    Function(u32),
    SecondaryConstructor(usize, &'a IrSecondaryCtor),
}

/// Interleave source declarations through one stable ordering key. Generated constructors are
/// excluded only when their producer recorded an exact later placement; JVM access flags never
/// participate in this semantic schedule.
pub(super) fn source_ordered_members<'a>(
    ir: &IrFile,
    class: &'a IrClass,
    deferred_serialization_constructor: Option<u32>,
) -> Vec<SourceOrderedMember<'a>> {
    let function_order = |function: u32| {
        ir.fn_source_order
            .get(&function)
            .copied()
            .unwrap_or(u32::MAX)
    };
    // A property's accessors are one source-owned unit. A property with an accessor function (a
    // custom accessor, a generated serializer `descriptor`, a delegation forwarder) takes that
    // function's slot and placement, so an implicit peer accessor cannot overtake it.
    let accessor_owner = |function: u32| {
        class
            .properties
            .iter()
            .find(|property| property.getter == Some(function) || property.setter == Some(function))
    };
    let first_accessor = |property: &IrProperty| property.getter.or(property.setter);
    let mut ordered: Vec<(u32, SourceOrderedMember<'a>)> = Vec::with_capacity(
        class.properties.len() + class.methods.len() + class.secondary_ctors.len(),
    );
    ordered.extend(
        class
            .properties
            .iter()
            .filter(|property| first_accessor(property).is_none())
            .map(|property| {
                (
                    property.source_order,
                    SourceOrderedMember::Property(property),
                )
            }),
    );
    ordered.extend(
        class
            .methods
            .iter()
            .copied()
            .filter(|function| !ir.serialization_cache_methods.contains(function))
            .filter_map(|function| match accessor_owner(function) {
                Some(property) => (first_accessor(property) == Some(function)).then(|| {
                    (
                        function_order(function),
                        SourceOrderedMember::Property(property),
                    )
                }),
                None => Some((
                    function_order(function),
                    SourceOrderedMember::Function(function),
                )),
            }),
    );
    ordered.extend(
        class
            .secondary_ctors
            .iter()
            .enumerate()
            .filter(|(ordinal, _)| {
                !defer_serialization_constructor(*ordinal, deferred_serialization_constructor)
            })
            .map(|(ordinal, constructor)| {
                (
                    constructor.source_order,
                    SourceOrderedMember::SecondaryConstructor(ordinal, constructor),
                )
            }),
    );
    ordered.sort_by_key(|(order, _)| *order);
    let mut ordered: Vec<SourceOrderedMember<'a>> =
        ordered.into_iter().map(|(_, member)| member).collect();
    let mut functions: Vec<u32> = ordered
        .iter()
        .filter_map(|member| match member {
            SourceOrderedMember::Function(function) => Some(*function),
            _ => None,
        })
        .collect();
    order_lifted_functions(ir, &mut functions);
    let mut reordered = functions.into_iter();
    for member in &mut ordered {
        if let SourceOrderedMember::Function(function) = member {
            *function = reordered.next().expect("one function per function slot");
        }
    }
    ordered
}

/// Split the schedule around the primary `<init>`. kotlinc emits a value class's declared members
/// and its `Any` overrides (`toString-impl` … `equals`) first, then the private primary `<init>`,
/// then the generated representation members in a fixed order: `constructor-impl`, `box-impl`,
/// `unbox-impl`, `equals-impl0`. Every other class emits `<init>` first.
pub(super) fn split_around_primary_constructor<'a>(
    ir: &IrFile,
    class: &IrClass,
    ordered: Vec<SourceOrderedMember<'a>>,
) -> (Vec<SourceOrderedMember<'a>>, Vec<SourceOrderedMember<'a>>) {
    if !class.is_value || !class.has_primary_ctor {
        return (Vec::new(), ordered);
    }
    let representation_rank = |member: &SourceOrderedMember<'_>| match member {
        SourceOrderedMember::Function(function) => ir
            .jvm_value_class_representation_order
            .get(function)
            .copied(),
        _ => None,
    };
    let (mut after, before): (Vec<_>, Vec<_>) = ordered
        .into_iter()
        .partition(|member| representation_rank(member).is_some());
    after.sort_by_key(|member| representation_rank(member));
    (before, after)
}

/// Reorder the functions lowered from lambdas and local functions inside `members` into kotlinc's
/// placement, leaving every other member where it is.
///
/// kotlinc's LocalDeclarationPopupLowering lowers bodies in postfix order and, as it finishes a
/// body, appends that body's local functions to the class in source order; a local function
/// declared inside a lambda belongs to the body enclosing the lambda. So `fun box() { fun foo() {
/// fun bar() {} } }` places `box$foo$bar` before `box$foo`. The methods of indy lambdas are added
/// by a later lowering that walks the class's members in their order by then: first every lambda of
/// the declared members' bodies, then those of each lifted local function's body.
pub(super) fn order_lifted_functions(ir: &IrFile, members: &mut [u32]) {
    let slots: Vec<usize> = (0..members.len())
        .filter(|&slot| ir.lifted_functions.contains_key(&members[slot]))
        .collect();
    if slots.len() < 2 {
        return;
    }
    // Sequences keep the order in which their first function was registered: lowering registers
    // each declaration's lifted functions together, in declaration order.
    let mut sequences = Vec::new();
    for &slot in &slots {
        let (sequence, _) = &ir.lifted_functions[&members[slot]];
        if !sequences.contains(&sequence) {
            sequences.push(sequence);
        }
    }
    let sequence_rank = |sequence| sequences.iter().position(|&s| s == sequence);
    let site = |function: &u32| &ir.lifted_functions[function];
    let is_lambda = |function: &u32| {
        site(function)
            .1
            .path
            .last()
            .is_none_or(|step| step.name.is_none())
    };
    let (mut local_functions, lambdas): (Vec<u32>, Vec<u32>) = slots
        .iter()
        .map(|&slot| members[slot])
        .partition(|function| !is_lambda(function));
    local_functions.sort_by(|left, right| {
        let ((left_sequence, left_site), (right_sequence, right_site)) = (site(left), site(right));
        sequence_rank(left_sequence)
            .cmp(&sequence_rank(right_sequence))
            .then_with(|| {
                popup_body_order(owning_body(&left_site.path), owning_body(&right_site.path))
            })
            .then_with(|| own_position(&left_site.path).cmp(&own_position(&right_site.path)))
    });
    // A lambda's body owner: the declared member (every one of which precedes the lifted local
    // functions), or the lifted local function whose path is the lambda's owning body.
    let lambda_key = |function: &u32| {
        let (sequence, lambda_site) = site(function);
        let body = owning_body(&lambda_site.path);
        let owner = local_functions.iter().position(|local| {
            let (local_sequence, local_site) = site(local);
            local_sequence == sequence && *local_site.path == *body
        });
        let body_rank = match (body.is_empty(), owner) {
            (false, Some(owner)) => (1, owner),
            _ => (0, sequence_rank(sequence).unwrap_or(usize::MAX)),
        };
        (body_rank, own_position(&lambda_site.path))
    };
    let mut lambdas = lambdas;
    lambdas.sort_by_key(lambda_key);
    for (slot, function) in slots
        .into_iter()
        .zip(local_functions.into_iter().chain(lambdas))
    {
        members[slot] = function;
    }
}

fn own_position(path: &[crate::fir::FirLiftingStep]) -> Option<u32> {
    path.last().map(|step| step.position)
}

/// The body a lifted function is lowered with: the path of its nearest enclosing local function, or
/// the empty path of the container itself. Lambdas are not bodies of their own.
fn owning_body(path: &[crate::fir::FirLiftingStep]) -> &[crate::fir::FirLiftingStep] {
    let enclosing = &path[..path.len().saturating_sub(1)];
    let depth = enclosing
        .iter()
        .rposition(|step| step.name.is_some())
        .map_or(0, |index| index + 1);
    &enclosing[..depth]
}

/// Postfix order of two bodies of one sequence: a nested body finishes before the body enclosing
/// it, and sibling bodies finish in source order.
fn popup_body_order(
    left: &[crate::fir::FirLiftingStep],
    right: &[crate::fir::FirLiftingStep],
) -> std::cmp::Ordering {
    for (left_step, right_step) in left.iter().zip(right) {
        if left_step.position != right_step.position {
            return left_step.position.cmp(&right_step.position);
        }
    }
    // One path is a prefix of the other: the longer (nested) one finishes first.
    right.len().cmp(&left.len())
}
