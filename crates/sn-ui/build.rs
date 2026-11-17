//! Build the shipped icon theme into the binary.
//!
//! `assets/icons-src/theme.gresource.xml` lists the SVG files; this compiles
//! it into `icons.gresource` next to `OUT_DIR` so `icons.rs` can `include_bytes!`
//! it. The alternative — a checked-in compiled blob — means every future edit
//! is two steps and one forgotten byte-identical diff.

fn main() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest_dir.join("../../assets/icons-src");

    // `cargo:rerun-if-changed` on the directory, plus the files: a glob would
    // need a build dependency we do not otherwise use, and this is exhaustible.
    //  the original call predates the pinned glib-build-tools 0.20 API
    // (`compile_resources(source_dirs, gresource_xml, target_in_OUT_DIR)` —
    // the original 2nd/3rd args read as an older signature); adapted to the
    // 0.20 shape with the same intent: OUT_DIR/icons.gresource for icons.rs.
    // Paths are anchored on `root` because a build script's cwd is the
    // package directory, not the workspace root the original literals assume.
    let root = root.to_str().expect("assets/icons-src path is UTF-8");
    glib_build_tools::compile_resources(
        &[root],
        &format!("{root}/theme.gresource.xml"),
        "icons.gresource",
    );
}
