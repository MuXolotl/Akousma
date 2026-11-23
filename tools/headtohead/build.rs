//! Records the compiler and the library versions resolved in `Cargo.lock` for
//! the machine passport of the report.

fn main() {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let output = std::process::Command::new(rustc).arg("-V").output();
    let rustc = output.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    println!("cargo::rustc-env=BENCH_RUSTC={}", rustc.unwrap_or_default());
    let lock = std::fs::read_to_string("Cargo.lock").unwrap_or_default();
    let packages: Vec<_> = lock.split("[[package]]").collect();
    let versions: Vec<_> = ["akousma", "symphonia", "hound"]
        .iter()
        .map(|name| {
            let wanted = format!("name = \"{name}\"");
            let package = packages.iter().find(|p| p.lines().any(|l| l == wanted));
            let version =
                package.and_then(|p| p.lines().find_map(|l| l.strip_prefix("version = ")));
            format!("{name} {}", version.unwrap_or("unknown").trim_matches('"'))
        })
        .collect();
    println!("cargo::rustc-env=BENCH_LIBRARIES={}", versions.join(", "));
    println!("cargo::rerun-if-changed=Cargo.lock");
    println!("cargo::rerun-if-env-changed=RUSTC");
}
