fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // Recognize source executables even with an external Cargo target directory.
    let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let profile = output.ancestors().nth(3).unwrap();
    println!(
        "cargo:rustc-env=JOB_HARNESS_BUILD_PROFILE_DIR={}",
        profile.display()
    );
}
