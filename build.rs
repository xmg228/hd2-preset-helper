fn main() {
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
