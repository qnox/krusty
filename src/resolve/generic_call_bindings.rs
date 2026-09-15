//! Solving a call's generic formals from everything the call site supplies at once.
//!
//! A value argument constrains a formal according to the variance of the parameter position. An
//! extension receiver contributes another assignability constraint: the actual receiver must be
//! assignable to the formal receiver's instantiation. The two therefore belong in one constraint
//! set. In the motivating `Plug<P, B>` parameter, `P` occurs invariantly and fixes that side of the
//! solve, while the receiver contributes a lower bound for `P`.
//!
//! Unifying the receiver separately cannot express that, and which side wins is decided by ordering
//! rather than by the language. Binding the receiver first pinned the formal to the receiver's own
//! class, so `App().install(pluginOfPipe) { … }` judged `Plug<Pipe, Cfg>` against `Plug<App, B>`,
//! declined the overload and left its lambda unshaped. Binding it last merely inverted the loss:
//! once a non-`Nothing` argument binding occupies the formal, the receiver evidence is discarded.
//! Solved together, the formal takes the join of what each side requires.

use super::*;

impl Checker<'_> {
    /// Solve this overload's formals from the mapped arguments AND the extension receiver together.
    ///
    /// The receiver participates in the SAME constraint set rather than being unified into the
    /// result afterwards: it is a lower bound, not an equation, and a separate pass can only
    /// overwrite argument evidence or be discarded by it.
    pub(super) fn merge_mapped_generic_argument_bindings(
        &self,
        overload: &crate::libraries::FunctionInfo,
        receiver: Option<Ty>,
        args_and_partial: (&[ExprId], &[Option<Ty>]),
        argument_map: &[usize],
        named_whole_array_varargs: &[bool],
        explicit_type_arguments: &[Ty],
        bindings: &mut crate::symbol_resolver::GSigBinds,
    ) {
        let (args, arg_tys) = args_and_partial;
        let semantic = overload.semantic_signature();
        let source = self.fed_source();
        let inferred =
            crate::symbol_resolver::infer_generic_call_bindings_with_receiver_from_symbols(
                &source,
                &semantic,
                receiver,
                argument_map
                    .iter()
                    .copied()
                    .zip(arg_tys)
                    .enumerate()
                    .filter_map(|(argument, (parameter, actual))| {
                        if self
                            .unbound_contextual_result_signature(*args.get(argument)?)
                            .is_some()
                        {
                            return None;
                        }
                        let actual = (*actual)?;
                        let whole_array = named_whole_array_varargs
                            .get(argument)
                            .copied()
                            .unwrap_or(false)
                            || self.file.is_spread_arg(args[argument]);
                        Some((parameter, actual, whole_array))
                    }),
                overload.call_sig.vararg_index,
            );
        crate::symbol_resolver::merge_generic_bindings(
            &semantic,
            explicit_type_arguments,
            bindings,
            inferred,
        );
    }

    pub(super) fn mapped_generic_call_bindings(
        &self,
        overload: &crate::libraries::FunctionInfo,
        receiver: Option<Ty>,
        args_and_partial: (&[ExprId], &[Option<Ty>]),
        argument_map: &[usize],
        named_whole_array_varargs: &[bool],
        type_args: &[Ty],
    ) -> crate::symbol_resolver::GSigBinds {
        let (args, arg_tys) = args_and_partial;
        let semantic = overload.semantic_signature();
        let mut bindings = crate::symbol_resolver::seeded_gsig_binds(&semantic, type_args);
        // The receiver is solved WITH the arguments, not unified into the result afterwards. A
        // argument constrains a formal according to its parameter position, while an extension
        // receiver requires that the actual receiver be assignable to the formal's instantiation.
        // In the motivating `Plug<P, B>` parameter, `P` is invariant and the receiver contributes a
        // lower bound. Unifying it separately pinned `P` to the
        // receiver's own class, so `App().install(pluginOfPipe) { … }` judged `Plug<Pipe, Cfg>`
        // against `Plug<App, B>` and declined the overload, leaving its lambda unshaped; and doing
        // it after the arguments merely moved which side won. Both now constrain one solve, so the
        // formal takes the join of what each requires.
        self.merge_mapped_generic_argument_bindings(
            overload,
            receiver,
            (args, arg_tys),
            argument_map,
            named_whole_array_varargs,
            type_args,
            &mut bindings,
        );
        bindings
    }
}
