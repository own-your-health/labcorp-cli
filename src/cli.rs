//! The command tree (piekstra-cli/1, health-records/v1, documents/v1).

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use clap_complete::Shell;
use pk_cli_browser::BrowserArgs;
use pk_cli_core::CommonArgs;
use pk_cli_selfupdate::SelfUpdateArgs;

pub const BIN: &str = "labcorp";

/// Labcorp lab results and their report PDFs, read from Labcorp Patient
/// (patient.labcorp.com) as the signed-in account. Read-only; unofficial.
#[derive(Parser, Debug)]
#[command(name = BIN, version, about, long_about = None)]
pub struct Cli {
    #[command(flatten)]
    pub common: CommonArgs,

    /// Override the config file location.
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Read Labcorp Patient as this profile: one configured with `labcorp
    /// profiles add <NAME>` and signed in with `labcorp auth login --browser
    /// --profile <NAME>`. There is no default profile.
    #[arg(long, global = true, env = "LABCORP_PROFILE", value_name = "NAME")]
    pub profile: Option<String>,

    /// Whether --profile was given as a flag rather than through
    /// LABCORP_PROFILE (`source::given_on_line`).
    #[arg(skip)]
    pub profile_flag: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// The Labcorp Patient sign-in, one per --profile.
    #[command(subcommand)]
    Auth(AuthCmd),
    /// The Labcorp Patient logins: one profile per account (profile-list/v1).
    #[command(subcommand)]
    Profiles(ProfilesCmd),
    /// One read of Labcorp Patient's own API as --profile <NAME>, printed as
    /// the JSON it answers: `GET <PATH>` under
    /// https://portal-api.patient.cws.labcorp.com. Read-only: no other
    /// method is sent.
    Api(ApiArgs),
    /// Where the config file is, and what it holds.
    #[command(subcommand)]
    Config(ConfigCmd),
    /// Update to the latest release from GitHub.
    SelfUpdate(SelfUpdateArgs),
    /// Print a shell completion script.
    Completions { shell: Shell },
    /// Machine-readable capability discovery (cli-info/v1).
    Info,
}

#[derive(Subcommand, Debug)]
pub enum AuthCmd {
    /// Sign in to Labcorp Patient as --profile <NAME> and keep the session in
    /// labcorp's vault: encrypted files under ~/.config/labcorp/vault/,
    /// sealed with a key held in the OS keychain (auth-login/v1).
    ///
    /// --browser opens a Chrome window on patient.labcorp.com in the
    /// profile's own browser profile, where the account signs in on
    /// Labcorp's own page. One read of the account checks the session before
    /// it is stored, unless --no-verify.
    Login(LoginCmd),
    /// Whether the profile's session is stored, when it was captured and
    /// until when it is good (auth-status/v1). Sends no request.
    Status,
    /// Remove the profile's stored session, then the browser profile its
    /// --browser sign-in keeps (auth-logout/v1).
    Logout,
    /// Remove every profile's stored session from the vault and the vault's
    /// key from the keychain (auth-reset/v1): the way out when the key is
    /// lost or damaged. Asks first; without a terminal it needs --force
    /// (exit 6).
    Reset {
        /// Do not ask.
        #[arg(long)]
        force: bool,
    },
}

// The family's browser flags with labcorp's own beside them (cli-common
// DESIGN.md §2.2). A plain comment: a doc comment here would replace `auth
// login`'s about.
#[derive(Args, Debug)]
pub struct LoginCmd {
    #[command(flatten)]
    pub browser: BrowserArgs,
    /// Replace a session already stored for the profile.
    #[arg(long)]
    pub overwrite: bool,
    /// Store the session without the read that checks it.
    #[arg(long)]
    pub no_verify: bool,
}

#[derive(Subcommand, Debug)]
pub enum ProfilesCmd {
    /// The configured profiles, by name (profile-list/v1).
    #[command(visible_alias = "ls")]
    List,
    /// Add a profile: 1 to 32 lowercase letters, digits, `-` or `_`,
    /// starting with a letter or a digit (profile-add/v1).
    Add { name: String },
    /// Remove a profile with its stored session and the browser profile its
    /// --browser sign-in keeps (profile-remove/v1). Asks first; without a
    /// terminal it needs --force (exit 6).
    Remove {
        name: String,
        /// Do not ask.
        #[arg(long)]
        force: bool,
    },
}

/// `labcorp api GET <PATH>`: read-only, so the method is GET alone.
#[derive(Args, Debug, Clone)]
pub struct ApiArgs {
    /// GET (any case). No other method is sent.
    pub method: String,
    /// A path on https://portal-api.patient.cws.labcorp.com (leading /), or
    /// a URL on it.
    pub path: String,
}

#[derive(Subcommand, Debug)]
pub enum ConfigCmd {
    /// Print the config file path.
    Path,
    /// Show the configuration (the profiles; it holds no secret).
    Show,
}
