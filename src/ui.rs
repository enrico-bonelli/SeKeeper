use crate::model::{Credential, matching_credentials};
use crate::security::{IdleLock, generate_password, validate_new_password};
use crate::storage::SaveOutcome;
use crate::vault::{Vault, VaultError};
use gtk4::glib::{Propagation, clone};
use gtk4::prelude::*;
use gtk4::{
    Align, Application, ApplicationWindow, Box, Button, Dialog, Entry, Grid, HeaderBar, Label,
    ListBox, ListBoxRow, MessageDialog, MessageType, Orientation, PasswordEntry, ResponseType,
    ScrolledWindow, SearchEntry, SelectionMode, Stack, TextView,
};
use gtk4::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;
use zeroize::Zeroizing;

struct AppState {
    window: ApplicationWindow,
    vault_path: PathBuf,
    vault: RefCell<Option<Vault>>,
    stack: Stack,
    auth: Box,
    credentials: ListBox,
    credential_ids: RefCell<Vec<String>>,
    search: SearchEntry,
    empty_state: Label,
    edit_button: Button,
    delete_button: Button,
    add_button: Button,
    menu_button: gtk4::MenuButton,
    lock_button: Button,
    counter: Label,
    toast: gtk4::Revealer,
    toast_label: Label,
    toast_timer: RefCell<Option<glib::SourceId>>,
    details: details::State,
    clipboard: clipboard::State,
    unlocking: Cell<bool>,
    idle: RefCell<IdleLock>,
    timer: RefCell<Option<glib::SourceId>>,
    dialogs: RefCell<Vec<glib::WeakRef<gtk4::Window>>>,
    subscriptions: RefCell<Vec<(gio::DBusConnection, gio::SignalSubscriptionId)>>,
    closing: Cell<bool>,
    cloud: cloud::State,
    keyfile: keyfiles::State,
}

type SharedState = Rc<AppState>;

/// Runs slow or blocking work off the GTK thread. Errors arrive as text because
/// callers only show them; a panic in the worker is reported, never swallowed.
fn job<T: Send + 'static, E: std::fmt::Display + Send + 'static>(
    work: impl FnOnce() -> Result<T, E> + Send + 'static,
    done: impl FnOnce(Result<T, String>) + 'static,
) {
    glib::spawn_future_local(async move {
        done(match gio::spawn_blocking(work).await {
            Ok(result) => result.map_err(|error| error.to_string()),
            Err(_) => Err("operazione interrotta da un errore interno".into()),
        });
    });
}

pub fn build(application: &Application) {
    if let Some(window) = application.active_window() {
        window.present();
        return;
    }
    build_with_path(application, Vault::default_path());
}

