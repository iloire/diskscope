//! Stamps the commit into the binary.
//!
//! There is no `.git` next to a copied-out release build, so the SHA has to be
//! baked in at compile time rather than read at runtime. It is optional by
//! design: a build from a tarball simply shows no version.

fn main() {
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/heads");

    let sha = std::process::Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .filter(|sha| !sha.is_empty());

    if let Some(sha) = sha {
        println!("cargo:rustc-env=DISKSCOPE_COMMIT={sha}");
    }
}
