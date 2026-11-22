//! Configures Sekeeper for Android with one QR code: the key file and, when S3
//! is set up, the bucket with its access keys. Nothing has to be copied or
//! typed on the phone except the master password, which never leaves the PC.
//! The code is shown only after a confirmation, for a limited time, and
//! disappears when the vault locks.

use super::*;
use crate::s3;
use qrcode::{Color, EcLevel, QrCode};
use sekeeper_core::keyfile::KeyFile;
use sekeeper_core::setup::DeviceSetup;

const VISIBLE_SECONDS: u32 = 60;
const QUIET_ZONE: usize = 4;

/// Access keys put in the QR code, possibly different from this PC's own.
pub struct Keys {
    access: Zeroizing<String>,
    secret: Zeroizing<String>,
}

pub fn confirm(state: &SharedState) {
    let config = state.cloud.active_config();
    let dialog = Dialog::builder()
        .title("Configura il telefono")
        .transient_for(&state.window)
        .modal(true)
        .default_width(520)
        .build();
    track_dialog(state, &dialog);
    dialog.add_button("Annulla", ResponseType::Cancel);
    let show = dialog.add_button("Mostra il QR code", ResponseType::Accept);
    show.add_css_class("suggested-action");
    let content = dialog.content_area();
    content.set_spacing(12);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(20);
    content.set_margin_end(20);
    let warning = Label::new(Some(&format!(
        "Il QR code contiene il file chiave{}: chi lo fotografa ottiene tutto tranne la password \
         principale. Assicurati che nessuno veda lo schermo. Resta visibile per {VISIBLE_SECONDS} \
         secondi; il supporto con il file chiave deve essere collegato.",
        if config.is_some() {
            " e l'accesso al bucket S3"
        } else {
            ""
        }
    )));
    warning.set_wrap(true);
    warning.set_max_width_chars(60);
    content.append(&warning);

    let include = gtk4::CheckButton::with_label("Includi le chiavi AWS di questo PC");
    let access = Entry::new();
    access.set_placeholder_text(Some("Access Key ID dedicata al telefono"));
    let secret = PasswordEntry::new();
    secret.set_show_peek_icon(true);
    if let Some(config) = &config {
        let destination = Label::new(Some(&format!(
            "Bucket {} · {} · {}",
            config.bucket, config.region, config.prefix
        )));
        destination.set_halign(Align::Start);
        destination.add_css_class("dim-label");
        content.append(&destination);
        include.set_active(true);
        content.append(&include);
        let expander = gtk4::Expander::new(Some("Usa invece chiavi dedicate al telefono"));
        let fields = Box::new(Orientation::Vertical, 6);
        fields.set_margin_top(6);
        let hint = Label::new(Some(
            "Chiavi dedicate permettono di revocare solo il telefono se lo perdi.",
        ));
        hint.set_wrap(true);
        hint.set_halign(Align::Start);
        hint.add_css_class("dim-label");
        fields.append(&hint);
        fields.append(&access);
        secret.set_placeholder_text(Some("Secret Access Key"));
        fields.append(&secret);
        expander.set_child(Some(&fields));
        content.append(&expander);
    }
    let message = Label::new(None);
    message.add_css_class("error");
    message.set_wrap(true);
    content.append(&message);
    dialog.connect_response(clone!(
        #[weak]
        state,
        #[weak]
        include,
        #[weak]
        access,
        #[weak]
        secret,
        #[weak]
        message,
        move |dialog, response| {
            if response != ResponseType::Accept {
                scrub_widget(dialog.upcast_ref());
                dialog.close();
                return;
            }
            let dedicated = Keys {
                access: Zeroizing::new(access.text().trim().to_string()),
                secret: Zeroizing::new(secret.text().trim().to_string()),
            };
            scrub_widget(dialog.upcast_ref());
            let Some(config) = config.clone() else {
                dialog.close();
                transfer(&state, None, None);
                return;
            };
            if !dedicated.access.is_empty() || !dedicated.secret.is_empty() {
                if dedicated.access.is_empty() || dedicated.secret.is_empty() {
                    message.set_text("Inserisci sia l'Access Key ID sia la Secret Access Key.");
                    return;
                }
                dialog.close();
                transfer(&state, Some(config), Some(dedicated));
                return;
            }
            if !include.is_active() {
                dialog.close();
                transfer(&state, Some(config), None);
                return;
            }
            // The keyring may ask to be unlocked: never block the window.
            dialog.set_sensitive(false);
            message.set_text("Lettura delle chiavi dal portachiavi…");
            let lookup = config.clone();
            job(
                move || s3::Credentials::load(&lookup),
                clone!(
                    #[weak]
                    state,
                    #[weak]
                    dialog,
                    #[weak]
                    message,
                    move |result: Result<s3::Credentials, String>| {
                        dialog.set_sensitive(true);
                        match result {
                            Ok(credentials) if !credentials.session_token.is_empty() => {
                                message.set_text(
                                    "Questo PC usa chiavi temporanee con session token: non possono \
                                     essere trasferite. Usa chiavi dedicate al telefono.",
                                );
                            }
                            Ok(credentials) => {
                                dialog.close();
                                let keys = Keys {
                                    access: Zeroizing::new(credentials.access_key.clone()),
                                    secret: Zeroizing::new(credentials.secret_key.clone()),
                                };
                                transfer(&state, Some(config), Some(keys));
                            }
                            Err(error) => message.set_text(&error),
                        }
                    }
                ),
            );
        }
    ));
    dialog.present();
}

