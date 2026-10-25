//! DOCX parser (`P4-WP2`).
//!
//! A DOCX is a zip with `word/document.xml`. The reader walks the XML once and
//! keeps document order: headings (`w:pStyle`), list items (`w:numPr`), tables
//! (`w:tbl`), paragraphs and images (`w:drawing` / `w:pict`).
//!
//! An image is reported as `unparsed` (no OCR) and makes the document `partial`;
//! a table is reported with one cell block per `<w:tc>` so a reader can point at
//! a cell instead of a blob.
//!
//! Spans for DOCX are **text offsets**, not XML offsets: line numbers are the
//! paragraph ordinal and byte offsets are positions in the extracted text. That is
//! stated here because it differs from the Markdown/PDF parsers, which can point
//! into the artifact itself.

use std::io::{Cursor, Read};

use quick_xml::events::Event;
use quick_xml::Reader;
use zip::ZipArchive;

use super::{
    block_id, finish_document, ocr_diagnostic, DocumentParserPort, ParseLimits, ParseOutput,
};
use crate::domain::{
    BlockKind, DocumentBlock, DocumentError, DocumentSource, MediaFormat, ParseDiagnostic,
    ParseStatus, Result, Severity, Span,
};

/// Parser id.
pub const PARSER_ID: &str = "ukma-docx";
/// Parser version.
pub const PARSER_VERSION: &str = "docx/1";

/// DOCX parser.
#[derive(Debug, Default, Clone, Copy)]
pub struct DocxParser;

impl DocxParser {
    /// New parser.
    pub fn new() -> Self {
        Self
    }
}

fn heading_level(style: &str) -> Option<u8> {
    let lower = style.to_ascii_lowercase();
    if lower == "title" {
        return Some(1);
    }
    let rest = lower.strip_prefix("heading")?;
    rest.trim()
        .parse::<u8>()
        .ok()
        .filter(|level| (1..=6).contains(level))
}

