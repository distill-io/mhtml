//! Thin `mhtml` CLI: parse arguments with clap and delegate to the `cli`
//! library. All real work (parsing, listing, naming) lives in the library so
//! it can be unit- and integration-tested; this file only maps commands to
//! calls and outcomes to process exit codes.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use mhtml_cli::extract;
use mhtml_cli::list::{self, Outcome};

#[derive(Parser)]
#[command(name = "mhtml", about = "Inspect and extract MHTML web archives")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List the parts of an MHTML archive.
    List {
        /// Path to the `.mhtml` / `.mht` archive.
        file: PathBuf,
    },
    /// Extract an MHTML archive into a directory tree that renders offline.
    Extract {
        /// Path to the `.mhtml` / `.mht` archive.
        file: PathBuf,
        /// Output directory (default: the archive's filename stem, alongside it).
        #[arg(short = 'o', long = "output")]
        output: Option<PathBuf>,
        /// All-or-nothing: on any parse error, remove the output directory.
        #[arg(long)]
        strict: bool,
        /// How resources are named: `mirror` (URL hierarchy on disk, default) or
        /// `hash` (flat `<sha256>.<ext>` files plus a `manifest.json`).
        #[arg(long, value_enum, default_value_t = extract::Naming::Mirror)]
        naming: extract::Naming,
        /// Set `<base href>` on the extracted entry document (hash mode only) so
        /// its bare `<hash>.<ext>` references resolve against a CDN/S3 prefix.
        #[arg(long)]
        base_href: Option<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::List { file } => match list::run(&file) {
            Ok(Outcome::Complete) => ExitCode::SUCCESS,
            Ok(Outcome::Truncated) => ExitCode::from(1),
            Err(e) => {
                eprintln!("error: {e:#}");
                ExitCode::from(1)
            }
        },
        Command::Extract {
            file,
            output,
            strict,
            naming,
            base_href,
        } => {
            let out = output.unwrap_or_else(|| extract::default_out_dir(&file));
            match extract::run(&file, &out, strict, naming, base_href.as_deref()) {
                Ok(extract::Outcome::Success) => ExitCode::SUCCESS,
                Ok(extract::Outcome::Failed) => ExitCode::from(1),
                Err(e) => {
                    eprintln!("error: {e:#}");
                    ExitCode::from(1)
                }
            }
        }
    }
}
