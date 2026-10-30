//! Threads, monitors and atomics for the native backend's runtime library
//! (docs/java-compat/compiler.md 4.3), written against the JDK's public API:
//! `java.lang.Thread` on an OS thread of its own, `synchronized` and
//! `Object.wait`/`notify` as reentrant monitors, the `java.util.concurrent
//! .atomic` boxes as hardware atomics, and the JVM's rule that one thread
//! runs a class's static initializer while the others wait for it.
//!
//! Monitors are kept in a table by object address for now (the object
//! header stays one word); the thread objects the runtime knows are roots
//! the collector will need.
use super::{Builtin, Method, alloc, ctx, method, pending, rt, throw, vt, word};
use crate::heap::{Context, Heap};
use std::{
    collections::HashMap,
    sync::{
        Arc, Condvar, LazyLock, Mutex,
        atomic::{AtomicI32, AtomicI64, AtomicU64, Ordering},
    },
    time::Duration,
};

/// This thread's number for monitors: its context's, plus one (0: free).
fn me() -> u64 {
    // SAFETY: this thread's context, installed before its code runs.
    unsafe { (*ctx()).thread + 1 }
}

// ---- Monitors -----------------------------------------------------------

#[derive(Default)]
struct Monitor {
    /// The owning thread (0: none) and how many times it entered.
    state: Mutex<(u64, u32)>,
    /// Threads waiting to enter.
    entered: Condvar,
    /// Threads in `wait()`.
    waiting: Condvar,
}

static MONITORS: LazyLock<Mutex<HashMap<u64, Arc<Monitor>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn monitor(o: u64) -> Arc<Monitor> {
    MONITORS.lock().unwrap().entry(o).or_default().clone()
}

/// `monitorenter` (compiled code tested `o` for null).
pub extern "C" fn monitor_enter(o: u64) {
    let m = monitor(o);
    let me = me();
    let mut s = m.state.lock().unwrap();
    while s.0 != 0 && s.0 != me {
        s = m.entered.wait(s).unwrap();
    }
    s.0 = me;
    s.1 += 1;
}

/// `monitorexit`.
pub extern "C" fn monitor_exit(o: u64) {
    let m = monitor(o);
    let mut s = m.state.lock().unwrap();
    if s.0 != me() {
        drop(s);
        throw("java/lang/IllegalMonitorStateException", None);
        return;
    }
    s.1 -= 1;
    if s.1 == 0 {
        s.0 = 0;
        m.entered.notify_one();
    }
}

/// `Object.wait(millis)` (0: until notified).
fn wait_for(o: u64, millis: i64) {
    let m = monitor(o);
    let me = me();
    let mut s = m.state.lock().unwrap();
    if s.0 != me {
        drop(s);
        throw("java/lang/IllegalMonitorStateException", None);
        return;
    }
    // Let go of it entirely, wait, then take it back as many times.
    let count = s.1;
    *s = (0, 0);
    m.entered.notify_one();
    s = if millis > 0 {
        m.waiting
            .wait_timeout(s, Duration::from_millis(millis as u64))
            .unwrap()
            .0
    } else {
        m.waiting.wait(s).unwrap()
    };
    while s.0 != 0 {
        s = m.entered.wait(s).unwrap();
    }
    *s = (me, count);
}

extern "C" fn object_wait(o: u64) {
    wait_for(o, 0);
}
extern "C" fn object_wait_millis(o: u64, millis: i64) {
    wait_for(o, millis);
}

fn notify(o: u64, all: bool) {
    let m = monitor(o);
    let s = m.state.lock().unwrap();
    if s.0 != me() {
        drop(s);
        throw("java/lang/IllegalMonitorStateException", None);
        return;
    }
    if all {
        m.waiting.notify_all();
    } else {
        m.waiting.notify_one();
    }
}
extern "C" fn object_notify(o: u64) {
    notify(o, false);
}
extern "C" fn object_notify_all(o: u64) {
    notify(o, true);
}

