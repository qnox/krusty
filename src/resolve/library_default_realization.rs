//! How a call that omits a library declaration's default arguments gets them.
//!
//! A provider either states each omitted default as a constant, or reports that the declaration
//! fills its own: its compiled entry takes every operand and a mask of the omitted ones. The
//! second form is only usable when the call can pass that mask, so a declaration whose entry ends
//! in synthetic parameters a compiler plugin's transformation appended is authorized by the plugin
//! that owns that transformation, from the selected declaration's own annotations and tail.

use super::*;

impl Checker<'_> {
    /// The literal each omitted defaulted parameter takes, or `None` when the provider cannot say.
    ///
    /// Every omitted slot must be answered: a call that can fill three of four omitted parameters
    /// is a call that cannot be made, and half an answer here would pass the callee a value it
    /// never declared. The value must also FIT the parameter it fills — a provider that decoded a
    /// constant of the wrong shape is a provider that is wrong about the declaration, and passing
    /// it would be a silently miscompiled argument rather than a diagnosis.
    pub(super) fn library_default_literals(
        &self,
        call: ExprId,
        selected: &crate::libraries::FunctionInfo,
    ) -> Option<Vec<(usize, crate::libraries::DefaultValue)>> {
        let slots = self.resolved_call_arg_slots.get(&call)?;
        // Provider defaults are parallel to VALUE parameters. Context parameters are supplied
        // independently and must not shift this declaration-owned table.
        let parameters = selected.value_params();
        if selected.context_count > selected.call_sig.param_names.len() {
            return None;
        }
        let context_count = selected.context_count;
        let value_call_sig = selected.call_sig.suffix(context_count);
        let mut literals = Vec::new();
        for (parameter, argument) in slots.iter().enumerate() {
            if argument.is_some() || value_call_sig.vararg_index == Some(parameter) {
                continue;
            }
            let value = selected.default_values.get(parameter)?.clone()?;
            if !self
                .resolver()
                .default_literal_fits(&value, *parameters.get(parameter)?)
            {
                return None;
            }
            literals.push((parameter, value));
        }
        (!literals.is_empty()).then_some(literals)
    }

    /// Whether `callable` fills its own omitted arguments and this call can pass the mask that
    /// asks it to: a declaration realized by a transformed entry needs the enabled plugin that
    /// owns the entry's role and complete synthetic tail; any other declaration needs nothing
    /// more.
    pub(super) fn library_entry_fills_its_own_defaults(
        &self,
        callable: &crate::libraries::LibraryCallable,
    ) -> bool {
        callable.defaults_from_declaration
            && callable
                .transformed_entry_tail
                .as_deref()
                .is_none_or(|tail| {
                    self.native_plugins.passes_transformed_entry_defaults(
                        &crate::plugins::TransformedEntry {
                            annotations: &callable.annotations,
                            synthetic: tail,
                            parameters: callable.params.len(),
                        },
                    )
                })
    }
}
