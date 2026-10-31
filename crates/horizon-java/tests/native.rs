//! The native backend against Java's own results: test classes in
//! tests/java built with the local `javac`, compiled with Cranelift and
//! called directly. Skips when no `javac` is on the PATH.
#![cfg(feature = "native")]
use horizon_java::{ClassFile, heap::Context, native::Native};
use std::{path::PathBuf, process::Command, sync::OnceLock};

/// `t/NativeOps`, `t/Threads` and their nested classes, built once with `javac` (the tests
/// run side by side: one build, not one each into the same folder); None
/// without a `javac`.
fn classes() -> Option<&'static [Vec<u8>]> {
    static BUILT: OnceLock<Option<Vec<Vec<u8>>>> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/java");
            let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("horizon-java-native");
            let status = Command::new("javac")
                .args(["--release", "21", "-d"])
                .arg(&out)
                .arg(src.join("t/NativeOps.java"))
                .arg(src.join("t/Threads.java"))
                .arg(src.join("t/Streams.java"))
                .status();
            match status {
                Ok(s) if s.success() => {}
                Ok(s) => panic!("javac failed: {s}"),
                Err(_) => {
                    eprintln!("skipping: no javac");
                    return None;
                }
            }
            let mut classes = Vec::new();
            for e in std::fs::read_dir(out.join("t")).unwrap() {
                let p = e.unwrap().path();
                let name = p.file_name().unwrap().to_string_lossy().into_owned();
                if ["NativeOps", "Threads", "Streams"]
                    .iter()
                    .any(|p| name.starts_with(p))
                {
                    classes.push(std::fs::read(&p).unwrap());
                }
            }
            Some(classes)
        })
        .as_deref()
}

/// The test classes compiled natively.
fn native_ops() -> Option<Native> {
    let classes: Vec<ClassFile> = classes()?
        .iter()
        .map(|c| ClassFile::parse(c).unwrap())
        .collect();
    Some(Native::compile(&classes).unwrap())
}

#[test]
fn wide_array_post_increments_keep_both_values() {
    let Some(native) = native_ops() else { return };
    assert!(native.unsupported.is_empty(), "{:?}", native.unsupported);
    let longs = native.function("t/NativeOps.postIncLongs(I)J").unwrap();
    let doubles = native.function("t/NativeOps.postIncDoubles(I)D").unwrap();
    // SAFETY: the compiled methods have these signatures.
    let longs: extern "C" fn(*mut Context, i32) -> i64 = unsafe { std::mem::transmute(longs) };
    let doubles: extern "C" fn(*mut Context, i32) -> f64 = unsafe { std::mem::transmute(doubles) };
    let ctx = native.context();
    // before * 1000 + after, plus the element after.
    assert_eq!(longs(ctx, 1), 20 * 1000 + 21 + 21);
    assert_eq!(doubles(ctx, 2), 3.5 * 1000.0 + 4.5 + 4.5);
}

#[test]
fn a_lambda_in_an_abstract_class_calls_its_private_body() {
    let Some(native) = native_ops() else { return };
    assert!(native.unsupported.is_empty(), "{:?}", native.unsupported);
    let f = native.function("t/NativeOps.lambdaInAbstract(I)I").unwrap();
    // SAFETY: as above.
    let f: extern "C" fn(*mut Context, i32) -> i32 = unsafe { std::mem::transmute(f) };
    assert_eq!(f(native.context(), 4), (4 * 3 + 1) * 100 + (4 * 5 + 2));
}

/// `NativeOps.churn(n)`, as Java computes it.
fn churn(n: i64) -> i64 {
    let mut sum = 0;
    let mut list = Vec::new();
    for i in 0..n {
        let len = 8 + (i & 63);
        sum += len + i;
        if i & 1023 == 0 {
            list.clear();
        }
        list.push(i);
    }
    sum + list.iter().sum::<i64>()
}

#[test]
fn threads_allocate_side_by_side_each_in_its_own_buffer() {
    let Some(native) = native_ops() else { return };
    let f = native.function("t/NativeOps.churn(I)J").unwrap();
    // SAFETY: as above.
    let f: extern "C" fn(*mut Context, i32) -> i64 = unsafe { std::mem::transmute(f) };
    let n = 200_000;
    let before = native.heap().used();
    let shared = native.shared();
    std::thread::scope(|s| {
        for _ in 0..8 {
            s.spawn(move || {
                let thread = shared.attach();
                for _ in 0..3 {
                    assert_eq!(f(thread.context(), n), churn(i64::from(n)));
                }
            });
        }
    });
    // Many buffers were taken, and every thread let go of its context.
    assert!(native.heap().used() - before > 8 * horizon_java::heap::CHUNK);
    assert_eq!(native.heap().threads(), 1);
}