/// `Object`'s monitor methods, for rt.rs's `Object`.
pub fn object_methods() -> Vec<Method> {
    let m = |name, desc, f: usize| Method {
        name,
        desc,
        is_static: false,
        f,
    };
    vec![
        m("wait", "()V", object_wait as *const () as usize),
        m("wait", "(J)V", object_wait_millis as *const () as usize),
        m("notify", "()V", object_notify as *const () as usize),
        m("notifyAll", "()V", object_notify_all as *const () as usize),
    ]
}

// ---- Class initialization -----------------------------------------------

/// The classes being initialized (by their flag's address), and by whom.
static INITIALIZING: LazyLock<(Mutex<HashMap<u64, u64>>, Condvar)> =
    LazyLock::new(|| (Mutex::new(HashMap::new()), Condvar::new()));

/// Before a class's static initializer: 1 when this thread should run it;
/// 0 when it ran, or this thread is running it (a recursive request). Waits
/// while another thread runs it.
pub extern "C" fn class_init_begin(flag: u64) -> i32 {
    let (lock, done) = &*INITIALIZING;
    let me = me();
    let mut running = lock.lock().unwrap();
    loop {
        // SAFETY: `flag` is the class's init flag in the statics.
        if unsafe { (*(flag as *const AtomicU64)).load(Ordering::Acquire) } != 0 {
            return 0;
        }
        match running.get(&flag) {
            None => {
                running.insert(flag, me);
                return 1;
            }
            Some(&owner) if owner == me => return 0,
            Some(_) => running = done.wait(running).unwrap(),
        }
    }
}

/// After it ran: the class is initialized for every thread.
pub extern "C" fn class_init_end(flag: u64) {
    let (lock, done) = &*INITIALIZING;
    let mut running = lock.lock().unwrap();
    // SAFETY: as above.
    unsafe { (*(flag as *const AtomicU64)).store(1, Ordering::Release) };
    running.remove(&flag);
    done.notify_all();
}

// ---- Threads --------------------------------------------------------------

// Thread: [vtable][target Runnable][name String][daemon][number].
const T_TARGET: usize = 8;
const T_NAME: usize = 16;
const T_DAEMON: usize = 24;
const T_NUMBER: usize = 32;
pub(super) const THREAD_SIZE: i32 = 40;

/// What the runtime knows of a started thread.
#[derive(Default)]
struct Started {
    finished: Mutex<bool>,
    done: Condvar,
}

static STARTED: LazyLock<Mutex<HashMap<u64, Arc<Started>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static NUMBERS: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// This OS thread's `Thread` object (made when first asked for).
    static CURRENT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn set_word(o: u64, at: usize, v: u64) {
    // SAFETY: `o` is a Thread with a word at `at`.
    unsafe { super::set_word(o, at, v) };
}

pub(super) fn named(t: u64, name: u64) {
    let n = NUMBERS.fetch_add(1, Ordering::Relaxed);
    set_word(t, T_NUMBER, n);
    let name = if name != 0 {
        name
    } else {
        super::str_of(&format!("Thread-{n}"))
    };
    set_word(t, T_NAME, name);
}

extern "C" fn thread_init(t: u64) {
    named(t, 0);
}
pub(super) extern "C" fn thread_init_target(t: u64, target: u64) {
    set_word(t, T_TARGET, target);
    named(t, 0);
}
extern "C" fn thread_init_target_name(t: u64, target: u64, name: u64) {
    set_word(t, T_TARGET, target);
    named(t, name);
}
extern "C" fn thread_init_name(t: u64, name: u64) {
    named(t, name);
}

/// `Thread.run()`: its target's `run()`, if it has one.
extern "C" fn thread_run(t: u64) {
    // SAFETY: t is a Thread.
    let target = unsafe { word(t, T_TARGET) };
    if target != 0 {
        call_run(target);
    }
}

/// `o.run()` through its class.
pub(super) fn call_run(o: u64) {
    match method(o, "run ()V") {
        Some(f) => {
            // SAFETY: the selector's slot holds a (receiver) -> void method.
            let f: extern "C" fn(*mut Context, u64) = unsafe { std::mem::transmute(f) };
            f(ctx(), o);
        }
        None => throw(
            "java/lang/UnsupportedOperationException",
            Some("run() is not compiled for this object"),
        ),
    }
}

