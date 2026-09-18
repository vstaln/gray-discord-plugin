fn main() {
    use clap::Parser;
    let cli = gray_discord::cli::Cli::parse();
    let path = cli.config_path();
    match gray_discord::cli::run(&cli.command, &path) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
