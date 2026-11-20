use std::{env, path::PathBuf, process::Command};

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    println!("cargo:rerun-if-changed=NeoBoard.rc");
    println!("cargo:rerun-if-changed=../design/NeoBoard.ico");
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let (object, mut compiler) = if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        let object = out.join("NeoBoard.res");
        let mut cmd = Command::new("rc.exe");
        cmd.arg("/nologo").arg("/fo").arg(&object).arg("NeoBoard.rc");
        (object, cmd)
    } else {
        let object = out.join("NeoBoard.o");
        let mut cmd = Command::new(env::var_os("WINDRES").unwrap_or_else(|| "windres".into()));
        cmd.args(["-O", "coff", "-i", "NeoBoard.rc", "-o"]).arg(&object);
        (object, cmd)
    };
    assert!(compiler.status().expect("Windows SDK rc.exe or GNU windres required").success());
    println!("cargo:rustc-link-arg-bin=neochat-desktop={}", object.display());
}