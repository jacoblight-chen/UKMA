//! PDF parser: **text layer only** (`P4-WP2`).
//!
//! ADR-048 O-02 rules out OCR, so this parser only reads the text operators a PDF
//! already contains (`Tj`, `TJ`, `'`, `"`). Consequences, all reported rather than
//! hidden:
//! - a page that is just a scanned image produces an `image` block with
//!   `parse_status = unparsed` plus `OCR_NOT_IMPLEMENTED`, and the document is
//!   `partial`;
//! - a page with neither text nor images emits `PAGE_WITHOUT_TEXT` and is also
//!   `partial`, because assuming it is empty would be a guess.

use lopdf::Document;

use super::{
    block_id, finish_document, ocr_diagnostic, DocumentParserPort, ParseLimits, ParseOutput,
};
use crate::domain::{
    BlockKind, DocumentBlock, DocumentSource, MediaFormat, ParseDiagnostic, ParseStatus, Result,
    Severity, Span,
};

/// Parser id.
pub const PARSER_ID: &str = "ukma-pdf-textlayer";
/// Parser version.
pub const PARSER_VERSION: &str = "pdf-textlayer/1";

/// PDF text-layer parser.
#[derive(Debug, Default, Clone, Copy)]
pub struct PdfParser;

impl PdfParser {
    /// New parser.
    pub fn new() -> Self {
        Self
    }
}

/// Decode a PDF string object into text.
fn decode_string(bytes: &[u8]) -> String {
    // UTF-16BE with a BOM is the only self-describing encoding in a PDF string.
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let mut units: Vec<u16> = Vec::new();
        for pair in bytes[2..].as_chunks::<2>().0 {
            units.push(u16::from_be_bytes(*pair));
        }
        return String::from_utf16_lossy(&units);
    }
    bytes
        .iter()
        .map(|byte| {
            if byte.is_ascii_graphic() || *byte == b' ' {
                *byte as char
            } else if *byte == b'\n' || *byte == b'\r' {
                ' '
            } else {
                '\u{FFFD}'
            }
        })
        .collect()
}

/// Extract the text of one content stream.
fn text_from_content(content: &[u8]) -> String {
    let mut out = String::new();
    let mut operands: Vec<Vec<u8>> = Vec::new();
    let mut index = 0usize;
    while index < content.len() {
        let byte = content[index];
        match byte {
            b'(' => {
                let mut depth = 1usize;
                index += 1;
                let mut literal: Vec<u8> = Vec::new();
                while index < content.len() && depth > 0 {
                    match content[index] {
                        b'\\' => {
                            index += 1;
                            if index < content.len() {
                                let escaped = content[index];
                                literal.push(match escaped {
                                    b'n' => b'\n',
                                    b'r' => b'\r',
                                    b't' => b'\t',
                                    other => other,
                                });
                                index += 1;
                            }
                        }
                        b'(' => {
                            depth += 1;
                            literal.push(b'(');
                            index += 1;
                        }
                        b')' => {
                            depth -= 1;
                            if depth > 0 {
                                literal.push(b')');
                            }
                            index += 1;
                        }
                        other => {
                            literal.push(other);
                            index += 1;
                        }
                    }
                }
                operands.push(literal);
            }
            b'<' if content.get(index + 1) != Some(&b'<') => {
                index += 1;
                let mut hex: Vec<u8> = Vec::new();
                while index < content.len() && content[index] != b'>' {
                    let ch = content[index];
                    if ch.is_ascii_hexdigit() {
                        hex.push(ch);
                    }
                    index += 1;
                }
                index += 1;
                let mut decoded = Vec::new();
                for pair in hex.chunks(2) {
                    if pair.len() == 2 {
                        if let Ok(text) = std::str::from_utf8(pair) {
                            if let Ok(value) = u8::from_str_radix(text, 16) {
                                decoded.push(value);
                            }
                        }
                    }
                }
                operands.push(decoded);
            }
            b'T' => {
                let operator = content
                    .get(index..(index + 2).min(content.len()))
                    .unwrap_or_default();
                match operator {
                    b"Tj" | b"'" | b"\"" => {
                        if let Some(last) = operands.pop() {
                            out.push_str(&decode_string(&last));
                            out.push(' ');
                        }
                        index += 2;
                    }
                    b"TJ" => {
                        // an array of strings and kerning numbers
                        for operand in operands.drain(..) {
                            out.push_str(&decode_string(&operand));
                        }
                        out.push(' ');
                        index += 2;
                    }
                    b"Td" | b"TD" | b"T*" => {
                        out.push('\n');
                        operands.clear();
                        index += 2;
                    }
                    _ => {
                        index += 1;
                    }
                }
            }
            _ => {
                index += 1;
            }
        }
    }
    out.trim().to_string()
}

