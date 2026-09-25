//! Declaration status a current-module override inherits, derived from frozen override edges.
//!
//! kotlinc resolves a declaration's `ReturnValueStatus` in `FirMustUseReturnValueStatusComponent`.
//! With the return-value checker disabled, the default of every supported kotlinc, a declaration
//! takes the status of the first declaration it directly overrides and is `Unspecified` otherwise,
//! whatever it is annotated with. A Java declaration records no status of its own, so the search
//! continues past it to the next overridden declaration; that is how an override of
//! `java.util.ArrayList.add` inherits Kotlin's `MutableCollection.add` status.
//!
//! Status resolution also carries the `operator` and `infix` modifiers down an override chain: a
//! function overriding one that has either modifier has it too, whatever it declares, and so does
//! an override of that override.

use std::collections::HashMap;
use std::hash::Hash;

use crate::fir::{
    DeclarationFlags, DeclarationId, InheritedCallableStatus, ResolvedFunctionOverride,
    ResolvedFunctionOverrideTarget, ResolvedModuleIndex, ResolvedPropertyOverride,
    ResolvedPropertyOverrideTarget,
};
use crate::types::ReturnValueStatus;

/// A resolved declaration status and what one override edge says about its overridden declaration.
trait Status: Copy + Default {
    /// The overridden declaration's facts as an edge records them.
    type Edge: Copy;

    /// The facts a resolved current-module declaration contributes to an override of it.
    fn as_edge(self) -> Self::Edge;

    /// A declaration's status from its own declared facts and its overridden declarations, nearest
    /// first.
    fn inherit(own: Self, overridden: &[Self::Edge]) -> Self;
}

impl Status for ReturnValueStatus {
    /// `None` for a Java declaration, which records no status.
    type Edge = Option<ReturnValueStatus>;

    fn as_edge(self) -> Self::Edge {
        Some(self)
    }

    fn inherit(_own: Self, overridden: &[Self::Edge]) -> Self {
        overridden.iter().find_map(|edge| *edge).unwrap_or_default()
    }
}

#[derive(Clone, Copy)]
struct OverriddenFunction {
    return_value: Option<ReturnValueStatus>,
    operator: bool,
    infix: bool,
}

impl Status for InheritedCallableStatus {
    type Edge = OverriddenFunction;

    fn as_edge(self) -> Self::Edge {
        OverriddenFunction {
            return_value: Some(self.return_value),
            operator: self.operator,
            infix: self.infix,
        }
    }

    fn inherit(own: Self, overridden: &[Self::Edge]) -> Self {
        InheritedCallableStatus {
            return_value: overridden
                .iter()
                .find_map(|edge| edge.return_value)
                .unwrap_or_default(),
            operator: own.operator || overridden.iter().any(|edge| edge.operator),
            infix: own.infix || overridden.iter().any(|edge| edge.infix),
        }
    }
}

/// What one override edge names as its overridden declaration.
#[derive(Clone, Copy)]
enum Overridden<Id, Edge> {
    Module(Id),
    External(Edge),
}

/// One classifier's frozen edges, as published for that classifier.
pub(super) struct ClassifierEdges<'a> {
    pub(super) classifier: DeclarationId,
    pub(super) functions: &'a [ResolvedFunctionOverride],
    pub(super) properties: &'a [ResolvedPropertyOverride],
}

