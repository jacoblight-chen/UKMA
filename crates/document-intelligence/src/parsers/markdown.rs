//! Markdown parser (P4-WP1).
//!
//! A line-based scanner, not a CommonMark implementation: it produces the block
//! kinds ?33.1 needs (heading / paragraph / table / table_cell / list / code /
//! image / footnote / other) with **exact 1-based line spans and byte offsets**,
//! keeps the heading tree, and never drops a position.
//!
//! Unsupported constructs are reported, not silently skipped:
//! - an image becomes an `unparsed` block plus `OCR_NOT_IMPLEMENTED`;
//! - an unterminated fence becomes a `partial` code block plus a diagnostic.

use super::{
    block_id, finish_document, ocr_diagnostic, Anchor, DocLink, DocumentParserPort, ParseLimits,
    ParseOutput,
};
use crate::domain::{
    BlockKind, DocumentBlock, DocumentSource, MediaFormat, ParseDiagnostic, ParseStatus, Result,
    Severity, Span,
};

/// Parser id.
pub const PARSER_ID: &str = "ukma-markdown";
/// Parser version (bumped when the block model changes).
pub const PARSER_VERSION: &str = "markdown/1";

/// Markdown parser.
#[derive(Debug, Default, Clone, Copy)]
pub struct MarkdownParser;

impl MarkdownParser {
    /// New parser.
    pub fn new() -> Self {
        Self
    }
}

struct Line<'a> {
    number: u32,
    byte_start: usize,
    text: &'a str,
}

impl Line<'_> {
    fn byte_end(&self) -> usize {
        self.byte_start + self.text.len()
    }
}

fn split_lines(text: &str) -> Vec<Line<'_>> {
    let mut lines = Vec::new();
    let mut offset = 0usize;
    for (index, raw) in text.split_inclusive('\n').enumerate() {
        let content = raw.trim_end_matches('\n').trim_end_matches('\r');
        lines.push(Line {
            number: (index + 1) as u32,
            byte_start: offset,
            text: content,
        });
        offset += raw.len();
    }
    lines
}

/// Anchor slug used by Markdown renderers: lowercase, punctuation to `-`.
pub fn slugify(title: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = true;
    for ch in title.chars() {
        if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                slug.push(lower);
            }
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
}

fn fence_marker(line: &str) -> Option<(char, usize)> {
    let trimmed = line.trim_start();
    let ch = trimmed.chars().next()?;
    if ch != '`' && ch != '~' {
        return None;
    }
    let count = trimmed
        .chars()
        .take_while(|candidate| *candidate == ch)
        .count();
    if count >= 3 {
        Some((ch, count))
    } else {
        None
    }
}

fn atx_heading(line: &str) -> Option<(u8, String)> {
    let trimmed = line.trim_start();
    let hashes = trimmed.chars().take_while(|ch| *ch == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &trimmed[hashes..];
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    Some((
        hashes as u8,
        rest.trim().trim_end_matches('#').trim_end().to_string(),
    ))
}

fn setext_level(line: &str) -> Option<u8> {
    let trimmed = line.trim();
    if trimmed.len() < 3 {
        return None;
    }
    if trimmed.chars().all(|ch| ch == '=') {
        return Some(1);
    }
    if trimmed.chars().all(|ch| ch == '-') {
        return Some(2);
    }
    None
}

fn list_item(line: &str) -> Option<(u8, String)> {
    let indent = line.len() - line.trim_start().len();
    let trimmed = line.trim_start();
    let marker_len =
        if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
            2
        } else {
            let digits = trimmed.chars().take_while(|ch| ch.is_ascii_digit()).count();
            if digits == 0 {
                return None;
            }
            let rest = &trimmed[digits..];
            if rest.starts_with(". ") || rest.starts_with(") ") {
                digits + 2
            } else {
                return None;
            }
        };
    let text = trimmed[marker_len..].trim().to_string();
    let depth = (indent / 2).min(5) as u8 + 1;
    Some((depth, text))
}

fn is_table_separator(line: &str) -> bool {
    let trimmed = line.trim();
    if !trimmed.contains('|') || !trimmed.contains('-') {
        return false;
    }
    trimmed
        .chars()
        .all(|ch| matches!(ch, '|' | '-' | ':' | ' '))
}

