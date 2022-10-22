#[cfg(skeptic)]
fn main() {
    // generates doc tests for `README.md`.
    skeptic::generate_doc_tests(&["README.md"]);
}

#[cfg(rustver)]
fn main() {
    use rustc_version::version;
    let version = version().expect("Can't get the rustc version");
    println!(
        "cargo:rustc-env=RUSTC_SEMVER={}.{}",
        version.major, version.minor
    );
}

#[cfg(not(any(skeptic, rustver)))]
fn main() {}
