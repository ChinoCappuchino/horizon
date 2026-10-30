//! `java.util.concurrent.ForkJoinPool`, its worker threads and the tasks
//! it hands back, for the native backend's runtime library, written
//! against the JDK's public API.
//!
//! A pool keeps its queue here and starts its workers on its first task,
//! through the program's worker factory when it has one (vanilla makes its
//! own `ForkJoinWorkerThread` subclass). A worker runs its `onStart`, takes
//! tasks until the pool shuts down, then runs its `onTermination`; a task's
//! uncaught exception goes to the pool's handler. Work stealing and
//! `ForkJoinTask`'s fork/join are not here yet: tasks run in queue order.
use super::{Builtin, Method, alloc, ctx, method, pending, str_of, threads, throw, vt, word};
use crate::heap::Context;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Condvar, LazyLock, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

const POOL: &str = "java/util/concurrent/ForkJoinPool";
const WORKER: &str = "java/util/concurrent/ForkJoinWorkerThread";
const TASK: &str = "horizon/rt/PoolTask";
const EXECUTOR: &str = "java/util/concurrent/ThreadPoolExecutor";
const LOOP: &str = "horizon/rt/PoolLoop";

struct Pool {
    number: u64,
    /// The most workers it runs.
    parallelism: usize,
    /// A ForkJoinPool's worker factory; a thread pool's `ThreadFactory`.
    factory: u64,
    handler: u64,
    /// A thread pool (`Executors`): workers are plain threads, started when
    /// a task finds none idle.
    plain: bool,
    idle: std::sync::atomic::AtomicUsize,
    queue: Mutex<VecDeque<u64>>,
    work: Condvar,
    shutdown: AtomicBool,
    /// Workers started, and those still running (with their condition).
    started: Mutex<usize>,
    /// The next worker's number (its constructor runs while `started` is
    /// held: the factory makes it).
    next_index: AtomicU64,
    live: Mutex<usize>,
    ended: Condvar,
}

static POOLS: LazyLock<Mutex<Vec<Arc<Pool>>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static POOL_NUMBERS: AtomicU64 = AtomicU64::new(1);

/// The pool object `p` stands for: [vtable][index in POOLS].
fn pool(p: u64) -> Arc<Pool> {
    // SAFETY: p is a ForkJoinPool.
    let index = unsafe { word(p, 8) } as usize;
    POOLS.lock().unwrap()[index].clone()
}

fn make_pool(p: u64, parallelism: i32, factory: u64, handler: u64) {
    make(p, parallelism, factory, handler, false);
}

fn make(p: u64, parallelism: i32, factory: u64, handler: u64, plain: bool) {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let state = Arc::new(Pool {
        number: POOL_NUMBERS.fetch_add(1, Ordering::Relaxed),
        parallelism: if parallelism > 0 {
            parallelism as usize
        } else {
            cores
        },
        factory,
        handler,
        plain,
        idle: std::sync::atomic::AtomicUsize::new(0),
        queue: Mutex::new(VecDeque::new()),
        work: Condvar::new(),
        shutdown: AtomicBool::new(false),
        started: Mutex::new(0),
        next_index: AtomicU64::new(0),
        live: Mutex::new(0),
        ended: Condvar::new(),
    });
    let mut pools = POOLS.lock().unwrap();
    pools.push(state);
    // SAFETY: p is a fresh ForkJoinPool.
    unsafe { super::set_word(p, 8, (pools.len() - 1) as u64) };
}

extern "C" fn pool_init(p: u64) {
    make_pool(p, 0, 0, 0);
}
extern "C" fn pool_init_parallelism(p: u64, parallelism: i32) {
    make_pool(p, parallelism, 0, 0);
}
extern "C" fn pool_init_full(p: u64, parallelism: i32, factory: u64, handler: u64, _fifo: i32) {
    make_pool(p, parallelism, factory, handler);
}

extern "C" fn common_pool() -> u64 {
    super::collections::once("ForkJoinPool.commonPool", || {
        let p = alloc(16, vt(POOL));
        let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
        make_pool(p, (cores.max(2) - 1) as i32, 0, 0);
        p
    })
}

