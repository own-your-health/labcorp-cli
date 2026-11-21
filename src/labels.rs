//! The household's own names for the people a profile reads: `patients
//! label` (medical-common DESIGN.md §1). One sign-in reads its account, its
//! dependents and the accounts shared with it, so `auth login` cannot name
//! them; a label is set per person afterwards.
//!
//! The labels live in the profile's settings in labcorp's config, beside
//! every person a read of the account has listed, by their patient id.
//! `patients label` checks its `--patient` against those, so it makes no
//! request and reads no secret. Nothing here keeps a name Labcorp shows.

use std::collections::BTreeMap;

use pk_cli_config::profile::ProfileName;
use pk_cli_config::ConfigStore;
use pk_cli_core::CliError;
use pk_med_records::dto::kind;
use pk_med_records::PatientContext;
use serde::{Deserialize, Serialize};

use crate::app::Config;
use crate::cli::{LabelArgs, BIN};
use crate::fhir;
use crate::portal::{Kind, Person};

/// A profile's settings: each person a read of the account has listed, by
/// patient id.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Members {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub members: BTreeMap<String, Member>,
    /// Settings this build does not know (a later labcorp's), written back
    /// as they were read.
    #[serde(flatten)]
    pub other: serde_json::Map<String, serde_json::Value>,
}

/// What labcorp keeps of a person between runs: never a name.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    /// The household's own name for the person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// `self` for the account signed in, `proxy` for a dependent or an
    /// account shared with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relationship: Option<String>,
    /// The latest read of the account did not list them: the sharing ended,
    /// or Labcorp's answer was short. Kept, with their label, until they are
    /// listed again or the profile is removed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unlisted: bool,
    /// Fields this build does not know, written back as they were read.
    #[serde(flatten)]
    pub other: serde_json::Map<String, serde_json::Value>,
}

/// Whether `q` (a `--patient` value) is the id `id`, case aside: a digest
/// may be written in either case.
pub fn is_the_id(q: &str, id: &str) -> bool {
    q.eq_ignore_ascii_case(id)
}

/// Whether `q` (a `--patient` value) is the label `label`, case aside.
pub fn is_the_label(q: &str, label: Option<&str>) -> bool {
    label.is_some_and(|l| l.eq_ignore_ascii_case(q))
}

impl Members {
    /// The id of the person whose label is `q`, case aside.
    fn by_label(&self, q: &str) -> Option<&str> {
        self.members
            .iter()
            .find(|(_, m)| is_the_label(q, m.label.as_deref()))
            .map(|(id, _)| id.as_str())
    }

    /// Whether `q` is a person's id, case aside.
    fn is_id(&self, q: &str) -> bool {
        self.members.keys().any(|id| is_the_id(q, id))
    }
}

fn relationship(p: &Person) -> &'static str {
    match p.kind {
        Kind::Account => pk_med_records::dto::relationship::SELF,
        Kind::Dependent | Kind::Linked => pk_med_records::dto::relationship::PROXY,
    }
}

/// After a read of the account: `people` given the labels the profile holds
/// for them, and the people the read listed added to the profile's
/// settings. Nobody is removed, so an answer that lists fewer people keeps
/// every label. The config is written only when a person is new, comes
/// back, or changed relationship, or one is no longer listed, and then from
/// a fresh read of it, changing no label: a `patients label` saved since
/// the first read keeps its label.
pub fn record(store: &ConfigStore, profile: &ProfileName, people: &mut [Person]) -> Result<(), CliError> {
    let members: Members = store.load::<Config>()?.profiles.settings(profile)?;
    let id_of: Vec<String> = people.iter().map(fhir::patient_id).collect();
    let stale = |p: &Person, m: Option<&Member>| {
        m.is_none_or(|m| m.relationship.as_deref() != Some(relationship(p)) || m.unlisted)
    };
    let listed = |id: &str| id_of.iter().any(|i| i == id);
    let changed = people
        .iter()
        .zip(&id_of)
        .any(|(p, id)| stale(p, members.members.get(id)))
        || members.members.iter().any(|(id, m)| !m.unlisted && !listed(id));
    for (p, id) in people.iter_mut().zip(&id_of) {
        p.label = members.members.get(id).and_then(|m| m.label.clone());
    }
    if !changed {
        return Ok(());
    }
    let mut config = store.load::<Config>()?;
    let mut fresh: Members = config.profiles.settings(profile)?;
    for (id, m) in fresh.members.iter_mut() {
        m.unlisted = !listed(id);
    }
    for (p, id) in people.iter().zip(&id_of) {
        let m = fresh.members.entry(id.clone()).or_default();
        m.relationship = Some(relationship(p).into());
        m.unlisted = false;
    }
    config.profiles.set_settings(profile, &fresh)?;
    store.save(&config)
}

