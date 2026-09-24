use std::env;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};

fn normalize(mut line: String) -> String {
    let trimmed_length = line.trim_end().len();
    line.truncate(trimmed_length);
    line
}

fn parse_fields(line: &str) -> Result<(&str, &str, &str), String> {
    let mut parts = line.splitn(4, ' ');

    let timestamp = parts
        .next()
        .ok_or_else(|| "missing timestamp".to_string())?;

    let level = parts
        .next()
        .ok_or_else(|| "missing log level".to_string())?;

    let token_field = parts
        .next()
        .ok_or_else(|| "missing token field".to_string())?;

    let message_field = parts
        .next()
        .ok_or_else(|| "missing message field".to_string())?;

    let token = token_field
        .strip_prefix("token=")
        .ok_or_else(|| "token field must start with `token=`".to_string())?;

    let message = message_field
        .strip_prefix("message=")
        .ok_or_else(|| "message field must start with `message=`".to_string())?;

    if token.is_empty() {
        return Err("token cannot be empty".to_string());
    }

    if message.is_empty() {
        return Err("message cannot be empty".to_string());
    }

    Ok((timestamp, level, message))
}

fn parse_template_fields(line: &str) -> Result<(&str, &str, &str, &str), String> {
    let mut parts = line.splitn(4, ' ');

    let timestamp = parts
        .next()
        .ok_or_else(|| "missing timestamp".to_string())?;

    let level = parts
        .next()
        .ok_or_else(|| "missing log level".to_string())?;

    let token_field = parts
        .next()
        .ok_or_else(|| "missing token field".to_string())?;

    let message_field = parts
        .next()
        .ok_or_else(|| "missing message field".to_string())?;

    let token = token_field
        .strip_prefix("token=")
        .ok_or_else(|| "token field must start with `token=`".to_string())?;

    let message = message_field
        .strip_prefix("message=")
        .ok_or_else(|| "message field must start with `message=`".to_string())?;

    if token.is_empty() {
        return Err("token cannot be empty".to_string());
    }

    if message.is_empty() {
        return Err("message cannot be empty".to_string());
    }

    Ok((timestamp, level, token, message))
}

fn validate_line(line: &str) -> Result<(), String> {
    let (timestamp, level, message) = parse_fields(line)?;

    if timestamp.len() != 20 || !timestamp.contains('T') || !timestamp.ends_with('Z') {
        return Err(format!("invalid timestamp `{timestamp}`"));
    }

    if !matches!(level, "DEBUG" | "INFO" | "WARN" | "ERROR") {
        return Err(format!("unsupported log level `{level}`"));
    }

    if message.trim().is_empty() {
        return Err("message cannot be empty".to_string());
    }

    Ok(())
}

fn redact_token(line: &mut String) -> Result<(), String> {
    let marker = "token=";

    let token_start = line
        .find(marker)
        .ok_or_else(|| "missing token field".to_string())?
        + marker.len();

    let remaining = &line[token_start..];

    let token_length = remaining.find(' ').unwrap_or(remaining.len());

    let token_end = token_start + token_length;

    if token_start == token_end {
        return Err("token cannot be empty".to_string());
    }

    let mask = "*".repeat(line[token_start..token_end].chars().count());

    line.replace_range(token_start..token_end, &mask);

    Ok(())
}

fn process_line(line: String) -> Result<(), String> {
    let mut line = normalize(line);

    validate_line(&line)?;
    redact_token(&mut line)?;

    let (timestamp, level, message) = parse_fields(&line)?;

    let base_address = line.as_ptr() as usize;

    let timestamp_offset = timestamp.as_ptr() as usize - base_address;
    let level_offset = level.as_ptr() as usize - base_address;
    let message_offset = message.as_ptr() as usize - base_address;

    println!(
        "{level:<5} {timestamp} | {message} \
         [offsets: timestamp={timestamp_offset}, \
         level={level_offset}, message={message_offset}]"
    );

    Ok(())
}

fn parse_timestamp_components(timestamp: &str) -> Result<(u32, u32, u32), String> {
    if timestamp.len() != 20 || !timestamp.contains('T') || !timestamp.ends_with('Z') {
        return Err(format!("invalid timestamp `{timestamp}`"));
    }

    let hour = timestamp[11..13]
        .parse::<u32>()
        .map_err(|_| format!("invalid timestamp `{timestamp}`"))?;
    let minute = timestamp[14..16]
        .parse::<u32>()
        .map_err(|_| format!("invalid timestamp `{timestamp}`"))?;
    let second = timestamp[17..19]
        .parse::<u32>()
        .map_err(|_| format!("invalid timestamp `{timestamp}`"))?;

    Ok((hour, minute, second))
}

fn format_timestamp(base_timestamp: &str, seconds_to_add: usize) -> Result<String, String> {
    let (hour, minute, second) = parse_timestamp_components(base_timestamp)?;
    let total_seconds = hour * 3600 + minute * 60 + second + seconds_to_add as u32;
    let wrapped_seconds = total_seconds % (24 * 3600);

    let next_hour = wrapped_seconds / 3600;
    let next_minute = (wrapped_seconds % 3600) / 60;
    let next_second = wrapped_seconds % 60;

    Ok(format!(
        "{}T{:02}:{:02}:{:02}Z",
        &base_timestamp[0..10],
        next_hour,
        next_minute,
        next_second
    ))
}

fn generate_log_line(template_line: &str, sequence: usize) -> Result<String, String> {
    let (timestamp, level, token, message) = parse_template_fields(template_line)?;
    let generated_timestamp = format_timestamp(timestamp, sequence)?;
    let entry = sequence + 1;

    Ok(format!(
        "{generated_timestamp} {level} token={token}-{entry} message={message} [copy {entry}]"
    ))
}

