mod helpers;
mod history;
mod keyfile;
mod preferences;
mod s3;
mod security;
mod storage;
mod sync;
mod ui;
mod vault;

use gtk4::Application;
use gtk4::prelude::*;
use sekeeper_core::i18n::{detect_from_env, set_lang, tr};
use sekeeper_core::{model, transfer};

const APP_ID: &str = "io.github.sekeeper.Sekeeper";

fn main() {
    if let Err(error) = security::protect_process() {
        set_lang(detect_from_env());
        eprintln!(
            "{}: {error}",
            tr(
                "Sekeeper: unable to enable process protection",
                "Sekeeper: impossibile attivare la protezione del processo",
            )
        );
        std::process::exit(1);
    }
    set_lang(detect_from_env());
    register_resources();
    let application = Application::builder().application_id(APP_ID).build();
    application.connect_activate(ui::build);
    application.run();
}

fn register_resources() {
    gtk4::gio::resources_register_include!("sekeeper.gresource").unwrap_or_else(|error| {
        panic!(
            "{}: {error}",
            tr(
                "invalid embedded Sekeeper resources",
                "risorse incorporate di Sekeeper non valide",
            )
        )
    });
}