/// A new worker for pool `p`: the program's factory's, or a plain one.
fn new_worker(p: u64, state: &Pool) -> u64 {
    if state.factory != 0
        && let Some(f) = method(
            state.factory,
            "newThread (Ljava/util/concurrent/ForkJoinPool;)Ljava/util/concurrent/ForkJoinWorkerThread;",
        )
    {
        // SAFETY: the selector's slot: (receiver, pool) -> worker.
        let f: extern "C" fn(*mut Context, u64, u64) -> u64 = unsafe { std::mem::transmute(f) };
        return f(ctx(), state.factory, p);
    }
    let w = alloc(WORKER_SIZE as usize, vt(WORKER));
    worker_init(w, p);
    w
}

/// Starts the pool's workers: a ForkJoinPool's all on its first task, a
/// thread pool's one when a task finds none idle.
fn start_workers(p: u64, state: &Pool) {
    let mut started = state.started.lock().unwrap();
    if state.plain {
        if state.idle.load(Ordering::Acquire) == 0 && *started < state.parallelism {
            let lp = alloc(16, vt(LOOP));
            // SAFETY: a fresh PoolLoop.
            unsafe { super::set_word(lp, 8, p) };
            let t = plain_thread(state, lp);
            if t == 0 || pending() {
                return;
            }
            *started += 1;
            *state.live.lock().unwrap() += 1;
            threads::thread_start(t);
        }
        return;
    }
    while *started < state.parallelism {
        let w = new_worker(p, state);
        if w == 0 || pending() {
            return;
        }
        *started += 1;
        *state.live.lock().unwrap() += 1;
        threads::thread_start(w);
    }
}

fn submit_task(p: u64, task: u64) -> bool {
    let state = pool(p);
    if state.shutdown.load(Ordering::Acquire) {
        throw(
            "java/util/concurrent/RejectedExecutionException",
            Some("the pool is shut down"),
        );
        return false;
    }
    state.queue.lock().unwrap().push_back(task);
    state.work.notify_one();
    start_workers(p, &state);
    true
}

extern "C" fn pool_execute(p: u64, task: u64) {
    if task == 0 {
        throw("java/lang/NullPointerException", None);
        return;
    }
    submit_task(p, task);
}

// A task the pool hands back (`submit`): [vtable][task][kind][done][result].
const T_TASK: usize = 8;
const T_KIND: usize = 16;
const T_DONE: usize = 24;
const T_RESULT: usize = 32;
const TASK_SIZE: usize = 40;
/// Its kind: a Runnable, or a Callable (whose `call()` gives the result).
const RUNNABLE: u64 = 0;
const CALLABLE: u64 = 1;

/// Tasks waiting on their result share one condition.
static FINISHED: LazyLock<(Mutex<()>, Condvar)> =
    LazyLock::new(|| (Mutex::new(()), Condvar::new()));

fn wrap(task: u64, kind: u64) -> u64 {
    let t = alloc(TASK_SIZE, vt(TASK));
    // SAFETY: a fresh PoolTask.
    unsafe {
        super::set_word(t, T_TASK, task);
        super::set_word(t, T_KIND, kind);
    }
    t
}

extern "C" fn pool_submit_runnable(p: u64, task: u64) -> u64 {
    let t = wrap(task, RUNNABLE);
    if submit_task(p, t) { t } else { 0 }
}
extern "C" fn pool_submit_callable(p: u64, task: u64) -> u64 {
    let t = wrap(task, CALLABLE);
    if submit_task(p, t) { t } else { 0 }
}

