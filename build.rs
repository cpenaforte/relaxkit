//! Re-runs the build when the embedded recipe directories change, so an added or edited recipe or fixture is compiled in.

fn main() {
    println!("cargo:rerun-if-changed=recipes");
    println!("cargo:rerun-if-changed=src/recipe/testdata");
}
