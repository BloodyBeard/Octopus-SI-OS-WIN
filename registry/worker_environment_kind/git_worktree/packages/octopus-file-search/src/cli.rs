use std::num::NonZero;
use std::path::PathBuf;

use clap::ArgAction;
use clap::Parser;

#[doc = " Fuzzy matches filenames under a directory."]
#[derive(Parser)]
#[command(version)]
pub struct Cli {
    #[doc = " Whether to output results in JSON format."]
    #[clap(long, default_value = "false")]
    pub json: bool,

    #[doc = " Maximum number of results to return."]
    #[clap(long, short = 'l', default_value = "64")]
    pub limit: NonZero<usize>,

    #[doc = " Directory to search."]
    #[clap(long, short = 'C')]
    pub cwd: Option<PathBuf>,

    #[doc = " Include matching file indices in the output."]
    #[arg(long, default_value = "false")]
    pub compute_indices: bool,

    #[doc = " Number of worker threads to use."]
    #[clap(long, default_value = "2")]
    pub threads: NonZero<usize>,

    #[doc = " Exclude patterns"]
    #[arg(short, long, action = ArgAction::Append)]
    pub exclude: Vec<String>,

    #[doc = " Search pattern."]
    pub pattern: Option<String>,
}
