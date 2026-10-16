use publisher_pdf::{ExportMode, ExportRequest, generate_pdf};
use serde_json::json;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

const HELP: &str = "publisher-pdf — generate PDF from a saved finished publication

Usage:
  publisher-pdf --input saved.publisher-v2 --output new-directory --profile CMYK.icc --mode pdf|pdfx4-candidate [--allow-warnings]

The output directory must not exist and its parent must already exist.
Creates publication.pdf, then receipt.json. Completion requires exit code zero
and a parseable receipt with a matching PDF hash. After any failure or interruption,
retain the output for diagnosis and retry in a new directory, even if both files exist.
Review warnings before using --allow-warnings; they remain in the receipt.
Keep the packaged color, font and raster workers beside this command.
PDF/X-4 candidate output requires independent verification; generation does not authorize professional export.
PDF/X candidate metadata uses the saved title and current UTC time. For repeatable
bytes, pass --metadata-utc with the same RFC 3339 creation timestamp to each run.

  --help       Show this help
  --version    Show the generator version";

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && args[0] == "--help" {
        println!("{HELP}");
        return ExitCode::SUCCESS;
    }
    if args.len() == 1 && args[0] == "--version" {
        println!("publisher-pdf {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let request = match parse(args) {
        Ok(request) => request,
        Err(message) => {
            eprintln!(
                "{}",
                json!({"error": {"code": "arguments", "message": message}})
            );
            return ExitCode::from(2);
        }
    };
    match generate_pdf(&request) {
        Ok(receipt) => {
            // The same receipt has already encoded successfully and was written last.
            println!(
                "{}",
                serde_json::to_string_pretty(&receipt).expect("receipt already encoded")
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!(
                "{}",
                json!({"error": {
                    "code": error.code(), "message": error.to_string(), "findings": error.findings(),
                }})
            );
            ExitCode::FAILURE
        }
    }
}

fn parse(args: Vec<OsString>) -> Result<ExportRequest, String> {
    let (mut input, mut output, mut profile, mut mode) = (None, None, None, None);
    let mut allow_warnings = false;
    let mut metadata_utc = None;
    let mut args = args.into_iter();
    while let Some(flag) = args.next() {
        let flag = flag
            .to_str()
            .ok_or("Options must be valid text; use --help for usage.")?;
        if flag == "--allow-warnings" {
            if allow_warnings {
                return Err("--allow-warnings was supplied twice.".into());
            }
            allow_warnings = true;
            continue;
        }
        if !matches!(
            flag,
            "--input" | "--output" | "--profile" | "--mode" | "--metadata-utc"
        ) {
            return Err(format!("Unknown option {flag}; use --help for usage."));
        }
        let value = args
            .next()
            .ok_or_else(|| format!("{flag} requires a value."))?;
        if value.is_empty() || value.to_str().is_some_and(|value| value.starts_with("--")) {
            return Err(format!("{flag} requires a value."));
        }
        match flag {
            "--metadata-utc" => {
                if metadata_utc.is_some() {
                    return Err("--metadata-utc was supplied twice.".into());
                }
                let timestamp = value
                    .to_str()
                    .ok_or("--metadata-utc requires a text timestamp.")?;
                if print_core::PdfCandidateMetadata::at_utc("validation", timestamp).is_none() {
                    return Err("--metadata-utc requires a valid RFC 3339 timestamp with a four-digit year.".into());
                }
                metadata_utc = Some(timestamp.to_owned());
            }
            "--input" => set_path(&mut input, value, flag)?,
            "--output" => set_path(&mut output, value, flag)?,
            "--profile" => set_path(&mut profile, value, flag)?,
            "--mode" => {
                if mode.is_some() {
                    return Err("--mode was supplied twice.".into());
                }
                mode = Some(match value.to_str() {
                    Some("pdf") => ExportMode::Pdf,
                    Some("pdfx4-candidate") => ExportMode::Pdfx4Candidate,
                    _ => return Err("Unsupported mode; choose pdf or pdfx4-candidate. Independent verification is pending.".into()),
                });
            }
            _ => unreachable!("known options checked above"),
        }
    }
    Ok(ExportRequest {
        input: input.ok_or("Missing --input; use --help for usage.")?,
        output: output.ok_or("Missing --output; use --help for usage.")?,
        profile: profile.ok_or("Missing --profile; use --help for usage.")?,
        mode: mode.ok_or("Missing --mode; choose pdf or pdfx4-candidate.")?,
        allow_warnings,
        metadata_utc,
    })
}

fn set_path(target: &mut Option<PathBuf>, value: OsString, flag: &str) -> Result<(), String> {
    if target.is_some() {
        return Err(format!("{flag} was supplied twice."));
    }
    *target = Some(PathBuf::from(value));
    Ok(())
}