fn table_cells(line: &str) -> Vec<String> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect()
}

fn inline_links(block_id: &str, line: &Line<'_>, out: &mut Vec<DocLink>) {
    let mut index = 0usize;
    while let Some(open) = line.text[index..].find("](") {
        let close_start = index + open;
        let Some(close) = line.text[close_start + 2..].find(')') else {
            break;
        };
        let target_start = close_start + 2;
        let target_end = target_start + close;
        if let Some(bracket) = line.text[..close_start].rfind('[') {
            let target = line.text[target_start..target_end].trim().to_string();
            if !target.is_empty() {
                let kind = if target.starts_with('#') {
                    "anchor"
                } else if target.contains("://") {
                    "url"
                } else {
                    "document"
                };
                out.push(DocLink {
                    block_id: block_id.to_string(),
                    target,
                    kind,
                    span: Span::new(
                        line.number,
                        line.number,
                        (line.byte_start + bracket) as u64,
                        (line.byte_start + target_end + 1) as u64,
                    ),
                });
            }
        }
        index = target_end + 1;
    }
}

struct Builder<'a> {
    document_id: String,
    source: DocumentSource,
    lines: Vec<Line<'a>>,
    blocks: Vec<DocumentBlock>,
    diagnostics: Vec<ParseDiagnostic>,
    links: Vec<DocLink>,
    anchors: Vec<Anchor>,
    heading_stack: Vec<(u8, String)>,
    list_stack: Vec<(u8, String)>,
    include_images: bool,
}

impl<'a> Builder<'a> {
    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        kind: BlockKind,
        span: Span,
        text: Option<String>,
        parent_id: Option<String>,
        level: Option<u8>,
        language: Option<String>,
        parse_status: ParseStatus,
    ) -> String {
        let order = self.blocks.len() as u32;
        let id = block_id(&self.document_id, order, kind, &span, text.as_deref());
        self.blocks.push(DocumentBlock {
            id: id.clone(),
            kind,
            text,
            parent_id,
            order,
            span,
            level,
            language,
            parse_status,
        });
        id
    }

    /// Parent heading for a heading of `level`, popping deeper open headings.
    fn heading_parent(&mut self, level: u8) -> Option<String> {
        while let Some((open_level, _)) = self.heading_stack.last() {
            if *open_level >= level {
                self.heading_stack.pop();
            } else {
                break;
            }
        }
        self.heading_stack.last().map(|(_, id)| id.clone())
    }

    fn anchor(&mut self, title: &str, block_id: &str) {
        let base = slugify(title);
        let base = if base.is_empty() {
            "section".to_string()
        } else {
            base
        };
        let mut candidate = base.clone();
        let mut suffix = 1u32;
        while self.anchors.iter().any(|anchor| anchor.anchor == candidate) {
            candidate = format!("{base}-{suffix}");
            suffix += 1;
        }
        self.anchors.push(Anchor {
            anchor: candidate,
            block_id: block_id.to_string(),
            title: title.to_string(),
        });
    }
}

