//! Print the extracted symbols and edges of a file (debugging aid).
//!
//! Usage: `cargo run -p code-intelligence --example parsed -- <language> <path>`

use code_intelligence::parsers::engine::UnifiedParser;
use code_intelligence::ParserPort;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let language = args.next().ok_or("usage: parsed <language> <path>")?;
    let path = args.next().ok_or("usage: parsed <language> <path>")?;
    let source = std::fs::read_to_string(&path)?;
    let parsed = UnifiedParser::new().parse(&path, &language, &source)?;
    println!("parser_version: {}", parsed.parser_version);
    for symbol in &parsed.symbols {
        println!(
            "SYMBOL {:<40} {:?} {}:{}-{} sig={:?}",
            symbol.qualified_name,
            symbol.kind,
            symbol.definition.file_id,
            symbol.definition.line_start,
            symbol.definition.line_end,
            symbol.signature
        );
    }
    for edge in &parsed.edges {
        println!(
            "EDGE   {:<40} {:?} {:?} -> {:?}",
            edge.source, edge.kind, edge.resolution, edge.target
        );
    }
    for diagnostic in &parsed.diagnostics {
        println!("DIAG   {} {}", diagnostic.code, diagnostic.message);
    }
    Ok(())
}
