#![allow(unexpected_cfgs)] // for `#[cfg(rustver)]` in this build.rs.

const ALLOWED_CFG_NAMES: &[&str] = &[
    "armv5te",
    "beta_clippy",
    "circleci",
    "kani",
    "mips",
    "rustver",
    "trybuild",
];

#[cfg(rustver)]
fn main() {
    use rustc_version::version;
    let version = version().expect("Can't get the rustc version");
    println!(
        "cargo:rustc-env=RUSTC_SEMVER={}.{}",
        version.major, version.minor
    );

    allow_cfgs(ALLOWED_CFG_NAMES);
}

#[cfg(not(rustver))]
fn main() {
    allow_cfgs(ALLOWED_CFG_NAMES);
}

/// Tells `rustc` to allow `#[cfg(...)]` with the given names.
fn allow_cfgs(names: &[&str]) {
    for name in names.iter() {
        println!("cargo:rustc-check-cfg=cfg({name})");
    }
}
