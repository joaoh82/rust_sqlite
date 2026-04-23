extern crate napi_build;

fn main() {
    // Generates the N-API symbol compatibility shims so the built
    // `.node` file loads cleanly under every Node.js release
    // compatible with our N-API version (v9 → Node 18+).
    napi_build::setup();
}
