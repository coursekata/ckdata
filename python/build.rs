//! Package the actual build compiler's upstream notices, unchanged.
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-env-changed=RUSTC");
    let rustc = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(&rustc).args(["--print", "sysroot"]).output()?;
    if !output.status.success() {
        return Err("could not locate the build compiler's Rust notices".into());
    }
    let source = PathBuf::from(String::from_utf8(output.stdout)?.trim()).join("share/doc/rust");
    let destination =
        PathBuf::from(env::var_os("OUT_DIR").ok_or("missing Cargo OUT_DIR")?).join("rust-notices");
    if destination.exists() {
        fs::remove_dir_all(&destination)?;
    }
    fs::create_dir_all(destination.join("licenses"))?;
    fs::copy(
        source.join("COPYRIGHT-library.html"),
        destination.join("COPYRIGHT-library.html"),
    )?;
    for entry in fs::read_dir(source.join("licenses"))? {
        let entry = entry?;
        fs::copy(
            entry.path(),
            destination.join("licenses").join(entry.file_name()),
        )?;
    }
    println!(
        "cargo:rerun-if-changed={}",
        source.join("COPYRIGHT-library.html").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        source.join("licenses").display()
    );
    let version = Command::new(&rustc).arg("-vV").output()?;
    if !version.status.success() {
        return Err("could not identify the build compiler".into());
    }
    fs::write(destination.join("rustc.txt"), version.stdout)?;
    Ok(())
}