#[test]
fn java_threads_share_monitors_atomics_and_class_initialization() {
    let Some(native) = native_ops() else { return };
    assert!(native.unsupported.is_empty(), "{:?}", native.unsupported);
    let ctx = native.context();
    let get = |name: &str| native.function(name).unwrap_or_else(|| panic!("no {name}"));
    // SAFETY: the compiled methods have these signatures.
    let two: extern "C" fn(*mut Context, i32, i32) -> i32 =
        unsafe { std::mem::transmute(get("t/Threads.synchronizedCounter(II)I")) };
    assert_eq!(two(ctx, 8, 20_000), 160_000, "synchronized methods");
    let blocks: extern "C" fn(*mut Context, i32, i32) -> i32 =
        unsafe { std::mem::transmute(get("t/Threads.synchronizedBlocks(II)I")) };
    assert_eq!(blocks(ctx, 8, 20_000), 160_000, "synchronized blocks");
    let atomics: extern "C" fn(*mut Context, i32, i32) -> i32 =
        unsafe { std::mem::transmute(get("t/Threads.atomics(II)I")) };
    assert_eq!(atomics(ctx, 8, 20_000), 160_000, "AtomicInteger");
    let subclass: extern "C" fn(*mut Context, i32) -> i64 =
        unsafe { std::mem::transmute(get("t/Threads.subclass(I)J")) };
    assert_eq!(subclass(ctx, 1000), 500_500, "a Thread subclass's run()");
    let handoff: extern "C" fn(*mut Context, i32) -> i64 =
        unsafe { std::mem::transmute(get("t/Threads.handoff(I)J")) };
    assert_eq!(handoff(ctx, 2000), 2000 * 2001 / 2, "wait and notifyAll");
    let race: extern "C" fn(*mut Context, i32) -> i32 =
        unsafe { std::mem::transmute(get("t/Threads.initRace(I)I")) };
    assert_eq!(
        race(ctx, 8),
        8 * 42 + 1000,
        "one initialization, seen by all"
    );
    let pool: extern "C" fn(*mut Context, i32, i32) -> i64 =
        unsafe { std::mem::transmute(get("t/Threads.pool(II)J")) };
    assert_eq!(
        pool(ctx, 4, 1000),
        1000 * 1_000_000 + 7 * 10_000 + 4 * 100 + 4 * 10 + 1,
        "a ForkJoinPool with the program's own workers"
    );
    let executors: extern "C" fn(*mut Context, i32) -> i64 =
        unsafe { std::mem::transmute(get("t/Threads.executors(I)J")) };
    assert_eq!(
        executors(ctx, 200),
        400 * 1000 + 100 + 10 + 1,
        "thread pools from Executors, with the program's thread factories"
    );
}

#[test]
fn streams_and_collectors_give_javas_results() {
    let Some(native) = native_ops() else { return };
    let f = native.function("t/Streams.checks()I").unwrap();
    // SAFETY: as above.
    let f: extern "C" fn(*mut Context) -> i32 = unsafe { std::mem::transmute(f) };
    let bits = f(native.context());
    // Each bit a check (t/Streams.java): which ones failed, if any.
    assert_eq!(
        bits,
        (1 << 12) - 1,
        "failed checks: {:#b}",
        !bits & ((1 << 12) - 1)
    );
}

#[test]
fn multi_dimensional_arrays_are_nested_arrays() {
    let Some(native) = native_ops() else { return };
    let f = native.function("t/NativeOps.multi(II)I").unwrap();
    // SAFETY: as above.
    let f: extern "C" fn(*mut Context, i32, i32) -> i32 = unsafe { std::mem::transmute(f) };
    assert_eq!(
        f(native.context(), 3, 4),
        4 * 10000 + 3000 + 400 + 7 + 'z' as i32
    );
}

#[test]
fn collections_algorithms_give_javas_results() {
    let Some(native) = native_ops() else { return };
    let f = native.function("t/Streams.collectionsChecks()I").unwrap();
    // SAFETY: as above.
    let f: extern "C" fn(*mut Context) -> i32 = unsafe { std::mem::transmute(f) };
    let bits = f(native.context());
    assert_eq!(
        bits,
        (1 << 7) - 1,
        "failed checks: {:#b}",
        !bits & ((1 << 7) - 1)
    );
}

#[test]
fn strings_split_join_and_format_as_java_does() {
    let Some(native) = native_ops() else { return };
    let f = native.function("t/Streams.stringChecks()I").unwrap();
    // SAFETY: as above.
    let f: extern "C" fn(*mut Context) -> i32 = unsafe { std::mem::transmute(f) };
    let bits = f(native.context());
    assert_eq!(
        bits,
        (1 << 8) - 1,
        "failed checks: {:#b}",
        !bits & ((1 << 8) - 1)
    );
}