fn build_with_path(
    application: &Application,
    vault_path: Result<PathBuf, VaultError>,
) -> Option<SharedState> {
    gtk4::Window::set_default_icon_name(crate::APP_ID);
    gtk4::IconTheme::for_display(&gdk::Display::default().expect("display disponibile"))
        .add_resource_path("/io/github/sekeeper/Sekeeper/icons/hicolor");
    appearance::initialize();
    let window = ApplicationWindow::builder()
        .application(application)
        .title("Sekeeper")
        .default_width(980)
        .default_height(680)
        .build();

    let header = HeaderBar::new();
    let add_button = Button::with_label("Aggiungi");
    add_button.add_css_class("suggested-action");
    add_button.set_visible(false);
    header.pack_start(&add_button);
    let cloud_button = Button::with_label("S3…");
    let lock_button = Button::with_label("Blocca");
    lock_button.set_visible(false);
    let menu = gio::Menu::new();
    menu.append(Some("Importa da CSV…"), Some("win.import"));
    menu.append(Some("Esporta in CSV…"), Some("win.export"));
    let menu_button = gtk4::MenuButton::new();
    menu_button.set_icon_name("open-menu-symbolic");
    menu_button.set_tooltip_text(Some("Importa ed esporta"));
    menu_button.set_menu_model(Some(&menu));
    menu_button.set_visible(false);
    header.pack_end(&menu_button);
    header.pack_end(&cloud_button);
    header.pack_end(&lock_button);
    window.set_titlebar(Some(&header));

    let stack = Stack::new();
    stack.set_vexpand(true);
    stack.set_hexpand(true);
    stack.set_transition_type(gtk4::StackTransitionType::Crossfade);
    let root = Box::new(Orientation::Vertical, 0);
    root.append(&stack);
    let overlay = gtk4::Overlay::new();
    overlay.set_child(Some(&root));
    window.set_child(Some(&overlay));

    let toast_label = Label::new(None);
    toast_label.add_css_class("toast");
    let toast_revealer = gtk4::Revealer::new();
    toast_revealer.set_child(Some(&toast_label));
    toast_revealer.set_transition_type(gtk4::RevealerTransitionType::SlideUp);
    toast_revealer.set_halign(Align::Center);
    toast_revealer.set_valign(Align::End);
    toast_revealer.set_can_target(false);
    overlay.add_overlay(&toast_revealer);

    let auth_page = Box::new(Orientation::Vertical, 0);
    auth_page.add_css_class("auth-page");
    let auth = Box::new(Orientation::Vertical, 0);
    auth.add_css_class("auth-card");
    auth.set_valign(Align::Center);
    auth.set_halign(Align::Center);
    auth.set_vexpand(true);
    auth.set_margin_top(24);
    auth.set_margin_bottom(24);
    let auth_scroller = ScrolledWindow::new();
    auth_scroller.set_hscrollbar_policy(gtk4::PolicyType::Never);
    auth_scroller.set_child(Some(&auth));
    auth_scroller.set_vexpand(true);
    auth_page.append(&auth_scroller);
    stack.add_named(&auth_page, Some("auth"));

    let edit_button = Button::with_label("Modifica");
    let delete_button = Button::with_label("Elimina");
    delete_button.add_css_class("destructive-action");
    edit_button.set_sensitive(false);
    delete_button.set_sensitive(false);
    let details = details::State::new(&edit_button, &delete_button);

    let sidebar = Box::new(Orientation::Vertical, 10);
    sidebar.add_css_class("sidebar");
    sidebar.set_size_request(290, -1);
    let search = SearchEntry::new();
    search.set_placeholder_text(Some("Cerca per nome, username o indirizzo…"));
    search.set_hexpand(true);
    sidebar.append(&search);

    let credentials = ListBox::new();
    credentials.set_selection_mode(SelectionMode::Single);
    credentials.set_activate_on_single_click(false);
    let empty_state = Label::new(Some("Nessuna credenziale. Premi Aggiungi per iniziare."));
    empty_state.set_wrap(true);
    empty_state.set_margin_top(24);
    empty_state.set_margin_start(12);
    empty_state.set_margin_end(12);
    empty_state.set_justify(gtk4::Justification::Center);
    empty_state.add_css_class("dim-label");
    credentials.set_placeholder(Some(&empty_state));
    let scroller = ScrolledWindow::new();
    scroller.set_vexpand(true);
    scroller.set_hscrollbar_policy(gtk4::PolicyType::Never);
    scroller.set_child(Some(&credentials));
    sidebar.append(&scroller);
    let counter = Label::new(None);
    counter.set_halign(Align::Start);
    counter.add_css_class("dim-label");
    sidebar.append(&counter);

    let main = gtk4::Paned::new(Orientation::Horizontal);
    main.set_start_child(Some(&sidebar));
    main.set_end_child(Some(&details.root));
    main.set_position(330);
    main.set_shrink_start_child(false);
    main.set_shrink_end_child(false);
    main.set_resize_start_child(false);
    stack.add_named(&main, Some("vault"));

    let vault_path = match vault_path {
        Ok(path) => path,
        Err(error) => {
            show_error(&window, &error.to_string());
            return None;
        }
    };
    let cloud = cloud::State::new(&vault_path);
    let status_bar = Box::new(Orientation::Horizontal, 12);
    status_bar.add_css_class("status-bar");
    cloud.status.set_hexpand(true);
    cloud.status.set_halign(Align::Start);
    status_bar.append(&cloud.status);
    let lock_hint = Label::new(Some("Blocco automatico dopo 5 minuti di inattività"));
    lock_hint.add_css_class("dim-label");
    status_bar.append(&lock_hint);
    root.append(&status_bar);
    let keyfile = keyfiles::State::default();
    keyfile.restore(&vault_path);
    let state = Rc::new(AppState {
        window: window.clone(),
        vault_path,
        vault: RefCell::new(None),
        stack,
        auth,
        credentials,
        credential_ids: RefCell::new(Vec::new()),
        search,
        empty_state,
        edit_button,
        delete_button,
        add_button,
        menu_button,
        lock_button,
        counter,
        toast: toast_revealer,
        toast_label,
        toast_timer: RefCell::new(None),
        details,
        clipboard: clipboard::State::default(),
        unlocking: Cell::new(false),
        idle: RefCell::new(IdleLock::new(Instant::now())),
        timer: RefCell::new(None),
        dialogs: RefCell::new(Vec::new()),
        subscriptions: RefCell::new(Vec::new()),
        closing: Cell::new(false),
        cloud,
        keyfile,
    });
    details::connect(&state);
    for (name, action) in [
        ("import", transfer::choose_import as fn(&SharedState)),
        ("export", transfer::confirm_export),
    ] {
        let entry = gio::SimpleAction::new(name, None);
        entry.connect_activate(clone!(
            #[weak]
            state,
            move |_, _| {
                if state.vault.borrow().is_some() {
                    action(&state);
                }
            }
        ));
        window.add_action(&entry);
    }
    cloud_button.connect_clicked(clone!(
        #[weak]
        state,
        move |_| cloud::show_settings(&state)
    ));
    state.search.connect_search_changed(clone!(
        #[weak]
        state,
        move |_| refresh_credentials(&state)
    ));
    state
        .search
        .connect_stop_search(|search| search.set_text(""));
    let search_shortcut = gtk4::EventControllerKey::new();
    search_shortcut.set_propagation_phase(gtk4::PropagationPhase::Capture);
    search_shortcut.connect_key_pressed(clone!(
        #[weak]
        state,
        #[upgrade_or]
        Propagation::Proceed,
        move |_, key, _, modifiers| {
            if modifiers.contains(gdk::ModifierType::CONTROL_MASK)
                && matches!(key, gdk::Key::f | gdk::Key::F)
                && state.vault.borrow().is_some()
            {
                state.idle.borrow_mut().touch(Instant::now());
                state.search.grab_focus();
                state.search.select_region(0, -1);
                return Propagation::Stop;
            }
            Propagation::Proceed
        }
    ));
    window.add_controller(search_shortcut);

    state.credentials.connect_selected_rows_changed(clone!(
        #[weak]
        state,
        move |list| {
            let selected = list.selected_row().is_some();
            state.edit_button.set_sensitive(selected);
            state.delete_button.set_sensitive(selected);
            details::update(&state, selected_credential(&state).as_ref());
        }
    ));
    state.credentials.connect_row_activated(clone!(
        #[weak]
        state,
        move |_, _| {
            if let Some(credential) = selected_credential(&state) {
                show_editor(&state, Some(credential));
            }
        }
    ));
    state.add_button.connect_clicked(clone!(
        #[weak]
        state,
        move |_| show_editor(&state, None)
    ));
    state.edit_button.connect_clicked(clone!(
        #[weak]
        state,
        move |_| {
            if let Some(credential) = selected_credential(&state) {
                show_editor(&state, Some(credential));
            }
        }
    ));
    state.delete_button.connect_clicked(clone!(
        #[weak]
        state,
        move |_| confirm_delete(&state)
    ));
    state.lock_button.connect_clicked(clone!(
        #[weak]
        state,
        move |_| lock_vault(&state)
    ));
    window.connect_close_request(clone!(
        #[strong]
        state,
        move |_| {
            state.closing.set(true);
            cloud::locked(&state);
            clear_secrets(&state);
            if let Some(timer) = state.timer.borrow_mut().take() {
                timer.remove();
            }
            for (connection, subscription) in state.subscriptions.borrow_mut().drain(..) {
                connection.signal_unsubscribe(subscription);
            }
            Propagation::Proceed
        }
    ));

    install_css();
    install_activity_tracking(&state, &window);
    let timer = glib::timeout_add_seconds_local(
        1,
        clone!(
            #[weak]
            state,
            #[upgrade_or]
            glib::ControlFlow::Break,
            move || {
                check_idle(&state);
                cloud::tick(&state);
                glib::ControlFlow::Continue
            }
        ),
    );
    *state.timer.borrow_mut() = Some(timer);
    watch_session(&state);
    appearance::watch(&state);
    show_authentication(&state);
    window.present();
    Some(state)
}

