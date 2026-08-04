use super::common;

#[test]
fn concurrent_compile_lib_calls_with_one_tag_get_separate_scratch_dirs() {
    let outs: Vec<Option<(usize, std::path::PathBuf)>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..3usize)
            .map(|i| {
                scope.spawn(move || {
                    let src = format!("package lib\nclass Shared{i} {{ fun v(): Int = {i} }}\n");
                    common::compile_lib("parallel_fixture_shared_tag", &src).map(|out| (i, out))
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("compile_lib thread panicked"))
            .collect()
    });

    let Some(built) = outs.into_iter().collect::<Option<Vec<_>>>() else {
        return;
    };
    for (i, out) in &built {
        assert!(
            out.join(format!("lib/Shared{i}.class")).is_file(),
            "thread {i}: its own class is missing from {}; another call clobbered the scratch dir",
            out.display()
        );
    }
    let distinct = built
        .iter()
        .map(|(_, out)| out.clone())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(distinct.len(), built.len(), "scratch dirs were shared");
}
