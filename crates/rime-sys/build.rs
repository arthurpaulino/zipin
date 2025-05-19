//! Build driver for `rime-sys`.
//!
//! Auto-fetches librime into `<target>/librime/` (shallow clone, recursive
//! submodules) on first build, then:
//!
//! 1. Runs librime's own `deps.mk` to build the static deps it ships
//!    (glog, leveldb, marisa-trie, opencc, yaml-cpp). Installed into
//!    `librime/{include,lib,share}` where librime's custom `Find*.cmake`
//!    modules look for them.
//! 2. Configures librime itself with `BUILD_STATIC=ON`, building inside the
//!    librime tree's conventional `build/` directory.
//! 3. Generates Rust bindings via `bindgen` from `rime_api.h` +
//!    `rime_api_stdbool.h`.
//! 4. Emits static-link directives for the resulting `librime.a` plus
//!    its transitive `.a` archives.
//!
//! Boost is sourced from the system. On Linux librime needs the `regex`
//! component compiled (`libboost-regex-dev`); other platforms only need
//! Boost headers. Override the search root with `BOOST_ROOT`.
//!
//! Override the librime checkout location with `ZIPIN_LIBRIME_DIR`; pinned
//! tag is `LIBRIME_TAG` below.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Pinned upstream tag. Bumping it moves librime + every one of its
/// in-tree submodules in one step (no partial upgrades).
///
/// Procedure:
///   1. Wipe `<workspace>/target/librime/` so the new tag clones cleanly.
///   2. Update this constant.
///   3. `cargo build` — re-check that bindgen output + the link
///      directives in `emit_link_directives` still match.
///   4. Diff the librime tree's `LICENSE` files against
///      `<workspace>/LICENSE-THIRD-PARTY.md`; refresh attribution if any
///      dep was added, dropped, or relicensed.
const LIBRIME_TAG: &str = "1.16.1";

fn main() {
    let workspace_root = workspace_root();
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let librime_dir = librime_checkout(&workspace_root, &out_dir);

    ensure_librime_checkout(&librime_dir);

    let cmake_lists = librime_dir.join("CMakeLists.txt");
    if !cmake_lists.exists() {
        emit_warning(
            "librime checkout incomplete; emitting stub bindings. The final \
             link will fail until the clone succeeds (check network + git).",
        );
        write_stub_bindings(&out_dir);
        return;
    }

    let header_dir = librime_dir.join("src");
    let api_header = header_dir.join("rime_api.h");
    let stdbool_header = header_dir.join("rime_api_stdbool.h");

    println!("cargo:rerun-if-changed={}", cmake_lists.display());
    println!("cargo:rerun-if-changed={}", api_header.display());
    println!("cargo:rerun-if-changed={}", stdbool_header.display());
    println!("cargo:rerun-if-env-changed=BOOST_ROOT");
    println!("cargo:rerun-if-env-changed=ZIPIN_LIBRIME_DIR");
    println!("cargo:rerun-if-env-changed=RIME_BUILD_JOBS");

    build_librime_deps(&librime_dir);
    let librime_build_dir = build_librime(&librime_dir);

    generate_bindings(&header_dir, &out_dir);

    emit_link_directives(&librime_dir, &librime_build_dir);
}

fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .nth(2)
        .expect("workspace root above crates/rime-sys")
        .to_path_buf()
}

/// Resolve where the librime source tree lives. Precedence:
/// 1. `ZIPIN_LIBRIME_DIR` (explicit override).
/// 2. `$CARGO_TARGET_DIR/librime`.
/// 3. `<workspace>/target/librime`.
///
/// Lives outside the workspace source tree so the repo stays first-party
/// only and a `git status` doesn't show 60k lines of vendored C++.
fn librime_checkout(workspace_root: &Path, out_dir: &Path) -> PathBuf {
    if let Some(custom) = env::var_os("ZIPIN_LIBRIME_DIR") {
        return absolutize(workspace_root, PathBuf::from(custom));
    }
    if let Some(t) = env::var_os("CARGO_TARGET_DIR") {
        return absolutize(workspace_root, PathBuf::from(t)).join("librime");
    }
    // Final fallback: walk OUT_DIR ancestors until we find a `target` dir.
    // OUT_DIR layout is `<target>/<profile>/build/<crate>-<hash>/out`.
    for anc in out_dir.ancestors() {
        if anc.file_name().and_then(|n| n.to_str()) == Some("target") {
            return anc.join("librime");
        }
    }
    workspace_root.join("target").join("librime")
}

