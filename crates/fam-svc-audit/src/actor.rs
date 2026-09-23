//! Who made the request.
//!
//! The audit log is worth little if every line says `user`, so the identity is part of the record
//! rather than a property of the process: a person at the keyboard, the program itself, or an agent
//! holding a named token. On the wire an actor is one string - `user`, `system`, `agent:<label>` -
//! because a nested object would add about ten bytes to every line for information that fits in
//! twelve characters.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// How long a token label may be in a record. Labels are chosen by whoever issues the token
/// (increment 11), and an unbounded one is a way to write an arbitrarily large audit line through a
/// field nobody reads.
const MAX_LABEL_UNITS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Actor {
    /// A person, through the shell.
    User,
    /// The program's own bookkeeping - the seal at the end of a session, for instance.
    System,
    /// An agent holding a token, named by whatever label the token carries.
    Agent(String),
}

impl Actor {
    /// The label an agent carries. Empty becomes `unnamed` rather than being rejected: a record
    /// that says "some agent" beats a missing record.
    pub fn agent(label: &str) -> Self {
        let trimmed = label.trim();
        let mut characters: Vec<char> = trimmed.chars().take(MAX_LABEL_UNITS).collect();
        if trimmed.chars().count() > MAX_LABEL_UNITS {
            characters.truncate(MAX_LABEL_UNITS.saturating_sub(1));
            characters.push('~');
        }
        let label: String = characters.into_iter().collect();
        if label.is_empty() {
            return Self::Agent("unnamed".into());
        }
        Self::Agent(label)
    }

    pub fn encoded(&self) -> String {
        match self {
            Self::User => "user".into(),
            Self::System => "system".into(),
            Self::Agent(label) => format!("agent:{label}"),
        }
    }

    /// The inverse of [`Actor::encoded`]. A label keeps any colons it holds, so the two round-trip.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "user" => Some(Self::User),
            "system" => Some(Self::System),
            other => other
                .strip_prefix("agent:")
                .map(|label| Self::Agent(label.to_owned())),
        }
    }
}

impl fmt::Display for Actor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.encoded())
    }
}

impl Serialize for Actor {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.encoded())
    }
}

impl<'de> Deserialize<'de> for Actor {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        // Whatever a reader cannot name is kept verbatim rather than dropped: a record whose actor
        // is spelled unknown is still a record, and `verify` must not lose lines to be pedantic.
        Ok(Self::parse(&text).unwrap_or_else(|| Self::Agent(unknown_label(&text))))
    }
}

fn unknown_label(text: &str) -> String {
    let limit = MAX_LABEL_UNITS + 16;
    let clipped: String = text.chars().take(limit).collect();
    format!("?{clipped}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_actors_encode_distinctly() {
        assert_eq!(Actor::User.encoded(), "user");
        assert_eq!(Actor::System.encoded(), "system");
        assert_eq!(
            Actor::agent("read-only-demo").encoded(),
            "agent:read-only-demo"
        );
    }

    #[test]
    fn every_form_round_trips_including_a_label_with_a_colon() {
        for actor in [
            Actor::User,
            Actor::System,
            Actor::agent("demo"),
            Actor::agent("team:build:bot"),
        ] {
            assert_eq!(Actor::parse(&actor.encoded()).as_ref(), Some(&actor));
        }
    }

    #[test]
    fn a_blank_label_still_leaves_a_record_that_says_an_agent_asked() {
        assert_eq!(Actor::agent("   "), Actor::agent(""));
        assert_eq!(Actor::agent("").encoded(), "agent:unnamed");
    }

    #[test]
    fn a_long_label_is_clipped_and_says_so() {
        let actor = Actor::agent(&"t".repeat(200));
        let encoded = actor.encoded();
        assert_eq!(encoded.chars().count(), "agent:".len() + MAX_LABEL_UNITS);
        assert!(encoded.ends_with('~'));
    }

    /// A label is escaped, not sanitised, so a quote in one cannot break the line - the record test
    /// proves that. A spelling this version does not know is kept rather than dropped, so an audit
    /// file written by a newer build still verifies line by line.
    #[test]
    fn an_unknown_spelling_survives_as_itself() {
        let actor = Actor::parse("robot:main");
        assert_eq!(actor, None, "only the three named forms parse");
        let record: Actor = serde_json::from_str(r#""robot:main""#).expect("parses");
        assert_eq!(record.encoded(), "agent:?robot:main");
    }
}
