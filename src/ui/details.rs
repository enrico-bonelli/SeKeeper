use super::*;

const HIDDEN_PASSWORD: &str = "••••••••••••";

pub struct State {
    pub root: Stack,
    avatar: Label,
    title: Label,
    username: Label,
    password: Label,
    reveal: gtk4::ToggleButton,
    copy_username: Button,
    copy_password: Button,
    copy_address: Button,
    address: Label,
    address_row: Box,
    notes: Label,
    notes_card: Box,
    dates: Label,
    selected: RefCell<Option<String>>,
}

fn icon_button(icon: &str, tooltip: &str) -> Button {
    let button = Button::from_icon_name(icon);
    button.set_tooltip_text(Some(tooltip));
    button.set_valign(Align::Center);
    button.add_css_class("flat");
    button
}

fn value_label() -> Label {
    let label = Label::new(None);
    label.set_halign(Align::Start);
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
    label.set_selectable(true);
    label.add_css_class("field-value");
    label
}

fn field_row(title: &str, value: &Label) -> (Box, Box) {
    let row = Box::new(Orientation::Horizontal, 6);
    row.add_css_class("field-row");
    let text = Box::new(Orientation::Vertical, 2);
    text.set_hexpand(true);
    let caption = Label::new(Some(title));
    caption.set_halign(Align::Start);
    caption.add_css_class("field-label");
    text.append(&caption);
    text.append(value);
    row.append(&text);
    let actions = Box::new(Orientation::Horizontal, 2);
    row.append(&actions);
    (row, actions)
}

impl State {
    pub fn new(edit: &Button, delete: &Button) -> Self {
        let root = Stack::new();
        root.set_hexpand(true);

        let placeholder = Box::new(Orientation::Vertical, 8);
        placeholder.set_valign(Align::Center);
        placeholder.set_halign(Align::Center);
        let image = gtk4::Image::from_icon_name(crate::APP_ID);
        image.set_pixel_size(84);
        image.set_opacity(0.55);
        placeholder.append(&image);
        let hint = Label::new(Some("Seleziona una credenziale"));
        hint.add_css_class("placeholder-title");
        placeholder.append(&hint);
        let detail = Label::new(Some("Qui potrai copiarne i dati senza mostrarli."));
        detail.add_css_class("dim-label");
        placeholder.append(&detail);
        root.add_named(&placeholder, Some("empty"));

        let content = Box::new(Orientation::Vertical, 18);
        content.add_css_class("details");
        let header = Box::new(Orientation::Horizontal, 14);
        let avatar = Label::new(None);
        avatar.add_css_class("avatar");
        avatar.add_css_class("avatar-large");
        avatar.set_valign(Align::Center);
        header.append(&avatar);
        let title = Label::new(None);
        title.add_css_class("details-title");
        title.set_hexpand(true);
        title.set_halign(Align::Start);
        title.set_xalign(0.0);
        title.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        header.append(&title);
        edit.set_valign(Align::Center);
        delete.set_valign(Align::Center);
        header.append(edit);
        header.append(delete);
        content.append(&header);

        let card = Box::new(Orientation::Vertical, 0);
        card.add_css_class("field-card");
        let username = value_label();
        let (row, actions) = field_row("Username", &username);
        let copy_username = icon_button("edit-copy-symbolic", "Copia username");
        actions.append(&copy_username);
        card.append(&row);

        let password = value_label();
        password.set_selectable(false);
        password.add_css_class("field-secret");
        let (row, actions) = field_row("Password", &password);
        let reveal = gtk4::ToggleButton::new();
        reveal.set_icon_name("view-reveal-symbolic");
        reveal.set_tooltip_text(Some("Mostra la password"));
        reveal.set_valign(Align::Center);
        reveal.add_css_class("flat");
        actions.append(&reveal);
        let copy_password = icon_button("edit-copy-symbolic", "Copia password");
        actions.append(&copy_password);
        card.append(&row);

        let address = value_label();
        let (address_row, actions) = field_row("Indirizzo", &address);
        let copy_address = icon_button("edit-copy-symbolic", "Copia indirizzo");
        actions.append(&copy_address);
        card.append(&address_row);
        content.append(&card);

        let notes_card = Box::new(Orientation::Vertical, 4);
        notes_card.add_css_class("field-card");
        notes_card.add_css_class("notes");
        let caption = Label::new(Some("Note"));
        caption.set_halign(Align::Start);
        caption.add_css_class("field-label");
        notes_card.append(&caption);
        let notes = value_label();
        notes_card.append(&notes);
        content.append(&notes_card);

        let dates = Label::new(None);
        dates.set_halign(Align::Start);
        dates.set_wrap(true);
        dates.add_css_class("dim-label");
        content.append(&dates);

        let scroller = ScrolledWindow::new();
        scroller.set_hscrollbar_policy(gtk4::PolicyType::Never);
        scroller.set_child(Some(&content));
        root.add_named(&scroller, Some("credential"));
        root.set_visible_child_name("empty");
        Self {
            root,
            avatar,
            title,
            username,
            password,
            reveal,
            copy_username,
            copy_password,
            copy_address,
            address,
            address_row,
            notes,
            notes_card,
            dates,
            selected: RefCell::new(None),
        }
    }
}

// The password is fetched only when requested, so hidden values never sit in a
// label and an entry changed by a synchronization cannot show stale data.
fn selected(state: &SharedState) -> Option<Credential> {
    let id = state.details.selected.borrow().clone()?;
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

fn connect_copy(
    state: &SharedState,
    button: &Button,
    what: &'static str,
    field: fn(&Credential) -> &str,
) {
    button.connect_clicked(clone!(
        #[weak]
        state,
        move |_| {
            if let Some(credential) = selected(&state) {
                clipboard::copy(&state, field(&credential), what);
            }
        }
    ));
}

pub fn connect(state: &SharedState) {
    let details = &state.details;
    connect_copy(state, &details.copy_username, "Username copiato", |c| {
        &c.username
    });
    connect_copy(state, &details.copy_password, "Password copiata", |c| {
        &c.password
    });
    connect_copy(state, &details.copy_address, "Indirizzo copiato", |c| {
        &c.address
    });
    details.reveal.connect_toggled(clone!(
        #[weak]
        state,
        move |button| {
            let password = button.is_active().then(|| selected(&state)).flatten();
            match password {
                Some(credential) => state.details.password.set_text(&credential.password),
                None => state.details.password.set_text(HIDDEN_PASSWORD),
            }
        }
    ));
}

pub fn update(state: &SharedState, credential: Option<&Credential>) {
    let details = &state.details;
    details.reveal.set_active(false);
    details.password.set_text(HIDDEN_PASSWORD);
    let Some(credential) = credential else {
        *details.selected.borrow_mut() = None;
        details.username.set_text("");
        details.address.set_text("");
        details.notes.set_text("");
        details.title.set_text("");
        details.root.set_visible_child_name("empty");
        return;
    };
    *details.selected.borrow_mut() = Some(credential.id.clone());
    set_avatar(&details.avatar, &credential.description, true);
    details.title.set_text(&credential.description);
    details
        .title
        .set_tooltip_text(Some(&credential.description));
    details.username.set_text(&credential.username);
    details.address.set_text(&credential.address);
    details
        .address_row
        .set_visible(!credential.address.is_empty());
    details.notes.set_text(&credential.notes);
    details.notes_card.set_visible(!credential.notes.is_empty());
    details.dates.set_text(&format!(
        "Creata: {}  ·  Modificata: {}",
        format_timestamp(credential.created_at),
        format_timestamp(credential.updated_at)
    ));
    details.root.set_visible_child_name("credential");
}
