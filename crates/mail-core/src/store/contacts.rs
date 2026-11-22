//! The people in your mail, for filling in To and Cc as you type. Nothing
//! is asked of Google: the list is made from the mail already stored, so it
//! works offline and needs no access to your contacts.

use std::collections::HashMap;

use rusqlite::params;

use super::Store;
use crate::{compose::split_address_list, gmail::split_address, model::Account};

/// Someone you have written to, or who has written to you.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Contact {
    /// Their name as mail gives it; empty when only the address is known.
    pub name: String,
    pub address: String,
}

impl Contact {
    /// Whether what was typed starts their name, a later word of it, their
    /// address or its domain. Case doesn't matter.
    pub fn matches(&self, typed: &str) -> bool {
        let typed = typed.trim().to_lowercase();
        if typed.is_empty() {
            return false;
        }
        let name = self.name.to_lowercase();
        let address = self.address.to_lowercase();
        let starts_word = |text: &str| {
            text.match_indices(typed.as_str()).any(|(at, _)| {
                text[..at]
                    .chars()
                    .next_back()
                    .is_none_or(|before| !before.is_alphanumeric())
            })
        };
        starts_word(&name) || address.starts_with(&typed) || starts_word(&address)
    }

    /// As a recipient: `Ana Lima <ana@example.org>`, or the bare address.
    /// A name with a comma is quoted, so it stays one recipient.
    pub fn recipient(&self) -> String {
        let name: String = self
            .name
            .chars()
            .filter(|c| !c.is_control() && !"<>\"".contains(*c))
            .collect();
        let name = name.trim();
        if name.is_empty() || name.eq_ignore_ascii_case(&self.address) {
            self.address.clone()
        } else if name.contains([',', ';']) {
            format!("\"{name}\" <{}>", self.address)
        } else {
            format!("{name} <{}>", self.address)
        }
    }
}

/// What is known of one address while the list is made.
#[derive(Default)]
struct Seen {
    name: String,
    address: String,
    /// How many of your messages went to them.
    written_to: usize,
    /// How many of theirs you have.
    heard_from: usize,
    /// The newest mail either way, Unix milliseconds.
    last: i64,
    /// When the name was last seen, so the newest spelling wins.
    named_at: i64,
}

impl Store {
    /// Everyone in an account's stored mail, the people you write to most
    /// first, then those who write to you most, the most recent first among
    /// equals. Addresses that only send automated mail are left out unless
    /// you have written to them.
    pub fn contacts(&self, account: &Account) -> anyhow::Result<Vec<Contact>> {
        let mut seen: HashMap<String, Seen> = HashMap::new();
        let mut note = |name: &str, address: &str, date: i64, to_them: bool| {
            let address = address.trim();
            let valid = address
                .split_once('@')
                .is_some_and(|(local, domain)| !local.is_empty() && domain.contains('.'));
            if !valid || address.eq_ignore_ascii_case(&account.email) {
                return;
            }
            let entry = seen.entry(address.to_lowercase()).or_default();
            // Spelled as it first was; mail is read oldest first.
            if entry.address.is_empty() {
                entry.address = address.to_owned();
            }
            let name = name.trim();
            if !name.is_empty() && (entry.name.is_empty() || date >= entry.named_at) {
                entry.name = name.to_owned();
                entry.named_at = date;
            }
            if to_them {
                entry.written_to += 1;
            } else {
                entry.heard_from += 1;
            }
            entry.last = entry.last.max(date);
        };

        let mut statement = self.connection().prepare(
            "SELECT from_name, from_addr, to_addrs, cc_addrs, internal_date FROM messages
             WHERE account_id = ?1 ORDER BY internal_date, id",
        )?;
        let mut rows = statement.query(params![account.id])?;
        while let Some(row) = rows.next()? {
            let from_addr: String = row.get(1)?;
            let date: i64 = row.get(4)?;
            if from_addr.eq_ignore_ascii_case(&account.email) {
                for column in [2, 3] {
                    let list: String = row.get(column)?;
                    for entry in split_address_list(&list) {
                        let (name, address) = split_address(&entry);
                        note(&name, &address, date, true);
                    }
                }
            } else {
                let from_name: String = row.get(0)?;
                note(&from_name, &from_addr, date, false);
            }
        }

        let mut people: Vec<Seen> = seen
            .into_values()
            .filter(|person| person.written_to > 0 || !is_automated(&person.address))
            .collect();
        people.sort_by(|a, b| {
            (b.written_to, b.heard_from, b.last)
                .cmp(&(a.written_to, a.heard_from, a.last))
                .then_with(|| a.address.cmp(&b.address))
        });
        Ok(people
            .into_iter()
            .map(|person| Contact {
                name: person.name,
                address: person.address,
            })
            .collect())
    }
}