/// Whether the page carries at least one image XObject.
///
/// Uses lopdf's own resource walk instead of hand-rolling the resource
/// dictionary traversal: an inline resource dict, an indirect one and nested
/// Form XObjects are all handled the same way.
fn page_has_image(document: &Document, page_id: lopdf::ObjectId) -> bool {
    document
        .get_page_images(page_id)
        .map(|images| !images.is_empty())
        .unwrap_or(false)
}

impl DocumentParserPort for PdfParser {
    fn parser_id(&self) -> &'static str {
        PARSER_ID
    }

    fn parser_version(&self) -> &'static str {
        PARSER_VERSION
    }

    fn supports(&self, format: MediaFormat) -> bool {
        matches!(format, MediaFormat::Pdf)
    }

    fn parse(
        &self,
        source: &DocumentSource,
        bytes: &[u8],
        limits: &ParseLimits,
    ) -> Result<ParseOutput> {
        let hash = super::content_hash(bytes);
        let document_id = format!(
            "doc:v1:{}",
            hash.trim_start_matches("sha256:")
                .get(..16)
                .unwrap_or("0000000000000000")
        );
        let document = Document::load_mem(bytes).map_err(|error| {
            crate::domain::DocumentError::SourceParseFailed {
                message: format!("pdf: {error}"),
            }
        })?;

        let pages = document.get_pages();
        let mut blocks: Vec<DocumentBlock> = Vec::new();
        let mut diagnostics: Vec<ParseDiagnostic> = Vec::new();
        let mut line_cursor: u32 = 1;
        let mut byte_cursor: u64 = 0;

        for (number, page_id) in pages {
            let text = document
                .get_page_content(page_id)
                .map(|content| text_from_content(&content))
                .unwrap_or_default();
            let has_image = page_has_image(&document, page_id);
            let lines = text.lines().count().max(1) as u32;
            let span = Span::new(
                line_cursor,
                line_cursor + lines - 1,
                byte_cursor,
                byte_cursor + text.len() as u64,
            );
            let order = blocks.len() as u32;

            if text.is_empty() {
                if has_image {
                    let id = block_id(&document_id, order, BlockKind::Image, &span, None);
                    blocks.push(DocumentBlock {
                        id,
                        kind: BlockKind::Image,
                        text: None,
                        parent_id: None,
                        order,
                        span: span.clone(),
                        level: Some(number as u8),
                        language: None,
                        parse_status: ParseStatus::Unparsed,
                    });
                    if limits.include_images {
                        diagnostics.push(ocr_diagnostic(span.clone()));
                    }
                } else {
                    diagnostics.push(ParseDiagnostic {
                        code: "PAGE_WITHOUT_TEXT".to_string(),
                        message: format!("page {number} has no text layer and no image"),
                        span: Some(span.clone()),
                        severity: Severity::Warning,
                    });
                    let id = block_id(&document_id, order, BlockKind::Other, &span, None);
                    blocks.push(DocumentBlock {
                        id,
                        kind: BlockKind::Other,
                        text: None,
                        parent_id: None,
                        order,
                        span: span.clone(),
                        level: Some(number as u8),
                        language: None,
                        parse_status: ParseStatus::Partial,
                    });
                }
            } else {
                let id = block_id(
                    &document_id,
                    order,
                    BlockKind::Paragraph,
                    &span,
                    Some(&text),
                );
                blocks.push(DocumentBlock {
                    id,
                    kind: BlockKind::Paragraph,
                    text: Some(text.clone()),
                    parent_id: None,
                    order,
                    span: span.clone(),
                    level: Some(number as u8),
                    language: None,
                    parse_status: ParseStatus::Complete,
                });
                if has_image {
                    // a page can hold both a text layer and images
                    let image_order = blocks.len() as u32;
                    let id = block_id(&document_id, image_order, BlockKind::Image, &span, None);
                    blocks.push(DocumentBlock {
                        id,
                        kind: BlockKind::Image,
                        text: None,
                        parent_id: None,
                        order: image_order,
                        span: span.clone(),
                        level: Some(number as u8),
                        language: None,
                        parse_status: ParseStatus::Unparsed,
                    });
                    if limits.include_images {
                        diagnostics.push(ocr_diagnostic(span.clone()));
                    }
                }
            }

            line_cursor = span.line_end + 1;
            byte_cursor = span.byte_end;
        }

        if blocks.is_empty() {
            diagnostics.push(ParseDiagnostic {
                code: "PDF_WITHOUT_PAGES".to_string(),
                message: "the PDF declares no pages".to_string(),
                span: None,
                severity: Severity::Warning,
            });
        }

        let parsed = finish_document(
            document_id,
            source.clone(),
            PARSER_ID,
            PARSER_VERSION,
            blocks,
            diagnostics,
        );
        Ok(ParseOutput {
            document: parsed,
            links: Vec::new(),
            anchors: Vec::new(),
        })
    }
}
