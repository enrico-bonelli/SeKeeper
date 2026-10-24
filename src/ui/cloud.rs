use super::*;
use crate::{s3, sync};
use std::path::Path;
use std::time::Duration;

pub struct State {
    pub status: Label,
    config: RefCell<Option<s3::Config>>,
    busy: Cell<bool>,
    pending: Cell<bool>,
    generation: Cell<u64>,
    last_attempt: Cell<Instant>,
    catalog: RefCell<Option<sync::Catalog>>,
}
impl State {
    pub fn new(path: &Path) -> Self {
        let status = Label::new(None);
        status.set_wrap(true);
        status.set_margin_bottom(8);
        status.set_margin_start(12);
        status.set_margin_end(12);
        status.add_css_class("dim-label");
        let config = match s3::Config::load(path) {
            Ok(config) => {
                status.set_text(if config.as_ref().is_some_and(|c| c.enabled) {
                    "S3: pronto"
                } else {
                    "S3: disattivato"
                });
                config
            }
            Err(error) => {
                status.set_text(&error.to_string());
                None
            }
        };
        Self {
            status,
            config: RefCell::new(config),
            busy: Cell::new(false),
            pending: Cell::new(false),
            generation: Cell::new(0),
            last_attempt: Cell::new(Instant::now()),
            catalog: RefCell::new(None),
        }
    }
    pub fn enabled(&self) -> bool {
        self.config.borrow().as_ref().is_some_and(|c| c.enabled)
    }
}

fn job<T: Send + 'static>(
    work: impl FnOnce() -> s3::Result<T> + Send + 'static,
    done: impl FnOnce(s3::Result<T>) + 'static,
) {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = sender.send(work());
    });
    glib::spawn_future_local(async move {
        loop {
            match receiver.try_recv() {
                Ok(result) => {
                    done(result);
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    done(Err(s3::error("operazione S3 interrotta")));
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    glib::timeout_future(Duration::from_millis(100)).await
                }
            }
        }
    });
}

pub fn locked(state: &SharedState) {
    state
        .cloud
        .generation
        .set(state.cloud.generation.get().wrapping_add(1));
    state.cloud.pending.set(false);
    *state.cloud.catalog.borrow_mut() = None;
}

pub fn tick(state: &SharedState) {
    if state.cloud.last_attempt.get().elapsed() >= Duration::from_secs(60) {
        request_sync(state);
    }
}

