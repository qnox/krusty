//! How a top-level library function with default parameters realizes an omitted argument.
//!
//! A Kotlin declaration either has a static `name$default` bridge that evaluates the defaults, or,
//! when a compiler plugin transformed it (a Compose composable), is itself the entry that takes the
//! mask of omitted arguments after its source parameters. The second form is recognised only from
//! the declaration's own descriptor; a bridge that is absent, malformed, or declared on another
//! owner is not evidence of it.

use super::*;

impl JvmLibraries {
    /// Publish how `callable`, which declares parameter defaults, realizes an omitted argument:
    /// through its `$default` bridge, or as the declaration that takes the mask itself.
    pub(super) fn realize_top_level_defaults(&self, callable: &mut LibraryCallable) {
        callable.default_realization = self.top_level_default_realization(callable).map(Box::new);
        callable.defaults_from_declaration = callable.default_realization.is_none()
            && !callable.suspend
            && parse_method_desc(&callable.descriptor).is_some_and(|(descriptor, _)| {
                declaration_takes_default_mask(&descriptor, &callable.physical_params)
            });
    }

    pub(super) fn top_level_default_realization(
        &self,
        callable: &LibraryCallable,
    ) -> Option<crate::libraries::DefaultCallRealization> {
        let bridge_name = format!("{}$default", callable.physical_name());
        let (base_params, _) = parse_method_desc(&callable.descriptor)?;
        let is_continuation = |ty: Ty| {
            ty.obj_internal()
                .is_some_and(|name| name.matches("kotlin/coroutines/Continuation"))
        };
        let mut owner = Some(callable.owner);
        let mut seen = crate::fx_hash::HashSet::default();
        while let Some(current) = owner {
            if !seen.insert(current) {
                break;
            }
            let class = self.cp.find_name(current)?;
            crate::trace_compiler!(
                "default_semantics",
                "default realization owner={} scan={} bridge={} base={base_params:?} matching={:?}",
                callable.owner,
                current,
                bridge_name,
                class
                    .methods
                    .iter()
                    .filter(|method| method.name == bridge_name)
                    .map(|method| method.descriptor.as_str())
                    .collect::<Vec<_>>(),
            );
            if let Some(realization) = class.methods.iter().find_map(|method| {
                if !method.is_static() || method.name != bridge_name {
                    return None;
                }
                let (params, ret) = parse_method_desc(&method.descriptor)?;
                if !params.starts_with(&base_params) {
                    return None;
                }
                let mut suffix = &params[base_params.len()..];
                if callable.suspend {
                    let (continuation, rest) = suffix.split_first()?;
                    if !is_continuation(*continuation) {
                        return None;
                    }
                    suffix = rest;
                }
                let mask_count = base_params.len().div_ceil(32).max(1);
                if suffix.len() != mask_count + 1
                    || !suffix[..suffix.len() - 1]
                        .iter()
                        .all(|parameter| *parameter == Ty::Int)
                    || !suffix.last().copied().is_some_and(Ty::is_reference)
                {
                    return None;
                }
                Some(crate::libraries::DefaultCallRealization {
                    owner: callable.owner,
                    name: bridge_name.clone(),
                    descriptor: method.descriptor.clone(),
                    declaration_owner: current,
                    real_params: callable.physical_params.clone(),
                    mask_count,
                    ret,
                    suspend: callable.suspend,
                })
            }) {
                return Some(realization);
            }
            owner = class.super_class;
        }
        None
    }
}

/// Whether a declaration's physical `descriptor` is its `source` parameters followed by synthetic
/// ones that end with the words of the mask of omitted arguments: one `Int` per 32 source
/// parameters, after at least one other synthetic parameter (a compiled composable's composer and
/// `$changed` words).
fn declaration_takes_default_mask(descriptor: &[Ty], source: &[Ty]) -> bool {
    let mask_words = source.len().div_ceil(32).max(1);
    let Some(synthetic) = descriptor.strip_prefix(source) else {
        return false;
    };
    synthetic.len() > mask_words
        && synthetic[synthetic.len() - mask_words..]
            .iter()
            .all(|parameter| *parameter == Ty::Int)
}

#[cfg(test)]
mod tests {
    use super::declaration_takes_default_mask;
    use crate::types::Ty;

    fn composer() -> Ty {
        Ty::obj("androidx/compose/runtime/Composer")
    }

    #[test]
    fn a_declaration_with_a_composer_changed_and_mask_takes_the_mask() {
        let source = [Ty::String, Ty::Int];
        let descriptor = [Ty::String, Ty::Int, composer(), Ty::Int, Ty::Int];
        assert!(declaration_takes_default_mask(&descriptor, &source));
    }

    #[test]
    fn an_ordinary_declaration_without_synthetic_parameters_does_not() {
        let source = [Ty::String, Ty::Int];
        assert!(!declaration_takes_default_mask(&source, &source));
    }

    #[test]
    fn synthetic_parameters_that_are_only_a_mask_do_not() {
        let source = [Ty::String, Ty::Int];
        let descriptor = [Ty::String, Ty::Int, Ty::Int];
        assert!(!declaration_takes_default_mask(&descriptor, &source));
    }

    #[test]
    fn a_synthetic_tail_not_ending_with_the_mask_words_does_not() {
        let source = [Ty::String];
        let descriptor = [Ty::String, Ty::Int, composer()];
        assert!(!declaration_takes_default_mask(&descriptor, &source));
    }

    #[test]
    fn a_descriptor_that_does_not_start_with_the_source_parameters_does_not() {
        let source = [Ty::String, Ty::Int];
        let descriptor = [Ty::Long, Ty::Int, composer(), Ty::Int, Ty::Int];
        assert!(!declaration_takes_default_mask(&descriptor, &source));
    }

    #[test]
    fn thirty_three_source_parameters_need_two_mask_words() {
        let source = vec![Ty::Int; 33];
        let mut descriptor = source.clone();
        descriptor.extend([composer(), Ty::Int, Ty::Int, Ty::Int]);
        assert!(declaration_takes_default_mask(&descriptor, &source));
        let mut one_word = source.clone();
        one_word.extend([Ty::Int, composer(), Ty::Int]);
        assert!(!declaration_takes_default_mask(&one_word, &source));
    }
}