pub(super) fn publish_inherited_statuses(
    index: &mut ResolvedModuleIndex,
    classifiers: &[ClassifierEdges<'_>],
) {
    let functions = own_edges(
        index,
        classifiers,
        |classifier| classifier.functions,
        |index, edge| match edge.implementation {
            ResolvedFunctionOverrideTarget::Module(callable) => index
                .callable(callable)
                .map(|header| (callable, header.declaration)),
            ResolvedFunctionOverrideTarget::External(_) => None,
        },
        |edge| match edge.overridden {
            ResolvedFunctionOverrideTarget::Module(callable) => Overridden::Module(callable),
            ResolvedFunctionOverrideTarget::External(_) => {
                Overridden::External(OverriddenFunction {
                    return_value: edge.overridden_return_value_status,
                    operator: edge.overridden_operator,
                    infix: edge.overridden_infix,
                })
            }
        },
        |edge| edge.depth,
    );
    let properties = own_edges(
        index,
        classifiers,
        |classifier| classifier.properties,
        |index, edge| match edge.implementation {
            ResolvedPropertyOverrideTarget::Module(property) => index
                .property(property)
                .map(|header| (property, header.declaration)),
            ResolvedPropertyOverrideTarget::External(_) => None,
        },
        |edge| match edge.overridden {
            ResolvedPropertyOverrideTarget::Module(property) => Overridden::Module(property),
            ResolvedPropertyOverrideTarget::External(_) => {
                Overridden::External(edge.overridden_return_value_status)
            }
        },
        |edge| edge.depth,
    );
    let own_function = |callable| {
        let flags = index
            .callable(callable)
            .and_then(|header| index.declaration_header(header.declaration))
            .map(|header| header.flags);
        InheritedCallableStatus {
            return_value: ReturnValueStatus::Unspecified,
            operator: flags.is_some_and(|flags| flags.has(DeclarationFlags::OPERATOR)),
            infix: flags.is_some_and(|flags| flags.has(DeclarationFlags::INFIX)),
        }
    };
    let functions = derive(&functions, own_function, |callable| {
        let own = own_function(callable);
        let published = index.callable_inherited_status(callable);
        InheritedCallableStatus {
            operator: own.operator || published.operator,
            infix: own.infix || published.infix,
            ..published
        }
    });
    let properties = derive(
        &properties,
        |_| ReturnValueStatus::Unspecified,
        |property| index.property_return_value_status(property),
    );
    for (callable, status) in functions {
        index.publish_callable_inherited_status(callable, status);
    }
    for (property, status) in properties {
        index.publish_property_return_value_status(property, status);
    }
}

/// Each implementation's edges in nearest-first order. A classifier also publishes edges for an
/// implementation it inherits (a superclass member realizing one of its interfaces); those say
/// nothing about the member's own declaration, so only edges published by the member's declaring
/// classifier count.
fn own_edges<'a, Id, Edge, E>(
    index: &ResolvedModuleIndex,
    classifiers: &[ClassifierEdges<'a>],
    edges: impl Fn(&ClassifierEdges<'a>) -> &'a [Edge],
    implementation: impl Fn(&ResolvedModuleIndex, &Edge) -> Option<(Id, DeclarationId)>,
    overridden: impl Fn(&Edge) -> Overridden<Id, E>,
    depth: impl Fn(&Edge) -> u32,
) -> Vec<(Id, Vec<Overridden<Id, E>>)>
where
    Id: Copy + Eq + Hash,
    Edge: 'a,
{
    let mut order = Vec::new();
    let mut grouped: HashMap<Id, Vec<(u32, Overridden<Id, E>)>> = HashMap::new();
    for classifier in classifiers {
        for edge in edges(classifier) {
            let Some((id, declaration)) = implementation(index, edge) else {
                continue;
            };
            let declared_here = index
                .declaration_anchor(declaration)
                .is_some_and(|anchor| anchor.owner == Some(classifier.classifier));
            if !declared_here {
                continue;
            }
            grouped
                .entry(id)
                .or_insert_with(|| {
                    order.push(id);
                    Vec::new()
                })
                .push((depth(edge), overridden(edge)));
        }
    }
    order
        .into_iter()
        .map(|id| {
            let mut edges = grouped.remove(&id).unwrap_or_default();
            edges.sort_by_key(|(depth, _)| *depth);
            (id, edges.into_iter().map(|(_, edge)| edge).collect())
        })
        .collect()
}

/// Resolve every implementation. Source order is not topological, so a base declared later is
/// resolved on demand; a declaration outside this batch was published by an earlier one.
fn derive<Id: Copy + Eq + Hash, S: Status>(
    implementations: &[(Id, Vec<Overridden<Id, S::Edge>>)],
    own: impl Fn(Id) -> S,
    published: impl Fn(Id) -> S,
) -> Vec<(Id, S)> {
    let edges = implementations
        .iter()
        .map(|(id, edges)| (*id, edges.as_slice()))
        .collect::<HashMap<_, _>>();
    let mut resolved = HashMap::new();
    implementations
        .iter()
        .map(|(id, _)| (*id, resolve(*id, &edges, &own, &published, &mut resolved)))
        .collect()
}

fn resolve<Id: Copy + Eq + Hash, S: Status>(
    id: Id,
    edges: &HashMap<Id, &[Overridden<Id, S::Edge>]>,
    own: &impl Fn(Id) -> S,
    published: &impl Fn(Id) -> S,
    resolved: &mut HashMap<Id, Option<S>>,
) -> S {
    let Some(overridden) = edges.get(&id) else {
        return published(id);
    };
    match resolved.get(&id) {
        Some(Some(status)) => return *status,
        // An override graph is acyclic; a cycle means a diagnosed hierarchy, which inherits nothing.
        Some(None) => return own(id),
        None => {}
    }
    resolved.insert(id, None);
    let overridden = overridden
        .iter()
        .map(|edge| match *edge {
            Overridden::Module(base) => resolve(base, edges, own, published, resolved).as_edge(),
            Overridden::External(edge) => edge,
        })
        .collect::<Vec<_>>();
    let status = S::inherit(own(id), &overridden);
    resolved.insert(id, Some(status));
    status
}