impl DocumentParserPort for DocxParser {
    fn parser_id(&self) -> &'static str {
        PARSER_ID
    }

    fn parser_version(&self) -> &'static str {
        PARSER_VERSION
    }

    fn supports(&self, format: MediaFormat) -> bool {
        matches!(format, MediaFormat::Docx)
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
        let mut archive = ZipArchive::new(Cursor::new(bytes)).map_err(|error| {
            DocumentError::SourceParseFailed {
                message: format!("docx container: {error}"),
            }
        })?;
        let mut xml = String::new();
        {
            let mut entry = archive.by_name("word/document.xml").map_err(|error| {
                DocumentError::SourceParseFailed {
                    message: format!("docx: word/document.xml missing: {error}"),
                }
            })?;
            entry
                .read_to_string(&mut xml)
                .map_err(|error| DocumentError::SourceParseFailed {
                    message: format!("docx: cannot read document.xml: {error}"),
                })?;
        }

        let mut reader = Reader::from_str(&xml);
        reader.config_mut().trim_text(false);

        let mut blocks: Vec<DocumentBlock> = Vec::new();
        let mut diagnostics: Vec<ParseDiagnostic> = Vec::new();
        let mut line_cursor: u32 = 1;
        let mut byte_cursor: u64 = 0;

        let mut paragraph = String::new();
        let mut paragraph_style: Option<u8> = None;
        let mut in_paragraph = false;
        let mut in_list_item = false;
        let mut in_text = false;
        let mut in_table = false;
        let mut table_id: Option<String> = None;
        let mut table_rows: u32 = 0;
        let mut image_pending = false;

        let push_block = |blocks: &mut Vec<DocumentBlock>,
                          kind: BlockKind,
                          text: Option<String>,
                          parent: Option<String>,
                          level: Option<u8>,
                          status: ParseStatus,
                          line_cursor: &mut u32,
                          byte_cursor: &mut u64| {
            let lines = text
                .as_deref()
                .map(|text| text.lines().count().max(1) as u32)
                .unwrap_or(1);
            let span = Span::new(
                *line_cursor,
                *line_cursor + lines - 1,
                *byte_cursor,
                *byte_cursor + text.as_deref().map(|text| text.len() as u64).unwrap_or(0),
            );
            let order = blocks.len() as u32;
            let id = block_id("docx", order, kind, &span, text.as_deref());
            blocks.push(DocumentBlock {
                id,
                kind,
                text,
                parent_id: parent,
                order,
                span: span.clone(),
                level,
                language: None,
                parse_status: status,
            });
            *line_cursor = span.line_end + 1;
            *byte_cursor = span.byte_end;
        };

        loop {
            let event = reader
                .read_event()
                .map_err(|error| DocumentError::SourceParseFailed {
                    message: format!("docx xml: {error}"),
                })?;
            match event {
                Event::Start(element) => {
                    let name = element.name();
                    match name.as_ref() {
                        b"w:p" => {
                            in_paragraph = true;
                            paragraph.clear();
                            paragraph_style = None;
                            in_list_item = false;
                        }
                        b"w:pStyle" => {
                            for attribute in element.attributes().flatten() {
                                if attribute.key.as_ref() == b"w:val" {
                                    let value =
                                        String::from_utf8_lossy(&attribute.value).to_string();
                                    paragraph_style = heading_level(&value);
                                }
                            }
                        }
                        b"w:numPr" => in_list_item = true,
                        b"w:t" => in_text = true,
                        b"w:tbl" => {
                            in_table = true;
                            table_rows = 0;
                        }
                        b"w:tr" if in_table => table_rows += 1,
                        b"w:drawing" | b"w:pict" => image_pending = true,
                        _ => {}
                    }
                }
                Event::Empty(element) => {
                    let name = element.name();
                    if matches!(name.as_ref(), b"w:drawing" | b"w:pict") {
                        image_pending = true;
                    }
                    if name.as_ref() == b"w:pStyle" {
                        for attribute in element.attributes().flatten() {
                            if attribute.key.as_ref() == b"w:val" {
                                let value = String::from_utf8_lossy(&attribute.value).to_string();
                                paragraph_style = heading_level(&value);
                            }
                        }
                    }
                    if name.as_ref() == b"w:numPr" {
                        in_list_item = true;
                    }
                }
                Event::Text(text) => {
                    if in_text {
                        let decoded = String::from_utf8_lossy(text.as_ref()).to_string();
                        paragraph.push_str(&decoded);
                    }
                }
                Event::End(element) => {
                    let name = element.name();
                    match name.as_ref() {
                        b"w:t" => in_text = false,
                        b"w:p" => {
                            if in_paragraph {
                                in_paragraph = false;
                                let text = paragraph.trim().to_string();
                                if !text.is_empty() {
                                    if let Some(level) = paragraph_style {
                                        let kind = BlockKind::Heading;
                                        push_block(
                                            &mut blocks,
                                            kind,
                                            Some(text),
                                            table_id.clone(),
                                            Some(level),
                                            ParseStatus::Complete,
                                            &mut line_cursor,
                                            &mut byte_cursor,
                                        );
                                    } else if in_list_item {
                                        push_block(
                                            &mut blocks,
                                            BlockKind::List,
                                            Some(text),
                                            table_id.clone(),
                                            Some(1),
                                            ParseStatus::Complete,
                                            &mut line_cursor,
                                            &mut byte_cursor,
                                        );
                                    } else if in_table {
                                        push_block(
                                            &mut blocks,
                                            BlockKind::TableCell,
                                            Some(text),
                                            table_id.clone(),
                                            None,
                                            ParseStatus::Complete,
                                            &mut line_cursor,
                                            &mut byte_cursor,
                                        );
                                    } else {
                                        push_block(
                                            &mut blocks,
                                            BlockKind::Paragraph,
                                            Some(text),
                                            None,
                                            None,
                                            ParseStatus::Complete,
                                            &mut line_cursor,
                                            &mut byte_cursor,
                                        );
                                    }
                                }
                                paragraph.clear();
                            }
                        }
                        b"w:tbl" => {
                            in_table = false;
                            table_id = None;
                        }
                        _ => {}
                    }
                }
                Event::Eof => break,
                _ => {}
            }

            if in_table && table_id.is_none() {
                // the table block is created lazily so its id can parent the cells
                let mut level_cursor = line_cursor;
                let mut byte_cursor_local = byte_cursor;
                push_block(
                    &mut blocks,
                    BlockKind::Table,
                    Some("table".to_string()),
                    None,
                    Some(table_rows.min(255) as u8),
                    ParseStatus::Complete,
                    &mut level_cursor,
                    &mut byte_cursor_local,
                );
                line_cursor = level_cursor;
                byte_cursor = byte_cursor_local;
                table_id = blocks.last().map(|block| block.id.clone());
            }

            if image_pending {
                image_pending = false;
                push_block(
                    &mut blocks,
                    BlockKind::Image,
                    None,
                    table_id.clone(),
                    None,
                    ParseStatus::Unparsed,
                    &mut line_cursor,
                    &mut byte_cursor,
                );
                let span = blocks
                    .last()
                    .map(|block| block.span.clone())
                    .unwrap_or_else(|| Span::new(1, 1, 0, 0));
                if limits.include_images {
                    diagnostics.push(ocr_diagnostic(span));
                }
            }
        }

        if blocks.is_empty() {
            diagnostics.push(ParseDiagnostic {
                code: "DOCX_WITHOUT_BLOCKS".to_string(),
                message: "no paragraph, list, table or image was found".to_string(),
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
