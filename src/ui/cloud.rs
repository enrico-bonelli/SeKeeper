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

fn dialog_open(state: &SharedState) -> bool {
    state
        .dialogs
        .borrow()
        .iter()
        .filter_map(glib::WeakRef::upgrade)
        .any(|window| window.is_visible())
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
    if dialog_open(state) {
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
        move || -> s3::Result<_> { sync::download(&mut s3::S3::connect(config)?, &cache) },
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
                let may_install = !dialog_open(&state);
                let step = {
                    let mut slot = state.vault.borrow_mut();
                    sync::reconcile(
                        slot.as_mut().unwrap(),
                        &state.vault_path,
                        remote,
                        &revision,
                        may_install,
                    )
                };
                if step.catalog.is_some() {
                    *state.cloud.catalog.borrow_mut() = step.catalog;
                }
                if let Some(outcome) = step.installed {
                    refresh_credentials(&state);
                    show_save_warning(&state, outcome);
                }
                match step.next {
                    Ok(sync::Next::LocalChanged) => {
                        state.cloud.pending.set(true);
                        finish(&state, "S3: nuove modifiche locali da sincronizzare");
                    }
                    Ok(sync::Next::UpdateDeferred) => finish(
                        &state,
                        "S3: nuova versione disponibile; chiudi l'editor per aggiornarla",
                    ),
                    Ok(sync::Next::RetentionPending) => finish(
                        &state,
                        "S3: riepilogo locale salvato; conservazione non confermata, pulizia rimandata",
                    ),
                    Ok(sync::Next::Publish {
                        publication,
                        conflict,
                    }) => publish(&state, publication, generation, conflict),
                    Err(error) => finish(&state, &format!("S3: {error}")),
                }
            }
        ),
    );
}

pub fn resume_if_pending(state: &SharedState) {
    if state.cloud.pending.get() && !state.cloud.busy.get() {
        request_sync(state);
    }
}

fn publish(state: &SharedState, publication: sync::Publication, generation: u64, conflict: bool) {
    let config = state.cloud.config.borrow().clone().expect("S3 configurato");
    let cache = config.cache(&state.vault_path);
    let vault_path = state.vault_path.clone();
    state
        .cloud
        .status
        .set_text("S3: invio delle copie cifrate…");
    job(
        move || -> s3::Result<_> {
            publication.send(&mut s3::S3::connect(config)?, &cache, &vault_path)?;
            Ok(publication)
        },
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
                    Ok(_) if conflict => {
                        "S3: conflitto tra versioni. Apri S3 → Versioni e conflitti per scegliere."
                            .into()
                    }
                    Ok(publication) => {
                        if let Some(catalog) = state.cloud.catalog.borrow_mut().as_mut() {
                            catalog.retained(&publication);
                        }
                        "S3: copie sincronizzate; fino a 30 versioni complete conservate".into()
                    }
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
    if state.cloud.busy.get() || state.unlocking.get() {
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
    let retention = Label::new(Some(
        "Conservazione: ultime 30 copie complete. I conflitti aperti sospendono la pulizia. Serve il permesso IAM s3:DeleteObject sul prefisso del vault; aggiorna Sekeeper su tutti i PC.",
    ));
    retention.set_wrap(true);
    retention.set_max_width_chars(60);
    content.append(&retention);
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
            let retired = credentials.as_ref().map(|_| config.clone()).filter(|_| had_config);
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
                    let outcome = stored.save(&path)?;
                    // Best effort: the new keys already work either way.
                    if let Some(retired) = retired {
                        let _ = s3::Credentials::clear(&retired);
                    }
                    Ok::<_, s3::CloudError>(outcome)
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
                                    message.set_text(&error);
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
    format!(
        "{} — {} — {}{}",
        format_timestamp(Some(snapshot.revision.created_at)),
        snapshot.revision.device,
        &snapshot.id[..8],
        if head { " (corrente)" } else { "" }
    )
}

fn names(entries: &[String]) -> String {
    const SHOWN: usize = 5;
    let mut text = entries
        .iter()
        .take(SHOWN)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    if entries.len() > SHOWN {
        text.push_str(&format!(" e altre {}", entries.len() - SHOWN));
    }
    text
}

fn describe_difference(state: &SharedState, snapshot: &sync::Snapshot) -> String {
    let slot = state.vault.borrow();
    let Some(vault) = slot.as_ref() else {
        return String::new();
    };
    let Ok(other) = vault.read_credentials(&snapshot.bytes) else {
        return "Impossibile confrontare questa versione.".into();
    };
    let diff = sync::compare(&vault.data.credentials, &other);
    let mut lines = Vec::new();
    if !diff.added.is_empty() {
        lines.push(format!("Recupererai: {}", names(&diff.added)));
    }
    if !diff.removed.is_empty() {
        lines.push(format!("Perderai: {}", names(&diff.removed)));
    }
    if !diff.changed.is_empty() {
        lines.push(format!("Cambieranno: {}", names(&diff.changed)));
    }
    if lines.is_empty() {
        return "Stesse credenziali del vault attuale.".into();
    }
    lines.join("\n")
}

fn show_versions(state: &SharedState) {
    if state.cloud.busy.get() {
        show_error(
            &state.window,
            "Attendi il completamento dell'operazione S3 in corso.",
        );
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
        "La versione scelta sostituirà le credenziali locali. Dopo la risoluzione riprenderà la conservazione delle ultime 30 copie complete. Le modifiche delle versioni divergenti non vengono unite automaticamente.",
    ));
    explanation.set_wrap(true);
    explanation.set_max_width_chars(65);
    content.append(&explanation);
    let selection =
        gtk4::DropDown::from_strings(&labels.iter().map(String::as_str).collect::<Vec<_>>());
    content.append(&selection);
    let difference = Label::new(None);
    difference.set_wrap(true);
    difference.set_halign(Align::Start);
    difference.set_xalign(0.0);
    difference.set_max_width_chars(65);
    content.append(&difference);
    let message = Label::new(None);
    message.set_wrap(true);
    content.append(&message);
    let copies = Rc::new(copies);
    if let Some(first) = copies.first() {
        difference.set_text(&describe_difference(state, first));
    }
    selection.connect_selected_notify(clone!(
        #[weak]
        state,
        #[strong]
        copies,
        #[weak]
        difference,
        move |selection| {
            if let Some(selected) = copies.get(selection.selected() as usize) {
                difference.set_text(&describe_difference(&state, selected));
            }
        }
    ));
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
                move || -> s3::Result<_> { sync::download(&mut s3::S3::connect(config)?, &cache) },
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
                            Err(error) => message.set_text(&error),
                        }
                    }
                ),
            );
        }
    ));
    dialog.present();
    password.grab_focus();
}