/// `Thread.start()`: `run()` on a new OS thread with its own context.
pub(super) extern "C" fn thread_start(t: u64) {
    let state = Arc::new(Started::default());
    {
        let mut started = STARTED.lock().unwrap();
        if started.contains_key(&t) {
            drop(started);
            throw("java/lang/IllegalThreadStateException", None);
            return;
        }
        started.insert(t, state.clone());
    }
    // SAFETY: this thread's context; its heap and the runtime outlive the
    // program's threads (as the JVM outlives its own).
    let heap = unsafe { (*ctx()).heap } as usize;
    let runtime = rt() as *const super::Rt as usize;
    let name = String::from_utf16_lossy(super::units(unsafe { word(t, T_NAME) }));
    let spawned = std::thread::Builder::new().name(name).spawn(move || {
        // SAFETY: as above.
        let heap = unsafe { &*(heap as *const Heap) };
        let thread = heap.context();
        super::install(unsafe { &*(runtime as *const super::Rt) });
        super::install_context(&thread);
        CURRENT.with(|c| c.set(t));
        call_run(t);
        if pending() {
            let e = unsafe { (*ctx()).exception };
            eprintln!(
                "Exception in thread: {}",
                String::from_utf16_lossy(super::units(super::to_string(e)))
            );
            // SAFETY: this thread's context.
            unsafe { (*ctx()).exception = 0 };
        }
        heap.release(&thread);
        *state.finished.lock().unwrap() = true;
        state.done.notify_all();
    });
    if let Err(e) = spawned {
        throw("java/lang/OutOfMemoryError", Some(&e.to_string()));
    }
}

fn join_for(t: u64, millis: i64) {
    let Some(state) = STARTED.lock().unwrap().get(&t).cloned() else {
        return;
    };
    let mut finished = state.finished.lock().unwrap();
    while !*finished {
        if millis > 0 {
            let (f, timeout) = state
                .done
                .wait_timeout(finished, Duration::from_millis(millis as u64))
                .unwrap();
            finished = f;
            if timeout.timed_out() {
                return;
            }
        } else {
            finished = state.done.wait(finished).unwrap();
        }
    }
}
extern "C" fn thread_join(t: u64) {
    join_for(t, 0);
}
extern "C" fn thread_join_millis(t: u64, millis: i64) {
    join_for(t, millis);
}

extern "C" fn thread_is_alive(t: u64) -> i32 {
    STARTED
        .lock()
        .unwrap()
        .get(&t)
        .is_some_and(|s| !*s.finished.lock().unwrap())
        .into()
}

/// This thread's `Thread` object.
pub(super) fn current() -> u64 {
    thread_current()
}

extern "C" fn thread_current() -> u64 {
    let t = CURRENT.with(std::cell::Cell::get);
    if t != 0 {
        return t;
    }
    // A thread the program didn't start (the one that runs its main): one
    // named for what it is.
    let t = alloc(THREAD_SIZE as usize, vt("java/lang/Thread"));
    named(t, super::str_of(if me() == 1 { "main" } else { "native" }));
    CURRENT.with(|c| c.set(t));
    t
}

extern "C" fn thread_sleep(millis: i64) {
    std::thread::sleep(Duration::from_millis(millis.max(0) as u64));
}
extern "C" fn thread_yield() {
    std::thread::yield_now();
}
extern "C" fn thread_on_spin_wait() {
    std::hint::spin_loop();
}
extern "C" fn thread_get_name(t: u64) -> u64 {
    // SAFETY: t is a Thread.
    unsafe { word(t, T_NAME) }
}
extern "C" fn thread_set_name(t: u64, name: u64) {
    set_word(t, T_NAME, name);
}
extern "C" fn thread_set_daemon(t: u64, on: i32) {
    set_word(t, T_DAEMON, on as u64);
}
extern "C" fn thread_is_daemon(t: u64) -> i32 {
    // SAFETY: t is a Thread.
    (unsafe { word(t, T_DAEMON) } != 0).into()
}
extern "C" fn thread_id(t: u64) -> i64 {
    // SAFETY: t is a Thread.
    unsafe { word(t, T_NUMBER) as i64 }
}
extern "C" fn thread_interrupt(_t: u64) {}
extern "C" fn thread_is_interrupted(_t: u64) -> i32 {
    0
}

