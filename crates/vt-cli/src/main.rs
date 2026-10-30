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
    /// Bir paket dosyasını bu makineye uygular.
    Import(ImportArgs),
    /// Hedef yolu bulunamayan projelerin bekleyen verilerini listeler.
    Pending(PendingArgs),
    /// Bekleyen bir projeyi bu makinedeki klasörüne yerleştirir.
    Link(LinkArgs),
    /// Bir oturumda yerleştirilen dosyaları yeniden denetler.
    Verify(VerifyArgs),
    /// Bir oturumu geri alır.
    Rollback(RollbackArgs),
    /// Kaynak makinede taşınan Claude girişini yedekleyip kaldırır.
    LogoutSource(LogoutSourceArgs),
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

#[derive(Args)]
pub struct ImportArgs {
    /// Uygulanacak paket dosyası.
    #[arg(value_name = "dosya")]
    pub file: PathBuf,
    /// Projelerin aranacağı klasörler (varsayılan: ev dizini).
    #[arg(long, value_name = "yol,..", value_delimiter = ',')]
    pub roots: Vec<PathBuf>,
    /// Parolayı standart girdiden okur.
    #[arg(long)]
    pub passphrase_stdin: bool,
    /// Özeti JSON olarak yazar.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct PendingArgs {
    /// Hiçbir kaydın sahiplenmediği dosyaları listeler; onayla siler.
    #[arg(long)]
    pub orphans: bool,
    /// Sonucu JSON olarak yazar.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
#[command(group = clap::ArgGroup::new("hedef").required(true).args(["project", "auto"]))]
pub struct LinkArgs {
    /// Proje kimliği, uzak depo adresi ya da kaynaktaki eski yol.
    #[arg(value_name = "proje", requires = "path")]
    pub project: Option<String>,
    /// Projenin bu makinedeki klasörü.
    #[arg(value_name = "yol")]
    pub path: Option<PathBuf>,
    /// Hedefte bulunan tüm bekleyen projeleri bağlar.
    #[arg(long, conflicts_with_all = ["project", "path"])]
    pub auto: bool,
    /// --auto ile projelerin aranacağı klasörler (varsayılan: ev dizini).
    #[arg(long, value_name = "yol,..", value_delimiter = ',', requires = "auto")]
    pub roots: Vec<PathBuf>,
    /// Özeti JSON olarak yazar.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct VerifyArgs {
    /// Oturum kimliği (varsayılan: son rapor).
    #[arg(value_name = "oturum")]
    pub session: Option<String>,
    /// Sonucu JSON olarak yazar.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct RollbackArgs {
    /// Oturum kimliği (varsayılan: son oturum).
    #[arg(value_name = "oturum")]
    pub session: Option<String>,
    /// Önceki sırrı geri koymak için paket parolasını standart girdiden okur.
    #[arg(long)]
    pub passphrase_stdin: bool,
}

#[derive(Args)]
pub struct LogoutSourceArgs {
    /// Dışa aktarma oturumunun kimliği (import çıktısında yazar).
    #[arg(long, value_name = "oturum")]
    pub session: String,
    /// Yedek parolasını standart girdiden okur.
    #[arg(long)]
    pub passphrase_stdin: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = cmd::Ctx::from_process().and_then(|ctx| {
        let result = match &cli.command {
            Command::Scan(args) => cmd::scan::run(&ctx, args),
            Command::Export(args) => cmd::export::run(&ctx, args),
            Command::Import(args) => cmd::import::run(&ctx, args),
            Command::Pending(args) => cmd::pending::run(&ctx, args),
            Command::Link(args) => cmd::link::run(&ctx, args),
            Command::Verify(args) => cmd::verify::run(&ctx, args),
            Command::Rollback(args) => cmd::rollback::run(&ctx, args),
            Command::LogoutSource(args) => cmd::logout_source::run(&ctx, args),
        };
        let dumped = ctx.dump_test_secrets();
        result.and(dumped)
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // Errors may quote bundle-supplied text.
            eprintln!("hata: {}", ui::clean(&format!("{e:#}")));
            ExitCode::FAILURE
        }
    }
}
