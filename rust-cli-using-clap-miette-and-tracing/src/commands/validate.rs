use miette::{Result, miette};
use tracing::{debug, info};

use crate::{cli::ValidateArgs, profile::parse_profile};

pub fn validate(args: ValidateArgs) -> Result<()> {
    let source = std::fs::read_to_string(&args.file).map_err(|error| {
        miette!(
            "could not read {}: {error}",
            args.file.display()
        )
    })?;

    debug!(
        path = %args.file.display(),
        strict = args.strict,
        bytes = source.len(),
        "validating profile"
    );

    let profile = parse_profile(&args.file, &source, args.strict)?;

    info!(
        environment = %profile.environment,
        port = profile.port,
        "profile is valid"
    );

    println!(
        "{} is valid: environment={}, port={}",
        args.file.display(),
        profile.environment,
        profile.port
    );

    Ok(())
}
