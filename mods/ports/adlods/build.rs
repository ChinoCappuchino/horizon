//! The pack's deposits (tools/adlods_port.py's output, ADLODS_DEPOSITS), built in; none without.
fn main() {
    println!("cargo:rerun-if-env-changed=ADLODS_DEPOSITS");
    let out = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("deposits.json");
    let text = match std::env::var("ADLODS_DEPOSITS") {
        Ok(path) => {
            println!("cargo:rerun-if-changed={path}");
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("ADLODS_DEPOSITS {path}: {e}"))
        }
        Err(_) => "[]".into(),
    };
    std::fs::write(out, text).unwrap();
}
