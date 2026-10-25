//! Print every CST node kind with its span (debugging aid).
//!
//! Usage: `cargo run -p code-intelligence --example nodes -- <language> <path>`

use code_intelligence::parsers::spec;

fn walk(node: tree_sitter::Node<'_>, depth: usize, out: &mut Vec<String>) {
    out.push(format!(
        "{:indent$}{} @{}..{} named={}",
        "",
        node.kind(),
        node.start_byte(),
        node.end_byte(),
        node.is_named(),
        indent = depth * 2
    ));
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, depth + 1, out);
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let language = args.next().ok_or("usage: nodes <language> <path>")?;
    let path = args.next().ok_or("usage: nodes <language> <path>")?;
    let source = std::fs::read_to_string(&path)?;
    let spec = spec::spec(&language).ok_or_else(|| format!("no spec for `{language}`"))?;
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&(spec.grammar)())?;
    let tree = parser.parse(&source, None).ok_or("no tree")?;
    let mut out = Vec::new();
    walk(tree.root_node(), 0, &mut out);
    println!("{}", out.join("\n"));
    Ok(())
}
