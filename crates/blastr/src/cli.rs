//! The command-line interface of blastr.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// The top-level parser.
#[derive(Debug, Parser)]
#[command(
    name = "blastr",
    version,
    about = "Shows what a Rust code change can affect",
    long_about = None,
    after_help = "Run without a command to open the GUI."
)]
pub struct Cli {
    /// Options shared by every command.
    #[command(flatten)]
    pub globals: Globals,
    /// The command to run; without one the GUI opens.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Options accepted before or after a command.
#[derive(Debug, Args)]
#[allow(dead_code)]
pub struct Globals {
    /// Path to the Cargo.toml of the project
    #[arg(long, global = true, value_name = "PATH")]
    pub manifest_path: Option<PathBuf>,
    /// Cargo features to enable
    #[arg(long, global = true, value_name = "LIST", value_delimiter = ',')]
    pub features: Vec<String>,
    /// Enable all Cargo features
    #[arg(long, global = true)]
    pub all_features: bool,
    /// Target triple to analyse
    #[arg(long, global = true, value_name = "TRIPLE")]
    pub target: Option<String>,
    /// Output format
    #[arg(
        long,
        global = true,
        value_enum,
        value_name = "FORMAT",
        default_value = "human"
    )]
    pub format: Format,
    /// How many levels of impact to follow
    #[arg(long, global = true, value_name = "N")]
    pub depth: Option<u32>,
    /// Lowest confidence to report
    #[arg(long, global = true, value_enum, value_name = "LEVEL")]
    pub min_confidence: Option<Confidence>,
    /// List everything, not only the top items
    #[arg(long, global = true)]
    pub all: bool,
    /// Exit with code 2 when a finding of this class is reported
    #[arg(long, global = true, value_enum, value_name = "CLASS")]
    pub fail_on: Option<FailOn>,
    /// Restricted mode: do not run build scripts or procedural macros
    #[arg(long, global = true)]
    pub no_exec: bool,
    /// Increase log verbosity; repeat for more detail
    #[arg(short = 'v', long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,
}

/// A revision or range to compare against.
#[derive(Debug, Args)]
#[allow(dead_code)]
pub struct RevArgs {
    /// Revision or range to compare, such as `HEAD~1` or `main..HEAD`
    #[arg(value_name = "REV", conflicts_with_all = ["worktree", "staged"])]
    pub rev: Option<String>,
    /// Compare HEAD with the working tree
    #[arg(long, conflicts_with_all = ["rev", "staged"])]
    pub worktree: bool,
    /// Compare HEAD with the index
    #[arg(long, conflicts_with_all = ["rev", "worktree"])]
    pub staged: bool,
    /// Parent of a merge commit to compare against
    #[arg(long, value_name = "N")]
    pub parent: Option<u32>,
}

/// What to do.
#[derive(Debug, Subcommand)]
#[allow(dead_code)]
pub enum Command {
    /// Full report: changes, impact, tests to run, limits
    Analyze(RevArgs),
    /// Structural changes only, without impact
    Diff(RevArgs),
    /// Affected tests only
    Tests(RevArgs),
    /// What-if: the blast radius of one symbol
    Impact {
        /// The symbol to assume changed
        #[arg(value_name = "SYMBOL")]
        symbol: String,
        /// The kind of change to assume
        #[arg(long, value_enum, value_name = "KIND", default_value = "body")]
        assume: Assume,
    },
    /// Why an item is in the result
    Explain {
        /// The symbol to explain
        #[arg(value_name = "SYMBOL")]
        symbol: String,
        /// The revision or range the result came from
        #[command(flatten)]
        revs: RevArgs,
    },
    /// Build and cache the graph for a revision
    Index {
        /// Revision to index
        #[arg(value_name = "REV")]
        rev: Option<String>,
    },
    /// Inspect or clear the cache
    Cache {
        /// The cache operation
        #[command(subcommand)]
        command: CacheCommand,
    },
    /// Open the GUI on a repository
    Gui {
        /// Directory of the repository
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,
    },
    /// Print a shell completion script
    Completions {
        /// The shell to generate completions for
        #[arg(value_name = "SHELL", value_enum)]
        shell: clap_complete::Shell,
    },
}

/// Cache operations.
#[derive(Debug, Subcommand)]
#[allow(dead_code)]
pub enum CacheCommand {
    /// Show what the cache contains
    Status,
    /// Remove everything from the cache
    Clear,
}

/// The output format.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Format {
    /// Human-readable output
    Human,
    /// Machine-readable JSON
    Json,
    /// Markdown suitable for a pull request
    Markdown,
    /// Cargo JSON diagnostic messages
    Cargo,
}

/// How certain an impact is.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Confidence {
    /// The change certainly breaks this
    Definite,
    /// The change probably breaks this
    Likely,
    /// The change may break this
    Possible,
}

/// The class of finding that makes the run fail.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum FailOn {
    /// A caller no longer compiles
    Adaptation,
    /// Compiled code behaves differently
    Behaviour,
}

/// A hypothetical change to a symbol.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Assume {
    /// The body changed
    Body,
    /// The signature changed
    Signature,
    /// The item was removed
    Removed,
    /// The visibility changed
    Visibility,
    /// A field was removed
    FieldRemoved,
    /// A field changed its type
    FieldRetyped,
    /// A field was added
    FieldAdded,
    /// An enum variant was added
    VariantAdded,
    /// An enum variant was removed
    VariantRemoved,
}
