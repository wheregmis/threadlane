fn main() {
    // RUST_MIN_STACK (see .cargo/config.toml) only covers spawned threads; the
    // main thread's stack reserve comes from the PE header and stays at the
    // 1 MiB MSVC default. GPUI's recursive layout/paint passes on a debug
    // build exceed that once the element tree gets deep enough (repro: attach
    // a browser annotation image -> "thread 'main' has overflowed its stack").
    // Bump the exe's reserve to 32 MiB for Windows builds.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg=/STACK:33554432");
    }
}
