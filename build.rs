fn main() {
    // `teamlite-call` finds libcef.so next to itself (the CEF runtime is installed alongside it).
    if std::env::var_os("CARGO_FEATURE_CEF_CALLS").is_some() {
        println!("cargo:rustc-link-arg-bin=teamlite-call=-Wl,-rpath,$ORIGIN");
    }
}
