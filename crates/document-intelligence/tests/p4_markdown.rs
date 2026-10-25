//! Markdown parser golden test (`P4-WP1`, `DOC-001`).
//!
//! The expectation below is hand-written from the fixture, so the test proves the
//! block model, the 1-based line spans, the heading tree and the "never claim more
//! than we parsed" rules rather than restating whatever the scanner produced.

use std::path::PathBuf;

use document_intelligence::domain::{
    BlockKind, DocCompleteness, DocumentSource, MediaFormat, ParseStatus,
};
use document_intelligence::parsers::{markdown::MarkdownParser, DocumentParserPort, ParseLimits};

fn source() -> DocumentSource {
    DocumentSource {
        source_uri: "file:///spec.md".to_string(),
        media_type: "text/markdown".to_string(),
        content_hash: "sha256:placeholder".to_string(),
        revision: "rev-1".to_string(),
        language_hint: Some("en".to_string()),
        artifact_ref: "artifact:spec".to_string(),
    }
}

fn fixture() -> (String, Vec<u8>) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/s3-markdown/spec.md");
    let text = std::fs::read_to_string(&path).expect("fixture");
    (path.display().to_string(), text.into_bytes())
}

fn parse() -> document_intelligence::parsers::ParseOutput {
    let (_, bytes) = fixture();
    MarkdownParser::new()
        .parse(&source(), &bytes, &ParseLimits::default())
        .expect("markdown must parse")
}

#[test]
fn blocks_match_the_hand_written_expectation() {
    let output = parse();
    let rows: Vec<String> = output
        .document
        .blocks
        .iter()
        .map(|block| {
            format!(
                "{:?} {}-{} {:?}",
                block.kind, block.span.line_start, block.span.line_end, block.parse_status
            )
        })
        .collect();
    let expected = vec![
        "Heading 1-1 Complete",
        "Paragraph 3-3 Complete",
        "Heading 5-5 Complete",
        "Paragraph 7-7 Complete",
        "Code 9-11 Complete",
        "Table 13-15 Complete",
        // four table cells: two header cells + two body cells
        "TableCell 13-13 Complete",
        "TableCell 13-13 Complete",
        "TableCell 15-15 Complete",
        "TableCell 15-15 Complete",
        "List 17-17 Complete",
        "List 18-18 Complete",
        "Image 20-20 Unparsed",
        "Heading 22-23 Complete",
        "Paragraph 25-25 Complete",
    ];
    assert_eq!(rows, expected, "actual blocks: {rows:#?}");
}

#[test]
fn heading_tree_and_anchors_are_kept() {
    let output = parse();
    let headings: Vec<(String, u8, Option<String>)> = output
        .document
        .blocks
        .iter()
        .filter(|block| block.kind == BlockKind::Heading)
        .map(|block| {
            let parent_title = block.parent_id.as_ref().and_then(|parent| {
                output
                    .document
                    .blocks
                    .iter()
                    .find(|candidate| &candidate.id == parent)
                    .and_then(|candidate| candidate.text.clone())
            });
            (
                block.text.clone().unwrap_or_default(),
                block.level.unwrap_or(0),
                parent_title,
            )
        })
        .collect();
    assert_eq!(
        headings,
        vec![
            ("Overview".to_string(), 1, None),
            ("Details".to_string(), 2, Some("Overview".to_string())),
            ("Appendix".to_string(), 1, None),
        ]
    );

    let anchors: Vec<String> = output
        .anchors
        .iter()
        .map(|anchor| anchor.anchor.clone())
        .collect();
    assert_eq!(anchors, vec!["overview", "details", "appendix"]);
    assert!(output.block_for_anchor("appendix").is_some());
    assert!(output.block_for_anchor("nope").is_none());
}

#[test]
fn code_block_keeps_its_language_and_body() {
    let output = parse();
    let code = output
        .block_for_anchor("details")
        .and_then(|heading| {
            output.document.blocks.iter().find(|block| {
                block.kind == BlockKind::Code && block.parent_id.as_deref() == Some(heading)
            })
        })
        .expect("code block under Details");
    assert_eq!(code.language.as_deref(), Some("rust"));
    assert_eq!(code.text.as_deref(), Some("fn main() {}\n"));
}

