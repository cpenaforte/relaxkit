//! Re-runs the build when the embedded recipe directory changes, so an added or edited recipe is compiled in.

fn main() {
    println!("cargo:rerun-if-changed=recipes");
}
