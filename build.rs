use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=data/sekeeper.gresource.xml");
    println!(
        "cargo:rerun-if-changed=data/icons/hicolor/scalable/apps/io.github.sekeeper.Sekeeper.svg"
    );
    let output =
        PathBuf::from(env::var_os("OUT_DIR").expect("Cargo OUT_DIR")).join("sekeeper.gresource");
    let status = Command::new("glib-compile-resources")
        .arg("--sourcedir=data")
        .arg("--target")
        .arg(output)
        .arg("data/sekeeper.gresource.xml")
        .status()
        .expect("glib-compile-resources necessario: installa gli strumenti di sviluppo GLib");
    assert!(
        status.success(),
        "impossibile compilare le risorse di Sekeeper"
    );
}