pub fn request_sync(state: &SharedState) {
    if state.closing.get() || !state.cloud.enabled() || state.vault.borrow().is_none() {
        return;
    }
    if state.cloud.busy.get() {
        state.cloud.pending.set(true);
        return;
    }
    // In particular, do not race a settings dialog with a transfer using the old
    // destination or credentials. Resume after the dialog has actually hidden.
    if state
        .dialogs
        .borrow()
        .iter()
        .filter_map(glib::WeakRef::upgrade)
        .any(|w| w.is_visible())
    {
        state.cloud.pending.set(true);
        state.cloud.last_attempt.set(Instant::now());
        return;
    }
    let config = state.cloud.config.borrow().clone().expect("S3 configurato");
    state.cloud.last_attempt.set(Instant::now());
    let preparation = state
        .vault
        .borrow_mut()
        .as_mut()
        .expect("vault aperto")
        .enable_sync(&config.device);
    match preparation {
        Ok(SaveOutcome::Durable) => {}
        Ok(_) => {
            state.cloud.status.set_text(
                "S3: copia locale salvata, ma conservazione dello storico non confermata; riprova",
            );
            return;
        }
        Err(error) => {
            state.cloud.status.set_text(&format!("S3: {error}"));
            return;
        }
    }
    state.cloud.busy.set(true);
    state.cloud.pending.set(false);
    state.cloud.last_attempt.set(Instant::now());
    state.cloud.status.set_text("S3: controllo delle copie…");
    let generation = state.cloud.generation.get();
    let revision = state
        .vault
        .borrow()
        .as_ref()
        .unwrap()
        .data
        .revision
        .as_ref()
        .unwrap()
        .id
        .clone();
    let cache = config.cache(&state.vault_path);
    job(
        move || sync::download(&mut s3::S3::connect(config)?, &cache),
        clone!(
            #[weak]
            state,
            move |result| {
                if state.closing.get() {
                    return;
                }
                if generation != state.cloud.generation.get() || state.vault.borrow().is_none() {
                    finish(&state, "S3: operazione terminata; vault bloccato");
                    return;
                }
                let remote = match result {
                    Ok(remote) => remote,
                    Err(error) => {
                        finish(&state, &format!("S3: {error}"));
                        return;
                    }
                };
                let current = state
                    .vault
                    .borrow()
                    .as_ref()
                    .unwrap()
                    .data
                    .revision
                    .as_ref()
                    .unwrap()
                    .id
                    .clone();
                if current != revision {
                    state.cloud.pending.set(true);
                    finish(&state, "S3: nuove modifiche locali da sincronizzare");
                    return;
                }
                let built = {
                    let slot = state.vault.borrow();
                    sync::Catalog::build(slot.as_ref().unwrap(), &state.vault_path, remote)
                };
                let catalog = match built {
                    Ok(catalog) => catalog,
                    Err(error) => {
                        finish(&state, &format!("S3: {error}"));
                        return;
                    }
                };
                let conflict = matches!(catalog.decision(&current), sync::Decision::Conflict);
                if let sync::Decision::FastForward(id) = catalog.decision(&current) {
                    let editor_open = state
                        .dialogs
                        .borrow()
                        .iter()
                        .filter_map(glib::WeakRef::upgrade)
                        .any(|w| w.is_visible());
                    if editor_open {
                        *state.cloud.catalog.borrow_mut() = Some(catalog);
                        finish(
                            &state,
                            "S3: nuova versione disponibile; chiudi l'editor per aggiornarla",
                        );
                        return;
                    }
                    let result = state
                        .vault
                        .borrow_mut()
                        .as_mut()
                        .unwrap()
                        .install_remote(catalog.snapshots[&id].bytes.clone());
                    match result {
                        Ok(outcome) => {
                            refresh_credentials(&state);
                            show_save_warning(&state, outcome);
                        }
                        Err(error) => {
                            finish(&state, &format!("S3: {error}"));
                            return;
                        }
                    }
                }
                let copies = catalog.copies();
                *state.cloud.catalog.borrow_mut() = Some(catalog);
                publish(&state, copies, generation, conflict);
            }
        ),
    );
}

pub fn resume_if_pending(state: &SharedState) {
    if state.cloud.pending.get() && !state.cloud.busy.get() {
        request_sync(state);
    }
}

fn publish(state: &SharedState, copies: Vec<(String, Vec<u8>)>, generation: u64, conflict: bool) {
    let config = state.cloud.config.borrow().clone().expect("S3 configurato");
    let cache = config.cache(&state.vault_path);
    state
        .cloud
        .status
        .set_text("S3: invio delle copie cifrate…");
    job(
        move || sync::publish(&mut s3::S3::connect(config)?, &cache, &copies),
        clone!(
            #[weak]
            state,
            move |result| {
                if state.closing.get() {
                    return;
                }
                if generation != state.cloud.generation.get() {
                    finish(&state, "S3: trasferimento terminato");
                    return;
                }
                let message = match result {
                    Err(error) => format!("S3: {error}"),
                    Ok(()) if conflict => {
                        "S3: conflitto tra versioni. Apri S3 → Versioni e conflitti per scegliere."
                            .into()
                    }
                    Ok(()) => "S3: copie sincronizzate".into(),
                };
                finish(&state, &message);
            }
        ),
    );
}

fn finish(state: &SharedState, message: &str) {
    state.cloud.busy.set(false);
    state.cloud.status.set_text(message);
    if state.cloud.pending.replace(false) && state.vault.borrow().is_some() {
        request_sync(state);
    }
}