/// `PoolTask.run()`: the task, then its result for `get()`.
extern "C" fn task_run(t: u64) {
    // SAFETY: t is a PoolTask.
    let (task, kind) = unsafe { (word(t, T_TASK), word(t, T_KIND)) };
    let mut result = 0;
    if kind == CALLABLE {
        match method(task, "call ()Ljava/lang/Object;") {
            Some(f) => {
                // SAFETY: (receiver) -> Object.
                let f: extern "C" fn(*mut Context, u64) -> u64 = unsafe { std::mem::transmute(f) };
                result = f(ctx(), task);
            }
            None => throw(
                "java/lang/UnsupportedOperationException",
                Some("call() is not compiled for this object"),
            ),
        }
    } else {
        threads::call_run(task);
    }
    let (lock, done) = &*FINISHED;
    let _held = lock.lock().unwrap();
    // SAFETY: t is a PoolTask.
    unsafe {
        super::set_word(t, T_RESULT, result);
        (*((t + T_DONE as u64) as *const AtomicU64)).store(1, Ordering::Release);
    }
    done.notify_all();
}

fn task_done(t: u64) -> bool {
    // SAFETY: t is a PoolTask.
    unsafe { (*((t + T_DONE as u64) as *const AtomicU64)).load(Ordering::Acquire) != 0 }
}

extern "C" fn task_is_done(t: u64) -> i32 {
    task_done(t).into()
}
extern "C" fn task_get(t: u64) -> u64 {
    let (lock, done) = &*FINISHED;
    let mut held = lock.lock().unwrap();
    while !task_done(t) {
        held = done.wait(held).unwrap();
    }
    // SAFETY: t is a PoolTask.
    unsafe { word(t, T_RESULT) }
}
extern "C" fn task_cancel(_t: u64, _interrupt: i32) -> i32 {
    0
}

extern "C" fn pool_shutdown(p: u64) {
    let state = pool(p);
    state.shutdown.store(true, Ordering::Release);
    state.work.notify_all();
}
extern "C" fn pool_shutdown_now(p: u64) -> u64 {
    let state = pool(p);
    state.queue.lock().unwrap().clear();
    pool_shutdown(p);
    super::collections::new_array_list()
}
extern "C" fn pool_is_shutdown(p: u64) -> i32 {
    pool(p).shutdown.load(Ordering::Acquire).into()
}
extern "C" fn pool_is_terminated(p: u64) -> i32 {
    let state = pool(p);
    (state.shutdown.load(Ordering::Acquire) && *state.live.lock().unwrap() == 0).into()
}
extern "C" fn pool_await_termination(p: u64, timeout: i64, unit: u64) -> i32 {
    let state = pool(p);
    let nanos = super::system::to_nanos(timeout, unit);
    let deadline = Instant::now() + Duration::from_nanos(nanos.max(0) as u64);
    let mut live = state.live.lock().unwrap();
    while *live > 0 {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return 0;
        }
        live = state.ended.wait_timeout(live, left).unwrap().0;
    }
    1
}
extern "C" fn pool_parallelism(p: u64) -> i32 {
    pool(p).parallelism as i32
}

// ---- Workers -------------------------------------------------------------

// ForkJoinWorkerThread: a Thread, then [pool][index in it].
const W_POOL: usize = threads::THREAD_SIZE as usize;
const W_INDEX: usize = W_POOL + 8;
const WORKER_SIZE: i32 = threads::THREAD_SIZE + 16;

extern "C" fn worker_init(w: u64, p: u64) {
    let state = pool(p);
    let index = state.next_index.fetch_add(1, Ordering::Relaxed);
    // SAFETY: w is a ForkJoinWorkerThread.
    unsafe {
        super::set_word(w, W_POOL, p);
        super::set_word(w, W_INDEX, index as u64);
    }
    threads::named(
        w,
        str_of(&format!(
            "ForkJoinPool-{}-worker-{}",
            state.number,
            index + 1
        )),
    );
}
extern "C" fn worker_pool(w: u64) -> u64 {
    // SAFETY: w is a ForkJoinWorkerThread.
    unsafe { word(w, W_POOL) }
}
extern "C" fn worker_index(w: u64) -> i32 {
    // SAFETY: as above.
    unsafe { word(w, W_INDEX) as i32 }
}
extern "C" fn worker_hook(_w: u64) {}
extern "C" fn worker_end_hook(_w: u64, _e: u64) {}