// ---- Atomics ---------------------------------------------------------------

/// The value word of an atomic box (after its header).
fn cell32<'a>(o: u64) -> &'a AtomicI32 {
    // SAFETY: an atomic box has its value at 8, 8-byte aligned.
    unsafe { &*((o + 8) as *const AtomicI32) }
}
fn cell64<'a>(o: u64) -> &'a AtomicI64 {
    // SAFETY: as above.
    unsafe { &*((o + 8) as *const AtomicI64) }
}
fn cell_ref<'a>(o: u64) -> &'a AtomicU64 {
    // SAFETY: as above.
    unsafe { &*((o + 8) as *const AtomicU64) }
}
const SC: Ordering = Ordering::SeqCst;

extern "C" fn ai_init(_o: u64) {}
extern "C" fn ai_init_value(o: u64, v: i32) {
    cell32(o).store(v, SC);
}
extern "C" fn ai_get(o: u64) -> i32 {
    cell32(o).load(SC)
}
extern "C" fn ai_set(o: u64, v: i32) {
    cell32(o).store(v, SC);
}
extern "C" fn ai_get_and_set(o: u64, v: i32) -> i32 {
    cell32(o).swap(v, SC)
}
extern "C" fn ai_cas(o: u64, expect: i32, v: i32) -> i32 {
    cell32(o).compare_exchange(expect, v, SC, SC).is_ok().into()
}
extern "C" fn ai_get_and_add(o: u64, d: i32) -> i32 {
    cell32(o).fetch_add(d, SC)
}
extern "C" fn ai_add_and_get(o: u64, d: i32) -> i32 {
    cell32(o).fetch_add(d, SC).wrapping_add(d)
}
extern "C" fn ai_increment_and_get(o: u64) -> i32 {
    ai_add_and_get(o, 1)
}
extern "C" fn ai_decrement_and_get(o: u64) -> i32 {
    ai_add_and_get(o, -1)
}
extern "C" fn ai_get_and_increment(o: u64) -> i32 {
    ai_get_and_add(o, 1)
}
extern "C" fn ai_get_and_decrement(o: u64) -> i32 {
    ai_get_and_add(o, -1)
}
extern "C" fn ai_long_value(o: u64) -> i64 {
    i64::from(ai_get(o))
}
extern "C" fn ai_double_value(o: u64) -> f64 {
    f64::from(ai_get(o))
}

extern "C" fn al_init_value(o: u64, v: i64) {
    cell64(o).store(v, SC);
}
extern "C" fn al_get(o: u64) -> i64 {
    cell64(o).load(SC)
}
extern "C" fn al_set(o: u64, v: i64) {
    cell64(o).store(v, SC);
}
extern "C" fn al_get_and_set(o: u64, v: i64) -> i64 {
    cell64(o).swap(v, SC)
}
extern "C" fn al_cas(o: u64, expect: i64, v: i64) -> i32 {
    cell64(o).compare_exchange(expect, v, SC, SC).is_ok().into()
}
extern "C" fn al_get_and_add(o: u64, d: i64) -> i64 {
    cell64(o).fetch_add(d, SC)
}
extern "C" fn al_add_and_get(o: u64, d: i64) -> i64 {
    cell64(o).fetch_add(d, SC).wrapping_add(d)
}
extern "C" fn al_increment_and_get(o: u64) -> i64 {
    al_add_and_get(o, 1)
}
extern "C" fn al_decrement_and_get(o: u64) -> i64 {
    al_add_and_get(o, -1)
}
extern "C" fn al_get_and_increment(o: u64) -> i64 {
    al_get_and_add(o, 1)
}
extern "C" fn al_get_and_decrement(o: u64) -> i64 {
    al_get_and_add(o, -1)
}
extern "C" fn al_int_value(o: u64) -> i32 {
    al_get(o) as i32
}
extern "C" fn al_double_value(o: u64) -> f64 {
    al_get(o) as f64
}