/// Addresses nobody reads, such as `no-reply@` and `bounces@`.
fn is_automated(address: &str) -> bool {
    let local = address
        .split('@')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let letters: String = local.chars().filter(char::is_ascii_alphanumeric).collect();
    ["noreply", "donotreply", "mailerdaemon", "postmaster"]
        .iter()
        .any(|robot| letters.contains(robot))
        || local.starts_with("bounce")
        || local.starts_with("notification")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Message;

    const ME: &str = "me@example.com";

    fn mail(id: &str, date: i64, from: (&str, &str), to: &str, cc: &str) -> Message {
        Message {
            gmail_id: id.into(),
            thread_id: id.into(),
            internal_date: date,
            from_name: from.0.into(),
            from_addr: from.1.into(),
            to: to.into(),
            cc: cc.into(),
            ..Message::default()
        }
    }

    fn contacts(messages: &[Message]) -> Vec<Contact> {
        let mut store = Store::in_memory();
        let account = store.add_account(ME).unwrap();
        store.save_messages(account.id, messages).unwrap();
        store.contacts(&account).unwrap()
    }

    fn addresses(contacts: &[Contact]) -> Vec<&str> {
        contacts.iter().map(|c| c.address.as_str()).collect()
    }

    #[test]
    fn people_you_write_to_come_before_people_who_write_to_you() {
        let found = contacts(&[
            mail("1", 10, ("Shop", "orders@shop.example"), ME, ""),
            mail("2", 20, ("Shop", "orders@shop.example"), ME, ""),
            mail("3", 30, ("Shop", "orders@shop.example"), ME, ""),
            mail(
                "4",
                40,
                ("Me", ME),
                "Ana Lima <ana@example.org>",
                "bo@example.com",
            ),
            mail(
                "5",
                50,
                ("Me", "ME@example.com"),
                "\"Lima, Ana\" <Ana@Example.org>",
                "",
            ),
            mail("6", 60, ("Cy", "cy@example.net"), ME, ""),
        ]);
        assert_eq!(
            addresses(&found),
            [
                "ana@example.org",
                "bo@example.com",
                "orders@shop.example",
                "cy@example.net"
            ]
        );
        // The newest spelling of a name, and never yourself.
        assert_eq!(found[0].name, "Lima, Ana");
        assert_eq!(found[0].recipient(), "\"Lima, Ana\" <ana@example.org>");
        assert_eq!(found[1].recipient(), "bo@example.com");
        assert_eq!(found[3].recipient(), "Cy <cy@example.net>");
    }

    #[test]
    fn automated_senders_are_left_out_unless_you_wrote_to_them() {
        let found = contacts(&[
            mail("1", 10, ("Shop", "no-reply@shop.example"), ME, ""),
            mail("2", 20, ("Bank", "DoNotReply@bank.example"), ME, ""),
            mail("3", 30, ("Rail", "noreply@rail.example"), ME, ""),
            mail("4", 40, ("Me", ME), "noreply@rail.example", ""),
            mail("5", 50, ("Broken", "not an address"), ME, ""),
        ]);
        assert_eq!(addresses(&found), ["noreply@rail.example"]);
    }

    #[test]
    fn typing_matches_the_start_of_a_name_word_or_of_the_address() {
        let ana = Contact {
            name: "Ana Lima".into(),
            address: "a.lima@studio.example".into(),
        };
        for typed in ["an", "ANA", "lim", "ana l", "a.li", "studio", "lima@"] {
            assert!(ana.matches(typed), "{typed}");
        }
        for typed in ["", "  ", "na", "ima", "example.org", "xyz"] {
            assert!(!ana.matches(typed), "{typed}");
        }
    }
}
