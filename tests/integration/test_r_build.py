"""Exercise the R build templates with tiny, synthetic Rust crates."""

import io
import json
import os
import shutil
import subprocess
import tarfile
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]


def run(args, cwd, env):
    result = subprocess.run(
        args, check=False, cwd=cwd, env=env, text=True, capture_output=True
    )
    output = result.stdout + result.stderr
    if result.returncode:
        raise RuntimeError(f"{args} failed:\n{output}")
    return output


def fixture(base):
    package = base / "package with spaces"
    source = package / "src"
    rust = source / "rust"
    dependency = rust / "dependency"
    for directory in (package / "tools", rust, dependency):
        directory.mkdir(parents=True, exist_ok=True)
    for name in ("config.R", "msrv.R"):
        shutil.copyfile(ROOT / "r/tools" / name, package / "tools" / name)
    for name in ("Makevars.in", "Makevars.win.in", "Makevars.common"):
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
    (rust / "document.rs").write_text(
        'fn main() { println!("document-profile={}", if cfg!(debug_assertions) { "debug" } else { "release" }); }\n'
    )
    (dependency / "Cargo.toml").write_text(
        '[package]\nname="cache-dependency"\nversion="0.0.1"\nedition="2024"\n'
        '[lib]\npath="lib.rs"\n'
    )
    (dependency / "lib.rs").write_text("pub fn value() -> u32 { 1 }\n")
    (source / "harness.mk").write_text(
        "SHLIB = cacheprobe.so\ninclude Makevars\n$(SHLIB):\n\ttouch $@\n"
    )
    return package


def check(base, persistent, debug=False):
    package = fixture(base)
    source = package / "src"
    env = os.environ.copy()
    for name in ("CARGO_HOME", "CARGO_TARGET_DIR", "DEBUG", "NOT_CRAN"):
        env.pop(name, None)
    home = base / "cargo home with spaces"
    target = base / "cargo target with spaces"
    if debug:
        env["DEBUG"] = "1"
    # Neither a pre-existing local Cargo home nor a supplied vendor tree is ours.
    local_home = source / ".cargo"
    local_home.mkdir()
    (local_home / "caller-owned").touch()
    vendor = source / "vendor"
    vendor.mkdir()
    (vendor / "caller-owned").touch()
    (source / "rust/vendor-config.toml").write_text("")
    # These would be removed if a cache path were split into shell words.
    bystanders = [base / "cargo", source / "target", source / "with", source / "spaces"]
    for path in bystanders:
        path.mkdir()
        (path / "caller-owned").touch()
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
    assert first.count("Compiling cache-dependency") == 1, first
    profile = "debug" if debug else "release"
    assert f"document-profile={profile}" in first, first
    assert not (target / ("release" if debug else "debug")).exists()
    if persistent:
        assert list(target.glob(f"{profile}/deps/libcache_dependency-*.rlib"))
        library = target / profile / "libckdata_r.a"
        assert library.exists()
        run(["make", "-f", "harness.mk", "clean"], source, env)
        assert library.exists(), "clean removed a caller-owned cache artifact"
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
    assert (local_home / "caller-owned").exists()
    assert (vendor / "caller-owned").exists()
    assert not list(source.glob(".ckdata-cargo.*"))
    assert all((path / "caller-owned").exists() for path in bystanders)


def check_failure_cleanup(base):
    package = fixture(base)
    source = package / "src"
    env = os.environ.copy()
    for name in ("CARGO_HOME", "CARGO_TARGET_DIR", "DEBUG", "NOT_CRAN"):
        env.pop(name, None)
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
    (source / "rust/lib.rs").write_text("this is intentionally invalid Rust")
    try:
        run(["make", "-f", "harness.mk", "all"], source, env)
    except RuntimeError as error:
        assert "expected one of" in str(error), str(error)
    else:
        raise AssertionError("invalid synthetic Rust compiled")
    assert not list(source.glob(".ckdata-cargo.*")), (
        "failed build leaked its temporary Cargo home"
    )


def check_vendor_archive(base):
    package = fixture(base)
    source = package / "src"
    env = os.environ.copy()
    for name in ("CARGO_HOME", "CARGO_TARGET_DIR", "DEBUG", "NOT_CRAN"):
        env.pop(name, None)
    (source / "rust/vendor-config.toml").write_text("")
    (source / "rust/document.rs").write_text(
        'fn main() { assert_eq!(std::fs::read_to_string("vendor/synthetic").expect("archive sentinel must be extracted"), "synthetic vendored artifact"); println!("vendor-sentinel-observed"); }\n'
    )
    with tarfile.open(source / "rust/vendor.tar.xz", "w:xz") as archive:
        payload = b"synthetic vendored artifact"
        entry = tarfile.TarInfo("vendor/synthetic")
        entry.size = len(payload)
        archive.addfile(entry, io.BytesIO(payload))
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
    output = run(["make", "-f", "harness.mk", "all"], source, env)
    assert "vendor-sentinel-observed" in output, output
    assert not (source / "vendor").exists()
    assert not (source / ".ckdata-vendor-created").exists()
    assert not list(source.glob(".ckdata-cargo.*"))