pub fn show_settings(state: &SharedState) {
    if state.cloud.busy.get() {
        show_error(
            &state.window,
            "Attendi il completamento dell'operazione S3 in corso.",
        );
        return;
    }
    let config = state.cloud.config.borrow().clone().unwrap_or_default();
    let had_config = state.cloud.config.borrow().is_some();
    let dialog = Dialog::builder()
        .title("Copie cifrate su AWS S3")
        .transient_for(&state.window)
        .modal(true)
        .default_width(550)
        .build();
    track_dialog(state, &dialog);
    dialog.add_button("Annulla", ResponseType::Cancel);
    let save = dialog.add_button("Salva", ResponseType::Accept);
    let content = dialog.content_area();
    content.set_spacing(12);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);
    let enabled = gtk4::CheckButton::with_label("Abilita copie e sincronizzazione S3");
    enabled.set_active(config.enabled);
    content.append(&enabled);
    let explanation = Label::new(Some(
        "Usa lo stesso bucket e prefisso sui tuoi PC. Il vault resta disponibile senza rete. Le credenziali vengono conservate nel portachiavi di questo PC.",
    ));
    explanation.set_wrap(true);
    explanation.set_max_width_chars(60);
    content.append(&explanation);
    let grid = Grid::new();
    grid.set_row_spacing(10);
    grid.set_column_spacing(12);
    content.append(&grid);
    let bucket = Entry::new();
    bucket.set_text(&config.bucket);
    let region = Entry::new();
    region.set_text(&config.region);
    let prefix = Entry::new();
    prefix.set_text(&config.prefix);
    let device = Entry::new();
    device.set_text(&config.device);
    let access = Entry::new();
    access.set_placeholder_text(Some("Lascia vuoto per mantenere le chiavi salvate"));
    let secret = PasswordEntry::new();
    secret.set_show_peek_icon(true);
    let token = PasswordEntry::new();
    token.set_show_peek_icon(true);
    attach_field(&grid, 0, "Bucket", &bucket);
    attach_field(&grid, 1, "Regione", &region);
    attach_field(&grid, 2, "Prefisso", &prefix);
    attach_field(&grid, 3, "Nome PC", &device);
    attach_field(&grid, 4, "Access Key ID", &access);
    attach_field(&grid, 5, "Secret Access Key", &secret);
    attach_field(&grid, 6, "Session token (opzionale)", &token);
    if state.vault.borrow().is_some() && state.cloud.enabled() {
        let now = Button::with_label("Sincronizza ora");
        content.append(&now);
        now.connect_clicked(clone!(
            #[weak]
            state,
            #[weak]
            dialog,
            move |_| {
                dialog.close();
                request_sync(&state);
            }
        ));
        let versions = Button::with_label("Versioni e conflitti…");
        content.append(&versions);
        versions.connect_clicked(clone!(
            #[weak]
            state,
            #[weak]
            dialog,
            move |_| {
                dialog.close();
                show_versions(&state);
            }
        ));
    }
    let message = Label::new(None);
    message.set_wrap(true);
    message.add_css_class("error");
    content.append(&message);
    dialog.connect_response(clone!(
        #[weak]
        state,
        #[weak]
        save,
        #[weak]
        enabled,
        #[weak]
        bucket,
        #[weak]
        region,
        #[weak]
        prefix,
        #[weak]
        device,
        #[weak]
        access,
        #[weak]
        secret,
        #[weak]
        token,
        #[weak]
        message,
        move |dialog, response| {
            if response != ResponseType::Accept {
                dialog.close();
                return;
            }
            let mut next = config.clone();
            next.enabled = enabled.is_active();
            next.bucket = bucket.text().trim().into();
            next.region = region.text().trim().into();
            next.prefix = prefix.text().trim().into();
            if !next.prefix.ends_with('/') {
                next.prefix.push('/');
            }
            next.device = device.text().trim().into();
            if let Err(error) = next.validate() {
                message.set_text(&error.to_string());
                return;
            }
            let credentials = if access.text().is_empty()
                && secret.text().is_empty()
                && token.text().is_empty()
            {
                if next.enabled && !had_config {
                    message.set_text("Inserisci le credenziali AWS del dispositivo.");
                    return;
                }
                None
            } else {
                let value = s3::Credentials {
                    access_key: access.text().to_string(),
                    secret_key: secret.text().to_string(),
                    session_token: token.text().to_string(),
                };
                if let Err(error) = value.validate() {
                    message.set_text(&error.to_string());
                    return;
                }
                Some(value)
            };
            // Keep the active keyring item intact until the new configuration
            // has actually committed. A failed save cannot rotate active keys.
            if credentials.is_some() { next.credential_id = format!("{:032x}", rand::random::<u128>()); }
            access.set_text("");
            secret.set_text("");
            token.set_text("");
            save.set_sensitive(false);
            let path = state.vault_path.clone();
            let stored = next.clone();
            state.cloud.busy.set(true);
            let weak_dialog = dialog.downgrade();
            let weak_message = message.downgrade();
            let weak_save = save.downgrade();
            job(
                move || {
                    if let Some(credentials) = credentials {
                        credentials.store(&stored)?;
                    }
                    stored.save(&path)
                },
                clone!(
                    #[weak]
                    state,
                    move |result| {
                        state.cloud.busy.set(false);
                        if state.closing.get() {
                            return;
                        }
                        match result {
                            Ok(outcome) => {
                                *state.cloud.config.borrow_mut() = Some(next);
                                *state.cloud.catalog.borrow_mut() = None;
                                state
                                    .cloud
                                    .generation
                                    .set(state.cloud.generation.get().wrapping_add(1));
                                if let Some(dialog) = weak_dialog.upgrade() {
                                    dialog.close();
                                }
                                state.cloud.status.set_text(if state.cloud.enabled() {
                                    "S3: pronto"
                                } else {
                                    "S3: disattivato"
                                });
                                if state.vault.borrow().is_some() {
                                    request_sync(&state);
                                } else {
                                    show_authentication(&state);
                                }
                                if !matches!(outcome, SaveOutcome::Durable) {
                                    show_error(&state.window, "Le impostazioni S3 sono state aggiornate, ma la loro conservazione in caso di spegnimento improvviso non è confermata.");
                                }
                            }
                            Err(error) => {
                                if let Some(message) = weak_message.upgrade() {
                                    message.set_text(&error.to_string());
                                }
                                if let Some(save) = weak_save.upgrade() {
                                    save.set_sensitive(true);
                                }
                                state.cloud.status.set_text(&format!("S3: {error}"));
                            }
                        }
                    }
                ),
            );
        }
    ));
    dialog.present();
}

