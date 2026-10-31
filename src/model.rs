use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

#[derive(Clone, Default, Deserialize, Serialize, Zeroize, ZeroizeOnDrop)]
pub struct Credential {
    pub id: String,
    pub description: String,
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub address: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<u64>,
}

impl Credential {
    pub fn same_content(&self, other: &Self) -> bool {
        self.description == other.description
            && self.username == other.username
            && self.password == other.password
            && self.address == other.address
            && self.notes == other.notes
    }
}

pub fn matching_credentials<'a>(credentials: &'a [Credential], query: &str) -> Vec<&'a Credential> {
    let query = query.trim().to_lowercase();
    credentials
        .iter()
        .filter(|credential| {
            // Secrets stay out of the search: password and notes are never matched.
            [
                &credential.description,
                &credential.username,
                &credential.address,
            ]
            .iter()
            .any(|field| field.to_lowercase().contains(&query))
        })
        .collect()
}

#[derive(Clone, Default, Deserialize, Serialize, Zeroize, ZeroizeOnDrop)]
pub struct VaultData {
    pub credentials: Vec<Credential>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<Revision>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lineage: Vec<RevisionLink>,
}

// Authenticated ancestry only: no credentials or previous vault contents.
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize, Zeroize, ZeroizeOnDrop)]
pub struct RevisionLink {
    pub id: String,
    pub parents: Vec<String>,
}

#[derive(Clone, Deserialize, Serialize, Zeroize, ZeroizeOnDrop)]
pub struct Revision {
    pub vault_id: String,
    pub id: String,
    pub parents: Vec<String>,
    pub created_at: u64,
    pub device: String,
}

#[cfg(test)]
mod tests;
