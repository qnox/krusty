//! Where a lifted local callable takes each captured value and captured receiver.
//!
//! The checked body lists its captured values, then its captured receivers; that is their identity
//! (`FirCaptureParameter` ordinals). kotlinc's closure annotator instead takes the dispatch receiver
//! where the body first uses it, among the values it reads directly: `prefix + count` in a member
//! captures `this` first, `count + prefix` captures `count` first. A capture the body reaches only
//! through a nested closure keeps its relative place after the ones it reads directly.

use crate::fx_hash::HashMap;

use super::{FirBody, FirCaptureSource, FirExprKind, FirStatementKind, OriginStore};

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
        for statement in &mut self.statements {
            if let FirStatementKind::LocalFunction { body, .. } = &mut statement.kind {
                body.order_capture_parameters(origins);
            }
        }
        for expression in &mut self.expressions {
            if let FirExprKind::Lambda { body, .. } = &mut expression.kind {
                body.order_capture_parameters(origins);
            }
        }
    }

    fn order_capture_parameters(&mut self, origins: &OriginStore) {
        self.order_nested_capture_parameters(origins);
        self.capture_parameters = Box::default();
        let mut values = HashMap::default();
        let mut receiver_reads = vec![u32::MAX; self.implicit_receiver_captures.len()];
        for expression in &self.expressions {
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
                    let first = values
                        .entry((*enclosing_depth, FirCaptureSource::Value(*source)))
                        .or_insert(position);
                    *first = (*first).min(position);
                }
                FirExprKind::CapturedImplicitReceiver {
                    enclosing_depth,
                    current,
                    depth,
                    path,
                } => {
                    let ordinal = self.implicit_receiver_captures.iter().position(|capture| {
                        capture.enclosing_depth == *enclosing_depth
                            && capture.current == *current
                            && capture.depth == *depth
                            && capture.path == *path
                    });
                    if let Some(first) = ordinal.map(|ordinal| &mut receiver_reads[ordinal]) {
                        *first = (*first).min(position);
                    }
                }
                _ => {}
            }
        }
        let value_reads = self
            .captures
            .iter()
            .map(|capture| {
                values
                    .get(&(capture.enclosing_depth, capture.source))
                    .copied()
                    .unwrap_or(u32::MAX)
            })
            .collect::<Vec<_>>();
        // Merge the two lists, each in its own order: a receiver goes ahead of the next value when
        // the body reads it first.
        let (mut value, mut receiver) = (0, 0);
        let mut order = Vec::with_capacity(value_reads.len() + receiver_reads.len());
        while value < value_reads.len() || receiver < receiver_reads.len() {
            let receiver_first = receiver < receiver_reads.len()
                && value_reads
                    .get(value)
                    .is_none_or(|&first| receiver_reads[receiver] < first);
            if receiver_first {
                order.push(FirCaptureParameter::Receiver(index(receiver)));
                receiver += 1;
            } else {
                order.push(FirCaptureParameter::Value(index(value)));
                value += 1;
            }
        }
        if order != self.capture_parameters() {
            self.capture_parameters = order.into();
        }
    }
}

fn index(position: usize) -> u32 {
    u32::try_from(position).expect("too many FIR capture parameters")
}