/// Builds the QR text from the key file read now from its medium and shows it.
pub(super) fn transfer(state: &SharedState, config: Option<s3::Config>, keys: Option<Keys>) {
    if state.vault.borrow().is_none() {
        return;
    }
    let file = match state.keyfile.load_file() {
        Ok(file) => file,
        Err(error) => {
            show_error(&state.window, &error.to_string());
            return;
        }
    };
    let (text, note) = match &config {
        None => (
            KeyFile::transfer_text(file.as_slice()).map_err(|e| e.to_string()),
            "Contiene il file chiave. Configura S3 sul telefono.",
        ),
        Some(config) => {
            let (access, secret) = keys.as_ref().map_or(("", ""), |keys| {
                (keys.access.as_str(), keys.secret.as_str())
            });
            (
                DeviceSetup::new(
                    file.as_slice(),
                    &config.bucket,
                    &config.region,
                    &config.prefix,
                    access,
                    secret,
                )
                .map(|setup| setup.to_text())
                .map_err(|e| e.to_string()),
                if keys.is_some() {
                    "Contiene il file chiave, il bucket e le chiavi AWS."
                } else {
                    "Contiene il file chiave e il bucket. Inserisci le chiavi AWS sul telefono."
                },
            )
        }
    };
    match text.and_then(|text| matrix(&text)) {
        Ok(matrix) => show(state, matrix, note),
        Err(error) => show_error(&state.window, &error),
    }
}

/// QR modules as a square grid of dark cells, quiet zone included.
struct Matrix {
    size: usize,
    dark: Zeroizing<Vec<u8>>,
}

fn matrix(text: &str) -> Result<Matrix, String> {
    let code = QrCode::with_error_correction_level(text.as_bytes(), EcLevel::M)
        .map_err(|error| format!("Impossibile creare il QR code: {error}"))?;
    let width = code.width();
    let size = width + 2 * QUIET_ZONE;
    let mut dark = Zeroizing::new(vec![0u8; size * size]);
    for (index, color) in code.to_colors().into_iter().enumerate() {
        if color == Color::Dark {
            let (row, column) = (index / width + QUIET_ZONE, index % width + QUIET_ZONE);
            dark[row * size + column] = 1;
        }
    }
    Ok(Matrix { size, dark })
}

fn show(state: &SharedState, matrix: Matrix, note: &str) {
    let dialog = Dialog::builder()
        .title("Configurazione per il telefono")
        .transient_for(&state.window)
        .modal(true)
        .build();
    track_dialog(state, &dialog);
    dialog.add_button("Chiudi", ResponseType::Close);
    let content = dialog.content_area();
    content.set_spacing(14);
    content.set_margin_top(20);
    content.set_margin_bottom(20);
    content.set_margin_start(24);
    content.set_margin_end(24);
    let explanation = Label::new(Some(&format!(
        "In Sekeeper per Android scegli «Inquadra il QR dal PC». {note}"
    )));
    explanation.set_wrap(true);
    explanation.set_max_width_chars(46);
    content.append(&explanation);

    let area = gtk4::DrawingArea::new();
    area.set_content_width(400);
    area.set_content_height(400);
    area.set_halign(Align::Center);
    area.set_draw_func(move |_, cairo, width, height| {
        // Always black on white, whatever the desktop theme.
        let side = f64::from(width.min(height));
        let cell = side / matrix.size as f64;
        cairo.set_source_rgb(1.0, 1.0, 1.0);
        cairo.rectangle(0.0, 0.0, side, side);
        let _ = cairo.fill();
        cairo.set_source_rgb(0.0, 0.0, 0.0);
        for row in 0..matrix.size {
            for column in 0..matrix.size {
                if matrix.dark[row * matrix.size + column] == 1 {
                    cairo.rectangle(
                        column as f64 * cell,
                        row as f64 * cell,
                        cell + 0.5,
                        cell + 0.5,
                    );
                }
            }
        }
        let _ = cairo.fill();
    });
    content.append(&area);

    let countdown = Label::new(None);
    countdown.add_css_class("dim-label");
    content.append(&countdown);
    let remaining = Rc::new(Cell::new(VISIBLE_SECONDS));
    countdown.set_text(&format!("Si chiude tra {VISIBLE_SECONDS} secondi"));
    let timer = glib::timeout_add_seconds_local(
        1,
        clone!(
            #[weak]
            dialog,
            #[weak]
            countdown,
            #[upgrade_or]
            glib::ControlFlow::Break,
            move || {
                let left = remaining.get().saturating_sub(1);
                remaining.set(left);
                if left == 0 {
                    dialog.close();
                    return glib::ControlFlow::Break;
                }
                countdown.set_text(&format!("Si chiude tra {left} secondi"));
                glib::ControlFlow::Continue
            }
        ),
    );
    let timer = Rc::new(RefCell::new(Some(timer)));
    dialog.connect_destroy(move |_| {
        if let Some(timer) = timer.borrow_mut().take() {
            timer.remove();
        }
    });
    dialog.connect_response(|dialog, _| dialog.close());
    dialog.connect_close_request(|dialog| {
        // The drawing holds the secrets: destroy it rather than hide it.
        dialog.destroy();
        Propagation::Proceed
    });
    dialog.present();
}
