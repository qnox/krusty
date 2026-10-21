//! Where a lifted local callable takes each captured value and captured receiver.
//!
//! The checked body lists its captured values, then its captured receivers; that is their identity
//! (`FirCaptureParameter` ordinals). kotlinc's closure annotator instead takes first the captures
//! the body reads itself, where it first reads them: `prefix + count` in a member captures `this`
//! first, `count + prefix` captures `count` first, and `if (length > 0) "" + count + prefix` in a
//! member extension takes the extension receiver, then `count`, then `this`. After those come the
//! captures of each nested lambda, called local function and created anonymous object, in the
//! order the body meets them and each in that callee's own order: `plainIt { prefix } + count`
//! captures `count`, then `this`.

use crate::fx_hash::HashMap;

use super::{
    DeclarationId, FirBody, FirCaptureSource, FirExprKind, FirStatementKind, LocalCallableId,
    OriginStore,
};
use crate::fir::FirLocalClassCaptureSource;

/// One lifted capture parameter, by its ordinal among the body's values or receivers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirCaptureParameter {
    Value(u32),
    Receiver(u32),
}

impl FirBody {
    /// The lifted capture parameters in physical order.
    pub fn capture_parameters(&self) -> Vec<FirCaptureParameter> {
        if !self.capture_parameters.is_empty() {
            return self.capture_parameters.to_vec();
        }
        let values =
            (0..self.captures.len()).map(|ordinal| FirCaptureParameter::Value(index(ordinal)));
        let receivers = (0..self.implicit_receiver_captures.len())
            .map(|ordinal| FirCaptureParameter::Receiver(index(ordinal)));
        values.chain(receivers).collect()
    }

    /// The physical position of the captured value `ordinal`.
    pub fn value_capture_position(&self, ordinal: usize) -> u32 {
        self.capture_position(FirCaptureParameter::Value(index(ordinal)))
    }

    /// The physical position of the captured receiver `ordinal`.
    pub fn receiver_capture_position(&self, ordinal: usize) -> u32 {
        self.capture_position(FirCaptureParameter::Receiver(index(ordinal)))
    }

    fn capture_position(&self, parameter: FirCaptureParameter) -> u32 {
        let position = self
            .capture_parameters()
            .iter()
            .position(|&candidate| candidate == parameter)
            .expect("a capture ordinal names one of the body's capture parameters");
        index(position)
    }

    /// Order the capture parameters of every local function and lambda nested in this body, once
    /// their capture sets are final.
    pub(crate) fn order_nested_capture_parameters(&mut self, origins: &OriginStore) {
        let mut closures = Vec::new();
        collect_closures(
            self,
            None,
            origins,
            &mut Vec::new(),
            &mut Vec::new(),
            &mut closures,
        );
        let mut orders = vec![None; closures.len()];
        for closure in 0..closures.len() {
            build_order(&closures, closure, &mut orders);
        }
        apply_orders(self, None, &orders, &mut Vec::new(), &mut 0);
    }
}

/// A capture named by the body that supplies it, so the same capture compares equal from every
/// nested callable that forwards it. `supplier` is that body's id: zero for the checked body, one
/// past its closure index for a nested callable. Sibling bodies number their locals afresh, so
/// their nesting level alone would not tell their locals apart.
#[derive(Clone, Debug, PartialEq, Eq)]
enum CaptureKey {
    Value {
        supplier: usize,
        source: FirCaptureSource,
    },
    Receiver {
        supplier: usize,
        current: bool,
        depth: u32,
        path: Box<[DeclarationId]>,
    },
}

/// What one lifted callable's order is built from: the captures its body reads itself, by first
/// read, and the nested closures it meets, in source order.
#[derive(Default)]
struct Closure {
    reads: Vec<CaptureKey>,
    includes: Vec<Include>,
}

/// A closure a body meets: a nested or called lifted callable, or an anonymous object, whose
/// captures are already final in its own order.
enum Include {
    Callable(usize),
    Object(Vec<CaptureKey>),
}

/// The local functions, then the lambdas, declared directly in `body`.
fn nested_bodies(body: &FirBody) -> impl Iterator<Item = (LocalCallableId, &FirBody)> {
    let functions = body
        .statements
        .iter()
        .filter_map(|statement| match &statement.kind {
            FirStatementKind::LocalFunction { callable, body, .. } => Some((*callable, &**body)),
            _ => None,
        });
    let lambdas = body
        .expressions
        .iter()
        .filter_map(|expression| match &expression.kind {
            FirExprKind::Lambda { callable, body, .. } => Some((*callable, &**body)),
            _ => None,
        });
    functions.chain(lambdas)
}

