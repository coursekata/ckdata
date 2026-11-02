"""Exercise the R build templates with tiny, synthetic Rust crates."""

import os
import shutil
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def run(args, cwd, env):
    result = subprocess.run(
        args, check=False, cwd=cwd, env=env, text=True, capture_output=True
    )
    output = result.stdout + result.stderr
    if result.returncode:
        raise RuntimeError(f"{args} failed:\n{output}")
    return output


def fixture(base):
    package = base / "package"
    source = package / "src"
    rust = source / "rust"
    dependency = rust / "dependency"
    for directory in (package / "tools", rust, dependency):
        directory.mkdir(parents=True, exist_ok=True)
    for name in ("config.R", "msrv.R"):
        shutil.copyfile(ROOT / "r/tools" / name, package / "tools" / name)
    for name in ("Makevars.in", "Makevars.win.in"):
        shutil.copyfile(ROOT / "r/src" / name, source / name)
    (package / "DESCRIPTION").write_text(
        "Package: cacheprobe\nVersion: 0.0.1\nSystemRequirements: Cargo, rustc >= 1.94\n"
    )
    (rust / "Cargo.toml").write_text(
        '[package]\nname="cacheprobe"\nversion="0.0.1"\nedition="2024"\n'
        '[lib]\nname="ckdata_r"\ncrate-type=["staticlib"]\npath="lib.rs"\n'
        '[[bin]]\nname="document"\npath="document.rs"\n'
        '[dependencies]\ncache-dependency={path="dependency"}\n[workspace]\n'
    )
    (rust / "lib.rs").write_text(
        "pub fn value() -> u32 { cache_dependency::value() }\n"
    )
    (rust / "document.rs").write_text("fn main() {}\n")
    (dependency / "Cargo.toml").write_text(
        '[package]\nname="cache-dependency"\nversion="0.0.1"\nedition="2024"\n'
        '[lib]\npath="lib.rs"\n'
    )
    (dependency / "lib.rs").write_text("pub fn value() -> u32 { 1 }\n")
    (source / "harness.mk").write_text(
        "SHLIB = cacheprobe.so\ninclude Makevars\n$(SHLIB):\n\ttouch $@\n"
    )
    return package


def check(base, persistent):
    package = fixture(base)
    source = package / "src"
    env = os.environ.copy()
    for name in ("CARGO_HOME", "CARGO_TARGET_DIR", "DEBUG", "NOT_CRAN"):
        env.pop(name, None)
    home = base / "cargo-home"
    target = base / "cargo-target"
    if persistent:
        home.mkdir()
        (home / "caller-owned").touch()
        env.update(CARGO_HOME=str(home), CARGO_TARGET_DIR=str(target))
    run(
        [
            "cargo",
            "generate-lockfile",
            "--offline",
            "--manifest-path",
            "src/rust/Cargo.toml",
        ],
        package,
        env,
    )
    run(["Rscript", "tools/config.R"], package, env)
    first = run(["make", "-f", "harness.mk", "all"], source, env)
    assert "Compiling cache-dependency" in first, first
    if persistent:
        assert list(target.glob("release/deps/libcache_dependency-*.rlib"))
        run(["make", "-f", "harness.mk", "clean"], source, env)
        second = run(["make", "-f", "harness.mk", "all"], source, env)
        assert "Compiling cache-dependency" not in second, second
        assert (home / "caller-owned").exists()
        # A restored cache must still rebuild changed source.
        (source / "rust/dependency/lib.rs").write_text(
            "pub fn value() -> u32 { 200 }\n"
        )
        changed = run(["make", "-f", "harness.mk", "all"], source, env)
        assert "Compiling cache-dependency" in changed, changed
    else:
        assert not (source / "rust/target").exists()
    assert not (source / ".cargo").exists()


def check_windows(base):
    package = fixture(base)
    target = base / "cargo-target"
    env = os.environ.copy()
    env.update(CARGO_TARGET_DIR=str(target), CARGO_HOME=str(base / "cargo-home"))
    # Generate the Windows template on this host; do not execute cross-compiled binaries.
    run(
        ["Rscript", "-e", '.Platform$OS.type <- "windows"; source("tools/config.R")'],
        package,
        env,
    )
    source = package / "src"
    (source / "harness.mk").write_text(
        "SHLIB = cacheprobe.dll\nWIN = 64\ninclude Makevars.win\n$(SHLIB):\n\ttouch $@\n"
    )
    commands = run(["make", "-n", "-f", "harness.mk", "all"], source, env)
    assert f";{target}/libgcc_mock" in commands, commands
    assert f"{source}/{target}" not in commands, commands
    assert f"--target-dir={target}" in commands, commands
    cleanup = run(["make", "-n", "-f", "harness.mk", "clean"], source, env)
    assert f" {target} " not in cleanup, cleanup


with tempfile.TemporaryDirectory(prefix="ckdata-cache-check-") as directory:
    for persistent in (False, True):
        base = Path(directory) / ("persistent" if persistent else "default")
        check(base, persistent)
    check_windows(Path(directory) / "windows")
print("R Cargo cache: cold build, warm build, source invalidation and cleanup passed")
