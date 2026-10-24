use super::*;
use crate::keyfile::KeyFile;
use gtk4::{FileChooserAction, FileChooserDialog};
use std::io;

#[derive(Default)]
pub struct State {
    path: RefCell<Option<PathBuf>>,
    labels: RefCell<Vec<glib::WeakRef<Label>>>,
}

impl State {
    pub fn load(&self) -> io::Result<KeyFile> {
        let path = self.path.borrow();
        let path = path
            .as_ref()
            .ok_or_else(|| io::Error::other("Seleziona il file chiave del vault."))?;
        KeyFile::read(path).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("Impossibile leggere il file chiave: {error}"),
            )
        })
    }

    pub fn select(&self, path: PathBuf) {
        *self.path.borrow_mut() = Some(path);
        self.labels.borrow_mut().retain(|weak| {
            if let Some(label) = weak.upgrade() {
                self.update_label(&label);
                true
            } else {
                false
            }
        });
    }

    fn update_label(&self, label: &Label) {
        let path = self.path.borrow();
        label.set_text(
            &path
                .as_ref()
                .and_then(|path| path.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Nessun file chiave selezionato".into()),
        );
        label.set_tooltip_text(path.as_ref().and_then(|path| path.to_str()));
    }
}

pub fn controls(state: &SharedState, parent: &impl IsA<gtk4::Window>, creating: bool) -> Box {
    let content = Box::new(Orientation::Vertical, 6);
    let buttons = Box::new(Orientation::Horizontal, 8);
    let select = Button::with_label("Scegli file chiave…");
    buttons.append(&select);
    content.append(&buttons);
    let name = Label::new(None);
    name.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
    name.set_max_width_chars(42);
    name.add_css_class("dim-label");
    state.keyfile.update_label(&name);
    {
        let mut labels = state.keyfile.labels.borrow_mut();
        labels.retain(|label| label.upgrade().is_some());
        labels.push(name.downgrade());
    }
    content.append(&name);
    let parent = parent.as_ref();
    select.connect_clicked(clone!(
        #[weak]
        state,
        #[weak]
        parent,
        move |_| choose(&state, &parent, false)
    ));
    if creating {
        let generate = Button::with_label("Genera file chiave…");
        buttons.append(&generate);
        generate.connect_clicked(clone!(
            #[weak]
            state,
            #[weak]
            parent,
            move |_| choose(&state, &parent, true)
        ));
    }
    content
}

fn choose(state: &SharedState, parent: &gtk4::Window, generate: bool) {
    // Keep this dialog in-process: a portal failure must not turn the key-file
    // buttons into silent no-ops. GTK owns toplevel windows until destruction.
    let chooser = FileChooserDialog::new(
        Some(if generate {
            "Genera un nuovo file chiave"
        } else {
            "Seleziona il file chiave del vault"
        }),
        Some(parent),
        if generate {
            FileChooserAction::Save
        } else {
            FileChooserAction::Open
        },
        &[
            ("Annulla", ResponseType::Cancel),
            (
                if generate { "Genera" } else { "Seleziona" },
                ResponseType::Accept,
            ),
        ],
    );
    track_dialog(state, &chooser);
    chooser.set_modal(true);
    chooser.set_destroy_with_parent(true);
    chooser.set_default_size(760, 520);
    chooser.set_default_response(ResponseType::Accept);
    if generate {
        chooser.set_current_name("sekeeper.key");
    }
    chooser.connect_response(clone!(
        #[weak] state,
        move |chooser, response| {
            if response == ResponseType::Accept {
                let result = (|| -> io::Result<(PathBuf, Option<SaveOutcome>)> {
                    let path = chooser.file().and_then(|file| file.path()).ok_or_else(|| io::Error::other("Seleziona un file locale; monta prima il supporto che lo contiene."))?;
                    if generate && state.vault_path.parent().is_some_and(|parent| path.starts_with(parent)) {
                        return Err(io::Error::other("Salva il file chiave fuori dalla cartella del vault, per conservarlo separatamente."));
                    }
                    let outcome = if generate { Some(KeyFile::generate(&path)?) } else { None };
                    KeyFile::read(&path)?;
                    Ok((path, outcome))
                })();
                match result {
                    Ok((path, outcome)) => {
                        state.keyfile.select(path);
                        if let Some(SaveOutcome::DurabilityUncertain(error)) = outcome {
                            show_error(&state.window, &format!("Il file chiave è stato creato, ma la conservazione sul supporto non è confermata. Verifica e conserva una copia prima di usarlo.\n\n{error}"));
                        }
                    }
                    Err(error) => {
                        show_error(chooser, &error.to_string());
                        return;
                    }
                }
            }
            chooser.destroy();
        }
    ));
    chooser.present();
}
