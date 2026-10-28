fn main() {
    // Ask Windows for administrator rights up front (UAC prompt on launch).
    // A build script runs on the host, so test the *target* OS from Cargo's env.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        use embed_manifest::manifest::ExecutionLevel;
        use embed_manifest::{embed_manifest, new_manifest};
        embed_manifest(new_manifest("Fatstick.Helper").requested_execution_level(ExecutionLevel::RequireAdministrator))
            .expect("unable to embed manifest");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