fn show_authentication(state: &SharedState) {
    show_authentication_mode(state, false);
}

fn show_authentication_mode(state: &SharedState, force_create: bool) {
    while let Some(child) = state.auth.first_child() {
        state.auth.remove(&child);
    }

    let backups = Vault::available_backups(&state.vault_path);
    let creating = !state.vault_path.exists()
        && backups.is_empty()
        && (!state.cloud.enabled() || force_create);
    let recovering =
        !state.vault_path.exists() && backups.is_empty() && state.cloud.enabled() && !creating;
    let title = Label::new(Some(if creating {
        "Crea il tuo vault"
    } else if recovering {
        "Recupera il tuo vault"
    } else {
        "Sblocca Sekeeper"
    }));
    title.add_css_class("auth-title");
    title.set_margin_bottom(8);
    let logo = gtk4::Image::from_icon_name(crate::APP_ID);
    logo.set_pixel_size(84);
    logo.set_margin_bottom(14);
    state.auth.append(&logo);
    state.auth.append(&title);

    let explanation = Label::new(Some(if creating {
        "Scegli una passphrase unica di almeno 15 caratteri e genera o seleziona un file chiave. Serviranno entrambi per aprire il vault e i backup. Conserva il file separatamente e fanne una copia di recupero."
    } else if recovering {
        "Questo PC non ha ancora un vault locale. Per recuperarlo da S3 servono la password principale e lo stesso file chiave usato sugli altri PC."
    } else {
        "Inserisci la password principale e seleziona il file chiave per aprire il vault."
    }));
    explanation.set_wrap(true);
    explanation.set_max_width_chars(46);
    explanation.set_justify(gtk4::Justification::Center);
    explanation.add_css_class("dim-label");
    explanation.set_margin_bottom(22);
    state.auth.append(&explanation);

    let form = Grid::new();
    form.set_row_spacing(12);
    form.set_column_spacing(12);
    let password_label = Label::new(Some("Password"));
    password_label.set_halign(Align::End);
    let password = PasswordEntry::new();
    password.set_show_peek_icon(true);
    password.set_width_chars(30);
    form.attach(&password_label, 0, 0, 1, 1);
    form.attach(&password, 1, 0, 1, 1);

    let confirmation = PasswordEntry::new();
    if creating {
        let confirmation_label = Label::new(Some("Conferma password"));
        confirmation_label.set_halign(Align::End);
        confirmation.set_show_peek_icon(true);
        form.attach(&confirmation_label, 0, 1, 1, 1);
        form.attach(&confirmation, 1, 1, 1, 1);
    }
    state.auth.append(&form);
    state
        .auth
        .append(&keyfiles::controls(state, &state.window, creating));

    let error = Label::new(None);
    error.add_css_class("error");
    error.set_wrap(true);
    error.set_max_width_chars(46);
    error.set_margin_top(12);
    // Some themes paint a background behind .error, even with no text.
    error.set_visible(false);
    error.connect_label_notify(|label| label.set_visible(!label.text().is_empty()));
    state.auth.append(&error);

    let progress = Box::new(Orientation::Horizontal, 8);
    progress.set_halign(Align::Center);
    progress.set_margin_top(12);
    let spinner = gtk4::Spinner::new();
    progress.append(&spinner);
    progress.append(&Label::new(Some(if creating {
        "Creazione del vault…"
    } else {
        "Verifica di password e file chiave…"
    })));
    progress.set_visible(false);
    state.auth.append(&progress);

    let submit = Button::with_label(if creating {
        "Crea vault"
    } else if recovering {
        "Recupera da S3…"
    } else {
        "Sblocca"
    });
    submit.add_css_class("suggested-action");
    submit.add_css_class("primary");
    submit.set_margin_top(18);
    state.auth.append(&submit);
    if recovering {
        let create = Button::with_label("Crea un nuovo vault vuoto");
        create.add_css_class("flat");
        create.set_margin_top(6);
        state.auth.append(&create);
        create.connect_clicked(clone!(
            #[weak]
            state,
            move |_| show_authentication_mode(&state, true)
        ));
    }
    if state.cloud.enabled() && !recovering {
        let recover = Button::with_label("Recupera da S3…");
        recover.add_css_class("flat");
        recover.set_margin_top(6);
        state.auth.append(&recover);
        recover.connect_clicked(clone!(
            #[weak]
            state,
            #[weak]
            password,
            move |_| cloud::show_recover(&state, &password)
        ));
    }
    if !backups.is_empty() {
        let restore = Button::with_label("Ripristina una copia precedente…");
        restore.add_css_class("flat");
        restore.set_margin_top(6);
        state.auth.append(&restore);
        restore.connect_clicked(clone!(
            #[weak]
            state,
            #[weak]
            password,
            move |_| show_restore(&state, &password)
        ));
    }
    state.window.set_default_widget(Some(&submit));
    password.grab_focus();

    let action = clone!(
        #[weak]
        state,
        #[weak]
        password,
        #[weak_allow_none]
        confirmation,
        #[weak]
        error,
        #[weak]
        progress,
        #[weak]
        spinner,
        move || {
            if recovering {
                cloud::show_recover(&state, &password);
                return;
            }
            let master_password = Zeroizing::new(password.text().to_string());
            if master_password.is_empty() {
                error.set_text("La password non può essere vuota.");
                return;
            }
            if creating {
                if let Err(message) = validate_new_password(&master_password) {
                    error.set_text(message);
                    return;
                }
                let Some(confirmation) = &confirmation else {
                    return;
                };
                let repeated = Zeroizing::new(confirmation.text().to_string());
                if *master_password != *repeated {
                    error.set_text("Le password non coincidono.");
                    return;
                }
            }
            error.set_text("");
            let keyfile = match state.keyfile.load() {
                Ok(keyfile) => keyfile,
                Err(failure) => {
                    error.set_text(&failure.to_string());
                    return;
                }
            };
            password.set_text("");
            if let Some(confirmation) = confirmation {
                confirmation.set_text("");
            }
            // The KDF takes long enough to freeze the window: keep it off the
            // GTK thread, with the form disabled so nothing can race the result.
            state.unlocking.set(true);
            state.auth.set_sensitive(false);
            progress.set_visible(true);
            spinner.start();
            let path = state.vault_path.clone();
            job(
                move || {
                    if creating {
                        Vault::create(&path, &master_password, &keyfile)
                            .map(|(vault, outcome)| (vault, Some(outcome)))
                    } else {
                        Vault::open(&path, &master_password, &keyfile).map(|vault| (vault, None))
                    }
                },
                clone!(
                    #[weak]
                    state,
                    #[weak_allow_none]
                    error,
                    #[weak_allow_none]
                    password,
                    #[weak_allow_none]
                    progress,
                    #[weak_allow_none]
                    spinner,
                    move |result| {
                        state.unlocking.set(false);
                        state.auth.set_sensitive(true);
                        if let (Some(progress), Some(spinner)) = (progress, spinner) {
                            spinner.stop();
                            progress.set_visible(false);
                        }
                        if state.closing.get() {
                            return;
                        }
                        match result {
                            Ok((vault, outcome)) => {
                                *state.vault.borrow_mut() = Some(vault);
                                keyfiles::remember(&state);
                                show_vault(&state);
                                if let Some(outcome) = outcome {
                                    show_save_warning(&state, outcome);
                                }
                            }
                            Err(failure) => {
                                if let (Some(error), Some(password)) = (error, password) {
                                    error.set_text(&failure);
                                    password.grab_focus();
                                }
                            }
                        }
                    }
                ),
            );
        }
    );
    submit.connect_clicked({
        let action = action.clone();
        move |_| action()
    });
    password.connect_activate({
        let action = action.clone();
        move |_| action()
    });
    confirmation.connect_activate(move |_| action());

    state.stack.set_visible_child_name("auth");
    show_session_controls(state, false);
}

