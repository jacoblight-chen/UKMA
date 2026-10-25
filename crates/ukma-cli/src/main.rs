//! `ukma` binary entry point.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let gateway = ukma_mcp::default_gateway();
    match ukma_cli::run(&gateway, &args) {
        Ok(output) => println!("{output}"),
        Err(error) => {
            eprintln!("ukma: {error}");
            std::process::exit(ukma_cli::exit_code(&error));
        }
    }
}
