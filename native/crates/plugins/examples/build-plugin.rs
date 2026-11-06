use std::fs;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/uppercase");
    let bytes = wat::parse_file(folder.join("plugin.wat"))?;
    fs::write(folder.join("plugin.wasm"), bytes)?;
    println!("Prepared the Uppercase plugin folder: {}", folder.display());
    Ok(())
}
