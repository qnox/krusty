//! `next()` and `previous()` of the runtime list iterator past the list's ends.
//!
//! Kotlin's `ListIterator` contract, and the stdlib's `AbstractList` iterator every target
//! realizes it with, throw `NoSuchElementException` from `next()` when `hasNext()` is false and
//! from `previous()` when `hasPrevious()` is false. A Kotlin `catch` observes it, so the runtime
//! constructs the checked exception class rather than trapping.

use super::encode::{BlockType, Code, Function, Module};
use super::objects::REFERENCE;
use super::runtime::{Runtime, EQZ32};
use krusty::backend::BackendClassifierSource;
use krusty::libraries::RuntimeExceptionKind;

/// A step of the runtime list iterator that has an end of the list to check.
#[derive(Clone, Copy)]
pub(super) enum EndedStep {
    Next,
    Previous,
}

impl Runtime {
    /// `(iterator) -> element`: the runtime list iterator's `step`, `next()` or `previous()`, which
    /// throws `NoSuchElementException` when there is no such element.
    pub(super) fn checked_list_iterator_step(
        &mut self,
        module: &mut Module,
        classifiers: &dyn BackendClassifierSource,
        step: EndedStep,
    ) -> Result<u32, String> {
        let (slot, has, take) = match step {
            EndedStep::Next => (0, self.array_lists.has_next, self.array_lists.next),
            EndedStep::Previous => (1, self.array_lists.has_previous, self.array_lists.previous),
        };
        if let Some(function) = self.array_lists.checked_steps[slot] {
            return Ok(function);
        }
        let exception = self.exception_class(
            module,
            classifiers,
            RuntimeExceptionKind::NoSuchElement.classifier(),
        )?;
        let mut code = Code::default();
        // The walk's shape comes first, as the JVM's iterator checks it before its ends: a walk
        // of a list that changed raises `ConcurrentModificationException` even when it is at an end.
        code.local_get(0).call(self.array_lists.ranges.check_walk);
        code.local_get(0).call(has).op(EQZ32).if_(BlockType::Empty);
        self.exceptions
            .construct_class(&mut code, exception, None, self.string)?;
        code.throw(self.exceptions.tag).end();
        code.local_get(0).call(take);
        let ty = module.func_type(vec![REFERENCE], vec![REFERENCE]);
        let function = module.declare();
        module.define(
            function,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
        self.array_lists.checked_steps[slot] = Some(function);
        Ok(function)
    }
}