extern "C" fn ab_init_value(o: u64, v: i32) {
    cell32(o).store(i32::from(v != 0), SC);
}
extern "C" fn ab_get(o: u64) -> i32 {
    cell32(o).load(SC)
}
extern "C" fn ab_set(o: u64, v: i32) {
    cell32(o).store(i32::from(v != 0), SC);
}
extern "C" fn ab_get_and_set(o: u64, v: i32) -> i32 {
    cell32(o).swap(i32::from(v != 0), SC)
}
extern "C" fn ab_cas(o: u64, expect: i32, v: i32) -> i32 {
    cell32(o)
        .compare_exchange(i32::from(expect != 0), i32::from(v != 0), SC, SC)
        .is_ok()
        .into()
}

extern "C" fn ar_init_value(o: u64, v: u64) {
    cell_ref(o).store(v, SC);
}
extern "C" fn ar_get(o: u64) -> u64 {
    cell_ref(o).load(SC)
}
extern "C" fn ar_set(o: u64, v: u64) {
    cell_ref(o).store(v, SC);
}
extern "C" fn ar_get_and_set(o: u64, v: u64) -> u64 {
    cell_ref(o).swap(v, SC)
}
extern "C" fn ar_cas(o: u64, expect: u64, v: u64) -> i32 {
    cell_ref(o)
        .compare_exchange(expect, v, SC, SC)
        .is_ok()
        .into()
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
    let atomic = |name: &'static str, super_name: &'static str, methods| Builtin {
        name,
        super_name: Some(super_name),
        interfaces: &[],
        size: 16,
        is_final: true,
        is_interface: false,
        methods,
    };
    vec![
        Builtin {
            name: "java/lang/Runnable",
            super_name: Some(O),
            interfaces: &[],
            size: 8,
            is_final: false,
            is_interface: true,
            methods: Vec::new(),
        },
        Builtin {
            name: "java/lang/Thread",
            super_name: Some(O),
            interfaces: &["java/lang/Runnable"],
            size: THREAD_SIZE,
            is_final: false,
            is_interface: false,
            methods: vec![
                m("<init>", "()V", f!(thread_init)),
                m("<init>", "(Ljava/lang/Runnable;)V", f!(thread_init_target)),
                m(
                    "<init>",
                    "(Ljava/lang/Runnable;Ljava/lang/String;)V",
                    f!(thread_init_target_name),
                ),
                m("<init>", "(Ljava/lang/String;)V", f!(thread_init_name)),
                m("run", "()V", f!(thread_run)),
                m("start", "()V", f!(thread_start)),
                m("join", "()V", f!(thread_join)),
                m("join", "(J)V", f!(thread_join_millis)),
                m("isAlive", "()Z", f!(thread_is_alive)),
                m("getName", "()Ljava/lang/String;", f!(thread_get_name)),
                m("setName", "(Ljava/lang/String;)V", f!(thread_set_name)),
                m("setDaemon", "(Z)V", f!(thread_set_daemon)),
                m("isDaemon", "()Z", f!(thread_is_daemon)),
                m("getId", "()J", f!(thread_id)),
                m("threadId", "()J", f!(thread_id)),
                m("interrupt", "()V", f!(thread_interrupt)),
                m("isInterrupted", "()Z", f!(thread_is_interrupted)),
                s("currentThread", "()Ljava/lang/Thread;", f!(thread_current)),
                s("sleep", "(J)V", f!(thread_sleep)),
                s("yield", "()V", f!(thread_yield)),
                s("onSpinWait", "()V", f!(thread_on_spin_wait)),
            ],
        },
        atomic(
            "java/util/concurrent/atomic/AtomicInteger",
            "java/lang/Number",
            vec![
                m("<init>", "()V", f!(ai_init)),
                m("<init>", "(I)V", f!(ai_init_value)),
                m("get", "()I", f!(ai_get)),
                m("intValue", "()I", f!(ai_get)),
                m("longValue", "()J", f!(ai_long_value)),
                m("doubleValue", "()D", f!(ai_double_value)),
                m("getPlain", "()I", f!(ai_get)),
                m("getAcquire", "()I", f!(ai_get)),
                m("set", "(I)V", f!(ai_set)),
                m("lazySet", "(I)V", f!(ai_set)),
                m("setPlain", "(I)V", f!(ai_set)),
                m("setRelease", "(I)V", f!(ai_set)),
                m("getAndSet", "(I)I", f!(ai_get_and_set)),
                m("compareAndSet", "(II)Z", f!(ai_cas)),
                m("weakCompareAndSetPlain", "(II)Z", f!(ai_cas)),
                m("getAndAdd", "(I)I", f!(ai_get_and_add)),
                m("addAndGet", "(I)I", f!(ai_add_and_get)),
                m("incrementAndGet", "()I", f!(ai_increment_and_get)),
                m("decrementAndGet", "()I", f!(ai_decrement_and_get)),
                m("getAndIncrement", "()I", f!(ai_get_and_increment)),
                m("getAndDecrement", "()I", f!(ai_get_and_decrement)),
            ],
        ),
        atomic(
            "java/util/concurrent/atomic/AtomicLong",
            "java/lang/Number",
            vec![
                m("<init>", "()V", f!(ai_init)),
                m("<init>", "(J)V", f!(al_init_value)),
                m("get", "()J", f!(al_get)),
                m("longValue", "()J", f!(al_get)),
                m("intValue", "()I", f!(al_int_value)),
                m("doubleValue", "()D", f!(al_double_value)),
                m("getPlain", "()J", f!(al_get)),
                m("getAcquire", "()J", f!(al_get)),
                m("set", "(J)V", f!(al_set)),
                m("lazySet", "(J)V", f!(al_set)),
                m("setPlain", "(J)V", f!(al_set)),
                m("setRelease", "(J)V", f!(al_set)),
                m("getAndSet", "(J)J", f!(al_get_and_set)),
                m("compareAndSet", "(JJ)Z", f!(al_cas)),
                m("weakCompareAndSetPlain", "(JJ)Z", f!(al_cas)),
                m("getAndAdd", "(J)J", f!(al_get_and_add)),
                m("addAndGet", "(J)J", f!(al_add_and_get)),
                m("incrementAndGet", "()J", f!(al_increment_and_get)),
                m("decrementAndGet", "()J", f!(al_decrement_and_get)),
                m("getAndIncrement", "()J", f!(al_get_and_increment)),
                m("getAndDecrement", "()J", f!(al_get_and_decrement)),
            ],
        ),
        atomic(
            "java/util/concurrent/atomic/AtomicBoolean",
            O,
            vec![
                m("<init>", "()V", f!(ai_init)),
                m("<init>", "(Z)V", f!(ab_init_value)),
                m("get", "()Z", f!(ab_get)),
                m("getPlain", "()Z", f!(ab_get)),
                m("set", "(Z)V", f!(ab_set)),
                m("lazySet", "(Z)V", f!(ab_set)),
                m("getAndSet", "(Z)Z", f!(ab_get_and_set)),
                m("compareAndSet", "(ZZ)Z", f!(ab_cas)),
            ],
        ),
        atomic(
            "java/util/concurrent/atomic/AtomicReference",
            O,
            vec![
                m("<init>", "()V", f!(ai_init)),
                m("<init>", "(Ljava/lang/Object;)V", f!(ar_init_value)),
                m("get", "()Ljava/lang/Object;", f!(ar_get)),
                m("getPlain", "()Ljava/lang/Object;", f!(ar_get)),
                m("getAcquire", "()Ljava/lang/Object;", f!(ar_get)),
                m("set", "(Ljava/lang/Object;)V", f!(ar_set)),
                m("lazySet", "(Ljava/lang/Object;)V", f!(ar_set)),
                m("setRelease", "(Ljava/lang/Object;)V", f!(ar_set)),
                m(
                    "getAndSet",
                    "(Ljava/lang/Object;)Ljava/lang/Object;",
                    f!(ar_get_and_set),
                ),
                m(
                    "compareAndSet",
                    "(Ljava/lang/Object;Ljava/lang/Object;)Z",
                    f!(ar_cas),
                ),
            ],
        ),
    ]
}