fn show_session_controls(state: &SharedState, unlocked: bool) {
    state.lock_button.set_visible(unlocked);
    state.add_button.set_visible(unlocked);
    state.menu_button.set_visible(unlocked);
}

fn toast(state: &SharedState, message: &str) {
    state.toast_label.set_text(message);
    state.toast.set_reveal_child(true);
    if let Some(timer) = state.toast_timer.borrow_mut().take() {
        timer.remove();
    }
    let timer = glib::timeout_add_seconds_local_once(
        4,
        clone!(
            #[weak]
            state,
            move || {
                state.toast_timer.borrow_mut().take();
                state.toast.set_reveal_child(false);
            }
        ),
    );
    *state.toast_timer.borrow_mut() = Some(timer);
}

fn show_vault(state: &SharedState) {
    state.window.set_default_widget(None::<&Button>);
    state.idle.borrow_mut().touch(Instant::now());
    refresh_credentials(state);
    state.stack.set_visible_child_name("vault");
    show_session_controls(state, true);
    cloud::request_sync(state);
}

fn refresh_credentials(state: &SharedState) {
    let selected_id = selected_credential_id(state);
    let query = state.search.text();
    let (total, entries) = {
        let slot = state.vault.borrow();
        let credentials = slot
            .as_ref()
            .map(|vault| vault.data.credentials.as_slice())
            .unwrap_or_default();
        let mut entries: Vec<_> = matching_credentials(credentials, &query)
            .into_iter()
            .map(|credential| {
                (
                    credential.id.clone(),
                    credential.description.clone(),
                    credential.username.clone(),
                )
            })
            .collect();
        // Sorting affects the list only: the saved order stays stable for sync.
        entries.sort_by_cached_key(|(_, description, _)| {
            glib::CollationKey::from(description.to_lowercase())
        });
        (credentials.len(), entries)
    };
    state.credentials.unselect_all();
    while let Some(row) = state.credentials.row_at_index(0) {
        state.credentials.remove(&row);
    }
    *state.credential_ids.borrow_mut() = entries.iter().map(|(id, _, _)| id.clone()).collect();
    for (_, description, username) in &entries {
        let content = Box::new(Orientation::Horizontal, 12);
        let avatar = Label::new(None);
        avatar.add_css_class("avatar");
        avatar.set_valign(Align::Center);
        set_avatar(&avatar, description, false);
        content.append(&avatar);
        let text = Box::new(Orientation::Vertical, 2);
        text.set_valign(Align::Center);
        text.set_hexpand(true);
        let label = Label::new(Some(description));
        label.set_halign(Align::Start);
        label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        label.add_css_class("row-title");
        text.append(&label);
        let subtitle = Label::new(Some(username));
        subtitle.set_halign(Align::Start);
        subtitle.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        subtitle.add_css_class("row-subtitle");
        text.append(&subtitle);
        content.append(&text);
        let row = ListBoxRow::new();
        row.set_child(Some(&content));
        state.credentials.append(&row);
    }
    state.empty_state.set_text(if total == 0 {
        "Nessuna credenziale. Premi Aggiungi per iniziare."
    } else {
        "Nessun risultato. Prova un'altra descrizione o cancella la ricerca."
    });
    let counter = if state.vault.borrow().is_none() {
        String::new()
    } else if query.trim().is_empty() {
        format!("{total} credenzial{}", if total == 1 { "e" } else { "i" })
    } else {
        format!("{} di {total} credenziali", entries.len())
    };
    state.counter.set_text(&counter);
    state.edit_button.set_sensitive(false);
    state.delete_button.set_sensitive(false);
    let selected_index = selected_id.and_then(|id| {
        state
            .credential_ids
            .borrow()
            .iter()
            .position(|visible_id| *visible_id == id)
    });
    if let Some(index) = selected_index.and_then(|index| i32::try_from(index).ok()) {
        state
            .credentials
            .select_row(state.credentials.row_at_index(index).as_ref());
    }
}