/// A selector's method of `o` with no result: (receiver, args).
fn call_void(o: u64, nd: &str, args: &[u64]) {
    let Some(f) = method(o, nd) else { return };
    // SAFETY: the selector's slot holds a method of this shape.
    unsafe {
        match args {
            [] => std::mem::transmute::<usize, extern "C" fn(*mut Context, u64)>(f)(ctx(), o),
            [a] => {
                std::mem::transmute::<usize, extern "C" fn(*mut Context, u64, u64)>(f)(ctx(), o, *a)
            }
            [a, b] => std::mem::transmute::<usize, extern "C" fn(*mut Context, u64, u64, u64)>(f)(
                ctx(),
                o,
                *a,
                *b,
            ),
            _ => unreachable!(),
        }
    }
}

/// `ForkJoinWorkerThread.run()`: the pool's tasks until it shuts down.
extern "C" fn worker_run(w: u64) {
    let state = pool(worker_pool(w));
    call_void(w, "onStart ()V", &[]);
    serve(&state, w);
    call_void(w, "onTermination (Ljava/lang/Throwable;)V", &[0]);
    worker_ended(&state);
}

/// A thread pool's worker loop (`PoolLoop.run()`, on a thread its factory made).
extern "C" fn loop_run(lp: u64) {
    // SAFETY: lp is a PoolLoop.
    let state = pool(unsafe { word(lp, 8) });
    serve(&state, threads::current());
    worker_ended(&state);
}

fn worker_ended(state: &Pool) {
    let mut live = state.live.lock().unwrap();
    *live -= 1;
    state.ended.notify_all();
}

/// The thread a thread pool's worker runs on: its `ThreadFactory`'s, or a
/// plain one.
fn plain_thread(state: &Pool, lp: u64) -> u64 {
    if state.factory != 0
        && let Some(f) = method(
            state.factory,
            "newThread (Ljava/lang/Runnable;)Ljava/lang/Thread;",
        )
    {
        // SAFETY: (receiver, Runnable) -> Thread.
        let f: extern "C" fn(*mut Context, u64, u64) -> u64 = unsafe { std::mem::transmute(f) };
        return f(ctx(), state.factory, lp);
    }
    let t = alloc(threads::THREAD_SIZE as usize, vt("java/lang/Thread"));
    threads::thread_init_target(t, lp);
    t
}

/// Runs the pool's tasks on worker `w` until it shuts down.
fn serve(state: &Pool, w: u64) {
    loop {
        let task = {
            let mut queue = state.queue.lock().unwrap();
            loop {
                if let Some(t) = queue.pop_front() {
                    break Some(t);
                }
                if state.shutdown.load(Ordering::Acquire) {
                    break None;
                }
                state.idle.fetch_add(1, Ordering::AcqRel);
                queue = state.work.wait(queue).unwrap();
                state.idle.fetch_sub(1, Ordering::AcqRel);
            }
        };
        let Some(task) = task else { break };
        threads::call_run(task);
        if pending() {
            // SAFETY: this thread's context.
            let e = unsafe { std::mem::replace(&mut (*ctx()).exception, 0) };
            if state.handler != 0 {
                call_void(
                    state.handler,
                    "uncaughtException (Ljava/lang/Thread;Ljava/lang/Throwable;)V",
                    &[w, e],
                );
            } else {
                eprintln!(
                    "Exception in a pool worker: {}",
                    String::from_utf16_lossy(super::units(super::to_string(e)))
                );
            }
            // SAFETY: as above (a handler's own exception is dropped).
            unsafe { (*ctx()).exception = 0 };
        }
    }
}

// ---- Executors -----------------------------------------------------------------

fn new_executor(max: i32, factory: u64) -> u64 {
    let p = alloc(16, vt(EXECUTOR));
    make(p, max, factory, 0, true);
    p
}
/// Unbounded in the JDK; here at most this many threads at once.
const CACHED: i32 = 1024;
extern "C" fn executors_cached() -> u64 {
    new_executor(CACHED, 0)
}
extern "C" fn executors_cached_with(factory: u64) -> u64 {
    new_executor(CACHED, factory)
}
extern "C" fn executors_fixed(n: i32) -> u64 {
    new_executor(n.max(1), 0)
}
extern "C" fn executors_fixed_with(n: i32, factory: u64) -> u64 {
    new_executor(n.max(1), factory)
}
extern "C" fn executors_single() -> u64 {
    new_executor(1, 0)
}
extern "C" fn executors_single_with(factory: u64) -> u64 {
    new_executor(1, factory)
}

