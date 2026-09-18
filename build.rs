//! Stages the built interface so the binary carries it.
//!
//! The interface is a Svelte bundle built into `dist/web`, which is not in
//! version control and is not in the published crate. `dist/web` is also the
//! wrong thing to name at runtime: a path that exists on the machine that
//! compiled the binary says nothing about the machine that runs it, and after
//! `cargo install` it names a directory that was never there.
//!
//! So the bundle is copied into `OUT_DIR` at build time and a table of its
//! contents is generated beside it. A build with the bundle present carries
//! it; a build without one, which is what `cargo install` from a registry
//! does, carries an empty table and says the interface is not built.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let bundle = manifest.join("dist/web");

    println!("cargo:rerun-if-changed={}", bundle.display());

    let mut files = Vec::new();
    collect(&bundle, &bundle, &mut files);
    files.sort();

    let mut table = String::from(
        "/// Every file of the built interface, by the path a browser asks for.\n\
         static BUNDLED: &[(&str, &[u8])] = &[\n",
    );
    for name in &files {
        let full = bundle.join(name);
        writeln!(
            table,
            "    ({:?}, include_bytes!({:?})),",
            name.replace('\\', "/"),
            full.display().to_string()
        )
        .expect("the table is written to a string");
    }
    table.push_str("];\n");

    std::fs::write(out.join("interface.rs"), table).expect("the asset table is written");
}

/// Every file under `root`, as a path relative to it.
fn collect(root: &Path, directory: &Path, found: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, found);
        } else if let Ok(relative) = path.strip_prefix(root) {
            found.push(relative.to_string_lossy().into_owned());
        }
    }
}