fn describe(snapshot: &sync::Snapshot, head: bool) -> String {
    let date = i64::try_from(snapshot.revision.created_at)
        .ok()
        .and_then(|v| glib::DateTime::from_unix_local(v).ok())
        .and_then(|d| d.format("%d/%m/%Y %H:%M:%S").ok())
        .map(|d| d.to_string())
        .unwrap_or_else(|| "data non disponibile".into());
    format!(
        "{date} — {} — {}{}",
        snapshot.revision.device,
        &snapshot.id[..8],
        if head { " (corrente)" } else { "" }
    )
}

fn show_versions(state: &SharedState) {
    if state.cloud.busy.get() {
        return;
    }
    let catalog = state.cloud.catalog.borrow();
    let Some(catalog) = catalog.as_ref() else {
        show_error(
            &state.window,
            "Sincronizza prima per leggere le versioni S3 disponibili.",
        );
        return;
    };
    let mut copies: Vec<sync::Snapshot> = catalog.snapshots.values().cloned().collect();
    copies.sort_by_key(|s| {
        (
            std::cmp::Reverse(catalog.heads.contains(&s.id)),
            std::cmp::Reverse(s.revision.created_at),
        )
    });
    let heads = catalog.heads.clone();
    let labels: Vec<String> = copies
        .iter()
        .map(|s| describe(s, heads.contains(&s.id)))
        .collect();
    let dialog = Dialog::builder()
        .title("Versioni e conflitti S3")
        .transient_for(&state.window)
        .modal(true)
        .build();
    track_dialog(state, &dialog);
    dialog.add_button("Annulla", ResponseType::Cancel);
    dialog.add_button("Usa questa versione", ResponseType::Accept);
    let content = dialog.content_area();
    content.set_spacing(12);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);
    let explanation = Label::new(Some(
        "La versione scelta sostituirà le credenziali locali. Le altre copie cifrate resteranno conservate. Le modifiche delle versioni divergenti non vengono unite automaticamente.",
    ));
    explanation.set_wrap(true);
    explanation.set_max_width_chars(65);
    content.append(&explanation);
    let selection =
        gtk4::DropDown::from_strings(&labels.iter().map(String::as_str).collect::<Vec<_>>());
    content.append(&selection);
    let message = Label::new(None);
    message.set_wrap(true);
    content.append(&message);
    dialog.connect_response(clone!(
        #[weak]
        state,
        #[weak]
        selection,
        #[weak]
        message,
        move |dialog, response| {
            if response != ResponseType::Accept {
                dialog.close();
                return;
            }
            let Some(selected) = copies.get(selection.selected() as usize) else {
                return;
            };
            let result = {
                let mut slot = state.vault.borrow_mut();
                let Some(vault) = slot.as_mut() else {
                    dialog.close();
                    return;
                };
                vault.resolve_remote(&selected.bytes, heads.clone())
            };
            match result {
                Ok(outcome) => {
                    dialog.close();
                    refresh_credentials(&state);
                    show_save_warning(&state, outcome);
                }
                Err(error) => message.set_text(&error.to_string()),
            }
        }
    ));
    dialog.present();
}