/// `patients label --profile <NAME> --patient <ID> (<WHO> | --clear)`.
pub fn run(
    store: &ConfigStore,
    profile: &ProfileName,
    args: &LabelArgs,
    json: bool,
    quiet: bool,
) -> Result<(), CliError> {
    let who = match (args.who.as_deref().map(str::trim), args.clear) {
        (Some(""), false) => {
            return Err(CliError::Usage(
                "the label is empty: give the household's name for the person, or --clear".into(),
            ))
        }
        (Some(w), false) => Some(w.to_string()),
        (None, true) => None,
        _ => return Err(CliError::Usage("give a label or --clear, one of the two".into())),
    };
    let mut config = store.load::<Config>()?;
    let before: Members = config.profiles.settings(profile)?;
    let mut members = before.clone();
    // No echo of --patient: it may be an id, which is handled as Labcorp's
    // own id for the person (medical-common DESIGN.md §1.3).
    let q = args.patient.trim();
    let id = match members.members.keys().find(|id| is_the_id(q, id)) {
        Some(id) => id.clone(),
        None => match members.by_label(q) {
            Some(id) => id.to_string(),
            None => {
                return Err(CliError::NotFound(format!(
                    "--patient names no one profile `{profile}` has listed"
                ))
                .with_remedy(format!(
                    "`{BIN} patients list --profile {profile}` reads the account's people; give an id or a label it prints"
                )))
            }
        },
    };
    if let Some(w) = &who {
        if members.is_id(w) {
            return Err(CliError::Usage(
                "a label cannot be a person's id: --patient could not tell them apart".into(),
            ));
        }
        if let Some(holder) = members.by_label(w).filter(|holder| *holder != id) {
            // Never moved: a person the latest read left out may be back on
            // the next one, which a script would not hear about. The remedy
            // names the holder by the label typed, never by id.
            let whom = if members.members.get(holder).is_some_and(|m| m.unlisted) {
                "someone the latest `patients list` did not list"
            } else {
                "another person on this profile"
            };
            return Err(
                CliError::Usage(format!("{whom} has that label; a label names one person")).with_remedy(format!(
                    "clear it first: `{BIN} patients label --profile {profile} --patient {} --clear`",
                    shell_word(w)
                )),
            );
        }
    }
    let member = members
        .members
        .get_mut(&id)
        .expect("the id was found among the members");
    if member.unlisted && !quiet {
        eprintln!("this person was not in the latest `patients list`: the sharing may have ended");
    }
    member.label = who;
    let member = member.clone();
    if members != before {
        config.profiles.set_settings(profile, &members)?;
        store.save(&config)?;
    }
    print(json, &id, &member);
    Ok(())
}

/// `w` as one shell word: as it is when it needs no quoting, else in single
/// quotes.
fn shell_word(w: &str) -> String {
    if !w.is_empty() && w.chars().all(|c| c.is_ascii_alphanumeric() || "-_.@+".contains(c)) {
        w.to_string()
    } else {
        format!("'{}'", w.replace('\'', r"'\''"))
    }
}