def check_windows(base, debug=False):
    package = fixture(base)
    target = base / "cargo target with spaces"
    env = os.environ.copy()
    for name in ("DEBUG", "NOT_CRAN"):
        env.pop(name, None)
    env.update(
        CARGO_TARGET_DIR=str(target), CARGO_HOME=str(base / "cargo home with spaces")
    )
    if debug:
        env["DEBUG"] = "1"
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
    assert f'cd "{target}" && pwd -P' in commands, commands
    assert f"{source}/{target}" not in commands, commands
    assert f'--target-dir "{target}"' in commands, commands
    assert "cargo run" in commands, commands
    assert ("--release" in commands.split("cargo run", 1)[1]) is not debug, commands
    cleanup = run(["make", "-n", "-f", "harness.mk", "clean"], source, env)
    assert str(target) not in cleanup, cleanup
    # Execute the Windows shell recipe with a synthetic Cargo process. This checks
    # argv boundaries and the resolved linker path without cross-compiling Rust.
    binaries = base / "bin with spaces"
    binaries.mkdir()
    cargo = binaries / "cargo"
    cargo.write_text(
        "#!/usr/bin/env python3\nimport json, os, sys\n"
        'with open(os.environ["CARGO_PROBE_LOG"], "a") as log:\n'
        '    log.write(json.dumps({"args": sys.argv[1:], "library_path": os.environ["LIBRARY_PATH"]}) + "\\n")\n'
    )
    cargo.chmod(0o755)
    log = base / "cargo-arguments.jsonl"
    env.update(PATH=f"{binaries}{os.pathsep}{env['PATH']}", CARGO_PROBE_LOG=str(log))
    run(["make", "-f", "harness.mk", "all"], source, env)
    calls = [json.loads(line) for line in log.read_text().splitlines()]
    assert len(calls) == 2, calls
    for call in calls:
        arguments = call["args"]
        assert "--locked" in arguments, call
        assert arguments[arguments.index("--target-dir") + 1] == str(target), call
        assert call["library_path"].endswith(f";{target.resolve()}/libgcc_mock"), call


def check_windows_path_separator(base):
    package = fixture(base)
    # Simulate native Windows R's separator while stubbing only the external tools.
    expression = r"""old <- Sys.getenv("PATH")
.Platform$path.sep <- ";"
system <- function(command, intern) {
  stopifnot(identical(Sys.getenv("PATH"), paste0(old, ";", Sys.getenv("HOME"), "/.cargo/bin")))
  if (startsWith(command, "rustc")) "rustc 1.94.1" else "cargo 1.94.1"
}
source("tools/msrv.R")"""
    run(["Rscript", "-e", expression], package, os.environ.copy())


@pytest.mark.parametrize(
    "persistent,debug", [(False, False), (True, False), (True, True)]
)
def test_build_profiles_and_cache_ownership(tmp_path, persistent, debug):
    check(tmp_path, persistent, debug)


def test_failure_cleanup(tmp_path):
    check_failure_cleanup(tmp_path)


def test_vendor_archive_ownership(tmp_path):
    check_vendor_archive(tmp_path)


@pytest.mark.parametrize("debug", [False, True])
def test_windows_make_recipe(tmp_path, debug):
    check_windows(tmp_path, debug)


@pytest.mark.parametrize("persistent", [False, True])
@pytest.mark.parametrize("failure_stage", ["build", "run"])
def test_windows_failure_cleanup(tmp_path, persistent, failure_stage):
    package = fixture(tmp_path)
    source = package / "src"
    env = os.environ.copy()
    for name in ("CARGO_HOME", "CARGO_TARGET_DIR", "DEBUG", "NOT_CRAN"):
        env.pop(name, None)
    home = tmp_path / "cargo home with spaces"
    if persistent:
        home.mkdir()
        (home / "caller-owned").touch()
        env["CARGO_HOME"] = str(home)
    run(
        ["Rscript", "-e", '.Platform$OS.type <- "windows"; source("tools/config.R")'],
        package,
        env,
    )
    (source / "harness.mk").write_text(
        "SHLIB = cacheprobe.dll\nWIN = 64\ninclude Makevars.win\n$(SHLIB):\n\ttouch $@\n"
    )
    binaries = tmp_path / "bin with spaces"
    binaries.mkdir()
    cargo = binaries / "cargo"
    cargo.write_text(
        "#!/usr/bin/env python3\nimport json, os, sys\n"
        'with open(os.environ["CARGO_PROBE_LOG"], "a") as log:\n'
        '    log.write(json.dumps({"args": sys.argv[1:], "home": os.environ["CARGO_HOME"]}) + "\\n")\n'
        'sys.exit(37 if sys.argv[1] == os.environ["CARGO_FAIL_STAGE"] else 0)\n'
    )
    cargo.chmod(0o755)
    log = tmp_path / "cargo-arguments.jsonl"
    env.update(
        PATH=f"{binaries}{os.pathsep}{env['PATH']}",
        CARGO_PROBE_LOG=str(log),
        CARGO_FAIL_STAGE=failure_stage,
    )
    with pytest.raises(RuntimeError):
        run(["make", "-f", "harness.mk", "all"], source, env)
    calls = [json.loads(line) for line in log.read_text().splitlines()]
    assert [call["args"][0] for call in calls] == (
        ["build"] if failure_stage == "build" else ["build", "run"]
    )
    assert all("--locked" in call["args"] for call in calls)
    used_home = Path(calls[0]["home"])
    if persistent:
        assert used_home == home
        assert (home / "caller-owned").exists()
    else:
        assert not used_home.exists()
    assert not list(source.glob(".ckdata-cargo.*"))


def test_windows_path_separator(tmp_path):
    check_windows_path_separator(tmp_path)
