//! Document parsers (P4-WP1/WP2).
//!
//! One port, one output shape: every format produces the same
//! [`ParsedDocument`] so the service never branches on the format after parsing.
//!
//! **No OCR** (ADR-048 O-02): an image region is reported as a block with
//! `parse_status = unparsed` plus a diagnostic, and the document completeness
//! drops to `partial`. Reporting an empty image as success would be a lie.

pub mod docx;
pub mod markdown;
pub mod pdf;

use crate::domain::{
    BlockKind, DocCompleteness, DocumentBlock, DocumentSource, MediaFormat, ParseDiagnostic,
    ParseStatus, ParsedDocument, Result, Span,
};

/// One outbound reference found while parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocLink {
    /// Block the link appears in.
    pub block_id: String,
    /// Raw link target (`https://…`, `#anchor`, `other.md`).
    pub target: String,
    /// `anchor`, `url`, or `document`.
    pub kind: &'static str,
    /// Location of the link inside the artifact.
    pub span: Span,
}

/// A heading anchor, so a link can be resolved back to a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchor {
    /// Anchor slug.
    pub anchor: String,
    /// Block the anchor belongs to.
    pub block_id: String,
    /// Heading text.
    pub title: String,
}

/// Everything a parser produces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseOutput {
    /// The §33.1 parsed document.
    pub document: ParsedDocument,
    /// Outbound links (used by consistency checks).
    pub links: Vec<DocLink>,
    /// Heading anchors.
    pub anchors: Vec<Anchor>,
}

impl ParseOutput {
    /// Resolve a `#anchor` link target to a block.
    pub fn block_for_anchor(&self, anchor: &str) -> Option<&str> {
        self.anchors
            .iter()
            .find(|candidate| candidate.anchor == anchor)
            .map(|candidate| candidate.block_id.as_str())
    }

    /// Blocks whose kind is one of `kinds`.
    pub fn blocks_of(&self, kinds: &[BlockKind]) -> Vec<&DocumentBlock> {
        self.document
            .blocks
            .iter()
            .filter(|block| kinds.contains(&block.kind))
            .collect()
    }
}

/// Bounds on the work a parser may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseLimits {
    /// Maximum artifact bytes to read.
    pub max_bytes: u64,
    /// Whether image regions are reported (they are never parsed).
    pub include_images: bool,
}

impl Default for ParseLimits {
    fn default() -> Self {
        Self {
            max_bytes: 8 * 1024 * 1024,
            include_images: true,
        }
    }
}

/// The parser port (§33.1 `ParsedDocument` producer).
pub trait DocumentParserPort: Send + Sync {
    /// Parser id recorded in manifests.
    fn parser_id(&self) -> &'static str;
    /// Parser version recorded in manifests.
    fn parser_version(&self) -> &'static str;
    /// Formats this parser handles.
    fn supports(&self, format: MediaFormat) -> bool;
    /// Parse one artifact.
    fn parse(
        &self,
        source: &DocumentSource,
        bytes: &[u8],
        limits: &ParseLimits,
    ) -> Result<ParseOutput>;
}

/// One parser for every supported format, so the service never branches on media
/// type after registration.
#[derive(Debug, Default, Clone, Copy)]
pub struct DocumentParsers;

impl DocumentParsers {
    /// New dispatcher.
    pub fn new() -> Self {
        Self
    }
}

impl DocumentParserPort for DocumentParsers {
    fn parser_id(&self) -> &'static str {
        "ukma-document"
    }

    fn parser_version(&self) -> &'static str {
        "document/1"
    }

    fn supports(&self, format: MediaFormat) -> bool {
        matches!(
            format,
            MediaFormat::Markdown | MediaFormat::Pdf | MediaFormat::Docx
        )
    }

    fn parse(
        &self,
        source: &DocumentSource,
        bytes: &[u8],
        limits: &ParseLimits,
    ) -> Result<ParseOutput> {
        let format = source.validate_media_type()?;
        if bytes.len() as u64 > limits.max_bytes {
            return Err(crate::domain::DocumentError::SourceTooLarge {
                bytes: bytes.len() as u64,
                limit: limits.max_bytes,
            });
        }
        match format {
            MediaFormat::Markdown => markdown::MarkdownParser::new().parse(source, bytes, limits),
            MediaFormat::Pdf => pdf::PdfParser::new().parse(source, bytes, limits),
            MediaFormat::Docx => docx::DocxParser::new().parse(source, bytes, limits),
        }
    }
}

/// Deterministic, revision-scoped block id.
pub(crate) fn block_id(
    document_id: &str,
    order: u32,
    kind: BlockKind,
    span: &Span,
    text: Option<&str>,
) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(document_id.as_bytes());
    hasher.update(order.to_le_bytes());
    hasher.update(format!("{kind:?}").as_bytes());
    hasher.update(span.byte_start.to_le_bytes());
    hasher.update(span.byte_end.to_le_bytes());
    if let Some(text) = text {
        hasher.update(text.as_bytes());
    }
    let digest = hasher.finalize();
    let hex: String = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("blk:v1:{hex}")
}

/// `sha256:<hex>` of the artifact bytes.
pub(crate) fn content_hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    format!(
        "sha256:{}",
        digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

/// Diagnostic: images are not parsed because OCR is not enabled.
pub(crate) fn ocr_diagnostic(span: Span) -> ParseDiagnostic {
    ParseDiagnostic {
        code: "OCR_NOT_IMPLEMENTED".to_string(),
        message: "image region left unparsed: OCR is not enabled (ADR-048 O-02)".to_string(),
        span: Some(span),
        severity: crate::domain::Severity::Warning,
    }
}

/// Assemble a parsed document, deriving completeness from the blocks so a parser
/// cannot claim `complete` while returning unparsed blocks.
pub(crate) fn finish_document(
    document_id: String,
    source: DocumentSource,
    parser_id: &'static str,
    parser_version: &'static str,
    blocks: Vec<DocumentBlock>,
    diagnostics: Vec<ParseDiagnostic>,
) -> ParsedDocument {
    let unparsed = blocks
        .iter()
        .any(|block| block.parse_status != ParseStatus::Complete);
    let completeness = if unparsed {
        DocCompleteness::Partial
    } else {
        DocCompleteness::Complete
    };
    ParsedDocument {
        document_id,
        source,
        blocks,
        diagnostics,
        parser_id: parser_id.to_string(),
        parser_version: parser_version.to_string(),
        completeness,
    }
}