pub fn show_recover(state: &SharedState, initial: &PasswordEntry) {
    if state.cloud.busy.get() {
        show_error(
            &state.window,
            "Attendi il completamento dell'operazione S3 in corso.",
        );
        return;
    }
    if state.vault_path.exists() || !Vault::available_backups(&state.vault_path).is_empty() {
        show_error(
            &state.window,
            "Sblocca il vault locale per sincronizzarlo. Se è danneggiato, conserva una copia dei file locali prima di recuperare da S3.",
        );
        return;
    }
    let Some(config) = state.cloud.config.borrow().clone().filter(|c| c.enabled) else {
        return;
    };
    let dialog = Dialog::builder()
        .title("Recupera il vault da S3")
        .transient_for(&state.window)
        .modal(true)
        .build();
    track_dialog(state, &dialog);
    dialog.add_button("Annulla", ResponseType::Cancel);
    let recover = dialog.add_button("Recupera", ResponseType::Accept);
    let content = dialog.content_area();
    content.set_spacing(12);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);
    let password = PasswordEntry::new();
    password.set_show_peek_icon(true);
    password.set_placeholder_text(Some("Password principale del vault"));
    let initial_secret = Zeroizing::new(initial.text().to_string());
    initial.set_text("");
    password.set_text(&initial_secret);
    content.append(&password);
    content.append(&keyfiles::controls(state, &dialog, false));
    let selection = gtk4::DropDown::from_strings(&[]);
    selection.set_visible(false);
    content.append(&selection);
    let message = Label::new(Some(
        "Verranno scaricate e verificate le copie cifrate del bucket configurato.",
    ));
    message.set_wrap(true);
    message.set_max_width_chars(60);
    content.append(&message);
    let downloaded = Rc::new(RefCell::new(None::<Vec<(String, Vec<u8>)>>));
    dialog.connect_response(clone!(
        #[weak]
        state,
        #[weak]
        password,
        #[weak]
        recover,
        #[weak]
        message,
        #[weak]
        selection,
        move |dialog, response| {
            if response != ResponseType::Accept {
                dialog.close();
                return;
            }
            if password.text().is_empty() {
                message.set_text("Inserisci la password principale del vault.");
                return;
            }
            if let Err(error) = state.keyfile.load() {
                message.set_text(&error.to_string());
                return;
            }
            if let Some(copies) = downloaded.borrow().as_ref() {
                recover_downloaded(&state, dialog, &password, &selection, &message, copies);
                return;
            }
            let cache = config.cache(&state.vault_path);
            let config = config.clone();
            recover.set_sensitive(false);
            state.cloud.busy.set(true);
            message.set_text("Scaricamento delle copie cifrate…");
            let weak_dialog = dialog.downgrade();
            let downloaded = downloaded.clone();
            job(
                move || sync::download(&mut s3::S3::connect(config)?, &cache),
                clone!(
                    #[weak]
                    state,
                    #[weak_allow_none]
                    password,
                    #[weak_allow_none]
                    recover,
                    #[weak_allow_none]
                    message,
                    #[weak_allow_none]
                    selection,
                    move |result| {
                        state.cloud.busy.set(false);
                        if state.closing.get() {
                            return;
                        }
                        let (Some(password), Some(recover), Some(message), Some(selection)) =
                            (password, recover, message, selection)
                        else {
                            return;
                        };
                        let Some(dialog) = weak_dialog.upgrade().filter(|d| d.is_visible()) else {
                            return;
                        };
                        recover.set_sensitive(true);
                        match result {
                            Ok(copies) => {
                                *downloaded.borrow_mut() = Some(copies);
                                recover_downloaded(
                                    &state,
                                    &dialog,
                                    &password,
                                    &selection,
                                    &message,
                                    downloaded.borrow().as_ref().unwrap(),
                                );
                            }
                            Err(error) => message.set_text(&error.to_string()),
                        }
                    }
                ),
            );
        }
    ));
    dialog.present();
    password.grab_focus();
}

