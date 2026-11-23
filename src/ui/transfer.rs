use super::*;
use crate::{storage, transfer};
use gtk4::{FileChooserAction, FileChooserDialog};

fn chooser(
    state: &SharedState,
    title: &str,
    action: FileChooserAction,
    accept: &str,
) -> FileChooserDialog {
    let chooser = FileChooserDialog::new(
        Some(title),
        Some(&state.window),
        action,
        &[
            (tr("Cancel", "Annulla"), ResponseType::Cancel),
            (accept, ResponseType::Accept),
        ],
    );
    track_dialog(state, &chooser);
    chooser.set_modal(true);
    chooser.set_destroy_with_parent(true);
    chooser.set_default_size(760, 520);
    chooser
}

pub fn confirm_export(state: &SharedState) {
    let dialog = MessageDialog::new(
        Some(&state.window),
        gtk4::DialogFlags::MODAL,
        MessageType::Warning,
        gtk4::ButtonsType::None,
        tr(
            "Export credentials in plain text?",
            "Esportare le credenziali in chiaro?",
        ),
    );
    track_dialog(state, &dialog);
    dialog.set_secondary_text(Some(tr(
        "The CSV file is NOT encrypted: anyone who reads it can see all your passwords. Keep it on encrypted storage and delete it as soon as you are done.",
        "Il file CSV NON è cifrato: chiunque lo legga vede tutte le password. Conservalo su un supporto cifrato e cancellalo appena hai finito.",
    )));
    dialog.add_button(tr("Cancel", "Annulla"), ResponseType::Cancel);
    let export = dialog.add_button(tr("Choose file…", "Scegli il file…"), ResponseType::Accept);
    export.add_css_class("destructive-action");
    dialog.connect_response(clone!(
        #[weak]
        state,
        move |dialog, response| {
            dialog.close();
            if response == ResponseType::Accept {
                choose_export(&state);
            }
        }
    ));
    dialog.present();
}

fn choose_export(state: &SharedState) {
    let chooser = chooser(
        state,
        tr("Export credentials", "Esporta le credenziali"),
        FileChooserAction::Save,
        tr("Export", "Esporta"),
    );
    chooser.set_current_name("sekeeper-export.csv");
    chooser.connect_response(clone!(
        #[weak]
        state,
        move |chooser, response| {
            if response == ResponseType::Accept {
                match export(&state, chooser) {
                    Ok(message) => toast(&state, &message),
                    Err(error) => {
                        show_error(chooser, &error);
                        return;
                    }
                }
            }
            keyfiles::dismiss(chooser);
        }
    ));
    chooser.present();
}

fn export(state: &SharedState, chooser: &FileChooserDialog) -> Result<String, String> {
    let path = chooser
        .file()
        .and_then(|file| file.path())
        .ok_or_else(|| tr("Choose a local file.", "Scegli un file locale.").to_string())?;
    let (count, encoded) = {
        let slot = state.vault.borrow();
        let vault = slot
            .as_ref()
            .ok_or_else(|| tr("The vault is locked.", "Il vault è bloccato.").to_string())?;
        (
            vault.data.credentials.len(),
            transfer::encode(&vault.data.credentials),
        )
    };
    let outcome =
        storage::create_export(&path, encoded.as_bytes()).map_err(|error| match error.kind() {
            std::io::ErrorKind::AlreadyExists => tr(
                "The file already exists: choose another name.",
                "Il file esiste già: scegli un altro nome.",
            )
            .to_string(),
            _ => {
                if is_italian() {
                    format!("Esportazione non riuscita: {error}")
                } else {
                    format!("Export failed: {error}")
                }
            }
        })?;
    Ok(match outcome {
        SaveOutcome::Durable => {
            if is_italian() {
                format!("{count} credenziali esportate in chiaro")
            } else if count == 1 {
                "1 credential exported in plain text".into()
            } else {
                format!("{count} credentials exported in plain text")
            }
        }
        _ => {
            if is_italian() {
                format!("{count} credenziali esportate; scrittura sul supporto non confermata")
            } else if count == 1 {
                "1 credential exported; write to storage not confirmed".into()
            } else {
                format!("{count} credentials exported; write to storage not confirmed")
            }
        }
    })
}

pub fn choose_import(state: &SharedState) {
    let chooser = chooser(
        state,
        tr("Import credentials from CSV", "Importa credenziali da CSV"),
        FileChooserAction::Open,
        tr("Import", "Importa"),
    );
    let filter = gtk4::FileFilter::new();
    filter.set_name(Some(tr("CSV files", "File CSV")));
    filter.add_suffix("csv");
    chooser.add_filter(&filter);
    chooser.connect_response(clone!(
        #[weak]
        state,
        move |chooser, response| {
            if response == ResponseType::Accept
                && let Err(error) = import(&state, chooser)
            {
                show_error(chooser, &error);
                return;
            }
            keyfiles::dismiss(chooser);
        }
    ));
    chooser.present();
}

fn import(state: &SharedState, chooser: &FileChooserDialog) -> Result<(), String> {
    let path = chooser
        .file()
        .and_then(|file| file.path())
        .ok_or_else(|| tr("Choose a local file.", "Scegli un file locale.").to_string())?;
    let bytes = Zeroizing::new(
        storage::read_import(&path, transfer::MAX_IMPORT_BYTES).map_err(|error| {
            if is_italian() {
                format!("Impossibile leggere il file: {error}")
            } else {
                format!("Unable to read the file: {error}")
            }
        })?,
    );
    let credentials = transfer::decode(&bytes).map_err(|error| error.to_string())?;
    let report = {
        let mut slot = state.vault.borrow_mut();
        let vault = slot
            .as_mut()
            .ok_or_else(|| tr("The vault is locked.", "Il vault è bloccato.").to_string())?;
        vault
            .import_credentials(credentials)
            .map_err(|error| error.to_string())?
    };
    refresh_credentials(state);
    toast(
        state,
        &if is_italian() {
            format!(
                "Importate {} credenziali, {} già presenti",
                report.added, report.skipped
            )
        } else {
            format!(
                "Imported {} {}, {} already present",
                report.added,
                if report.added == 1 {
                    "credential"
                } else {
                    "credentials"
                },
                report.skipped
            )
        },
    );
    show_save_warning(state, report.outcome);
    Ok(())
}
