use super::*;

const CLEAR_AFTER_SECONDS: u32 = 30;
// Klipper and compatible history tools do not record content carrying this type.
const HISTORY_HINT: &str = "x-kde-passwordManagerHint";

#[derive(Default)]
pub struct State {
    timer: RefCell<Option<glib::SourceId>>,
}

fn clipboard(state: &SharedState) -> gdk::Clipboard {
    WidgetExt::display(&state.window).clipboard()
}

pub fn copy(state: &SharedState, text: &str, what: &str) {
    let provider = gdk::ContentProvider::new_union(&[
        gdk::ContentProvider::for_value(&text.to_value()),
        gdk::ContentProvider::for_bytes(HISTORY_HINT, &glib::Bytes::from_static(b"secret")),
    ]);
    if clipboard(state).set_content(Some(&provider)).is_err() {
        show_error(&state.window, "Impossibile usare gli appunti del desktop.");
        return;
    }
    if let Some(timer) = state.clipboard.timer.borrow_mut().take() {
        timer.remove();
    }
    let timer = glib::timeout_add_seconds_local_once(
        CLEAR_AFTER_SECONDS,
        clone!(
            #[weak]
            state,
            move || {
                state.clipboard.timer.borrow_mut().take();
                empty(&state);
            }
        ),
    );
    *state.clipboard.timer.borrow_mut() = Some(timer);
    toast(
        state,
        &format!("{what}: gli appunti si svuotano tra {CLEAR_AFTER_SECONDS} secondi"),
    );
}

/// Leaves anything the user copied from another application untouched.
fn empty(state: &SharedState) {
    let clipboard = clipboard(state);
    if clipboard.is_local() {
        let _ = clipboard.set_content(None::<&gdk::ContentProvider>);
    }
}

pub fn clear(state: &SharedState) {
    if let Some(timer) = state.clipboard.timer.borrow_mut().take() {
        timer.remove();
        empty(state);
    }
}
