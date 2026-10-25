//! Print the tree-sitter CST of a file, for writing and auditing language specs.
//!
//! Usage: `cargo run -p code-intelligence --example cst -- <language> <path|->`

use std::io::Read;

use code_intelligence::parsers::spec;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let language = args.next().ok_or("usage: cst <language> <path|->")?;
    let path = args.next().ok_or("usage: cst <language> <path|->")?;
    let source = if path == "-" {
        let mut buffer = String::new();
        std::io::stdin().read_to_string(&mut buffer)?;
        buffer
    } else {
        std::fs::read_to_string(&path)?
    };
    let grammar = spec::grammar_for(&language)
        .ok_or_else(|| format!("no compiled grammar for `{language}`"))?;
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&grammar)?;
    let tree = parser
        .parse(&source, None)
        .ok_or("parser returned no tree")?;
    println!("{}", tree.root_node().to_sexp());
    Ok(())
}