fn recover_downloaded(
    state: &SharedState,
    dialog: &Dialog,
    password: &PasswordEntry,
    selection: &gtk4::DropDown,
    message: &Label,
    copies: &[(String, Vec<u8>)],
) {
    let result = (|| -> s3::Result<(Vault, SaveOutcome)> {
        let first = copies
            .first()
            .ok_or_else(|| s3::error("nessuna copia S3 disponibile"))?;
        let secret = Zeroizing::new(password.text().to_string());
        let keyfile = state.keyfile.load()?;
        let seed = Vault::from_encrypted(&state.vault_path, first.1.clone(), &secret, &keyfile)?;
        let catalog = sync::Catalog::build(&seed, &state.vault_path, copies.to_vec())?;
        if catalog.heads.len() > 1 && !selection.is_visible() {
            let labels: Vec<String> = catalog
                .heads
                .iter()
                .map(|id| describe(&catalog.snapshots[id], true))
                .collect();
            selection.set_model(Some(&gtk4::StringList::new(
                &labels.iter().map(String::as_str).collect::<Vec<_>>(),
            )));
            selection.set_selected(0);
            selection.set_visible(true);
            return Err(s3::error(
                "Ci sono versioni divergenti. Scegli quella da recuperare e premi di nuovo Recupera. Le altre copie resteranno in S3.",
            ));
        }
        let index = if catalog.heads.len() == 1 {
            0
        } else {
            selection.selected() as usize
        };
        let id = catalog
            .heads
            .get(index)
            .ok_or_else(|| s3::error("scegli una versione"))?;
        let snapshot = &catalog.snapshots[id];
        let mut vault =
            Vault::from_encrypted(&state.vault_path, snapshot.bytes.clone(), &secret, &keyfile)?;
        let outcome = if catalog.heads.len() > 1 {
            vault.recover_resolving(catalog.heads)?
        } else {
            vault.recover_remote()?
        };
        Ok((vault, outcome))
    })();
    match result {
        Ok((vault, outcome)) => {
            password.set_text("");
            dialog.close();
            *state.vault.borrow_mut() = Some(vault);
            show_vault(state);
            show_save_warning(state, outcome);
        }
        Err(error) => message.set_text(&error.to_string()),
    }
}
