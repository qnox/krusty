//! Common-IR facts a backend consumes as complete.
//!
//! A backend realizes a property from its layout and a captured mutable local from its recorded
//! holder parameter. It does not look for either anywhere else: not by an accessor's or field's
//! spelling, and not by scanning a body for the holder operations it happens to perform. So common
//! IR proves both tables complete before it crosses into a backend.

use crate::fir::{FirPropertyReferenceTarget, PropertyId, SourceFileId};

use super::super::{Callee, ExprId, FunId, IrCheckedOperation, IrExpr, IrFile};

/// A fact a backend reads as complete is missing from common IR.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IncompleteIrFact {
    /// A property declared in this file, or referenced by it, has no storage and accessor layout.
    PropertyLayout(PropertyId),
    /// A function's parameter receives a shared capture holder, through a capture edge or by
    /// reading or writing through it, but is not recorded as one in
    /// `IrFile::shared_capture_parameters`.
    SharedCaptureParameter { function: FunId, parameter: u32 },
}

impl IrFile {
    /// Prove that every property this file declares or references from its own declarations has a
    /// layout, and that every parameter a function uses as a shared capture holder is recorded.
    /// `source` is the file this common IR was lowered from.
    pub fn validate_complete_facts(&self, source: SourceFileId) -> Result<(), IncompleteIrFact> {
        self.validate_property_layouts(source)?;
        self.validate_shared_capture_parameters()
    }

    fn validate_property_layouts(&self, source: SourceFileId) -> Result<(), IncompleteIrFact> {
        let declared_here = self
            .referenced_module_properties
            .iter()
            .filter(|(_, property)| property.source.source == source)
            .map(|(property, _)| *property);
        for property in self.checked_properties.keys().copied().chain(declared_here) {
            if !self.local_property_layouts.contains_key(&property) {
                return Err(IncompleteIrFact::PropertyLayout(property));
            }
        }
        for expression in &self.exprs {
            let referenced = match expression {
                IrExpr::Checked(IrCheckedOperation::PropertyRead { target, .. })
                | IrExpr::Checked(IrCheckedOperation::PropertyWrite { target, .. }) => *target,
                IrExpr::Checked(IrCheckedOperation::PropertyReference { target, .. }) => {
                    match target {
                        FirPropertyReferenceTarget::Module(property)
                        | FirPropertyReferenceTarget::SpecializedModule { property, .. } => {
                            *property
                        }
                        FirPropertyReferenceTarget::Classifier { .. }
                        | FirPropertyReferenceTarget::External { .. } => continue,
                    }
                }
                _ => continue,
            };
            // A property another file declares is realized from its module fact; one this file
            // declares, from its layout.
            let elsewhere = self
                .referenced_module_properties
                .get(&referenced)
                .is_some_and(|property| property.source.source != source);
            if !elsewhere && !self.local_property_layouts.contains_key(&referenced) {
                return Err(IncompleteIrFact::PropertyLayout(referenced));
            }
        }
        Ok(())
    }