fn generate_logfile(input_path: &str, output_path: &str, count: usize) -> Result<(), String> {
    let file = File::open(input_path)
        .map_err(|error| format!("could not open `{input_path}`: {error}"))?;
    let reader = BufReader::new(file);

    let templates = reader
        .lines()
        .map(|line_result| {
            line_result.map_err(|error| format!("could not read template line: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;

    if templates.is_empty() {
        return Err("sample log is empty".to_string());
    }

    let mut output = File::create(output_path)
        .map_err(|error| format!("could not create `{output_path}`: {error}"))?;

    for index in 0..count {
        let template = &templates[index % templates.len()];
        let generated_line = generate_log_line(template, index)?;

        writeln!(output, "{generated_line}")
            .map_err(|error| format!("could not write `{output_path}`: {error}"))?;
    }

    Ok(())
}

fn parser_usage() -> String {
    "usage: zero-copy-log-parser <log-file>".to_string()
}

fn generator_usage() -> String {
    "usage: generate <sample-log> <output-log> [count]".to_string()
}

pub fn run_parser() -> Result<(), String> {
    let mut args = env::args().skip(1);

    let path = args.next().ok_or_else(parser_usage)?;

    if args.next().is_some() {
        return Err(parser_usage());
    }

    let file = File::open(&path).map_err(|error| format!("could not open `{path}`: {error}"))?;

    let reader = BufReader::new(file);

    for (index, line_result) in reader.lines().enumerate() {
        let line_number = index + 1;

        let line =
            line_result.map_err(|error| format!("could not read line {line_number}: {error}"))?;

        process_line(line).map_err(|error| format!("line {line_number}: {error}"))?;
    }

    Ok(())
}

pub fn run_generator() -> Result<(), String> {
    let mut args = env::args().skip(1);

    let sample_path = args.next().ok_or_else(generator_usage)?;
    let output_path = args.next().ok_or_else(generator_usage)?;

    let count = match args.next() {
        Some(value) => value
            .parse::<usize>()
            .map_err(|error| format!("invalid count `{value}`: {error}"))?,
        None => 500,
    };

    if args.next().is_some() {
        return Err(generator_usage());
    }

    generate_logfile(&sample_path, &output_path, count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn normalizes_trailing_whitespace() {
        let line = String::from(
            "2026-09-15T10:20:30Z INFO \
             token=abc message=Started   ",
        );

        let normalized = normalize(line);

        assert_eq!(
            normalized,
            "2026-09-15T10:20:30Z INFO \
             token=abc message=Started"
        );
    }

    #[test]
    fn parses_fields_as_slices() {
        let line = String::from(
            "2026-09-15T10:20:30Z INFO \
             token=abc123 message=Server started",
        );

        let (timestamp, level, message) = parse_fields(&line).unwrap();

        assert_eq!(timestamp, "2026-09-15T10:20:30Z");
        assert_eq!(level, "INFO");
        assert_eq!(message, "Server started");

        let base = line.as_ptr() as usize;
        let end = base + line.len();

        for field in [timestamp, level, message] {
            let address = field.as_ptr() as usize;

            assert!(address >= base);
            assert!(address < end);
        }
    }

    #[test]
    fn redacts_token_in_place() {
        let mut line = String::from(
            "2026-09-15T10:20:30Z INFO \
             token=abc123 message=Server started",
        );

        redact_token(&mut line).unwrap();

        assert_eq!(
            line,
            "2026-09-15T10:20:30Z INFO \
             token=****** message=Server started"
        );
    }

    #[test]
    fn rejects_an_unknown_level() {
        let line = String::from(
            "2026-09-15T10:20:30Z TRACE \
             token=abc message=Started",
        );

        let result = validate_line(&line);

        assert_eq!(result, Err("unsupported log level `TRACE`".to_string()));
    }

    #[test]
    fn generates_log_lines_from_sample() {
        let line = "2026-09-15T10:20:30Z INFO token=abc123 message=Server started";

        let generated = generate_log_line(line, 0).unwrap();

        assert_eq!(
            generated,
            "2026-09-15T10:20:30Z INFO token=abc123-1 message=Server started [copy 1]"
        );

        let next = generate_log_line(line, 59).unwrap();

        assert_eq!(
            next,
            "2026-09-15T10:21:29Z INFO token=abc123-60 message=Server started [copy 60]"
        );
    }

    #[test]
    fn writes_a_500_line_logfile() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();

        let temp_dir = std::env::temp_dir();
        let input_path = temp_dir.join(format!("zero-copy-log-parser-sample-{unique}.log"));
        let output_path = temp_dir.join(format!("zero-copy-log-parser-output-{unique}.log"));

        fs::write(
            &input_path,
            "2026-09-15T10:20:30Z INFO token=abc123 message=Server started\n2026-09-15T10:21:04Z WARN token=user-456 message=Disk usage is above 80%\n",
        )
        .unwrap();

        generate_logfile(
            input_path.to_str().unwrap(),
            output_path.to_str().unwrap(),
            500,
        )
        .unwrap();

        let generated = fs::read_to_string(&output_path).unwrap();
        let lines: Vec<&str> = generated.lines().collect();

        assert_eq!(lines.len(), 500);
        assert!(lines[0].contains("token=abc123-1"));
        assert!(lines[1].contains("token=user-456-2"));

        let _ = fs::remove_file(input_path);
        let _ = fs::remove_file(output_path);
    }
}