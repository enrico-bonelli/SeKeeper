use super::*;
use crate::security::IDLE_TIMEOUT;

fn widgets(widget: &gtk4::Widget) -> Vec<gtk4::Widget> {
    let mut result = vec![widget.clone()];
    let mut child = widget.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        result.extend(widgets(&widget));
    }
    result
}

fn passwords(widget: &gtk4::Widget) -> Vec<PasswordEntry> {
    widgets(widget)
        .into_iter()
        .filter_map(|widget| widget.downcast::<PasswordEntry>().ok())
        .collect()
}

fn auth_button(state: &SharedState, label: &str) -> Button {
    widgets(state.auth.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<Button>().ok())
        .find(|button| button.label().as_deref() == Some(label))
        .unwrap()
}

/// Unlocking and restoring run on a worker: let the main loop deliver the result.
fn wait_until(condition: impl Fn() -> bool) {
    let context = glib::MainContext::default();
    let deadline = Instant::now() + std::time::Duration::from_secs(60);
    while !condition() {
        assert!(Instant::now() < deadline, "operazione non completata");
        context.iteration(false);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn unlock(state: &SharedState, label: &str) {
    auth_button(state, label).emit_clicked();
    wait_until(|| !state.unlocking.get());
}

#[test]
#[ignore = "richiede un display GTK; eseguire con xvfb-run -a cargo test ui::tests -- --ignored"]
fn unlock_existing_vault_idle_lock_editor_cleanup_and_backup_restore() {
    gtk4::init().expect("display GTK necessario");
    crate::register_resources();
    let context = glib::MainContext::default();
    let _guard = context.acquire().unwrap();
    let application = Application::builder()
        .application_id("io.github.sekeeper.Tests")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    application.register(None::<&gio::Cancellable>).unwrap();
    let directory = std::env::temp_dir().join(format!(
        "sekeeper-ui-test-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("vault");
    let outcome =
        crate::storage::write_atomic(&path, include_bytes!("../../tests/fixtures/vault-v3"), true)
            .unwrap();
    assert!(matches!(outcome, SaveOutcome::Durable));
    let state = build_with_path(&application, Ok(path.clone())).unwrap();
    assert!(
        gtk4::IconTheme::for_display(&gdk::Display::default().unwrap()).has_icon(crate::APP_ID)
    );

    let key_path = directory.join("public-test.key");
    assert!(matches!(
        crate::storage::create_keyfile(
            &key_path,
            include_bytes!("../../tests/fixtures/public-test.key")
        )
        .unwrap(),
        SaveOutcome::Durable
    ));
    // Missing key files must never allow password-only unlock.
    passwords(state.auth.upcast_ref())[0].set_text("public fixture master password");
    auth_button(&state, "Sblocca").emit_clicked();
    assert!(state.vault.borrow().is_none());
    state.keyfile.select(key_path);

    // The confirmation widget is not parented when unlocking an existing vault.
    assert_eq!(passwords(state.auth.upcast_ref()).len(), 1);
    passwords(state.auth.upcast_ref())[0].set_text("public fixture master password");
    auth_button(&state, "Sblocca").emit_clicked();
    // The window keeps running during the KDF, with the form disabled.
    assert!(state.unlocking.get() && !state.auth.is_sensitive());
    wait_until(|| !state.unlocking.get());
    assert!(state.vault.borrow().is_some() && state.auth.is_sensitive());
    assert_eq!(state.stack.visible_child_name().as_deref(), Some("vault"));
    for description in ["Posta lavoro", "Banca Èlite"] {
        let mut entry = Credential::default();
        entry.description = description.into();
        entry.username = "demo".into();
        entry.password = "public-test-secret".into();
        let outcome = state
            .vault
            .borrow_mut()
            .as_mut()
            .unwrap()
            .add_credential(entry)
            .unwrap();
        assert!(matches!(outcome, SaveOutcome::Durable));
    }
    refresh_credentials(&state);
    state.search.set_text("banca èLITE");
    state.search.emit_by_name::<()>("search-changed", &[]);
    assert_eq!(state.credential_ids.borrow().len(), 1);
    assert_eq!(state.counter.text().as_str(), "1 di 3 credenziali");
    state
        .credentials
        .select_row(state.credentials.row_at_index(0).as_ref());
    let credential = selected_credential(&state).unwrap();
    assert_eq!(credential.description, "Banca Èlite");
    assert!(credential.created_at.is_some());
    assert!(credential.updated_at.is_some());
    let id = credential.id.clone();

    // The details pane shows the selection, keeping the password out of labels.
    let details = &state.details;
    assert_eq!(
        details.root.visible_child_name().as_deref(),
        Some("credential")
    );
    let label = widgets(details.root.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<Label>().ok())
        .find(|label| label.has_css_class("field-secret"))
        .unwrap();
    assert!(!label.text().contains("public-test-secret"));
    let toggle = widgets(details.root.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk4::ToggleButton>().ok())
        .unwrap();
    toggle.set_active(true);
    assert_eq!(label.text().as_str(), "public-test-secret");
    toggle.set_active(false);
    assert!(!label.text().contains("public-test-secret"));

    // Copies stay in the clipboard only until the timeout or the next lock.
    clipboard::copy(&state, "public-test-secret", "Password");
    let system = WidgetExt::display(&state.window).clipboard();
    assert!(system.is_local());
    assert!(state.toast.reveals_child());
    assert!(
        system
            .formats()
            .contain_mime_type("x-kde-passwordManagerHint")
    );
    assert!(
        system
            .formats()
            .contain_mime_type("text/plain;charset=utf-8")
    );
    clipboard::clear(&state);
    assert!(system.formats().mime_types().is_empty());

    show_editor(&state, Some(credential));
    let editor = state
        .dialogs
        .borrow()
        .last()
        .unwrap()
        .upgrade()
        .unwrap()
        .downcast::<Dialog>()
        .unwrap();
    let entries: Vec<_> = widgets(editor.upcast_ref())
        .into_iter()
        .filter_map(|w| w.downcast::<Entry>().ok())
        .collect();
    entries[1].set_text("username modificato");
    editor.response(ResponseType::Accept);
    assert_eq!(selected_credential(&state).unwrap().id, id);
    assert_eq!(
        state.vault.borrow().as_ref().unwrap().data.credentials[0].username,
        "demo"
    );
    assert_eq!(
        state.vault.borrow().as_ref().unwrap().data.credentials[2].username,
        "username modificato"
    );
    let credential = selected_credential(&state).unwrap();
    show_editor(&state, Some(credential));
    let editor = state.dialogs.borrow().last().unwrap().upgrade().unwrap();
    let secret = passwords(editor.upcast_ref())[0].clone();
    assert_eq!(secret.text().as_str(), "public-test-secret");
    clipboard::copy(&state, "public-test-secret", "Password");
    let system = WidgetExt::display(&state.window).clipboard();
    state.idle.borrow_mut().touch(Instant::now() - IDLE_TIMEOUT);
    check_idle(&state);
    assert!(state.vault.borrow().is_none());
    assert_eq!(secret.text().as_str(), "");
    assert!(!editor.is_visible());
    assert!(state.credentials.row_at_index(0).is_none());
    assert!(state.credential_ids.borrow().is_empty());
    assert!(state.search.text().is_empty());
    assert!(system.formats().mime_types().is_empty());
    assert_eq!(
        state.details.root.visible_child_name().as_deref(),
        Some("empty")
    );

    passwords(state.auth.upcast_ref())[0].set_text("public fixture master password");
    unlock(&state, "Sblocca");
    state.search.set_text("banca");
    state.search.emit_by_name::<()>("search-changed", &[]);
    state
        .credentials
        .select_row(state.credentials.row_at_index(0).as_ref());
    confirm_delete(&state);
    let deletion = state
        .dialogs
        .borrow()
        .last()
        .unwrap()
        .upgrade()
        .unwrap()
        .downcast::<MessageDialog>()
        .unwrap();
    deletion.response(ResponseType::Accept);
    assert_eq!(
        state
            .vault
            .borrow()
            .as_ref()
            .unwrap()
            .data
            .credentials
            .len(),
        2
    );
    assert!(
        state
            .vault
            .borrow()
            .as_ref()
            .unwrap()
            .data
            .credentials
            .iter()
            .all(|credential| credential.id != id)
    );
    assert!(state.credentials.row_at_index(0).is_none());
    assert_eq!(state.counter.text().as_str(), "0 di 2 credenziali");
    state.search.emit_stop_search();
    state.search.emit_by_name::<()>("search-changed", &[]);
    assert_eq!(state.credential_ids.borrow().len(), 2);
    let outcome = state
        .vault
        .borrow_mut()
        .as_mut()
        .unwrap()
        .add_credential(Credential {
            id: String::new(),
            description: "nuova".into(),
            username: "demo".into(),
            password: "public test secret".into(),
            address: String::new(),
            notes: String::new(),
            created_at: None,
            updated_at: None,
        })
        .unwrap();
    assert!(matches!(outcome, SaveOutcome::Durable));
    lock_vault(&state);
    std::fs::remove_file(&path).unwrap();
    show_authentication(&state);
    assert_eq!(
        auth_button(&state, "Sblocca").label().as_deref(),
        Some("Sblocca")
    );
    passwords(state.auth.upcast_ref())[0].set_text("public fixture master password");
    auth_button(&state, "Ripristina una copia precedente…").emit_clicked();
    let restore = state
        .dialogs
        .borrow()
        .last()
        .unwrap()
        .upgrade()
        .unwrap()
        .downcast::<Dialog>()
        .unwrap();
    restore.response(ResponseType::Accept);
    wait_until(|| state.vault.borrow().is_some());
    assert_eq!(
        state
            .vault
            .borrow()
            .as_ref()
            .unwrap()
            .data
            .credentials
            .len(),
        2
    );
    assert!(path.exists());
    state.window.close();

    // Creation also requires a readable key file; it cannot fall back to a
    // password-only vault. Removing the medium after unlock is supported.
    let new_path = directory.join("new").join("vault");
    let state = build_with_path(&application, Ok(new_path.clone())).unwrap();
    for (button, action) in [
        ("Genera file chiave…", gtk4::FileChooserAction::Save),
        ("Scegli file chiave…", gtk4::FileChooserAction::Open),
    ] {
        auth_button(&state, button).emit_clicked();
        let chooser = state
            .dialogs
            .borrow()
            .last()
            .unwrap()
            .upgrade()
            .unwrap()
            .downcast::<gtk4::FileChooserDialog>()
            .expect("il pulsante deve aprire un selettore GTK");
        assert!(chooser.is_visible());
        assert!(chooser.is_modal());
        assert_eq!(chooser.action(), action);
        assert_eq!(
            chooser.transient_for().as_ref(),
            Some(state.window.upcast_ref())
        );
        chooser.response(ResponseType::Cancel);
        assert!(!chooser.is_visible());
    }
    for password in passwords(state.auth.upcast_ref()) {
        password.set_text("public fixture master password");
    }
    auth_button(&state, "Crea vault").emit_clicked();
    assert!(state.vault.borrow().is_none());
    assert!(!new_path.exists());
    let new_key = directory.join("new.key");
    assert!(matches!(
        crate::keyfile::KeyFile::generate(&new_key).unwrap(),
        SaveOutcome::Durable
    ));
    state.keyfile.select(new_key.clone());
    unlock(&state, "Crea vault");
    assert!(state.vault.borrow().is_some());
    std::fs::rename(&new_key, directory.join("disconnected.key")).unwrap();
    show_editor(&state, None);
    let editor = state
        .dialogs
        .borrow()
        .last()
        .unwrap()
        .upgrade()
        .unwrap()
        .downcast::<Dialog>()
        .unwrap();
    let entries: Vec<_> = widgets(editor.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<Entry>().ok())
        .collect();
    entries[0].set_text("Test senza supporto collegato");
    entries[1].set_text("demo");
    let generate = widgets(editor.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<Button>().ok())
        .find(|button| button.label().as_deref() == Some("Genera"))
        .unwrap();
    generate.emit_clicked();
    assert_eq!(passwords(editor.upcast_ref())[0].text().len(), 20);
    editor.response(ResponseType::Accept);
    assert_eq!(
        state
            .vault
            .borrow()
            .as_ref()
            .unwrap()
            .data
            .credentials
            .len(),
        1
    );
    lock_vault(&state);
    passwords(state.auth.upcast_ref())[0].set_text("public fixture master password");
    auth_button(&state, "Sblocca").emit_clicked();
    assert!(state.vault.borrow().is_none());
    state.keyfile.select(directory.join("disconnected.key"));
    // Only the location is remembered, and only after a successful unlock.
    let remember = widgets(state.auth.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk4::CheckButton>().ok())
        .unwrap();
    remember.set_active(true);
    unlock(&state, "Sblocca");
    assert_eq!(
        crate::preferences::Preferences::load(&new_path).keyfile,
        Some(directory.join("disconnected.key"))
    );
    assert_eq!(
        state
            .vault
            .borrow()
            .as_ref()
            .unwrap()
            .data
            .credentials
            .len(),
        1
    );
    state.window.close();

    // A later start already knows where the key file is.
    let state = build_with_path(&application, Ok(new_path.clone())).unwrap();
    passwords(state.auth.upcast_ref())[0].set_text("public fixture master password");
    unlock(&state, "Sblocca");
    assert!(state.vault.borrow().is_some());
    state.window.close();
    std::fs::remove_dir_all(directory).unwrap();
}
