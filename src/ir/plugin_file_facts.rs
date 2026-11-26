//! What a plugin recorded about a file at the frontend/backend handoff, for its own later phases.
//!
//! The IR does not name any plugin: each plugin stores one value of a type it owns, and reads it
//! back by that type. The type is the key, so two plugins cannot collide and a plugin cannot read
//! another's facts without naming its type. These are facts about the file, never about one
//! expression, so a copy of an expression inherits nothing from here (see `expression_copy`).

use crate::fx_hash::HashMap;
use std::any::{Any, TypeId};

#[derive(Default)]
pub struct PluginFileFacts {
    by_type: HashMap<TypeId, Box<dyn Any>>,
}

impl PluginFileFacts {
    /// Record `facts`, replacing any earlier value of the same type.
    pub fn record<T: Any>(&mut self, facts: T) {
        self.by_type.insert(TypeId::of::<T>(), Box::new(facts));
    }

    pub fn contains<T: Any>(&self) -> bool {
        self.by_type.contains_key(&TypeId::of::<T>())
    }

    pub fn get<T: Any>(&self) -> Option<&T> {
        self.by_type.get(&TypeId::of::<T>())?.downcast_ref()
    }

    /// Remove and return the recorded `T`, for a phase that consumes its facts.
    pub fn take<T: Any>(&mut self) -> Option<T> {
        self.by_type
            .remove(&TypeId::of::<T>())?
            .downcast::<T>()
            .ok()
            .map(|facts| *facts)
    }
}

#[cfg(test)]
mod tests {
    use super::PluginFileFacts;

    #[derive(Debug, PartialEq)]
    struct First(u32);
    #[derive(Debug, PartialEq)]
    struct Second(&'static str);

    #[test]
    fn each_type_is_its_own_slot() {
        let mut facts = PluginFileFacts::default();
        assert!(!facts.contains::<First>());
        facts.record(First(1));
        facts.record(Second("two"));
        assert_eq!(facts.get::<First>(), Some(&First(1)));
        assert_eq!(facts.get::<Second>(), Some(&Second("two")));
    }

    #[test]
    fn recording_again_replaces_the_value() {
        let mut facts = PluginFileFacts::default();
        facts.record(First(1));
        facts.record(First(2));
        assert_eq!(facts.take::<First>(), Some(First(2)));
    }

    #[test]
    fn taking_removes_only_that_type() {
        let mut facts = PluginFileFacts::default();
        facts.record(First(1));
        facts.record(Second("two"));
        assert_eq!(facts.take::<First>(), Some(First(1)));
        assert_eq!(facts.take::<First>(), None);
        assert!(!facts.contains::<First>());
        assert!(facts.contains::<Second>());
    }
}
