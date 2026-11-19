//! The commands that read Labcorp Patient as a profile: `api`.

use pk_cli_config::profile::ProfileName;
use pk_cli_core::{output, CliError};

use crate::app::Ctx;
use crate::cli::*;
use crate::client::{Client, Endpoints};
use crate::session::SessionStore;

/// The command's own arguments, checked before the vault or the site is
/// touched.
pub fn check_args(cli: &Cli) -> Result<(), CliError> {
    if let Command::Api(args) = &cli.command {
        if !args.method.eq_ignore_ascii_case("GET") {
            return Err(CliError::Usage(format!(
                "`{BIN} api` is read-only: it sends GET only, not {}",
                args.method
            )));
        }
        if !args.path.starts_with('/') && !args.path.starts_with(&format!("{}/", crate::site::API_BASE)) {
            return Err(CliError::Usage(format!(
                "a path on the portal API starts with `/`, or is a URL under {}",
                crate::site::API_BASE
            )));
        }
    }
    Ok(())
}

pub(crate) fn run(ctx: &Ctx, profile: &ProfileName) -> Result<(), CliError> {
    let store = SessionStore::open()?;
    let session = store.load(profile)?;
    let mut client = Client::new(profile, session, &store, Endpoints::default())?;
    match &ctx.cli.command {
        Command::Api(args) => {
            let v = client.get_json(&args.path)?;
            output::json(&v);
            Ok(())
        }
        _ => unreachable!("app::run sends only the login commands here"),
    }
}