pub fn builtins() -> Vec<Builtin> {
    let m = |name, desc, f: usize| Method {
        name,
        desc,
        is_static: false,
        f,
    };
    let s = |name, desc, f: usize| Method {
        name,
        desc,
        is_static: true,
        f,
    };
    macro_rules! f {
        ($f:expr) => {
            $f as *const () as usize
        };
    }
    const O: &str = "java/lang/Object";
    let interface = |name: &'static str| Builtin {
        name,
        super_name: Some(O),
        interfaces: &[],
        size: 8,
        is_final: false,
        is_interface: true,
        methods: Vec::new(),
    };
    vec![
        interface("java/util/concurrent/Executor"),
        interface("java/util/concurrent/ExecutorService"),
        interface("java/util/concurrent/Future"),
        interface("java/util/concurrent/Callable"),
        interface("java/util/concurrent/ForkJoinPool$ForkJoinWorkerThreadFactory"),
        interface("java/lang/Thread$UncaughtExceptionHandler"),
        Builtin {
            name: POOL,
            super_name: Some(O),
            interfaces: &[
                "java/util/concurrent/ExecutorService",
                "java/util/concurrent/Executor",
            ],
            size: 16,
            is_final: false,
            is_interface: false,
            methods: vec![
                m("<init>", "()V", f!(pool_init)),
                m("<init>", "(I)V", f!(pool_init_parallelism)),
                m(
                    "<init>",
                    "(ILjava/util/concurrent/ForkJoinPool$ForkJoinWorkerThreadFactory;Ljava/lang/Thread$UncaughtExceptionHandler;Z)V",
                    f!(pool_init_full),
                ),
                Method {
                    name: "commonPool",
                    desc: "()Ljava/util/concurrent/ForkJoinPool;",
                    is_static: true,
                    f: f!(common_pool),
                },
                m("execute", "(Ljava/lang/Runnable;)V", f!(pool_execute)),
                m(
                    "submit",
                    "(Ljava/lang/Runnable;)Ljava/util/concurrent/Future;",
                    f!(pool_submit_runnable),
                ),
                m(
                    "submit",
                    "(Ljava/util/concurrent/Callable;)Ljava/util/concurrent/Future;",
                    f!(pool_submit_callable),
                ),
                // ForkJoinPool's own, with the narrower result.
                m(
                    "submit",
                    "(Ljava/lang/Runnable;)Ljava/util/concurrent/ForkJoinTask;",
                    f!(pool_submit_runnable),
                ),
                m(
                    "submit",
                    "(Ljava/util/concurrent/Callable;)Ljava/util/concurrent/ForkJoinTask;",
                    f!(pool_submit_callable),
                ),
                m("shutdown", "()V", f!(pool_shutdown)),
                m("shutdownNow", "()Ljava/util/List;", f!(pool_shutdown_now)),
                m("isShutdown", "()Z", f!(pool_is_shutdown)),
                m("isTerminated", "()Z", f!(pool_is_terminated)),
                m(
                    "awaitTermination",
                    "(JLjava/util/concurrent/TimeUnit;)Z",
                    f!(pool_await_termination),
                ),
                m("getParallelism", "()I", f!(pool_parallelism)),
            ],
        },
        Builtin {
            name: WORKER,
            super_name: Some("java/lang/Thread"),
            interfaces: &[],
            size: WORKER_SIZE,
            is_final: false,
            is_interface: false,
            methods: vec![
                m(
                    "<init>",
                    "(Ljava/util/concurrent/ForkJoinPool;)V",
                    f!(worker_init),
                ),
                m("run", "()V", f!(worker_run)),
                m(
                    "getPool",
                    "()Ljava/util/concurrent/ForkJoinPool;",
                    f!(worker_pool),
                ),
                m("getPoolIndex", "()I", f!(worker_index)),
                m("onStart", "()V", f!(worker_hook)),
                m(
                    "onTermination",
                    "(Ljava/lang/Throwable;)V",
                    f!(worker_end_hook),
                ),
            ],
        },
        interface("java/util/concurrent/ThreadFactory"),
        Builtin {
            name: EXECUTOR,
            super_name: Some(O),
            interfaces: &[
                "java/util/concurrent/ExecutorService",
                "java/util/concurrent/Executor",
            ],
            size: 16,
            is_final: false,
            is_interface: false,
            methods: vec![
                m("execute", "(Ljava/lang/Runnable;)V", f!(pool_execute)),
                m(
                    "submit",
                    "(Ljava/lang/Runnable;)Ljava/util/concurrent/Future;",
                    f!(pool_submit_runnable),
                ),
                m(
                    "submit",
                    "(Ljava/util/concurrent/Callable;)Ljava/util/concurrent/Future;",
                    f!(pool_submit_callable),
                ),
                m("shutdown", "()V", f!(pool_shutdown)),
                m("shutdownNow", "()Ljava/util/List;", f!(pool_shutdown_now)),
                m("isShutdown", "()Z", f!(pool_is_shutdown)),
                m("isTerminated", "()Z", f!(pool_is_terminated)),
                m(
                    "awaitTermination",
                    "(JLjava/util/concurrent/TimeUnit;)Z",
                    f!(pool_await_termination),
                ),
            ],
        },
        Builtin {
            name: LOOP,
            super_name: Some(O),
            interfaces: &["java/lang/Runnable"],
            size: 16,
            is_final: true,
            is_interface: false,
            methods: vec![m("run", "()V", f!(loop_run))],
        },
        Builtin {
            name: "java/util/concurrent/Executors",
            super_name: Some(O),
            interfaces: &[],
            size: 8,
            is_final: true,
            is_interface: false,
            methods: vec![
                s(
                    "newCachedThreadPool",
                    "()Ljava/util/concurrent/ExecutorService;",
                    f!(executors_cached),
                ),
                s(
                    "newCachedThreadPool",
                    "(Ljava/util/concurrent/ThreadFactory;)Ljava/util/concurrent/ExecutorService;",
                    f!(executors_cached_with),
                ),
                s(
                    "newFixedThreadPool",
                    "(I)Ljava/util/concurrent/ExecutorService;",
                    f!(executors_fixed),
                ),
                s(
                    "newFixedThreadPool",
                    "(ILjava/util/concurrent/ThreadFactory;)Ljava/util/concurrent/ExecutorService;",
                    f!(executors_fixed_with),
                ),
                s(
                    "newSingleThreadExecutor",
                    "()Ljava/util/concurrent/ExecutorService;",
                    f!(executors_single),
                ),
                s(
                    "newSingleThreadExecutor",
                    "(Ljava/util/concurrent/ThreadFactory;)Ljava/util/concurrent/ExecutorService;",
                    f!(executors_single_with),
                ),
            ],
        },
        // What the pool hands back is a ForkJoinTask (a Future).
        Builtin {
            name: "java/util/concurrent/ForkJoinTask",
            super_name: Some(O),
            interfaces: &["java/util/concurrent/Future"],
            size: TASK_SIZE as i32,
            is_final: false,
            is_interface: false,
            methods: vec![
                m("isDone", "()Z", f!(task_is_done)),
                m("get", "()Ljava/lang/Object;", f!(task_get)),
                m("join", "()Ljava/lang/Object;", f!(task_get)),
                m("cancel", "(Z)Z", f!(task_cancel)),
            ],
        },
        Builtin {
            name: TASK,
            super_name: Some("java/util/concurrent/ForkJoinTask"),
            interfaces: &["java/lang/Runnable"],
            size: TASK_SIZE as i32,
            is_final: true,
            is_interface: false,
            methods: vec![m("run", "()V", f!(task_run))],
        },
    ]
}
