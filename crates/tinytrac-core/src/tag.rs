use serde::{Deserialize, Serialize};

use crate::TagId;

/// What time is tracked against. Deliberately neutral: a client for a freelancer,
/// a project for an employee, or any other category.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tag {
    pub id: TagId,
    name: String,
    /// Archived tags are hidden from pickers but keep their history.
    pub archived: bool,
    /// Link to the same entity in another system, e.g. a client in a PM tool.
    pub external: Option<ExternalRef>,
}

/// A reference to a record in an external system.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalRef {
    /// Which system and entity type, e.g. `"agency-pm/client"`.
    pub source: String,
    /// The record's id in that system.
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TagError {
    #[error("tag name cannot be empty")]
    EmptyName,
}

impl Tag {
    pub fn new(name: &str) -> Result<Self, TagError> {
        Ok(Self {
            id: TagId::generate(),
            name: validate(name)?,
            archived: false,
            external: None,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn rename(&mut self, name: &str) -> Result<(), TagError> {
        self.name = validate(name)?;
        Ok(())
    }
}

fn validate(name: &str) -> Result<String, TagError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(TagError::EmptyName);
    }
    Ok(name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_trimmed_and_must_not_be_blank() {
        let mut tag = Tag::new("  Acme ").unwrap();
        assert_eq!(tag.name(), "Acme");
        assert_eq!(tag.rename("   "), Err(TagError::EmptyName));
        assert_eq!(tag.name(), "Acme");
    }
}
