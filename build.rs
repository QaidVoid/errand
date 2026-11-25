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
//!
//! It also stamps `ERRAND_VERSION`, the version the binary reports, with the
//! commit it was built from when that is known: `0.5.0 (abc1234)`, or a bare
//! `0.5.0` when it is not.

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

    stamp_version(&manifest);
}

/// Stamps `ERRAND_VERSION`: the package version, and the short commit when
/// one is known from `ERRAND_BUILD_SHA`, the record `cargo publish` leaves,
/// or the checkout.
fn stamp_version(manifest: &Path) {
    println!("cargo:rerun-if-env-changed=ERRAND_BUILD_SHA");
    let commit = std::env::var("ERRAND_BUILD_SHA")
        .ok()
        .filter(|sha| !sha.is_empty())
        .or_else(|| packaged_commit(manifest))
        .or_else(|| checkout_commit(manifest));
    let version = std::env::var("CARGO_PKG_VERSION").expect("cargo sets the package version");
    let version = match commit {
        Some(sha) => format!("{version} ({})", &sha[..sha.len().min(7)]),
        None => version,
    };
    println!("cargo:rustc-env=ERRAND_VERSION={version}");
}

/// The commit `cargo publish` recorded, for a build from crates.io.
fn packaged_commit(manifest: &Path) -> Option<String> {
    let info = std::fs::read_to_string(manifest.join(".cargo_vcs_info.json")).ok()?;
    let rest = &info[info.find("\"sha1\"")? + 6..];
    let rest = &rest[rest.find('"')? + 1..];
    Some(rest[..rest.find('"')?].to_owned())
}

/// The commit the checkout's `HEAD` names, read from the files rather than
/// by running a tool, so a build needs nothing installed beside cargo.
///
/// Only files that exist are watched: cargo reruns a script on every build
/// for a watched path that is missing.
fn checkout_commit(manifest: &Path) -> Option<String> {
    let git = manifest.join(".git");
    let watch = |path: &Path| {
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    };
    let head_path = git.join("HEAD");
    watch(&head_path);
    let head = std::fs::read_to_string(&head_path).ok()?;
    let head = head.trim();
    let Some(reference) = head.strip_prefix("ref: ") else {
        return Some(head.to_owned()).filter(|sha| is_sha(sha));
    };
    let loose = git.join(reference);
    let packed = git.join("packed-refs");
    watch(&loose);
    watch(&packed);
    std::fs::read_to_string(&loose)
        .ok()
        .map(|sha| sha.trim().to_owned())
        .or_else(|| {
            std::fs::read_to_string(&packed)
                .ok()?
                .lines()
                .find_map(|line| {
                    let (sha, name) = line.split_once(' ')?;
                    (name == reference).then(|| sha.to_owned())
                })
        })
        .filter(|sha| is_sha(sha))
}

/// Whether `text` reads as a full commit id.
fn is_sha(text: &str) -> bool {
    text.len() >= 40 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
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