#[test]
fn a_lambda_calls_a_private_interface_method() {
    let Some(native) = native_ops() else { return };
    let f = native.function("t/NativeOps.interfaceLambda()I").unwrap();
    // SAFETY: as above.
    let f: extern "C" fn(*mut Context) -> i32 = unsafe { std::mem::transmute(f) };
    assert_eq!(f(native.context()), "hi bo".len() as i32);
}

#[test]
fn functional_interfaces_compose_as_java_does() {
    let Some(native) = native_ops() else { return };
    let f = native.function("t/Streams.functionChecks()I").unwrap();
    // SAFETY: as above.
    let f: extern "C" fn(*mut Context) -> i32 = unsafe { std::mem::transmute(f) };
    let bits = f(native.context());
    assert_eq!(
        bits,
        (1 << 7) - 1,
        "failed checks: {:#b}",
        !bits & ((1 << 7) - 1)
    );
}

#[test]
fn lambda_sites_at_the_same_offset_of_two_methods_stay_apart() {
    let Some(native) = native_ops() else { return };
    let f = native.function("t/NativeOps.sameOffsetLambdas()I").unwrap();
    // SAFETY: as above.
    let f: extern "C" fn(*mut Context) -> i32 = unsafe { std::mem::transmute(f) };
    assert_eq!(f(native.context()), 5 * 10 + "s4".len() as i32);
}

#[test]
fn floats_and_doubles_give_javas_results() {
    let Some(native) = native_ops() else { return };
    let f = native.function("t/Streams.floatChecks()I").unwrap();
    // SAFETY: as above.
    let f: extern "C" fn(*mut Context) -> i32 = unsafe { std::mem::transmute(f) };
    let bits = f(native.context());
    assert_eq!(
        bits,
        (1 << 7) - 1,
        "failed checks: {:#b}",
        !bits & ((1 << 7) - 1)
    );
}

#[test]
fn runtime_basics_give_javas_results() {
    let Some(native) = native_ops() else { return };
    let f = native.function("t/Streams.runtimeChecks()I").unwrap();
    // SAFETY: as above.
    let f: extern "C" fn(*mut Context) -> i32 = unsafe { std::mem::transmute(f) };
    let bits = f(native.context());
    assert_eq!(
        bits,
        (1 << 10) - 1,
        "failed checks: {:#b}",
        !bits & ((1 << 10) - 1)
    );
}

#[test]
fn class_reflection_basics_give_javas_results() {
    let Some(native) = native_ops() else { return };
    let f = native.function("t/Streams.classChecks()I").unwrap();
    // SAFETY: as above.
    let f: extern "C" fn(*mut Context) -> i32 = unsafe { std::mem::transmute(f) };
    let bits = f(native.context());
    assert_eq!(
        bits,
        (1 << 5) - 1,
        "failed checks: {:#b}",
        !bits & ((1 << 5) - 1)
    );
}

#[test]
fn bitsets_give_javas_results() {
    let Some(native) = native_ops() else { return };
    let f = native.function("t/Streams.bitsetChecks()I").unwrap();
    // SAFETY: as above.
    let f: extern "C" fn(*mut Context) -> i32 = unsafe { std::mem::transmute(f) };
    let bits = f(native.context());
    assert_eq!(
        bits,
        (1 << 7) - 1,
        "failed checks: {:#b}",
        !bits & ((1 << 7) - 1)
    );
}

#[test]
fn a_programs_own_list_gets_the_jdks_skeleton_methods() {
    let Some(native) = native_ops() else { return };
    let f = native.function("t/Streams.abstractChecks()I").unwrap();
    // SAFETY: as above.
    let f: extern "C" fn(*mut Context) -> i32 = unsafe { std::mem::transmute(f) };
    let bits = f(native.context());
    assert_eq!(
        bits,
        (1 << 4) - 1,
        "failed checks: {:#b}",
        !bits & ((1 << 4) - 1)
    );
}

#[test]
fn type_tests_and_catches_work_on_every_thread() {
    let Some(native) = native_ops() else { return };
    let f = native.function("t/Threads.typesOnThreads()I").unwrap();
    // SAFETY: as above.
    let f: extern "C" fn(*mut Context) -> i32 = unsafe { std::mem::transmute(f) };
    assert_eq!(f(native.context()), 0b1111);
}

#[test]
fn lambdas_are_instances_of_their_interfaces() {
    let Some(native) = native_ops() else { return };
    let f = native.function("t/NativeOps.lambdaInstanceOf()I").unwrap();
    // SAFETY: as above.
    let f: extern "C" fn(*mut Context) -> i32 = unsafe { std::mem::transmute(f) };
    assert_eq!(f(native.context()), 0b1111);
}
