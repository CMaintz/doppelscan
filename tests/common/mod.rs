use std::path::Path;
use std::process::Command;

/// Runs git in `dir` with a fixed identity and no line-ending conversion, so
/// fixtures commit byte for byte on every platform.
pub fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["-c", "core.autocrlf=false"])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}