fn selected_credential_id(state: &SharedState) -> Option<String> {
    let row = state.credentials.selected_row()?;
    let index = usize::try_from(row.index()).ok()?;
    state.credential_ids.borrow().get(index).cloned()
}

fn selected_credential(state: &SharedState) -> Option<Credential> {
    let id = selected_credential_id(state)?;
    state
        .vault
        .borrow()
        .as_ref()?
        .data
        .credentials
        .iter()
        .find(|credential| credential.id == id)
        .cloned()
}

/// The colour depends only on the name, so an entry keeps it across sessions.
fn set_avatar(avatar: &Label, description: &str, large: bool) {
    const COLOURS: u32 = 6;
    let initial: String = description
        .chars()
        .find(|character| character.is_alphanumeric())
        .map_or("?".into(), |character| character.to_uppercase().collect());
    let hash = description.bytes().fold(7u32, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(byte.into())
    });
    avatar.set_text(&initial);
    avatar.set_css_classes(&["avatar", if large { "avatar-large" } else { "" }]);
    avatar.add_css_class(&format!("avatar-{}", hash % COLOURS));
}

fn format_timestamp(timestamp: Option<u64>) -> String {
    timestamp
        .and_then(|value| i64::try_from(value).ok())
        .and_then(|value| glib::DateTime::from_unix_local(value).ok())
        .and_then(|date| date.format("%d/%m/%Y %H:%M:%S").ok())
        .map(|value| value.to_string())
        .unwrap_or_else(|| "Non disponibile".into())
}

fn show_editor(state: &SharedState, existing: Option<Credential>) {
    if state.vault.borrow().is_none() {
        return;
    }
    let editing = existing.is_some();
    let dialog = Dialog::builder()
        .title(if editing {
            "Modifica credenziale"
        } else {
            "Nuova credenziale"
        })
        .transient_for(&state.window)
        .modal(true)
        .default_width(520)
        .default_height(580)
        .build();
    track_dialog(state, &dialog);
    dialog.add_button("Annulla", ResponseType::Cancel);
    let save = dialog.add_button("Salva", ResponseType::Accept);
    save.add_css_class("suggested-action");
    dialog.set_default_response(ResponseType::Accept);

    let content = Box::new(Orientation::Vertical, 12);
    content.set_margin_top(18);
    content.set_margin_bottom(12);
    content.set_margin_start(18);
    content.set_margin_end(18);
    let grid = Grid::new();
    grid.set_row_spacing(12);
    grid.set_column_spacing(12);

    let description = Entry::new();
    let username = Entry::new();
    let password = PasswordEntry::new();
    password.set_show_peek_icon(true);
    let address = Entry::new();
    for entry in [&description, &username, &address] {
        entry.set_hexpand(true);
    }
    password.set_hexpand(true);
    attach_field(&grid, 0, "Descrizione *", &description);
    attach_field(&grid, 1, "Username *", &username);
    let password_row = Box::new(Orientation::Horizontal, 6);
    password_row.append(&password);
    let length = gtk4::SpinButton::with_range(8.0, 64.0, 1.0);
    length.set_value(20.0);
    length.set_tooltip_text(Some("Lunghezza della password generata"));
    password_row.append(&length);
    let generate = Button::with_label("Genera");
    generate.set_tooltip_text(Some("Sostituisce il campo con una password casuale"));
    generate.connect_clicked(clone!(
        #[weak]
        password,
        #[weak]
        length,
        move |_| {
            let generated = generate_password(length.value_as_int() as usize);
            password.set_text(&generated);
            password.set_show_peek_icon(true);
        }
    ));
    password_row.append(&generate);
    attach_field(&grid, 2, "Password *", &password_row);
    attach_field(&grid, 3, "Indirizzo", &address);
    let created_at = Label::new(Some(&if editing {
        format_timestamp(
            existing
                .as_ref()
                .and_then(|credential| credential.created_at),
        )
    } else {
        "Al primo salvataggio".into()
    }));
    let updated_at = Label::new(Some(&if editing {
        format_timestamp(
            existing
                .as_ref()
                .and_then(|credential| credential.updated_at),
        )
    } else {
        "Al primo salvataggio".into()
    }));
    for label in [&created_at, &updated_at] {
        label.set_halign(Align::Start);
        label.add_css_class("dim-label");
    }
    attach_field(&grid, 4, "Creata il", &created_at);
    attach_field(&grid, 5, "Ultima modifica", &updated_at);

    let notes_label = Label::new(Some("Note"));
    notes_label.set_halign(Align::Start);
    notes_label.set_margin_top(4);
    content.append(&grid);
    content.append(&notes_label);
    let notes = TextView::new();
    notes.set_wrap_mode(gtk4::WrapMode::WordChar);
    notes.set_accepts_tab(false);
    notes.set_top_margin(8);
    notes.set_bottom_margin(8);
    notes.set_left_margin(10);
    notes.set_right_margin(10);
    let notes_scroll = ScrolledWindow::new();
    notes_scroll.set_vexpand(true);
    notes_scroll.set_min_content_height(110);
    notes_scroll.set_child(Some(&notes));
    content.append(&notes_scroll);
    let error = Label::new(None);
    error.add_css_class("error");
    error.set_wrap(true);
    error.set_halign(Align::Start);
    content.append(&error);
    dialog.content_area().append(&content);

    let id = existing
        .as_ref()
        .map(|item| item.id.clone())
        .unwrap_or_default();
    if let Some(credential) = existing {
        description.set_text(&credential.description);
        username.set_text(&credential.username);
        password.set_text(&credential.password);
        address.set_text(&credential.address);
        notes.buffer().set_text(&credential.notes);
    }

    dialog.connect_response(clone!(
        #[weak]
        state,
        #[weak]
        dialog,
        #[weak]
        description,
        #[weak]
        username,
        #[weak]
        password,
        #[weak]
        address,
        #[weak]
        notes,
        #[weak]
        error,
        move |_, response| {
            if response == ResponseType::Cancel || response == ResponseType::DeleteEvent {
                scrub_widget(dialog.upcast_ref());
                dialog.close();
                return;
            }
            if response != ResponseType::Accept {
                return;
            }
            let description_text = description.text().trim().to_string();
            let username_text = username.text().trim().to_string();
            let mut password_text = Zeroizing::new(password.text().to_string());
            if description_text.is_empty() || username_text.is_empty() || password_text.is_empty() {
                error.set_text("Descrizione, username e password sono obbligatori.");
                return;
            }
            let buffer = notes.buffer();
            let mut notes_text = Zeroizing::new(
                buffer
                    .text(&buffer.start_iter(), &buffer.end_iter(), false)
                    .to_string(),
            );
            let credential = Credential {
                id: id.clone(),
                description: description_text,
                username: username_text,
                password: std::mem::take(&mut *password_text),
                address: address.text().trim().to_string(),
                notes: std::mem::take(&mut *notes_text),
                created_at: None,
                updated_at: None,
            };
            let result = {
                let mut slot = state.vault.borrow_mut();
                let Some(vault) = slot.as_mut() else {
                    scrub_widget(dialog.upcast_ref());
                    dialog.close();
                    return;
                };
                if editing {
                    vault.update_credential(credential)
                } else {
                    vault.add_credential(credential)
                }
            };
            match result {
                Ok(outcome) => {
                    scrub_widget(dialog.upcast_ref());
                    refresh_credentials(&state);
                    dialog.close();
                    show_save_warning(&state, outcome);
                }
                Err(failure) => error.set_text(&failure.to_string()),
            }
        }
    ));
    dialog.present();
    description.grab_focus();
}