fn absolutize(workspace_root: &Path, p: PathBuf) -> PathBuf {
    if p.is_absolute() {
        p
    } else {
        workspace_root.join(p)
    }
}

/// Shallow clone of librime + recursive shallow submodules into
/// `librime_dir`. Idempotent — skips when CMakeLists.txt already there.
fn ensure_librime_checkout(librime_dir: &Path) {
    if librime_dir.join("CMakeLists.txt").exists() {
        return;
    }
    if let Some(parent) = librime_dir.parent() {
        fs::create_dir_all(parent).expect("create librime parent dir");
    }
    let status = Command::new("git")
        .args([
            "clone",
            "--depth",
            "1",
            "--branch",
            LIBRIME_TAG,
            "--recurse-submodules",
            "--shallow-submodules",
            "https://github.com/rime/librime.git",
        ])
        .arg(librime_dir)
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => panic!("git clone librime exited with {s}"),
        Err(err) => panic!("git clone librime failed to spawn: {err}"),
    }
}

/// Marker file we drop after the deps build succeeds. Skips re-running on
/// every cargo invocation; deleting `librime/lib/` re-triggers.
fn deps_marker(librime_dir: &Path) -> PathBuf {
    librime_dir.join("lib/.zipin-deps-built")
}

fn build_librime_deps(librime_dir: &Path) {
    if deps_marker(librime_dir).exists() {
        return;
    }
    let mut cmd = Command::new("make");
    cmd.current_dir(librime_dir)
        .arg("-f")
        .arg("deps.mk")
        // deps.mk's own MAKEFLAGS append breaks under cargo's pre-set
        // MAKEFLAGS / jobserver. Disable its parallel-append, pass -jN
        // ourselves from NUM_JOBS (cargo-supplied).
        .env("NOPARALLEL", "1")
        .env_remove("MAKEFLAGS");
    if let Ok(jobs) = env::var("NUM_JOBS") {
        cmd.arg(format!("-j{jobs}"));
    }
    run(cmd, "librime deps.mk");

    if let Some(parent) = deps_marker(librime_dir).parent() {
        fs::create_dir_all(parent).ok();
    }
    fs::write(deps_marker(librime_dir), b"").ok();
}

fn build_librime(librime_dir: &Path) -> PathBuf {
    let build_dir = librime_dir.join("build");
    fs::create_dir_all(&build_dir).expect("create librime build dir");

    let mut configure = Command::new("cmake");
    configure
        .arg("-S")
        .arg(librime_dir)
        .arg("-B")
        .arg(&build_dir)
        .arg("-DCMAKE_BUILD_TYPE=Release")
        .arg("-DBUILD_STATIC=ON")
        .arg("-DBUILD_SHARED_LIBS=OFF")
        .arg("-DBUILD_TEST=OFF")
        .arg("-DBUILD_SAMPLE=OFF")
        .arg("-DENABLE_LOGGING=OFF")
        .arg("-DCMAKE_POSITION_INDEPENDENT_CODE=ON");

    // CMake < 3.25 does not auto-define `LINUX`; librime gates Boost min
    // version on it.
    if cfg!(target_os = "linux") {
        configure.arg("-DLINUX=ON");
    }

    // Skip Boost's modular `BoostConfig.cmake` (Boost 1.74 ships per-component
    // `boost_<name>Config.cmake` files separately, only present in the
    // matching `-dev` apt package). Fall back to CMake's legacy
    // `FindBoost.cmake` module which only needs headers + the .so.
    configure.arg("-DBoost_NO_BOOST_CMAKE=ON");

    if let Some(boost_root) = env::var_os("BOOST_ROOT") {
        configure.arg(format!("-DBOOST_ROOT={}", Path::new(&boost_root).display()));
    }

    run(configure, "cmake configure");

    // Cap cmake parallelism: librime + boost-regex template instantiations
    // are RAM-heavy enough to OOM on 16-core boxes. Override with
    // RIME_BUILD_JOBS for boxes with more headroom.
    let jobs = env::var("RIME_BUILD_JOBS").unwrap_or_else(|_| "8".into());
    let mut build = Command::new("cmake");
    build
        .arg("--build")
        .arg(&build_dir)
        .arg("--parallel")
        .arg(jobs);
    run(build, "cmake build");

    build_dir
}

