//! The command dispatch, and the commands that need no sign-in: `profiles`,
//! `config`, `info`, `completions`, `self-update`. The commands that read
//! Labcorp Patient are in [`crate::commands`].

use clap::{CommandFactory, FromArgMatches};
use pk_cli_config::profile::{ProfileName, Profiles};
use pk_cli_config::ConfigStore;
use pk_cli_core::info::{AuthInfo, CliInfo};
use pk_cli_core::{output, CliError};
use pk_cli_selfupdate::Updater;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::cli::*;
use crate::source;

const REPO: &str = "piekstra/labcorp-cli";
/// `info`'s profiles: health-records/v1 and documents/v1.
pub const PROFILES: &[&str] = &[pk_med_records::PROFILE, pk_cli_documents::PROFILE];

/// The config file: the profiles by name, and in each profile's settings
/// the people a read has listed, by patient id, with the household's labels
/// for them ([`crate::labels`]). No secret, Labcorp id or name is in it.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    /// The Labcorp Patient logins, one per account (`labcorp profiles add
    /// <NAME>`).
    #[serde(default, skip_serializing_if = "Profiles::is_empty")]
    pub profiles: Profiles,
}

pub(crate) struct Ctx<'a> {
    pub cli: &'a Cli,
    pub store: ConfigStore,
}

impl Ctx<'_> {
    pub fn json(&self) -> bool {
        self.cli.common.json
    }

    pub fn quiet(&self) -> bool {
        self.cli.common.quiet
    }

    pub fn note(&self, msg: &str) {
        if !self.cli.common.quiet {
            eprintln!("{msg}");
        }
    }

    pub fn profiles(&self) -> Result<Profiles, CliError> {
        Ok(self.store.load::<Config>()?.profiles)
    }
}

pub fn main() {
    // A reader that closes the pipe early (`labcorp results list | head`)
    // ends the process quietly, as it does any Unix tool, instead of a panic
    // in println!.
    #[cfg(unix)]
    // SAFETY: called first, before any thread starts; it only resets the
    // disposition Rust's runtime set for SIGPIPE back to the default.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let matches = Cli::command().get_matches();
    let mut cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    cli.profile_flag = source::given_on_line(&matches);
    if let Err(e) = run(&cli) {
        std::process::exit(output::fail(&e, cli.common.json));
    }
}

pub fn run(cli: &Cli) -> Result<(), CliError> {
    let ctx = Ctx {
        cli,
        store: ConfigStore::new(BIN).with_override(cli.config.clone()),
    };
    match &cli.command {
        Command::Auth(AuthCmd::Reset { force }) => {
            source::no_profile(cli, "auth reset")?;
            crate::auth::reset(*force, cli.common.interactive(), cli.common.json, cli.common.quiet)
        }
        Command::Auth(cmd) => {
            let profile = source::login_profile(cli, &ctx.profiles()?)?;
            match cmd {
                AuthCmd::Login(args) => crate::auth::login(args, &profile, ctx.json(), ctx.quiet()),
                AuthCmd::Status => crate::auth::status(&profile, ctx.json(), ctx.quiet()),
                AuthCmd::Logout => crate::auth::logout(&profile, ctx.json(), ctx.quiet()),
                AuthCmd::Reset { .. } => unreachable!("handled above"),
            }
        }
        Command::Profiles(cmd) => {
            source::no_profile(cli, "profiles")?;
            profiles_cmd(&ctx, cmd)
        }
        Command::Config(cmd) => {
            source::no_profile(cli, "config")?;
            config(&ctx, cmd)
        }
        Command::Patients(PatientsCmd::Label(args)) => {
            // The profile's settings alone: no vault, no request.
            let profile = source::login_profile(cli, &ctx.profiles()?)?;
            crate::labels::run(&ctx.store, &profile, args, ctx.json(), ctx.quiet())
        }
        Command::Patients(_) | Command::Records(_) | Command::Results(_) | Command::Documents(_) | Command::Api(_) => {
            // The command's own arguments first, so a bad one reaches neither
            // the vault nor the site.
            crate::commands::check_args(cli)?;
            let profile = source::login_profile(cli, &ctx.profiles()?)?;
            crate::commands::run(&ctx, &profile)
        }
        Command::SelfUpdate(args) => {
            source::no_profile(cli, "self-update")?;
            Updater {
                repo: REPO.into(),
                binary: BIN.into(),
                target: env!("BUILD_TARGET").into(),
                current: env!("CARGO_PKG_VERSION").into(),
            }
            .run(args, cli.common.json, cli.common.quiet)
        }
        Command::Completions { shell } => {
            source::no_profile(cli, "completions")?;
            clap_complete::generate(*shell, &mut Cli::command(), BIN, &mut std::io::stdout());
            Ok(())
        }
        Command::Info => {
            source::no_profile(cli, "info")?;
            output::json(&serde_json::to_value(info()).expect("CliInfo serializes"));
            Ok(())
        }
    }
}

