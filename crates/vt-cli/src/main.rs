mod cmd;
mod secret_patterns;
mod ui;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use vt_core::manifest::{Category, Tool};

/// AI kodlama aracı verilerini makineler arasında taşır.
#[derive(Parser)]
#[command(name = "vibe-transfer", version, arg_required_else_help = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Bu makinede neyin taşınacağını gösterir; hiçbir şey yazmaz.
    Scan(ScanArgs),
    /// Seçilen verileri şifreli bir paket dosyasına yazar.
    Export(ExportArgs),
}

#[derive(Args)]
pub struct ScanArgs {
    /// Araç (şimdilik yalnız claude).
    #[arg(long, value_name = "araç")]
    pub tool: Option<Tool>,
    /// Sonucu JSON olarak yazar.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct ExportArgs {
    /// Oluşturulacak paket dosyası; var olan bir dosyanın üzerine yazılmaz.
    #[arg(long, value_name = "dosya")]
    pub out: PathBuf,
    /// Araç (şimdilik yalnız claude).
    #[arg(long, value_name = "araç")]
    pub tool: Option<Tool>,
    /// Yalnız bu kategoriler: sessions, memory, settings, secrets, project-files, cache.
    #[arg(long, value_name = "kat,..", value_delimiter = ',')]
    pub only: Vec<Category>,
    /// Bu kategoriler hariç.
    #[arg(long, value_name = "kat,..", value_delimiter = ',')]
    pub exclude: Vec<Category>,
    /// İsteğe bağlı öğeleri (önbellek) de ekler.
    #[arg(long)]
    pub include_optional: bool,
    /// Parolayı üretmek yerine standart girdiden okur.
    #[arg(long)]
    pub passphrase_stdin: bool,
    /// Özeti JSON olarak yazar.
    #[arg(long)]
    pub json: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = cmd::Ctx::from_process().and_then(|ctx| match &cli.command {
        Command::Scan(args) => cmd::scan::run(&ctx, args),
        Command::Export(args) => cmd::export::run(&ctx, args),
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("hata: {e:#}");
            ExitCode::FAILURE
        }
    }
}
