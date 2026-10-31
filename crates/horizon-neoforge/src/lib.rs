//! Java mod compatibility (fork `NeuHorizon`, docs/java-compat/README.md):
//! the NeoForge and Minecraft classes a compiled NeoForge mod registers its
//! content with. The mod runs from its original jar, through
//! `horizon-java`'s compiler; what it registers (blocks with their states,
//! shapes and light, items with their components) is written as a Horizon
//! content mod, which the game loads as any other.
pub mod game;
pub mod host;
pub mod objects;
pub mod translate;
pub mod world;

pub use game::GameData;
pub use translate::{Session, Translation, translate, translate_all, translate_session, write};

/// The classes of a jar.
pub fn read_jar(path: &std::path::Path) -> Result<Vec<horizon_java::ClassFile>, String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    let mut classes = Vec::new();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        if entry.name().ends_with(".class") {
            let mut data = Vec::new();
            entry.read_to_end(&mut data).map_err(|e| e.to_string())?;
            classes.push(horizon_java::ClassFile::parse(&data).map_err(|e| e.to_string())?);
        }
    }
    Ok(classes)
}
