//! The translator's target, checked (docs/java-compat): Horizon's wasmtime
//! runs WebAssembly GC (Java objects as structs, arrays as GC arrays) and
//! exception handling (Java's throw and catch).
use wasmtime::{Config, Engine, Instance, Module, Store};

/// A `java/lang/Object`-like struct with a class id, a subclass with an int
/// field, an int array, and an exception caught by its tag.
const MODULE: &str = r#"
(module
  (type $object (sub (struct (field $class i32))))
  (type $point (sub $object (struct (field $class i32) (field $x (mut i32)))))
  (type $ints (array (mut i32)))
  (tag $throwable (param (ref null $object)))
  (func $make (param $x i32) (result (ref $point))
    (struct.new $point (i32.const 7) (local.get $x)))
  ;; new Point(x); p.x += 1; int[] a = new int[3]; a[2] = p.x; return a[2] + a.length
  (func (export "objects") (param $x i32) (result i32)
    (local $p (ref null $point)) (local $a (ref null $ints))
    (local.set $p (call $make (local.get $x)))
    (struct.set $point $x (local.get $p) (i32.add (struct.get $point $x (local.get $p)) (i32.const 1)))
    (local.set $a (array.new_default $ints (i32.const 3)))
    (array.set $ints (local.get $a) (i32.const 2) (struct.get $point $x (local.get $p)))
    (i32.add (array.get $ints (local.get $a) (i32.const 2)) (array.len (local.get $a))))
  ;; try { throw new Point(x) } catch (Throwable t) { return t.class }
  (func (export "throws") (param $x i32) (result i32)
    (block $caught (result (ref null $object))
      (try_table (catch $throwable $caught)
        (throw $throwable (call $make (local.get $x))))
      (unreachable))
    (struct.get $object $class))
)
"#;

#[test]
fn wasm_gc_objects_arrays_and_exceptions_run() {
    let mut config = Config::new();
    config
        .wasm_gc(true)
        .wasm_function_references(true)
        .wasm_exceptions(true);
    let engine = Engine::new(&config).unwrap();
    let module = Module::new(&engine, wat::parse_str(MODULE).unwrap()).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let objects = instance
        .get_typed_func::<i32, i32>(&mut store, "objects")
        .unwrap();
    assert_eq!(objects.call(&mut store, 41).unwrap(), 45);
    let throws = instance
        .get_typed_func::<i32, i32>(&mut store, "throws")
        .unwrap();
    assert_eq!(throws.call(&mut store, 1).unwrap(), 7);
}
