use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    generate_license_notices();
    slint_build::compile_with_config(
        "src/ui/panel.slint",
        slint_build::CompilerConfiguration::new()
            .with_style("fluent-dark".into())
            .with_default_translation_context(slint_build::DefaultTranslationContext::None)
            .with_bundled_translations("translations"),
    )
    .expect("failed to compile preset panel");
    println!("cargo:rerun-if-changed=assets/app-icon.ico");

    #[cfg(target_os = "windows")]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("assets/app-icon.ico")
            .compile()
            .expect("failed to embed Windows application icon");
    }
}

fn generate_license_notices() {
    const VERSION: &str = "cargo-about 0.9.3";
    let directory = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let notices = directory.join("third-party-notices.txt");
    let stamp = directory.join("license-inputs.hash");
    let target = env::var("TARGET").unwrap();
    let mut inputs = DefaultHasher::new();
    VERSION.hash(&mut inputs);
    target.hash(&mut inputs);
    for path in [
        "build.rs",
        "Cargo.toml",
        "Cargo.lock",
        "LICENSE",
        "packaging/about.toml",
        "packaging/notices.hbs",
    ] {
        println!("cargo:rerun-if-changed={path}");
        fs::read(path)
            .expect("could not read license generation input")
            .hash(&mut inputs);
    }
    let fingerprint = format!("{:016x}", inputs.finish());
    if notices.is_file() && fs::read_to_string(&stamp).is_ok_and(|saved| saved == fingerprint) {
        return;
    }
    let version = Command::new("cargo-about")
        .arg("--version")
        .output()
        .expect("install cargo-about 0.9.3 and add it to PATH before building");
    assert!(
        version.status.success() && String::from_utf8_lossy(&version.stdout).trim() == VERSION,
        "this build requires cargo-about 0.9.3"
    );
    // A failed generation must not make a partial file look like a cached result.
    fs::write(&stamp, "").expect("could not invalidate the license generation cache");
    let generated = Command::new("cargo-about")
        .args(["--color", "never", "-L", "warn", "generate"])
        .args(["--locked", "--fail", "--all-features"])
        .args(["--target", &target, "--config", "packaging/about.toml"])
        .args(["packaging/notices.hbs", "--output-file"])
        .arg(&notices)
        .output()
        .expect("could not run cargo-about");
    // Incomplete git clarifications can warn and fall back even with --fail.
    assert!(
        generated.status.success() && generated.stderr.is_empty(),
        "license generation failed or emitted warnings:\n{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let text = fs::read_to_string(&notices).expect("could not read generated licenses");
    assert!(
        text.contains("Dependency source archives")
            && text.contains(include_str!("LICENSE"))
            && !text.contains("SOURCE DOWNLOAD REQUIRED:")
            && !text.contains("Copyright (c) <year> <copyright holders>"),
        "review the project license, dependency source links and copyright notices before building"
    );
    fs::write(stamp, fingerprint).expect("could not record license generation inputs");
}
