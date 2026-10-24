use super::*;

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const INTERFACE: &str = "org.freedesktop.portal.Settings";
const NAMESPACE: &str = "org.freedesktop.appearance";
const KEY: &str = "color-scheme";

pub fn initialize() {
    if std::env::var_os("GTK_THEME").is_none()
        && let Some(dark) = configured_scheme()
    {
        apply(dark);
    }
}

fn configured_scheme() -> Option<bool> {
    let config = glib::user_config_dir();
    let gtk = std::fs::read_to_string(config.join("gtk-4.0/settings.ini")).unwrap_or_default();
    if let Some(value) = ini_value(&gtk, "Settings", "gtk-application-prefer-dark-theme") {
        match value {
            "true" | "1" => return Some(true),
            "false" | "0" => return Some(false),
            _ => {}
        }
    }
    if std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .split(':')
        .any(|desktop| desktop.eq_ignore_ascii_case("KDE"))
    {
        let kde = std::fs::read_to_string(config.join("kdeglobals")).unwrap_or_default();
        return kde_scheme(&kde);
    }
    None
}

fn ini_value<'a>(text: &'a str, section: &str, key: &str) -> Option<&'a str> {
    let mut current = "";
    let mut value = None;
    for line in text.lines().map(str::trim) {
        if line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            current = name;
        } else if current == section
            && let Some((name, setting)) = line.split_once('=')
            && name.trim() == key
        {
            value = Some(setting.trim());
        }
    }
    value
}

fn kde_scheme(text: &str) -> Option<bool> {
    let color = ini_value(text, "Colors:Window", "BackgroundNormal")?;
    let channels: Vec<u8> = color
        .split(',')
        .map(|value| value.trim().parse())
        .collect::<Result<_, _>>()
        .ok()?;
    let [r, g, b] = channels.as_slice() else {
        return None;
    };
    // Perceived brightness of the actual palette also handles custom schemes
    // whose names do not contain "Dark".
    Some(u32::from(*r) * 299 + u32::from(*g) * 587 + u32::from(*b) * 114 < 128_000)
}

fn apply(dark: bool) {
    let Some(settings) = gtk4::Settings::default() else {
        return;
    };
    settings.set_gtk_application_prefer_dark_theme(dark);
    // GTK >= 4.20 communicates the scheme through this newer property too.
    // Runtime lookup preserves our GTK 4.6 minimum and existing Rust bindings.
    if let Some(property) = settings.find_property("gtk-interface-color-scheme")
        && let Some(class) = glib::EnumClass::with_type(property.value_type())
        && let Some(value) = class.to_value_by_nick(if dark { "dark" } else { "light" })
    {
        settings.set_property_from_value(property.name(), &value);
    }
}

fn portal_scheme(mut value: glib::Variant) -> Option<bool> {
    // Read returns two variant wrappers on older portal implementations;
    // ReadOne and SettingChanged return one. Accept both representations.
    for _ in 0..2 {
        if !value.is::<glib::Variant>() {
            break;
        }
        value = value.as_variant()?;
    }
    match value.get::<u32>()? {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    }
}

pub fn watch(state: &SharedState) {
    if std::env::var_os("GTK_THEME").is_some() {
        return;
    }
    let baseline = gtk4::Settings::default()
        .is_some_and(|settings| settings.is_gtk_application_prefer_dark_theme());
    glib::MainContext::default().spawn_local(clone!(
        #[weak]
        state,
        async move {
            let Ok(connection) = gio::bus_get_future(gio::BusType::Session).await else {
                return;
            };
            if state.closing.get() {
                return;
            }
            let changes = Rc::new(Cell::new(0u64));
            let subscription = connection.signal_subscribe(
                Some(PORTAL),
                Some(INTERFACE),
                Some("SettingChanged"),
                Some(PATH),
                Some(NAMESPACE),
                gio::DBusSignalFlags::NONE,
                clone!(
                    #[weak]
                    state,
                    #[strong]
                    changes,
                    move |_, _, _, _, _, parameters| {
                        if state.closing.get() {
                            return;
                        }
                        if let Some((namespace, key, value)) =
                            parameters.get::<(String, String, glib::Variant)>()
                            && namespace == NAMESPACE
                            && key == KEY
                        {
                            changes.set(changes.get().wrapping_add(1));
                            apply(portal_scheme(value).unwrap_or(baseline));
                        }
                    }
                ),
            );
            state
                .subscriptions
                .borrow_mut()
                .push((connection.clone(), subscription));
            let generation = changes.get();
            for method in ["ReadOne", "Read"] {
                if let Ok(reply) = connection
                    .call_future(
                        Some(PORTAL),
                        PATH,
                        INTERFACE,
                        method,
                        Some(&(NAMESPACE, KEY).to_variant()),
                        None,
                        gio::DBusCallFlags::NONE,
                        1500,
                    )
                    .await
                {
                    if !state.closing.get()
                        && changes.get() == generation
                        && let Some(value) = reply.try_child_value(0)
                    {
                        apply(portal_scheme(value).unwrap_or(baseline));
                    }
                    break;
                }
            }
        }
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_settings_respect_sections_and_custom_dark_and_light_palettes() {
        let settings = "# example\n[Other]\ngtk-application-prefer-dark-theme=false\n[Settings]\ngtk-application-prefer-dark-theme = true\n";
        assert_eq!(
            ini_value(settings, "Settings", "gtk-application-prefer-dark-theme"),
            Some("true")
        );
        assert_eq!(
            kde_scheme(
                "[Colors:View]\nBackgroundNormal=255,255,255\n[Colors:Window]\nBackgroundNormal=32,35,38"
            ),
            Some(true)
        );
        assert_eq!(
            kde_scheme("[Colors:Window]\nBackgroundNormal=240,240,240"),
            Some(false)
        );
        for text in [
            "",
            "[Colors:Window]\nBackgroundNormal=bad",
            "[Colors:Window]\nBackgroundNormal=0,0,999",
            "[Colors:Window]\nBackgroundNormal=0,0",
        ] {
            assert_eq!(kde_scheme(text), None);
        }
    }

    #[test]
    fn portal_dark_light_default_and_legacy_wrappers_are_understood() {
        for (value, expected) in [(0u32, None), (1, Some(true)), (2, Some(false)), (42, None)] {
            let value = value.to_variant();
            assert_eq!(portal_scheme(value.clone()), expected);
            let wrapped = glib::Variant::from_variant(&value);
            assert_eq!(portal_scheme(wrapped.clone()), expected);
            assert_eq!(
                portal_scheme(glib::Variant::from_variant(&wrapped)),
                expected
            );
        }
        assert_eq!(portal_scheme("wrong type".to_variant()), None);
    }
}
