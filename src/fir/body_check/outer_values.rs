//! The lexical values a nested body can read from its enclosing bodies.
//!
//! Every visible binding of a name is kept, nearest first, with how many bodies out it lives. A
//! nearer binding shadows a farther one for an ordinary read, but a selection the checker made by
//! shadow depth (a context value, or a capture of a shadowed binding) still reaches the farther
//! one at its recorded coordinate.

use crate::fx_hash::HashMap;

use super::super::LocalBinding;

#[derive(Default)]
pub(super) struct OuterValues {
    by_name: HashMap<String, Vec<(u32, LocalBinding)>>,
}

impl OuterValues {
    /// The nearest binding of `name`, with its enclosing depth.
    pub(super) fn get(&self, name: &str) -> Option<&(u32, LocalBinding)> {
        self.by_name.get(name)?.first()
    }

    /// Every binding of `name`, nearest first.
    pub(super) fn shadowed<'s>(
        &'s self,
        name: &str,
    ) -> impl Iterator<Item = &'s (u32, LocalBinding)> + 's {
        self.by_name.get(name).into_iter().flatten()
    }

    /// Every binding of every name.
    pub(super) fn values(&self) -> impl Iterator<Item = &(u32, LocalBinding)> {
        self.by_name.values().flatten()
    }

    pub(super) fn names(&self) -> impl Iterator<Item = &String> {
        self.by_name.keys()
    }

    /// The values a body nested in this one sees: these, one body farther out, behind the
    /// enclosing body's own `scopes` (outermost first).
    pub(super) fn nested(&self, scopes: &[HashMap<String, LocalBinding>]) -> Self {
        let mut by_name = self
            .by_name
            .iter()
            .map(|(name, bindings)| {
                let bindings = bindings
                    .iter()
                    .map(|(depth, binding)| {
                        (
                            depth.checked_add(1).expect("too many nested bodies"),
                            *binding,
                        )
                    })
                    .collect::<Vec<_>>();
                (name.clone(), bindings)
            })
            .collect::<HashMap<_, _>>();
        for scope in scopes {
            for (name, binding) in scope {
                by_name
                    .entry(name.clone())
                    .or_default()
                    .insert(0, (0, *binding));
            }
        }
        Self { by_name }
    }

    /// Make `binding` the nearest binding of `name`, in place of the one that was.
    pub(super) fn replace_nearest(&mut self, name: String, binding: (u32, LocalBinding)) {
        let bindings = self.by_name.entry(name).or_default();
        match bindings.first_mut() {
            Some(nearest) => *nearest = binding,
            None => bindings.push(binding),
        }
    }
}
