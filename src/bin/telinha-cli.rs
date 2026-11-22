//! Versão de linha de comando do telinha.
use anyhow::Result;
use clap::Parser;
use dialoguer::{theme::ColorfulTheme, Confirm};
use std::time::Duration;
use telinha::capture;
use telinha::session::{Session, SessionConfig};

/// Compartilhamento de tela simples: escolhe uma tela ou janela, gera um link
/// temporário via Cloudflare Tunnel e derruba o link quando você encerra.
#[derive(Parser, Debug)]
#[command(name = "telinha-cli", version)]
struct Args {
    /// Porta local do servidor (o túnel aponta para ela)
    #[arg(short, long, default_value_t = 8080)]
    port: u16,

    /// Quadros por segundo (máx. 60)
    #[arg(long, default_value_t = 60)]
    fps: u32,

    /// Qualidade do JPEG (1-100)
    #[arg(long, default_value_t = 60)]
    quality: u8,

    /// Largura máxima da imagem enviada (reduz banda)
    #[arg(long, default_value_t = 1280)]
    max_width: u32,

    /// Derruba o link automaticamente depois de N minutos
    #[arg(long)]
    minutes: Option<u64>,

    /// Não abre túnel; só serve em http://127.0.0.1:<porta> (para testar)
    #[arg(long)]
    no_tunnel: bool,

    /// Compartilhar a tela N direto, sem menu (1 = primeira)
    #[arg(long, conflicts_with = "app")]
    screen: Option<usize>,

    /// Compartilhar a primeira janela cujo nome/título contém este texto, sem menu
    #[arg(long)]
    app: Option<String>,

    /// Compartilhar o som sem perguntar
    #[arg(long, conflicts_with = "no_audio")]
    audio: bool,

    /// Não compartilhar o som (sem perguntar)
    #[arg(long)]
    no_audio: bool,

    /// Caminho do executável do cloudflared
    #[arg(long, default_value = "cloudflared")]
    cloudflared: String,
}

fn main() -> Result<()> {
    let args = Args::parse();
    println!("\n=== telinha — compartilhamento de tela ===\n");

    let source = match capture::source_from_args(args.screen, args.app.as_deref())? {
        Some(s) => s,
        None => capture::choose_source()?,
    };
    let audio = if args.audio || args.no_audio {
        args.audio
    } else {
        Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt("Compartilhar o som do PC também?")
            .default(true)
            .interact()?
    };

    let rt = tokio::runtime::Runtime::new()?;
    let session = Session::start(
        SessionConfig {
            source,
            audio,
            fps: args.fps,
            quality: args.quality,
            max_width: args.max_width,
            port: args.port,
            tunnel: !args.no_tunnel,
            cloudflared: args.cloudflared,
        },
        rt.handle(),
    )?;

    println!("\n   Local: {}", session.local_url);
    if let Some(url) = &session.public_url {
        println!("\n🔗 Link para compartilhar:\n\n   {url}\n");
    }
    match args.minutes {
        Some(m) => println!("O link será derrubado em {m} minuto(s) ou ao apertar Ctrl+C."),
        None => println!("Aperte Ctrl+C para encerrar e derrubar o link."),
    }

    rt.block_on(async {
        let timeout = async {
            match args.minutes {
                Some(m) => tokio::time::sleep(Duration::from_secs(m * 60)).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = timeout => {}
        }
    });

    println!("\n🛑 Encerrando...");
    drop(session);
    std::process::exit(0);
}
