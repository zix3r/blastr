//! Times loading and extraction of one Cargo project.
//!
//! Usage: `cargo run --release -p blastr-ra --example extract_bench -- <project dir> [--threads N]`
//!
//! The project is loaded with a target directory of its own under the system
//! temporary directory, so the build output of the user's checkout is left
//! alone. The graph is serialised and hashed, so two runs can be compared by
//! their fingerprint instead of by the whole graph.

use std::path::PathBuf;
use std::time::Instant;

use blastr_model::{CancelToken, ExecMode, NoProgress};
use blastr_ra::{ExtractOptions, LoadOptions, Workspace};

/// The parsed command line.
struct Args {
    /// The directory holding the `Cargo.toml` to load.
    project: PathBuf,
    /// The worker count, `None` for the number of available cores.
    threads: Option<usize>,
}

fn main() {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            eprintln!("usage: extract_bench <project dir> [--threads N]");
            std::process::exit(2);
        }
    };

    let options = LoadOptions {
        features: Vec::new(),
        all_features: false,
        target: None,
        exec: ExecMode::Full,
        target_dir: Some(std::env::temp_dir().join("blastr-bench-target")),
    };
    let cancel = CancelToken::new();

    let started = Instant::now();
    let workspace =
        match Workspace::load(&args.project, &args.project, &options, &NoProgress, &cancel) {
            Ok(workspace) => workspace,
            Err(error) => {
                eprintln!("the project failed to load: {error}");
                std::process::exit(1);
            }
        };
    let load = started.elapsed();

    let extract_options = ExtractOptions {
        threads: args.threads,
    };
    let started = Instant::now();
    let graph = match workspace.extract_with(&extract_options, &NoProgress, &cancel) {
        Ok(extracted) => extracted,
        Err(error) => {
            eprintln!("the extraction failed: {error}");
            std::process::exit(1);
        }
    };
    let extract = started.elapsed();

    let fingerprint = match serde_json::to_string(&graph) {
        Ok(text) => blastr_model::hash_tokens([text.as_str()]),
        Err(error) => {
            eprintln!("the graph could not be serialised: {error}");
            std::process::exit(1);
        }
    };

    println!("project:     {}", args.project.display());
    println!("load:        {:.3} s", load.as_secs_f64());
    println!("extraction:  {:.3} s", extract.as_secs_f64());
    println!("symbols:     {}", graph.symbols.len());
    println!("edges:       {}", graph.edges.len());
    println!("unresolved:  {}", graph.unresolved.len());
    println!("inactive:    {}", graph.inactive.len());
    println!("crates:      {}", graph.crates.len());
    println!("fingerprint: {fingerprint:016x}");
}

fn parse_args(arguments: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut project = None;
    let mut threads = None;
    let mut arguments = arguments;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--help" | "-h" => {
                return Err("extract_bench <project dir> [--threads N]".to_owned());
            }
            "--threads" => {
                let value = arguments.next().ok_or("`--threads` needs a number")?;
                let parsed = value
                    .parse::<usize>()
                    .map_err(|_| format!("`{value}` is not a thread count"))?;
                threads = Some(parsed);
            }
            other => {
                if project.is_some() {
                    return Err(format!("unexpected argument `{other}`"));
                }
                project = Some(PathBuf::from(other));
            }
        }
    }
    let project = project.ok_or("no project directory given")?;
    Ok(Args { project, threads })
}
