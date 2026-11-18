use clap::Parser;

fn main() {
    let cli = evans2::cli::Cli::parse();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build the tokio runtime");
    if let Err(err) = rt.block_on(evans2::app::run(cli)) {
        eprintln!("{err:#}");
        std::process::exit(1);
    }
}
