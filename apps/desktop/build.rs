fn main() {
    println!("cargo:rerun-if-changed=src/macos_notify.m");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
        let obj = out_dir.join("macos_notify.o");
        let lib = out_dir.join("libmacos_notify.a");

        let status = std::process::Command::new("xcrun")
            .args([
                "clang",
                "-fobjc-arc",
                "-O2",
                "-mmacosx-version-min=12.0",
                "-c",
                "src/macos_notify.m",
                "-o",
            ])
            .arg(&obj)
            .status()
            .expect("invoke xcrun clang for macos_notify.m");
        assert!(
            status.success(),
            "clang failed compiling src/macos_notify.m"
        );

        let ar_status = std::process::Command::new("ar")
            .args(["rcs"])
            .arg(&lib)
            .arg(&obj)
            .status()
            .expect("invoke ar for libmacos_notify.a");
        assert!(ar_status.success(), "ar failed archiving libmacos_notify.a");

        println!("cargo:rustc-link-search=native={}", out_dir.display());
        println!("cargo:rustc-link-lib=static=macos_notify");
        println!("cargo:rustc-link-lib=framework=UserNotifications");
        println!("cargo:rustc-link-lib=framework=Foundation");
        println!("cargo:rustc-link-lib=framework=AppKit");
    }
}
