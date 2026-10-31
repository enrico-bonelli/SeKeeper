mod helpers;
mod history;
mod keyfile;
mod model;
mod preferences;
mod s3;
mod security;
mod storage;
mod sync;
mod transfer;
mod ui;
mod vault;

use gtk4::Application;
use gtk4::prelude::*;

const APP_ID: &str = "io.github.sekeeper.Sekeeper";

fn main() {
    if let Err(error) = security::protect_process() {
        eprintln!("Sekeeper: impossibile attivare la protezione del processo: {error}");
        std::process::exit(1);
    }
    register_resources();
    let application = Application::builder().application_id(APP_ID).build();
    application.connect_activate(ui::build);
    application.run();
}

fn register_resources() {
    gtk4::gio::resources_register_include!("sekeeper.gresource")
        .expect("risorse incorporate di Sekeeper non valide");
}