fn attach_field(grid: &Grid, row: i32, text: &str, entry: &impl IsA<gtk4::Widget>) {
    let label = Label::new(Some(text));
    label.set_halign(Align::End);
    grid.attach(&label, 0, row, 1, 1);
    grid.attach(entry, 1, row, 1, 1);
}

fn confirm_delete(state: &SharedState) {
    let Some(credential) = selected_credential(state) else {
        return;
    };
    let dialog = MessageDialog::new(
        Some(&state.window),
        gtk4::DialogFlags::MODAL,
        MessageType::Question,
        gtk4::ButtonsType::None,
        "Eliminare questa credenziale?",
    );
    track_dialog(state, &dialog);
    dialog.set_secondary_text(Some(&format!(
        "“{}” verrà eliminata definitivamente.",
        credential.description
    )));
    dialog.add_button("Annulla", ResponseType::Cancel);
    let delete = dialog.add_button("Elimina", ResponseType::Accept);
    delete.add_css_class("destructive-action");
    let id = credential.id.clone();
    drop(credential);
    dialog.connect_response(clone!(
        #[weak]
        state,
        move |dialog, response| {
            dialog.close();
            if response != ResponseType::Accept {
                return;
            }
            let result = {
                let mut slot = state.vault.borrow_mut();
                let Some(vault) = slot.as_mut() else {
                    return;
                };
                vault.delete_credential(&id)
            };
            match result {
                Ok(outcome) => {
                    refresh_credentials(&state);
                    show_save_warning(&state, outcome);
                }
                Err(error) => show_error(&state.window, &error.to_string()),
            }
        }
    ));
    dialog.present();
}

fn show_save_warning(state: &SharedState, outcome: SaveOutcome) {
    if let SaveOutcome::DurabilityUncertain(error) = outcome {
        show_error(
            &state.window,
            &format!(
                "Il file contiene le modifiche, ma non è stato possibile garantirne la conservazione in caso di spegnimento improvviso. Non ripetere l'operazione.\n\nDettaglio: {error}"
            ),
        );
    } else if let SaveOutcome::CloudCopyPending(error) = outcome {
        show_error(
            &state.window,
            &format!(
                "Il vault locale è salvato, ma la copia per S3 è in attesa. Riprova la sincronizzazione.\n\n{error}"
            ),
        );
    }
    cloud::request_sync(state);
}