/// Give `body`'s nested callables consecutive closure indices, then record `body`'s own closure
/// (when it is itself lifted, as `index`) and recurse, depth first. `apply_orders` walks the same
/// way.
fn collect_closures(
    body: &FirBody,
    index: Option<usize>,
    origins: &OriginStore,
    ancestors: &mut Vec<usize>,
    scopes: &mut Vec<HashMap<LocalCallableId, usize>>,
    closures: &mut Vec<Closure>,
) {
    ancestors.push(body_id(index));
    let first = closures.len();
    let mut scope = HashMap::default();
    for (offset, (callable, _)) in nested_bodies(body).enumerate() {
        scope.insert(callable, first + offset);
        closures.push(Closure::default());
    }
    scopes.push(scope);
    if let Some(index) = index {
        let mut closure = closure_of(body, origins, ancestors, scopes);
        closure
            .includes
            .retain(|include| !matches!(include, Include::Callable(callee) if *callee == index));
        closures[index] = closure;
    }
    for (offset, (_, nested)) in nested_bodies(body).enumerate() {
        collect_closures(
            nested,
            Some(first + offset),
            origins,
            ancestors,
            scopes,
            closures,
        );
    }
    scopes.pop();
    ancestors.pop();
}

/// The id `CaptureKey` names a body by.
fn body_id(index: Option<usize>) -> usize {
    index.map_or(0, |index| index + 1)
}

/// The id of the body that supplies a capture `enclosing_depth` above the parent of the innermost
/// of `ancestors`.
fn supplier(ancestors: &[usize], enclosing_depth: u32) -> Option<usize> {
    let parent = ancestors.len().checked_sub(2)?;
    let supplier = parent.checked_sub(enclosing_depth as usize)?;
    Some(ancestors[supplier])
}

/// The direct reads and met closures of the lifted `body`, the last of `ancestors`; `scopes` ends
/// with the scope of `body`'s own nested callables.
fn closure_of(
    body: &FirBody,
    origins: &OriginStore,
    ancestors: &[usize],
    scopes: &[HashMap<LocalCallableId, usize>],
) -> Closure {
    let supplier = |enclosing_depth: u32| supplier(ancestors, enclosing_depth);
    let resolve = |depth: u32, callable: &LocalCallableId| {
        scopes
            .len()
            .checked_sub(depth as usize + 1)
            .and_then(|scope| scopes[scope].get(callable))
            .copied()
    };
    let mut reads = Vec::new();
    let mut includes = Vec::new();
    for expression in &body.expressions {
        let Some(position) = origins
            .source_span(expression.origin)
            .map(|(_, span)| span.lo)
        else {
            continue;
        };
        match &expression.kind {
            FirExprKind::CapturedValueRead {
                enclosing_depth,
                source,
            }
            | FirExprKind::CapturedValueWrite {
                enclosing_depth,
                source,
                ..
            } => {
                if let Some(supplier) = supplier(*enclosing_depth) {
                    let source = FirCaptureSource::Value(*source);
                    reads.push((position, CaptureKey::Value { supplier, source }));
                }
            }
            FirExprKind::CapturedImplicitReceiver {
                enclosing_depth,
                current,
                depth,
                path,
            } => {
                if let Some(supplier) = supplier(*enclosing_depth) {
                    let key = CaptureKey::Receiver {
                        supplier,
                        current: *current,
                        depth: *depth,
                        path: path.clone(),
                    };
                    reads.push((position, key));
                }
            }
            FirExprKind::LocalCall { target, .. }
            | FirExprKind::LocalCallableReference { target, .. } => {
                includes.extend(
                    resolve(target.body_depth, &target.callable)
                        .map(|closure| (position, Include::Callable(closure))),
                );
            }
            FirExprKind::Lambda { callable, .. } => {
                includes.extend(
                    resolve(0, callable).map(|closure| (position, Include::Callable(closure))),
                );
            }
            FirExprKind::AnonymousObject(object) => {
                let keys = object
                    .captures
                    .iter()
                    .filter_map(|capture| match &capture.source {
                        FirLocalClassCaptureSource::Captured {
                            enclosing_depth,
                            source,
                        } => Some(CaptureKey::Value {
                            supplier: supplier(*enclosing_depth)?,
                            source: FirCaptureSource::Value(*source),
                        }),
                        FirLocalClassCaptureSource::CapturedImplicitReceiver {
                            enclosing_depth,
                            current,
                            depth,
                            path,
                        } => Some(CaptureKey::Receiver {
                            supplier: supplier(*enclosing_depth)?,
                            current: *current,
                            depth: *depth,
                            path: path.clone(),
                        }),
                        _ => None,
                    })
                    .collect();
                includes.push((position, Include::Object(keys)));
            }
            _ => {}
        }
    }
    reads.sort_by_key(|(position, _)| *position);
    includes.sort_by_key(|(position, _)| *position);
    Closure {
        reads: reads.into_iter().map(|(_, key)| key).collect(),
        includes: includes.into_iter().map(|(_, closure)| closure).collect(),
    }
}

