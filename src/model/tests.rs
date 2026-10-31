use super::*;

fn entry(id: &str, description: &str) -> Credential {
    Credential {
        id: id.into(),
        description: description.into(),
        username: "utente".into(),
        password: "public test secret".into(),
        address: String::new(),
        notes: String::new(),
        created_at: None,
        updated_at: None,
    }
}

#[test]
fn search_is_partial_unicode_case_insensitive_and_keeps_stable_ids() {
    let mut entries = [
        entry("first", "Posta personale"),
        entry("second", "CaffÈ — account"),
        entry("third", "Posta lavoro"),
    ];
    entries[1].username = "Barista".into();
    entries[2].address = "https://mail.example.org".into();
    entries[0].notes = "appunto riservato".into();
    let ids = |query: &str| {
        matching_credentials(&entries, query)
            .iter()
            .map(|c| c.id.as_str())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(" POSTA "), ["first", "third"]);
    assert_eq!(ids("caffè"), ["second"]);
    assert_eq!(ids("lavoro"), ["third"]);
    assert_eq!(ids("  "), ["first", "second", "third"]);
    assert_eq!(ids("barista"), ["second"]);
    assert_eq!(ids("EXAMPLE.org"), ["third"]);
    assert!(ids("appunto").is_empty());
    assert!(ids("public test secret").is_empty());
    assert!(ids("nessun risultato").is_empty());
}

#[test]
fn existing_entries_without_dates_remain_unknown_and_new_dates_roundtrip() {
    let old = r#"{"id":"old","description":"Demo","username":"demo","password":"public"}"#;
    let mut credential: Credential = serde_json::from_str(old).unwrap();
    assert_eq!(credential.created_at, None);
    assert_eq!(credential.updated_at, None);
    credential.created_at = Some(1_700_000_000);
    credential.updated_at = Some(1_700_000_100);
    let encoded = serde_json::to_string(&credential).unwrap();
    let decoded: Credential = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded.created_at, credential.created_at);
    assert_eq!(decoded.updated_at, credential.updated_at);
}
