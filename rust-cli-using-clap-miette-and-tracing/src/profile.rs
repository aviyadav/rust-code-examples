use std::path::Path;

use miette::Result;
use tracing::warn;

use crate::diagnostics::profile_error;

#[derive(Debug, PartialEq, Eq)]
pub struct Profile {
    pub environment: String,
    pub port: u16,
}

pub fn parse_profile(path: &Path, source: &str, strict: bool) -> Result<Profile> {
    let mut environment = None;
    let mut port = None;
    let mut offset = 0;

    for raw_line in source.split_inclusive('\n') {
        let line = raw_line.trim_end_matches(['\r', '\n']);
        let leading = line.len() - line.trim_start().len();
        let trimmed = line.trim();

        if trimmed.is_empty() || trimmed.starts_with('#') {
            offset += raw_line.len();
            continue;
        }

        let body_start = offset + leading;

        let Some((raw_key, raw_value)) = trimmed.split_once('=') else {
            return Err(profile_error(
                path,
                source,
                body_start..body_start + trimmed.len(),
                "expected `key=value`",
                "add an equals sign between the setting name and its value",
                "invalid profile entry",
            ));
        };

        let key = raw_key.trim();
        let value = raw_value.trim();
        let key_span = body_start..body_start + raw_key.len();

        let value_start = body_start
            + raw_key.len()
            + 1
            + (raw_value.len() - raw_value.trim_start().len());

        let value_span = value_start..value_start + value.len();

        match key {
            "environment" => {
                if !matches!(value, "development" | "staging" | "production") {
                    return Err(profile_error(
                        path,
                        source,
                        value_span,
                        "unsupported environment",
                        "use `development`, `staging`, or `production`",
                        format!("unknown environment `{value}`"),
                    ));
                }

                environment = Some(value.to_owned());
            }

            "port" => {
                let parsed_port = value.parse::<u16>().ok().filter(|port| *port > 0);

                let Some(parsed_port) = parsed_port else {
                    return Err(profile_error(
                        path,
                        source,
                        value_span,
                        "expected a valid TCP port",
                        "use a decimal number from 1 to 65535",
                        "`port` must be a number from 1 to 65535",
                    ));
                };

                port = Some(parsed_port);
            }

            unknown if strict => {
                return Err(profile_error(
                    path,
                    source,
                    key_span,
                    "unknown setting",
                    "remove it or run without `--strict` while migrating the file",
                    format!("unknown configuration key `{unknown}`"),
                ));
            }

            unknown => {
                warn!(key = unknown, "ignoring unknown configuration key");
            }
        }

        offset += raw_line.len();
    }

    let environment = environment.ok_or_else(|| {
        profile_error(
            path,
            source,
            0..0,
            "missing `environment` setting",
            "add `environment=development`, `staging`, or `production`",
            "profile is missing an environment",
        )
    })?;

    let port = port.ok_or_else(|| {
        profile_error(
            path,
            source,
            0..0,
            "missing `port` setting",
            "add a line such as `port=8080`",
            "profile is missing a port",
        )
    })?;

    Ok(Profile { environment, port })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_profile() {
        let source = "# production profile\nenvironment=production\nport=8080\n";
        let profile = parse_profile(Path::new("deploy.env"), source, true).unwrap();

        assert_eq!(
            profile,
            Profile {
                environment: "production".to_owned(),
                port: 8080,
            }
        );
    }

    #[test]
    fn allows_unknown_keys_when_not_strict() {
        let source = "environment=staging\nport=3000\nregion=us-east-1\n";
        let profile = parse_profile(Path::new("staging.env"), source, false).unwrap();

        assert_eq!(profile.environment, "staging");
        assert_eq!(profile.port, 3000);
    }

    #[test]
    fn rejects_unknown_keys_in_strict_mode() {
        let source = "environment=staging\nport=3000\nregion=us-east-1\n";
        let err = parse_profile(Path::new("staging.env"), source, true).unwrap_err();

        assert!(err.to_string().contains("unknown configuration key `region`"));
    }

    #[test]
    fn rejects_invalid_port() {
        let source = "environment=production\nport=eighty\n";
        let err = parse_profile(Path::new("deploy.env"), source, false).unwrap_err();

        assert!(err.to_string().contains("`port` must be a number from 1 to 65535"));
    }

    #[test]
    fn rejects_zero_port() {
        let source = "environment=development\nport=0\n";
        let err = parse_profile(Path::new("dev.env"), source, false).unwrap_err();

        assert!(err.to_string().contains("`port` must be a number from 1 to 65535"));
    }

    #[test]
    fn rejects_invalid_environment() {
        let source = "environment=qa\nport=8080\n";
        let err = parse_profile(Path::new("qa.env"), source, false).unwrap_err();

        assert!(err.to_string().contains("unknown environment `qa`"));
    }

    #[test]
    fn rejects_missing_equals() {
        let source = "environment production\nport=8080\n";
        let err = parse_profile(Path::new("bad.env"), source, false).unwrap_err();

        assert!(err.to_string().contains("invalid profile entry"));
    }

    #[test]
    fn rejects_missing_required_fields() {
        let err_env = parse_profile(Path::new("no-env.env"), "port=8080\n", false).unwrap_err();
        assert!(err_env.to_string().contains("profile is missing an environment"));

        let err_port =
            parse_profile(Path::new("no-port.env"), "environment=production\n", false).unwrap_err();
        assert!(err_port.to_string().contains("profile is missing a port"));
    }
}