enum Recovered {
    Choose(sync::Catalog),
    Done(Vault, SaveOutcome),
}

fn recover_downloaded(
    state: &SharedState,
    dialog: &Dialog,
    password: &PasswordEntry,
    selection: &gtk4::DropDown,
    message: &Label,
    copies: &[(String, Vec<u8>)],
) {
    let secret = Zeroizing::new(password.text().to_string());
    let keyfile = match state.keyfile.load() {
        Ok(keyfile) => keyfile,
        Err(error) => {
            message.set_text(&error.to_string());
            return;
        }
    };
    let copies = copies.to_vec();
    let path = state.vault_path.clone();
    let choice = selection
        .is_visible()
        .then(|| selection.selected() as usize);
    dialog.set_sensitive(false);
    message.set_text("Verifica delle copie cifrate…");
    state.cloud.busy.set(true);
    job(
        move || -> s3::Result<Recovered> {
            let catalog = sync::inspect_recovery(&path, copies, &secret, &keyfile)?;
            let head = match choice {
                _ if catalog.heads.len() == 1 => 0,
                Some(head) => head,
                None => return Ok(Recovered::Choose(catalog)),
            };
            let (vault, outcome) =
                sync::complete_recovery(&path, catalog, head, &secret, &keyfile)?;
            Ok(Recovered::Done(vault, outcome))
        },
        clone!(
            #[weak]
            state,
            #[weak]
            dialog,
            #[weak]
            password,
            #[weak]
            selection,
            #[weak]
            message,
            move |result| {
                state.cloud.busy.set(false);
                dialog.set_sensitive(true);
                match result {
                    Ok(Recovered::Choose(catalog)) => {
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
                        message.set_text(
                            "Ci sono versioni divergenti. Scegli quella da recuperare e premi di nuovo Recupera. Dopo la risoluzione riprenderà la conservazione delle ultime 30 copie complete.",
                        );
                    }
                    Ok(Recovered::Done(vault, outcome)) => {
                        password.set_text("");
                        dialog.close();
                        *state.vault.borrow_mut() = Some(vault);
                        keyfiles::remember(&state);
                        show_vault(&state);
                        show_save_warning(&state, outcome);
                    }
                    Err(error) => message.set_text(&error),
                }
            }
        ),
    );
}
