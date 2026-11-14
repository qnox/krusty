use super::*;

#[test]
fn emitted_initializer_is_not_an_allocation_or_function_capability() {
    let target = ExternalCallableId::from_raw(7);
    let initializer = EmittedDependencyCallable::constructor_initializer(target);
    assert_eq!(initializer.constructor_initializer_target(), Some(target));
    assert!(!initializer.is_constructor());
    assert!(!initializer.realizes_getter());
    assert_eq!(initializer.target(), target);
    assert_eq!(
        EmittedDependencyCallable::allocating_constructor(target).constructor_initializer_target(),
        Some(target)
    );
    assert_eq!(
        EmittedDependencyCallable::constructor(target).constructor_initializer_target(),
        None
    );
    assert_eq!(
        EmittedDependencyCallable::new(target, None, None).constructor_initializer_target(),
        None
    );
}
