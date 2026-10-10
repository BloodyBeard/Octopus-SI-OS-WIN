use std::path::PathBuf;

fn main() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schemas");
    std::fs::create_dir_all(&dir).expect("schemas dir");
    for (name, schema) in octopus_schemas::surfaces() {
        let path = dir.join(format!("{name}.schema.json"));
        std::fs::write(&path, octopus_schemas::schema_text(&schema)).expect("schema writes");
        println!("wrote {}", path.display());
    }
}