/// `info`: cli-info/v1, with the health-records/v1 and documents/v1
/// profiles, `login-profiles` since every command that reads records takes
/// `--profile`, and `patient-labels` for `patients label`.
pub fn info() -> CliInfo {
    CliInfo::new(
        BIN,
        env!("CARGO_PKG_VERSION"),
        &format!("https://github.com/{REPO}"),
        AuthInfo {
            required: true,
            method: "browser-session".into(),
            login_hint: Some(format!("{BIN} auth login --browser --profile <NAME>")),
        },
        &[
            "profiles",
            "patients",
            "records",
            "results",
            "documents",
            "api",
            pk_med_records::LOGIN_PROFILES,
            pk_med_records::PATIENT_LABELS,
        ],
    )
    .with_profiles(PROFILES)
}

// -------------------------------------------------------------- profiles

/// `profiles list|add|remove` (cli-common DESIGN.md §2.5): the logins, by
/// name, in the config. Removing one removes what labcorp keeps for it
/// first.
fn profiles_cmd(ctx: &Ctx, cmd: &ProfilesCmd) -> Result<(), CliError> {
    match cmd {
        ProfilesCmd::List => {
            let list = ctx.profiles()?.list();
            if ctx.json() {
                output::json(&serde_json::to_value(&list).unwrap_or_default());
            } else if list.profiles.is_empty() {
                ctx.note(&format!(
                    "no profiles: `{BIN} profiles add <NAME>`, one per Labcorp Patient account"
                ));
            } else {
                for p in &list.profiles {
                    println!("{}", p.name);
                }
            }
            Ok(())
        }
        ProfilesCmd::Add { name } => {
            let name = ProfileName::parse(name)?;
            let added = {
                let _lock = crate::labels::lock(&ctx.store)?;
                let mut cfg: Config = ctx.store.load()?;
                let added = cfg.profiles.add(name.clone());
                if added {
                    ctx.store.save(&cfg)?;
                }
                added
            };
            output::emit(
                ctx.json(),
                "profile-add",
                json!({ "profile": name.as_str(), "added": added }),
                |_| {
                    ctx.note(&if added {
                        format!("added profile {name}; sign in with `{BIN} auth login --browser --profile {name}`")
                    } else {
                        format!("profile {name} is already configured")
                    })
                },
            );
            Ok(())
        }
        ProfilesCmd::Remove { name, force } => {
            let name = ProfileName::parse(name)?;
            // Whether it can ask, before the config or the vault is read.
            pk_cli_core::confirm::require_confirmable(*force, ctx.cli.common.interactive(), "remove a profile")?;
            if !ctx.profiles()?.contains(&name) {
                return Err(CliError::NotFound(format!("no profile `{name}`"))
                    .with_remedy(format!("`{BIN} profiles list` shows the configured ones")));
            }
            pk_cli_core::confirm::confirm(
                *force,
                &format!("Remove profile {name}, its stored session and its browser profile?"),
            )?;
            let done = crate::auth::forget_login(&name)?;
            {
                let _lock = crate::labels::lock(&ctx.store)?;
                let mut cfg: Config = ctx.store.load()?;
                cfg.profiles.remove(&name);
                ctx.store.save(&cfg)?;
            }
            let mut v = json!({ "profile": name.as_str(), "session_removed": done.session_removed });
            if let Some(removed) = done.browser_profile_removed {
                v["browser_profile_removed"] = removed.into();
            }
            output::emit(ctx.json(), "profile-remove", v, |_| {
                ctx.note(&format!(
                    "removed profile {name}: {}",
                    if done.session_removed {
                        "its session was removed"
                    } else {
                        "no session was stored"
                    }
                ))
            });
            Ok(())
        }
    }
}

// ---------------------------------------------------------------- config

fn config(ctx: &Ctx, cmd: &ConfigCmd) -> Result<(), CliError> {
    match cmd {
        ConfigCmd::Path => {
            let path = ctx.store.path()?;
            if ctx.json() {
                output::json(&json!({ "schema": "config-path/v1", "path": path.display().to_string() }));
            } else {
                println!("{}", path.display());
            }
            Ok(())
        }
        ConfigCmd::Show => {
            let cfg: Config = ctx.store.load()?;
            let mut v = serde_json::to_value(&cfg).unwrap_or_default();
            crate::labels::summarize(&mut v);
            if ctx.json() {
                v["schema"] = "config/v1".into();
                output::json(&v);
            } else if v.as_object().is_some_and(|o| o.is_empty()) {
                ctx.note(&format!("no settings (`{BIN} profiles add <NAME>` adds a profile)"));
            } else {
                output::render(&v);
            }
            Ok(())
        }
    }
}