/// The capture order of closure `index`: its direct reads, then each met closure's order. A closure
/// met again while its own order is being built contributes what it has so far.
fn build_order(closures: &[Closure], index: usize, orders: &mut [Option<Vec<CaptureKey>>]) {
    if orders[index].is_some() {
        return;
    }
    let mut order = Vec::new();
    for key in &closures[index].reads {
        if !order.contains(key) {
            order.push(key.clone());
        }
    }
    orders[index] = Some(order.clone());
    for include in &closures[index].includes {
        let keys = match include {
            Include::Callable(callee) => {
                build_order(closures, *callee, orders);
                orders[*callee].clone().unwrap_or_default()
            }
            Include::Object(keys) => keys.clone(),
        };
        for key in &keys {
            if !order.contains(key) {
                order.push(key.clone());
            }
        }
        orders[index] = Some(order.clone());
    }
}

fn apply_orders(
    body: &mut FirBody,
    index: Option<usize>,
    orders: &[Option<Vec<CaptureKey>>],
    ancestors: &mut Vec<usize>,
    next: &mut usize,
) {
    ancestors.push(body_id(index));
    let first = *next;
    *next += nested_bodies(body).count();
    let mut offset = 0;
    let mut apply = |nested: &mut FirBody| {
        let index = first + offset;
        ancestors.push(body_id(Some(index)));
        let order = orders[index].as_deref().unwrap_or_default();
        nested.order_capture_parameters(ancestors, order);
        ancestors.pop();
        apply_orders(nested, Some(index), orders, ancestors, next);
        offset += 1;
    };
    for statement in &mut body.statements {
        if let FirStatementKind::LocalFunction { body, .. } = &mut statement.kind {
            apply(body);
        }
    }
    for expression in &mut body.expressions {
        if let FirExprKind::Lambda { body, .. } = &mut expression.kind {
            apply(body);
        }
    }
    ancestors.pop();
}

impl FirBody {
    /// Store the physical order of this lifted body's captures, the last of `ancestors`, ranked by
    /// `order`; a capture `order` does not name keeps its relative place after the ones it does.
    fn order_capture_parameters(&mut self, ancestors: &[usize], order: &[CaptureKey]) {
        self.capture_parameters = Box::default();
        let rank = |key: Option<CaptureKey>| {
            key.and_then(|key| order.iter().position(|candidate| *candidate == key))
                .unwrap_or(usize::MAX)
        };
        let supplier = |enclosing_depth: u32| supplier(ancestors, enclosing_depth);
        let mut parameters = self.capture_parameters();
        parameters.sort_by_cached_key(|parameter| match *parameter {
            FirCaptureParameter::Value(ordinal) => {
                let capture = &self.captures[ordinal as usize];
                rank(
                    supplier(capture.enclosing_depth).map(|supplier| CaptureKey::Value {
                        supplier,
                        source: capture.source,
                    }),
                )
            }
            FirCaptureParameter::Receiver(ordinal) => {
                let capture = &self.implicit_receiver_captures[ordinal as usize];
                rank(
                    supplier(capture.enclosing_depth).map(|supplier| CaptureKey::Receiver {
                        supplier,
                        current: capture.current,
                        depth: capture.depth,
                        path: capture.path.clone(),
                    }),
                )
            }
        });
        if parameters != self.capture_parameters() {
            self.capture_parameters = parameters.into();
        }
    }
}

fn index(position: usize) -> u32 {
    u32::try_from(position).expect("too many FIR capture parameters")
}
