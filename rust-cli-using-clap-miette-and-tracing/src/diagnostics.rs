use std::{ops::Range, path::Path};

use miette::{LabeledSpan, NamedSource, Report, miette};

pub fn profile_error(
    path: &Path,
    source: &str,
    span: Range<usize>,
    label: &'static str,
    help: &'static str,
    message: impl Into<String>,
) -> Report {
    miette!(
        labels = vec![LabeledSpan::at(span, label)],
        help = help,
        "{}",
        message.into()
    )
    .with_source_code(NamedSource::new(
        path.display().to_string(),
        source.to_owned(),
    ))
}