fn show_restore(state: &SharedState, password: &PasswordEntry) {
    let slots = Vault::available_backups(&state.vault_path);
    if slots.is_empty() {
        return;
    }
    let dialog = Dialog::builder()
        .title("Ripristina una copia precedente")
        .transient_for(&state.window)
        .modal(true)
        .build();
    track_dialog(state, &dialog);
    dialog.add_button("Annulla", ResponseType::Cancel);
    dialog.add_button("Ripristina", ResponseType::Accept);
    let content = dialog.content_area();
    content.set_spacing(12);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);
    let explanation = Label::new(Some(
        "Il ripristino sostituisce il vault con una copia precedente e annulla le modifiche successive. Il file attuale verrà conservato separatamente. Servono la password principale e il file chiave della copia scelta.",
    ));
    explanation.set_wrap(true);
    explanation.set_max_width_chars(55);
    content.append(&explanation);
    let labels: Vec<String> = slots
        .iter()
        .map(|slot| {
            format!(
                "Copia {slot} — {}{}",
                format_timestamp(Vault::backup_modified(&state.vault_path, *slot)),
                if *slot == 1 { " (più recente)" } else { "" }
            )
        })
        .collect();
    let choices: Vec<&str> = labels.iter().map(String::as_str).collect();
    let selection = gtk4::DropDown::from_strings(&choices);
    content.append(&selection);
    let initial_password = Zeroizing::new(password.text().to_string());
    password.set_text("");
    let password = PasswordEntry::new();
    password.set_placeholder_text(Some("Password principale"));
    password.set_show_peek_icon(true);
    password.set_text(&initial_password);
    content.append(&password);
    content.append(&keyfiles::controls(state, &dialog, false));
    let error = Label::new(None);
    error.add_css_class("error");
    error.set_wrap(true);
    content.append(&error);
    dialog.connect_response(clone!(
        #[weak]
        state,
        #[weak]
        password,
        #[weak]
        selection,
        #[weak]
        error,
        move |dialog, response| {
            if response != ResponseType::Accept {
                scrub_widget(dialog.upcast_ref());
                dialog.close();
                return;
            }
            let secret = Zeroizing::new(password.text().to_string());
            if secret.is_empty() {
                error.set_text("Inserisci la password principale.");
                return;
            }
            let Some(slot) = slots.get(selection.selected() as usize) else {
                return;
            };
            let keyfile = match state.keyfile.load() {
                Ok(keyfile) => keyfile,
                Err(failure) => {
                    error.set_text(&failure.to_string());
                    return;
                }
            };
            let slot = *slot;
            let path = state.vault_path.clone();
            dialog.set_sensitive(false);
            error.set_text("Verifica della copia…");
            job(
                move || Vault::restore_backup(&path, &secret, slot, &keyfile),
                clone!(
                    #[weak]
                    state,
                    #[weak]
                    dialog,
                    #[weak]
                    password,
                    #[weak]
                    error,
                    move |result| {
                        dialog.set_sensitive(true);
                        match result {
                            Ok((vault, outcome)) => {
                                password.set_text("");
                                *state.vault.borrow_mut() = Some(vault);
                                keyfiles::remember(&state);
                                dialog.close();
                                show_vault(&state);
                                show_save_warning(&state, outcome);
                            }
                            Err(failure) => error.set_text(&failure),
                        }
                    }
                ),
            );
        }
    ));
    dialog.present();
    password.grab_focus();
}

fn track_dialog(state: &SharedState, dialog: &impl IsA<gtk4::Window>) {
    let window = dialog.as_ref();
    let mut dialogs = state.dialogs.borrow_mut();
    dialogs.retain(|dialog| dialog.upgrade().is_some());
    dialogs.push(window.downgrade());
    window.connect_close_request(|window| {
        scrub_widget(window.upcast_ref());
        Propagation::Proceed
    });
    window.connect_hide(clone!(
        #[weak]
        state,
        move |_| {
            glib::idle_add_local_once(clone!(
                #[weak]
                state,
                move || cloud::resume_if_pending(&state)
            ));
        }
    ));
    install_activity_tracking(state, window);
}

fn scrub_widget(widget: &gtk4::Widget) {
    if let Some(entry) = widget.downcast_ref::<PasswordEntry>() {
        entry.set_text("");
    } else if let Some(entry) = widget.downcast_ref::<Entry>() {
        entry.set_text("");
    } else if let Some(view) = widget.downcast_ref::<TextView>() {
        view.buffer().set_text("");
    }
    let mut child = widget.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        scrub_widget(&widget);
    }
}

fn clear_secrets(state: &SharedState) {
    cloud::locked(state);
    clipboard::clear(state);
    state.toast.set_reveal_child(false);
    *state.vault.borrow_mut() = None;
    state.search.set_text("");
    state.credential_ids.borrow_mut().clear();
    scrub_widget(state.auth.upcast_ref());
    let dialogs = std::mem::take(&mut *state.dialogs.borrow_mut());
    for dialog in dialogs {
        if let Some(dialog) = dialog.upgrade() {
            scrub_widget(dialog.upcast_ref());
            match dialog.downcast::<gtk4::FileChooserDialog>() {
                Ok(chooser) => keyfiles::dismiss(&chooser),
                Err(dialog) => dialog.destroy(),
            }
        }
    }
    while let Some(row) = state.credentials.row_at_index(0) {
        state.credentials.remove(&row);
    }
    state.counter.set_text("");
    state.edit_button.set_sensitive(false);
    state.delete_button.set_sensitive(false);
    details::update(state, None);
}

fn lock_vault(state: &SharedState) {
    if state.vault.borrow().is_none() {
        return;
    }
    clear_secrets(state);
    show_authentication(state);
}

fn check_idle(state: &SharedState) {
    if state.vault.borrow().is_some() && state.idle.borrow().expired(Instant::now()) {
        lock_vault(state);
    }
}

fn install_activity_tracking(state: &SharedState, window: &impl IsA<gtk4::Window>) {
    let window = window.as_ref();
    let key = gtk4::EventControllerKey::new();
    key.set_propagation_phase(gtk4::PropagationPhase::Capture);
    key.connect_key_pressed(clone!(
        #[weak]
        state,
        #[upgrade_or]
        Propagation::Proceed,
        move |_, _, _, _| {
            state.idle.borrow_mut().touch(Instant::now());
            Propagation::Proceed
        }
    ));
    window.add_controller(key);
    let motion = gtk4::EventControllerMotion::new();
    motion.set_propagation_phase(gtk4::PropagationPhase::Capture);
    motion.connect_motion(clone!(
        #[weak]
        state,
        move |_, _, _| {
            state.idle.borrow_mut().touch(Instant::now());
        }
    ));
    window.add_controller(motion);
    let click = gtk4::GestureClick::new();
    click.set_button(0);
    click.set_propagation_phase(gtk4::PropagationPhase::Capture);
    click.connect_pressed(clone!(
        #[weak]
        state,
        move |_, _, _, _| {
            state.idle.borrow_mut().touch(Instant::now());
        }
    ));
    window.add_controller(click);
    let scroll = gtk4::EventControllerScroll::new(gtk4::EventControllerScrollFlags::BOTH_AXES);
    scroll.set_propagation_phase(gtk4::PropagationPhase::Capture);
    scroll.connect_scroll(clone!(
        #[weak]
        state,
        #[upgrade_or]
        Propagation::Proceed,
        move |_, _, _| {
            state.idle.borrow_mut().touch(Instant::now());
            Propagation::Proceed
        }
    ));
    window.add_controller(scroll);
}