fn generate_bindings(header_dir: &Path, out_dir: &Path) {
    let stdbool_header = header_dir.join("rime_api_stdbool.h");
    let api_header = header_dir.join("rime_api.h");

    let bindings = bindgen::Builder::default()
        .header(stdbool_header.to_string_lossy())
        .header(api_header.to_string_lossy())
        .clang_arg(format!("-I{}", header_dir.display()))
        .allowlist_function("rime_get_api_stdbool")
        .allowlist_type("RimeApi_stdbool")
        .allowlist_type("RimeContext_stdbool")
        .allowlist_type("RimeStatus_stdbool")
        .allowlist_type("RimeMenu_stdbool")
        .allowlist_type("RimeCommit_stdbool")
        .allowlist_type("RimeComposition")
        .allowlist_type("RimeCandidate")
        .allowlist_type("RimeTraits")
        .allowlist_type("RimeSessionId")
        .allowlist_type("RimeNotificationHandler")
        .blocklist_function("RimeSetup")
        .blocklist_function("RimeInitialize")
        .blocklist_function("RimeFinalize")
        .blocklist_function("rime_get_api")
        .layout_tests(false)
        .generate()
        .expect("bindgen failed for rime_api headers");

    bindings
        .write_to_file(out_dir.join("bindings.rs"))
        .expect("write bindings.rs");
}

fn emit_link_directives(librime_dir: &Path, librime_build_dir: &Path) {
    // librime archive: under build/lib (CMakeLists.txt installs there).
    println!(
        "cargo:rustc-link-search=native={}",
        librime_build_dir.join("lib").display()
    );
    // Some librime versions emit it directly under build/.
    println!(
        "cargo:rustc-link-search=native={}",
        librime_build_dir.display()
    );
    // CMake target is `rime-static` but its OUTPUT_NAME is `rime`, so the
    // archive on disk is `librime.a`.
    println!("cargo:rustc-link-lib=static=rime");

    // deps.mk installs each transitive .a into librime/lib/.
    println!(
        "cargo:rustc-link-search=native={}",
        librime_dir.join("lib").display()
    );

    for lib in ["opencc", "marisa", "leveldb", "yaml-cpp"] {
        println!("cargo:rustc-link-lib=static={lib}");
    }

    // Boost regex (Linux REQUIRED component; macOS/Windows header-only).
    if cfg!(target_os = "linux") {
        println!("cargo:rustc-link-lib=boost_regex");
    }

    // C++ stdlib.
    if cfg!(target_os = "macos") {
        println!("cargo:rustc-link-lib=c++");
    } else if cfg!(target_os = "linux") {
        println!("cargo:rustc-link-lib=stdc++");
    }
}

fn write_stub_bindings(out_dir: &Path) {
    let stub = "// Auto-generated stub. librime checkout missing; see\n\
                // crates/rime-sys/build.rs for the bring-up steps.\n";
    fs::write(out_dir.join("bindings.rs"), stub).expect("write stub bindings");
}

fn emit_warning(msg: &str) {
    println!("cargo:warning={msg}");
}

fn run(mut cmd: Command, label: &str) {
    let status = cmd
        .status()
        .unwrap_or_else(|err| panic!("{label}: failed to spawn: {err}"));
    if !status.success() {
        panic!("{label}: exited with {status}");
    }
}