#[test]
fn links_are_recorded_with_their_kind() {
    let output = parse();
    assert_eq!(output.links.len(), 2, "links: {:#?}", output.links);
    assert_eq!(output.links[0].target, "#appendix");
    assert_eq!(output.links[0].kind, "anchor");
    assert_eq!(output.links[0].span.line_start, 7);
    assert_eq!(output.links[1].target, "https://example.com/spec");
    assert_eq!(output.links[1].kind, "url");
}

#[test]
fn an_image_is_unparsed_and_makes_the_document_partial() {
    let output = parse();
    assert_eq!(output.document.completeness, DocCompleteness::Partial);
    assert_eq!(output.document.unparsed_blocks(), 1);
    let image = output
        .document
        .blocks
        .iter()
        .find(|block| block.kind == BlockKind::Image)
        .expect("image block");
    assert_eq!(image.parse_status, ParseStatus::Unparsed);
    assert!(image.text.is_none(), "no OCR means no text");
    let diagnostic = output
        .document
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "OCR_NOT_IMPLEMENTED")
        .expect("OCR diagnostic");
    assert_eq!(
        diagnostic.span.as_ref().map(|span| span.line_start),
        Some(20)
    );
}

#[test]
fn requesting_images_off_still_reports_them_as_unparsed() {
    let (_, bytes) = fixture();
    let output = MarkdownParser::new()
        .parse(
            &source(),
            &bytes,
            &ParseLimits {
                include_images: false,
                ..ParseLimits::default()
            },
        )
        .unwrap();
    assert_eq!(output.document.completeness, DocCompleteness::Partial);
    assert!(output
        .document
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "IMAGE_SKIPPED"));
}

#[test]
fn an_unterminated_fence_is_partial_not_silently_complete() {
    let bytes = b"# Title\n\n```rust\nfn main() {}\n".to_vec();
    let output = MarkdownParser::new()
        .parse(&source(), &bytes, &ParseLimits::default())
        .unwrap();
    assert_eq!(output.document.completeness, DocCompleteness::Partial);
    let code = output
        .document
        .blocks
        .iter()
        .find(|block| block.kind == BlockKind::Code)
        .expect("code block");
    assert_eq!(code.parse_status, ParseStatus::Partial);
    assert!(output
        .document
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "UNTERMINATED_CODE_FENCE"));
}

#[test]
fn spans_are_ordered_contiguous_and_inside_the_artifact() {
    let (_, bytes) = fixture();
    let output = parse();
    let len = bytes.len() as u64;
    let mut previous_end = 0u64;
    for block in &output.document.blocks {
        block.span.validate().expect("span invariants");
        assert!(block.span.byte_end <= len, "span inside artifact");
        if block.kind != BlockKind::TableCell {
            assert!(
                block.span.byte_start >= previous_end,
                "blocks are in artifact order: {:?}",
                block.kind
            );
            previous_end = block.span.byte_end;
        }
    }
}

#[test]
fn parsing_is_deterministic_and_ids_are_stable() {
    let first = parse();
    let second = parse();
    let ids = |output: &document_intelligence::parsers::ParseOutput| -> Vec<String> {
        output
            .document
            .blocks
            .iter()
            .map(|block| block.id.clone())
            .collect()
    };
    assert_eq!(ids(&first), ids(&second));
    assert!(first
        .document
        .blocks
        .iter()
        .all(|block| block.id.starts_with("blk:v1:")));
}

#[test]
fn only_markdown_is_supported_by_this_parser() {
    let parser = MarkdownParser::new();
    assert!(parser.supports(MediaFormat::Markdown));
    assert!(!parser.supports(MediaFormat::Pdf));
    assert!(!parser.supports(MediaFormat::Docx));
    assert_eq!(parser.parser_id(), "ukma-markdown");
}