fn watch_session(state: &SharedState) {
    glib::spawn_future_local(clone!(
        #[weak]
        state,
        async move {
            if let Ok(connection) = gio::bus_get_future(gio::BusType::Session).await {
                if state.closing.get() {
                    return;
                }
                for (sender, interface) in [
                    ("org.freedesktop.ScreenSaver", "org.freedesktop.ScreenSaver"),
                    ("org.gnome.ScreenSaver", "org.gnome.ScreenSaver"),
                    ("org.cinnamon.ScreenSaver", "org.cinnamon.ScreenSaver"),
                    ("org.mate.ScreenSaver", "org.mate.ScreenSaver"),
                ] {
                    let subscription = connection.signal_subscribe(
                        Some(sender),
                        Some(interface),
                        Some("ActiveChanged"),
                        None,
                        None,
                        gio::DBusSignalFlags::NONE,
                        clone!(
                            #[weak]
                            state,
                            move |_, _, _, _, _, parameters| {
                                if parameters.get::<(bool,)>() == Some((true,)) {
                                    lock_vault(&state);
                                }
                            }
                        ),
                    );
                    state
                        .subscriptions
                        .borrow_mut()
                        .push((connection.clone(), subscription));
                }
            }
            if let Ok(connection) = gio::bus_get_future(gio::BusType::System).await {
                if state.closing.get() {
                    return;
                }
                let subscription = connection.signal_subscribe(
                    Some("org.freedesktop.login1"),
                    Some("org.freedesktop.login1.Manager"),
                    Some("PrepareForSleep"),
                    Some("/org/freedesktop/login1"),
                    None,
                    gio::DBusSignalFlags::NONE,
                    clone!(
                        #[weak]
                        state,
                        move |_, _, _, _, _, parameters| {
                            if parameters.get::<(bool,)>() == Some((true,)) {
                                lock_vault(&state);
                            }
                        }
                    ),
                );
                state
                    .subscriptions
                    .borrow_mut()
                    .push((connection.clone(), subscription));
                {
                    let path = login_session(&connection).await;
                    if state.closing.get() {
                        return;
                    }
                    if let Some(path) = path {
                        let subscription = connection.signal_subscribe(
                            Some("org.freedesktop.login1"),
                            Some("org.freedesktop.login1.Session"),
                            Some("Lock"),
                            Some(&path),
                            None,
                            gio::DBusSignalFlags::NONE,
                            clone!(
                                #[weak]
                                state,
                                move |_, _, _, _, _, _| lock_vault(&state)
                            ),
                        );
                        state
                            .subscriptions
                            .borrow_mut()
                            .push((connection.clone(), subscription));
                        let subscription = connection.signal_subscribe(
                            Some("org.freedesktop.login1"),
                            Some("org.freedesktop.DBus.Properties"),
                            Some("PropertiesChanged"),
                            Some(&path),
                            Some("org.freedesktop.login1.Session"),
                            gio::DBusSignalFlags::NONE,
                            clone!(
                                #[weak]
                                state,
                                move |_, _, _, _, _, parameters| {
                                    if let Some((_, changed, _)) = parameters.get::<(
                                        String,
                                        std::collections::HashMap<String, glib::Variant>,
                                        Vec<String>,
                                    )>(
                                    ) {
                                        let locked =
                                            changed.get("LockedHint").and_then(|v| v.get::<bool>())
                                                == Some(true);
                                        let inactive =
                                            changed.get("Active").and_then(|v| v.get::<bool>())
                                                == Some(false);
                                        if locked || inactive {
                                            lock_vault(&state);
                                        }
                                    }
                                }
                            ),
                        );
                        state
                            .subscriptions
                            .borrow_mut()
                            .push((connection.clone(), subscription));
                    }
                }
            }
        }
    ));
}

/// Applications started by a systemd user manager belong to no session by PID,
/// which is the norm on current desktops. "auto" finds the user's graphical one.
async fn login_session(connection: &gio::DBusConnection) -> Option<String> {
    for (method, arguments) in [
        ("GetSessionByPID", (std::process::id(),).to_variant()),
        ("GetSession", ("auto",).to_variant()),
    ] {
        let reply = connection
            .call_future(
                Some("org.freedesktop.login1"),
                "/org/freedesktop/login1",
                "org.freedesktop.login1.Manager",
                method,
                Some(&arguments),
                None,
                gio::DBusCallFlags::NONE,
                2000,
            )
            .await;
        if let Some(path) = reply
            .ok()
            .and_then(|reply| reply.try_child_value(0))
            .and_then(|value| value.str().map(str::to_owned))
        {
            return Some(path);
        }
    }
    None
}

fn show_error(window: &impl IsA<gtk4::Window>, message: &str) {
    let dialog = MessageDialog::new(
        Some(window),
        gtk4::DialogFlags::MODAL,
        MessageType::Error,
        gtk4::ButtonsType::Close,
        "Sekeeper",
    );
    dialog.set_secondary_text(Some(message));
    dialog.set_destroy_with_parent(true);
    dialog.connect_response(|dialog, _| dialog.close());
    dialog.present();
}

fn install_css() {
    let provider = gtk4::CssProvider::new();
    provider.load_from_resource("/io/github/sekeeper/Sekeeper/style.css");
    gtk4::style_context_add_provider_for_display(
        &gdk::Display::default().expect("display disponibile"),
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

mod appearance;
mod clipboard;
mod cloud;
mod details;
mod keyfiles;
#[cfg(test)]
mod tests;
mod transfer;