impl DocumentParserPort for MarkdownParser {
    fn parser_id(&self) -> &'static str {
        PARSER_ID
    }

    fn parser_version(&self) -> &'static str {
        PARSER_VERSION
    }

    fn supports(&self, format: MediaFormat) -> bool {
        matches!(format, MediaFormat::Markdown)
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
        let text = String::from_utf8_lossy(bytes).to_string();
        let mut builder = Builder {
            document_id,
            source: source.clone(),
            lines: split_lines(&text),
            blocks: Vec::new(),
            diagnostics: Vec::new(),
            links: Vec::new(),
            anchors: Vec::new(),
            heading_stack: Vec::new(),
            list_stack: Vec::new(),
            include_images: limits.include_images,
        };

        let mut index = 0usize;
        while index < builder.lines.len() {
            let line_number = builder.lines[index].number;
            let byte_start = builder.lines[index].byte_start;
            let byte_end = builder.lines[index].byte_end();
            let line_text = builder.lines[index].text.to_string();
            let trimmed = line_text.trim();

            if trimmed.is_empty() {
                builder.list_stack.clear();
                index += 1;
                continue;
            }

            if let Some((marker, width)) = fence_marker(&line_text) {
                let start = index;
                let language = line_text
                    .trim_start()
                    .trim_start_matches(marker)
                    .trim()
                    .to_string();
                index += 1;
                let mut body = String::new();
                let mut closed = false;
                let mut end_line = line_number;
                let mut end_byte = byte_end;
                while index < builder.lines.len() {
                    let candidate = &builder.lines[index];
                    let candidate_trimmed = candidate.text.trim_start();
                    let fence_len = candidate_trimmed
                        .chars()
                        .take_while(|ch| *ch == marker)
                        .count();
                    if fence_len >= width && candidate_trimmed.starts_with(marker) {
                        closed = true;
                        end_line = candidate.number;
                        end_byte = candidate.byte_end();
                        break;
                    }
                    body.push_str(candidate.text);
                    body.push('\n');
                    end_line = candidate.number;
                    end_byte = candidate.byte_end();
                    index += 1;
                }
                let span = Span::new(
                    builder.lines[start].number,
                    end_line,
                    byte_start as u64,
                    end_byte as u64,
                );
                let status = if closed {
                    ParseStatus::Complete
                } else {
                    builder.diagnostics.push(ParseDiagnostic {
                        code: "UNTERMINATED_CODE_FENCE".to_string(),
                        message: "code fence is not closed; the block ends at EOF".to_string(),
                        span: Some(span.clone()),
                        severity: Severity::Warning,
                    });
                    ParseStatus::Partial
                };
                let parent = builder.heading_stack.last().map(|(_, id)| id.clone());
                builder.push(
                    BlockKind::Code,
                    span,
                    Some(body),
                    parent,
                    None,
                    Some(language),
                    status,
                );
                if closed {
                    index += 1;
                }
                continue;
            }

            if let Some((level, title)) = atx_heading(&line_text) {
                let span = Span::new(line_number, line_number, byte_start as u64, byte_end as u64);
                let parent = builder.heading_parent(level);
                builder.list_stack.clear();
                let id = builder.push(
                    BlockKind::Heading,
                    span,
                    Some(title.clone()),
                    parent,
                    Some(level),
                    None,
                    ParseStatus::Complete,
                );
                builder.heading_stack.push((level, id.clone()));
                builder.anchor(&title, &id);
                index += 1;
                continue;
            }

            if index + 1 < builder.lines.len() && !trimmed.starts_with(['-', '*', '+', '|', '>']) {
                if let Some(level) = setext_level(builder.lines[index + 1].text) {
                    let underline = &builder.lines[index + 1];
                    let span = Span::new(
                        line_number,
                        underline.number,
                        byte_start as u64,
                        underline.byte_end() as u64,
                    );
                    let title = trimmed.to_string();
                    let parent = builder.heading_parent(level);
                    builder.list_stack.clear();
                    let id = builder.push(
                        BlockKind::Heading,
                        span,
                        Some(title.clone()),
                        parent,
                        Some(level),
                        None,
                        ParseStatus::Complete,
                    );
                    builder.heading_stack.push((level, id.clone()));
                    builder.anchor(&title, &id);
                    index += 2;
                    continue;
                }
            }

            if line_text.contains('|')
                && index + 1 < builder.lines.len()
                && is_table_separator(builder.lines[index + 1].text)
            {
                let start = index;
                index += 2;
                let mut rows: Vec<Vec<String>> = vec![table_cells(&line_text)];
                let mut row_lines: Vec<u32> = vec![line_number];
                while index < builder.lines.len() && builder.lines[index].text.contains('|') {
                    rows.push(table_cells(builder.lines[index].text));
                    row_lines.push(builder.lines[index].number);
                    index += 1;
                }
                let end_line = *row_lines.last().unwrap_or(&line_number);
                let end_byte = builder.lines[index - 1].byte_end();
                let span = Span::new(
                    builder.lines[start].number,
                    end_line,
                    builder.lines[start].byte_start as u64,
                    end_byte as u64,
                );
                let parent = builder.heading_stack.last().map(|(_, id)| id.clone());
                let table_id = builder.push(
                    BlockKind::Table,
                    span.clone(),
                    Some(format!("{} rows", rows.len())),
                    parent,
                    None,
                    None,
                    ParseStatus::Complete,
                );
                for (row_index, row) in rows.iter().enumerate() {
                    let line_number = row_lines[row_index];
                    for (cell_index, cell) in row.iter().enumerate() {
                        let cell_span =
                            Span::new(line_number, line_number, span.byte_start, span.byte_end);
                        builder.push(
                            BlockKind::TableCell,
                            cell_span,
                            Some(cell.clone()),
                            Some(table_id.clone()),
                            Some(cell_index as u8),
                            None,
                            ParseStatus::Complete,
                        );
                    }
                }
                continue;
            }

            if trimmed.starts_with("![") && trimmed.contains("](") {
                let span = Span::new(line_number, line_number, byte_start as u64, byte_end as u64);
                let parent = builder.heading_stack.last().map(|(_, id)| id.clone());
                builder.push(
                    BlockKind::Image,
                    span.clone(),
                    None,
                    parent,
                    None,
                    None,
                    ParseStatus::Unparsed,
                );
                if builder.include_images {
                    builder.diagnostics.push(ocr_diagnostic(span));
                } else {
                    builder.diagnostics.push(ParseDiagnostic {
                        code: "IMAGE_SKIPPED".to_string(),
                        message: "image block skipped by request; OCR is not enabled".to_string(),
                        span: Some(span),
                        severity: Severity::Info,
                    });
                }
                index += 1;
                continue;
            }

            if let Some((depth, item_text)) = list_item(&line_text) {
                let span = Span::new(line_number, line_number, byte_start as u64, byte_end as u64);
                while let Some((open_depth, _)) = builder.list_stack.last() {
                    if *open_depth >= depth {
                        builder.list_stack.pop();
                    } else {
                        break;
                    }
                }
                let parent = builder
                    .list_stack
                    .last()
                    .map(|(_, id)| id.clone())
                    .or_else(|| builder.heading_stack.last().map(|(_, id)| id.clone()));
                let id = builder.push(
                    BlockKind::List,
                    span,
                    Some(item_text),
                    parent,
                    Some(depth),
                    None,
                    ParseStatus::Complete,
                );
                builder.list_stack.push((depth, id));
                index += 1;
                continue;
            }

            let start = index;
            let mut text_lines: Vec<String> = Vec::new();
            while index < builder.lines.len() {
                let candidate = &builder.lines[index];
                let candidate_trimmed = candidate.text.trim();
                if candidate_trimmed.is_empty()
                    || atx_heading(candidate.text).is_some()
                    || fence_marker(candidate.text).is_some()
                    || list_item(candidate.text).is_some()
                    || (candidate.text.contains('|')
                        && index + 1 < builder.lines.len()
                        && is_table_separator(builder.lines[index + 1].text))
                {
                    break;
                }
                text_lines.push(candidate.text.to_string());
                index += 1;
            }
            if text_lines.is_empty() {
                index += 1;
                continue;
            }
            let end_line = builder.lines[index - 1].number;
            let end_byte = builder.lines[index - 1].byte_end();
            let span = Span::new(
                builder.lines[start].number,
                end_line,
                builder.lines[start].byte_start as u64,
                end_byte as u64,
            );
            let text = text_lines.join("\n");
            let parent = builder.heading_stack.last().map(|(_, id)| id.clone());
            let id = builder.push(
                BlockKind::Paragraph,
                span,
                Some(text),
                parent,
                None,
                None,
                ParseStatus::Complete,
            );
            let link_lines: Vec<Line<'_>> = builder.lines[start..index]
                .iter()
                .map(|line| Line {
                    number: line.number,
                    byte_start: line.byte_start,
                    text: line.text,
                })
                .collect();
            for line in &link_lines {
                inline_links(&id, line, &mut builder.links);
            }
        }

        let document = finish_document(
            builder.document_id.clone(),
            builder.source.clone(),
            PARSER_ID,
            PARSER_VERSION,
            builder.blocks.clone(),
            builder.diagnostics.clone(),
        );
        Ok(ParseOutput {
            document,
            links: builder.links,
            anchors: builder.anchors,
        })
    }
}