/// The person as `patient-list/v1` holding them alone, or a line on the
/// person's own terminal.
fn print(json: bool, id: &str, m: &Member) {
    if json {
        let context = PatientContext {
            id: id.to_string(),
            organization: fhir::ORGANIZATION_ID.into(),
            label: m.label.clone(),
            relationship: m.relationship.clone(),
            kind: Some(kind::PATIENT.into()),
            member_role: None,
            fhir_user: None,
            name: None,
            session: None,
        };
        pk_med_records::patient_list(vec![context]).emit(true);
        return;
    }
    match &m.label {
        Some(l) => println!("{id} is labelled `{l}`"),
        None => println!("{id} has no label"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(id: &str, kind: Kind) -> Person {
        Person {
            id: id.into(),
            kind,
            linked_id: None,
            first_name: None,
            middle_name: None,
            last_name: None,
            birth_date: None,
            gender: None,
            label: None,
        }
    }

    fn store(name: &str, body: &str) -> (std::path::PathBuf, ConfigStore) {
        let dir = std::env::temp_dir().join(format!("labcorp-labels-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, body).unwrap();
        let store = ConfigStore::new(BIN).with_override(Some(path.clone()));
        (dir, store)
    }

    #[test]
    fn a_read_adds_people_and_keeps_their_labels() {
        let (dir, store) = store("read", r#"{"profiles":{"adult1":{}}}"#);
        let profile = ProfileName::parse("adult1").unwrap();
        let (alex, sam) = (person("1", Kind::Account), person("2", Kind::Dependent));
        let (alex_id, sam_id) = (fhir::patient_id(&alex), fhir::patient_id(&sam));
        let mut read = vec![alex.clone(), sam.clone()];
        record(&store, &profile, &mut read).unwrap();
        // A label set after the read; the next read lists Alex alone.
        let mut config = store.load::<Config>().unwrap();
        let mut members: Members = config.profiles.settings(&profile).unwrap();
        members.members.get_mut(&alex_id).unwrap().label = Some("me".into());
        config.profiles.set_settings(&profile, &members).unwrap();
        store.save(&config).unwrap();
        let mut again = vec![alex.clone()];
        record(&store, &profile, &mut again).unwrap();
        assert_eq!(again[0].label.as_deref(), Some("me"));
        let kept: Members = store.load::<Config>().unwrap().profiles.settings(&profile).unwrap();
        assert_eq!(kept.members[&alex_id].label.as_deref(), Some("me"));
        assert_eq!(kept.members[&alex_id].relationship.as_deref(), Some("self"));
        assert_eq!(kept.members[&sam_id].relationship.as_deref(), Some("proxy"));
        // Nobody is removed by a read that lists fewer people: marked, kept.
        assert!(kept.members[&sam_id].unlisted);
        assert!(!kept.members[&alex_id].unlisted);
        // The config holds no Labcorp id and no name.
        let raw = std::fs::read_to_string(dir.join("config.json")).unwrap();
        assert!(!raw.contains("\"1\"") && !raw.contains("\"2\""), "{raw}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn settings_this_build_does_not_know_survive_a_write() {
        let profile = ProfileName::parse("adult1").unwrap();
        let alex = person("1", Kind::Account);
        let id = fhir::patient_id(&alex);
        let body = format!(
            r#"{{"profiles":{{"adult1":{{"later":1,"members":{{"{id}":{{"label":"me","also_later":true}}}}}}}}}}"#
        );
        let (dir, store) = store("later", &body);
        let mut read = vec![alex];
        record(&store, &profile, &mut read).unwrap();
        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("config.json")).unwrap()).unwrap();
        let adult1 = &raw["profiles"]["adult1"];
        assert_eq!(adult1["later"], 1, "{raw}");
        assert_eq!(adult1["members"][&id]["also_later"], true, "{raw}");
        assert_eq!(adult1["members"][&id]["relationship"], "self", "{raw}");
        assert_eq!(read[0].label.as_deref(), Some("me"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_label_in_a_remedy_is_one_shell_word() {
        assert_eq!(shell_word("kid"), "kid");
        assert_eq!(shell_word("big kid"), "'big kid'");
        assert_eq!(shell_word("o'brien"), r"'o'\''brien'");
        assert_eq!(shell_word("$(x)"), "'$(x)'");
    }
}
