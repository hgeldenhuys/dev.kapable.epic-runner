fn main() {
    // Embed git commit hash at build time for --version output
    let hash = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=GIT_HASH={}", hash.trim());
    println!("cargo:rerun-if-changed=.git/HEAD");
}
