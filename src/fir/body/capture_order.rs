//! The order a lifted local function's method takes its captured values in.
//!
//! The checker declares a local function's captures before it checks the body, and orders them
//! once the body is checked (`body_check::capture_order`). Each capture keeps the position it was
//! declared at, so a caller that supplies the captures from the declared list can place them.

use super::{FirBody, FirCaptureSource};

impl FirBody {
    /// Put the captures named by `order`, each an `(enclosing_depth, source)` key, first and in
    /// that order. A capture `order` does not name keeps its relative place after them.
    pub fn order_captures(&mut self, order: &[(u32, FirCaptureSource)]) {
        let mut ordinals = Vec::with_capacity(self.captures.len());
        for &(depth, source) in order {
            let position = self
                .captures
                .iter()
                .position(|capture| capture.enclosing_depth == depth && capture.source == source);
            if let Some(position) = position.filter(|position| !ordinals.contains(position)) {
                ordinals.push(position);
            }
        }
        for position in 0..self.captures.len() {
            if !ordinals.contains(&position) {
                ordinals.push(position);
            }
        }
        self.captures = ordinals
            .iter()
            .map(|&position| self.captures[position])
            .collect();
        self.capture_declaration_ordinals = ordinals
            .into_iter()
            .map(|position| u32::try_from(position).expect("too many FIR captures"))
            .collect();
    }

    /// For each capture in order, the position it was declared at. Recorded by
    /// [`Self::order_captures`]; a body whose captures were never ordered has none.
    pub fn capture_declaration_ordinals(&self) -> &[u32] {
        &self.capture_declaration_ordinals
    }
}
