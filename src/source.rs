//! Which login a command reads as: `--profile <NAME>`, with `LABCORP_PROFILE`
//! as the environment's fallback (medical-common DESIGN.md §1.1). There is
//! no default profile, and a lone profile is never picked.

use clap::parser::ValueSource;
use clap::ArgMatches;
use pk_cli_config::profile::{self, ProfileName, Profiles};
use pk_cli_core::CliError;

use crate::cli::{Cli, BIN};

pub const PROFILE_ENV: &str = "LABCORP_PROFILE";

/// The profile a command signs in as: the flag, or clap's value from
/// `LABCORP_PROFILE`, which must be configured. A missing or unknown one is
/// exit 2 naming the configured ones, or `labcorp profiles add <NAME>` when
/// there are none.
pub fn resolve_profile(given: Option<&str>, configured: &Profiles) -> Result<ProfileName, CliError> {
    profile::resolve(given.filter(|g| !g.is_empty()), PROFILE_ENV, configured, BIN)
}

/// The profile of a command that uses a login.
pub fn login_profile(cli: &Cli, configured: &Profiles) -> Result<ProfileName, CliError> {
    resolve_profile(cli.profile.as_deref(), configured)
}

/// Whether `--profile` was given on the command line rather than through
/// its variable. A global flag given after a subcommand is that
/// subcommand's, so every level is asked.
pub fn given_on_line(matches: &ArgMatches) -> bool {
    let mut m = matches;
    let mut on_line = m.value_source("profile") == Some(ValueSource::CommandLine);
    while let Some((_, sub)) = m.subcommand() {
        m = sub;
        on_line |= m.value_source("profile") == Some(ValueSource::CommandLine);
    }
    on_line
}

/// For a command that needs no sign-in: `--profile` on its command line is
/// exit 2, and `LABCORP_PROFILE` is not read.
pub fn no_profile(cli: &Cli, command: &str) -> Result<(), CliError> {
    if cli.profile_flag {
        return Err(CliError::Usage(format!(
            "`{BIN} {command}` needs no sign-in and takes no --profile"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured(names: &[&str]) -> Profiles {
        let mut p = Profiles::new();
        for n in names {
            p.add(ProfileName::parse(n).unwrap());
        }
        p
    }

    #[test]
    fn a_profile_must_be_named_and_configured() {
        let two = configured(&["adult1", "adult2"]);
        assert_eq!(resolve_profile(Some("adult2"), &two).unwrap().as_str(), "adult2");
        let none = resolve_profile(None, &two).err().unwrap();
        assert_eq!(none.exit_code(), 2);
        assert!(none.to_string().contains("adult1"), "{none}");
        let unknown = resolve_profile(Some("adult3"), &two).err().unwrap();
        assert_eq!(unknown.exit_code(), 2);
        // A lone profile is never picked.
        let one = configured(&["adult2"]);
        assert_eq!(resolve_profile(None, &one).err().unwrap().exit_code(), 2);
        // None configured names the command that adds one.
        let e = resolve_profile(None, &Profiles::new()).err().unwrap();
        assert!(e.to_string().contains("profiles add") || e.remedy().is_some_and(|r| r.contains("profiles add")));
        assert_eq!(resolve_profile(Some(""), &one).err().unwrap().exit_code(), 2);
    }
}
