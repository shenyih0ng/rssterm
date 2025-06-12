fn get_commit_hash(len: Option<u8>) -> Option<String> {
    let hash_len = len.unwrap_or(6);

    if let Ok(output) = std::process::Command::new("jj")
        .args([
            "--ignore-working-copy",
            "--color=never",
            "log",
            "--no-graph",
            "-r=@-",
            "-T",
            &format!("commit_id.short({})", hash_len),
        ])
        .output()
    {
        if output.status.success() {
            return Some(String::from_utf8(output.stdout).unwrap());
        }
    }

    if let Ok(output) = std::process::Command::new("git")
        .args(["rev-parse", &format!("--short={}", hash_len), "HEAD"])
        .output()
    {
        if output.status.success() {
            return Some(String::from_utf8(output.stdout).unwrap());
        }
    }

    None
}

fn main() {
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    if let Some(commit_hash) = get_commit_hash(None) {
        println!("cargo:rustc-env=RSSTERM_VERSION={version}+{commit_hash}");
    } else {
        println!("cargo:rustc-env=RSSTERM_VERSION={version}");
    }
}