    /// A holder reaches a function's parameter through a capture edge. A slot holds one when the
    /// enclosing frame declares it with a new holder, or when it is a parameter already recorded
    /// as one. A lambda capture, a local or class-static call's leading argument, and a callable
    /// reference's capture each name the receiving function and parameter ordinal. Every such
    /// edge obliges that parameter to be recorded, whether or not the function reads the holder,
    /// so a function that only passes it on is covered. A parameter the function reads or writes
    /// through a holder operation must be recorded as well.
    fn validate_shared_capture_parameters(&self) -> Result<(), IncompleteIrFact> {
        for (function, declaration) in self.functions.iter().enumerate() {
            let Some(body) = declaration.body else {
                continue;
            };
            let function = FunId::try_from(function).expect("too many functions");
            // `this` takes slot 0 of a function with a dispatch receiver.
            let first = u32::from(declaration.dispatch_receiver.is_some());
            let parameters = u32::try_from(declaration.params.len()).expect("too many parameters");
            let frame = self.frame_expressions(body);
            let mut holders = self
                .shared_capture_parameters
                .keys()
                .filter(|(owner, _)| *owner == function)
                .map(|(_, parameter)| first + parameter)
                .collect::<std::collections::HashSet<_>>();
            for expression in &frame {
                if let IrExpr::Variable {
                    index,
                    init: Some(init),
                    ..
                } = self.expr(*expression)
                {
                    if matches!(self.expr(*init), IrExpr::RefNew { .. }) {
                        holders.insert(*index);
                    }
                }
            }
            for expression in &frame {
                match self.expr(*expression) {
                    IrExpr::Lambda {
                        impl_fn, captures, ..
                    } => {
                        for (parameter, capture) in captures.iter().enumerate() {
                            let parameter =
                                u32::try_from(parameter).expect("too many lambda captures");
                            let forwards_holder = matches!(
                                self.expr(*capture),
                                IrExpr::GetValue(slot) if holders.contains(slot)
                            );
                            if forwards_holder
                                && !self
                                    .shared_capture_parameters
                                    .contains_key(&(*impl_fn, parameter))
                            {
                                return Err(IncompleteIrFact::SharedCaptureParameter {
                                    function: *impl_fn,
                                    parameter,
                                });
                            }
                        }
                    }
                    IrExpr::Call { callee, args, .. } => {
                        let Some(parameters) = capture_parameter_ordinals(callee, args.len())
                        else {
                            continue;
                        };
                        let Some(function) = callee.source_function() else {
                            continue;
                        };
                        for (argument, parameter) in args.iter().zip(parameters) {
                            if forwards_holder(self, *argument, &holders)
                                && !self
                                    .shared_capture_parameters
                                    .contains_key(&(function, parameter))
                            {
                                return Err(IncompleteIrFact::SharedCaptureParameter {
                                    function,
                                    parameter,
                                });
                            }
                        }
                    }
                    IrExpr::CallableReference(reference) => {
                        for (parameter, capture) in reference.captures.iter().enumerate() {
                            let parameter =
                                u32::try_from(parameter).expect("too many reference captures");
                            if forwards_holder(self, *capture, &holders)
                                && !self
                                    .shared_capture_parameters
                                    .contains_key(&(reference.adapter, parameter))
                            {
                                return Err(IncompleteIrFact::SharedCaptureParameter {
                                    function: reference.adapter,
                                    parameter,
                                });
                            }
                        }
                    }
                    IrExpr::RefGet { holder, .. } | IrExpr::RefSet { holder, .. } => {
                        if let IrExpr::GetValue(slot) = self.expr(*holder) {
                            let parameter = slot.wrapping_sub(first);
                            if *slot >= first
                                && parameter < parameters
                                && !self
                                    .shared_capture_parameters
                                    .contains_key(&(function, parameter))
                            {
                                return Err(IncompleteIrFact::SharedCaptureParameter {
                                    function,
                                    parameter,
                                });
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// Every expression evaluated in the frame of the function whose body is `body`. A lambda's
    /// inline body reads the lambda's own value slots, not this function's; only its captures
    /// belong to this frame.
    fn frame_expressions(&self, body: ExprId) -> Vec<ExprId> {
        let mut frame = Vec::new();
        let mut pending = vec![body];
        let mut seen = std::collections::HashSet::<ExprId>::new();
        while let Some(expression) = pending.pop() {
            if !seen.insert(expression) {
                continue;
            }
            frame.push(expression);
            match self.expr(expression) {
                IrExpr::Lambda { captures, .. } => pending.extend(captures.iter().copied()),
                _ => super::super::for_each_child(&self.exprs, expression, &mut |child| {
                    pending.push(child)
                }),
            }
        }
        frame
    }
}

/// Parameter ordinals of the arguments a same-file local or class-static call supplies.
///
/// Captures are prepended to that argument list, so ordinal 0 is the callee's first parameter.
/// Omitted defaults are absent from the list and are not parameters the call forwards.
fn capture_parameter_ordinals(callee: &Callee, supplied: usize) -> Option<Vec<u32>> {
    let defaults: &[u32] = match callee {
        Callee::Local(_) | Callee::ClassStatic { .. } => &[],
        Callee::LocalWithDefaults { defaults, .. }
        | Callee::ClassStaticWithDefaults { defaults, .. } => defaults,
        _ => return None,
    };
    if defaults.is_empty() {
        return Some((0..u32::try_from(supplied).expect("too many call arguments")).collect());
    }
    let omitted = defaults
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    let mut ordinals = Vec::with_capacity(supplied);
    let mut parameter = 0u32;
    let limit = supplied + defaults.len();
    while ordinals.len() < supplied && (parameter as usize) < limit {
        if !omitted.contains(&parameter) {
            ordinals.push(parameter);
        }
        parameter += 1;
    }
    (ordinals.len() == supplied).then_some(ordinals)
}

fn forwards_holder(ir: &IrFile, value: ExprId, holders: &std::collections::HashSet<u32>) -> bool {
    matches!(ir.expr(value), IrExpr::GetValue(slot) if holders.contains(slot))
}

#[cfg(test)]
#[path = "completeness_tests.rs"]
mod tests;
