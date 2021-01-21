use std::error::Error;
use std::ffi::OsString;
use std::process::{exit, Command};

fn main() {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| OsString::from("rustc"));
    let command = format!("`{} --version`", rustc.to_string_lossy());

    let output = Command::new(&rustc)
        .arg("--version")
        .output()
        .unwrap_or_else(|e| {
            eprintln!("Error: failed to run {}: {}", command, e);
            exit(1)
        });

    let supports_maybe_uninit = parse(&output.stdout).unwrap_or_else(|e| {
        eprintln!("Error: failed to parse output of {}: {}", command, e);
        exit(1)
    });

    if supports_maybe_uninit {
        println!("cargo:rustc-cfg=supports_maybe_uninit");
    }
}

fn parse(output: &[u8]) -> Result<bool, Box<dyn Error>> {
    let s = std::str::from_utf8(output)?;
    let last_line = s.lines().last().unwrap_or(s);
    let mut words = last_line.trim().split(' ');
    if words.next() != Some("rustc") {
        return Err("version does not start with 'rustc'".into());
    }
    let mut triplet = words
        .next()
        .ok_or("output does not contain version triplet")?
        .split('.');
    if triplet.next() != Some("1") {
        return Err("rustc major version is not 1".into());
    }
    let minor: u32 = triplet
        .next()
        .ok_or("rustc version does not contain minor version")?
        .parse()
        .map_err(|e| format!("failed to parse minor version: {}", e))?;

    Ok(minor >= 36)
}
