//! Canonical Document Intelligence types (§33.1) and the request/response shapes
//! of the 14 document operations (DAPI-01..14).
//!
//! Domain types only: no database SDK, no second API registry, no second error
//! enum. Wire envelopes and JSON Schemas stay in `ukma-contracts`.

use serde::{Deserialize, Serialize};

/// Validation state of an extracted claim (`§33.1 ValidationState`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationState {
    /// Extracted but not checked by a human or a verifier.
    Unverified,
    /// Confirmed by a review step.
    Verified,
    /// Rejected by a review step.
    Rejected,
}

/// How strongly a statement is expressed (`§33.1 normative_strength`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NormativeStrength {
    /// Mandatory.
    Must,
    /// Recommended.
    Should,
    /// Optional.
    May,
    /// No normative keyword found.
    Unknown,
}

/// Lifecycle of a requirement candidate (`§33.1 status`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementStatus {
    /// Proposed by an extractor; never authoritative.
    Candidate,
    /// Accepted by a reviewer.
    Approved,
    /// Rejected by a reviewer.
    Rejected,
    /// Replaced by a newer revision.
    Superseded,
}

/// Kind of a document block (`§33.1 DocumentBlock.kind`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", schemars(rename = "DocumentBlockKind"))]
pub enum BlockKind {
    /// Section heading.
    Heading,
    /// Paragraph.
    Paragraph,
    /// Table.
    Table,
    /// Table cell.
    TableCell,
    /// List (container).
    List,
    /// Code block.
    Code,
    /// Image; without OCR it stays `unparsed`.
    Image,
    /// Footnote.
    Footnote,
    /// Anything else.
    Other,
}

/// Parse status of one block (`§33.1 parse_status`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParseStatus {
    /// Fully parsed.
    Complete,
    /// Partially parsed.
    Partial,
    /// Not parsed at all (for example an image without OCR).
    Unparsed,
}

/// Completeness of a parsed document (`§33.1 completeness`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocCompleteness {
    /// Everything was parsed.
    Complete,
    /// Something was skipped; the diagnostics say what.
    Partial,
    /// The format is not supported at all.
    Unsupported,
}

impl DocCompleteness {
    /// Canonical string.
    pub fn as_str(self) -> &'static str {
        match self {
            DocCompleteness::Complete => "complete",
            DocCompleteness::Partial => "partial",
            DocCompleteness::Unsupported => "unsupported",
        }
    }
}

/// Diagnostic severity (`§33.1`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Informational.
    Info,
    /// Warning.
    Warning,
    /// Error.
    Error,
}

/// Half-open byte range plus 1-based lines inside one artifact.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    /// First line (1-based).
    pub line_start: u32,
    /// Last line (1-based, inclusive).
    pub line_end: u32,
    /// First byte offset.
    pub byte_start: u64,
    /// End byte offset (exclusive).
    pub byte_end: u64,
}

impl Span {
    /// New span.
    pub fn new(line_start: u32, line_end: u32, byte_start: u64, byte_end: u64) -> Self {
        Self {
            line_start,
            line_end,
            byte_start,
            byte_end,
        }
    }

    /// Validate the invariants.
    pub fn validate(&self) -> Result<()> {
        if self.line_start == 0 || self.line_end == 0 {
            return Err(DocumentError::InvalidArgument {
                field: "span.line",
                reason: "lines are 1-based".into(),
            });
        }
        if self.line_end < self.line_start {
            return Err(DocumentError::InvalidArgument {
                field: "span.line_end",
                reason: "end precedes start".into(),
            });
        }
        if self.byte_end < self.byte_start {
            return Err(DocumentError::InvalidArgument {
                field: "span.byte_end",
                reason: "end precedes start".into(),
            });
        }
        Ok(())
    }

    /// Whether two spans overlap.
    pub fn overlaps(&self, other: &Span) -> bool {
        self.byte_start < other.byte_end && other.byte_start < self.byte_end
    }
}

/// Where a document came from (`§33.1 DocumentSource`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentSource {
    /// Original URI (informational; the artifact is the authority).
    pub source_uri: String,
    /// Media type of the artifact.
    pub media_type: String,
    /// `sha256:<hex>` of the artifact bytes.
    pub content_hash: String,
    /// Caller-provided revision label.
    pub revision: String,
    /// Language hint used by the parser.
    pub language_hint: Option<String>,
    /// Artifact reference.
    pub artifact_ref: String,
}

impl DocumentSource {
    /// Validate the declared media type against the supported set.
    pub fn validate_media_type(&self) -> Result<MediaFormat> {
        MediaFormat::from_media_type(&self.media_type).ok_or_else(|| {
            DocumentError::UnsupportedMediaType {
                media_type: self.media_type.clone(),
            }
        })
    }
}

/// Document formats this build can parse (ADR-048 O-02: no OCR).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaFormat {
    /// `text/markdown`
    Markdown,
    /// `application/pdf` (text layer only)
    Pdf,
    /// DOCX
    Docx,
}

impl MediaFormat {
    /// Map a media type to a supported format.
    pub fn from_media_type(media_type: &str) -> Option<Self> {
        let base = media_type
            .split(';')
            .next()
            .unwrap_or(media_type)
            .trim()
            .to_ascii_lowercase();
        match base.as_str() {
            "text/markdown" | "text/x-markdown" | "text/plain" => Some(MediaFormat::Markdown),
            "application/pdf" => Some(MediaFormat::Pdf),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
                Some(MediaFormat::Docx)
            }
            _ => None,
        }
    }

    /// Parser id recorded in manifests.
    pub fn parser_id(self) -> &'static str {
        match self {
            MediaFormat::Markdown => "ukma-markdown",
            MediaFormat::Pdf => "ukma-pdf-textlayer",
            MediaFormat::Docx => "ukma-docx",
        }
    }
}

/// One block of a parsed document (`§33.1 DocumentBlock`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentBlock {
    /// Block id (deterministic, revision-scoped).
    pub id: String,
    /// Block kind.
    pub kind: BlockKind,
    /// Text content, when parsed.
    pub text: Option<String>,
    /// Parent block (heading or list), when any.
    pub parent_id: Option<String>,
    /// Document order.
    pub order: u32,
    /// Location in the artifact.
    pub span: Span,
    /// Heading level (1-6) for headings and list depth for list items.
    pub level: Option<u8>,
    /// Fenced code language.
    pub language: Option<String>,
    /// Parse status.
    pub parse_status: ParseStatus,
}

/// One parse diagnostic (`§33.1 diagnostics[]`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParseDiagnostic {
    /// Stable code.
    pub code: String,
    /// Safe message.
    pub message: String,
    /// Where it happened.
    pub span: Option<Span>,
    /// Severity.
    pub severity: Severity,
}

/// Result of parsing one artifact (`§33.1 ParsedDocument`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedDocument {
    /// Document id.
    pub document_id: String,
    /// Source description.
    pub source: DocumentSource,
    /// Blocks in document order.
    pub blocks: Vec<DocumentBlock>,
    /// Diagnostics.
    pub diagnostics: Vec<ParseDiagnostic>,
    /// Parser id.
    pub parser_id: String,
    /// Parser version.
    pub parser_version: String,
    /// Completeness.
    pub completeness: DocCompleteness,
}

impl ParsedDocument {
    /// Whether every block was parsed.
    pub fn is_complete(&self) -> bool {
        self.completeness == DocCompleteness::Complete
    }

    /// Blocks that are not fully parsed.
    pub fn unparsed_blocks(&self) -> usize {
        self.blocks
            .iter()
            .filter(|block| block.parse_status != ParseStatus::Complete)
            .count()
    }
}

/// An extracted requirement (`§33.1 RequirementCandidate`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequirementCandidate {
    /// Candidate id.
    pub id: String,
    /// Optional external requirement id (for example `REQ-12` when the text has one).
    pub requirement_id: Option<String>,
    /// The sentence the requirement was extracted from.
    pub text: String,
    /// Normative strength.
    pub normative_strength: NormativeStrength,
    /// Evidence ids (block ids plus content hash).
    pub extracted_from: Vec<String>,
    /// Extractor confidence in percent (0-100; no floating point in the identity).
    pub confidence: u8,
    /// Verification state: always `unverified` until a reviewer acts.
    pub verification: ValidationState,
    /// Lifecycle status: always `candidate` until a reviewer acts.
    pub status: RequirementStatus,
    /// Document revision the candidate was extracted from.
    pub revision: String,
    /// E-tag of the candidate (optimistic concurrency for review).
    pub etag: String,
}

/// An extracted relation between two refs (`§33.1 ExtractedRelation`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractedRelation {
    /// Source ref.
    pub source: String,
    /// Target ref.
    pub target: String,
    /// Relation kind (`references`, `implements`, `contradicts`, …).
    pub relation_kind: String,
    /// Evidence ids.
    pub evidence: Vec<String>,
    /// Confidence in percent.
    pub confidence: u8,
    /// Verification state.
    pub verification: ValidationState,
}

/// One search hit (`§33.2 document.search`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    /// Document id.
    pub document_id: String,
    /// Block id.
    pub block_id: String,
    /// Block kind.
    pub kind: BlockKind,
    /// Matching text (whole block, trimmed to the caller's budget by the caller).
    pub snippet: String,
    /// Block span.
    pub span: Span,
    /// Score in percent (deterministic term match).
    pub score: u8,
}

/// One consistency finding (`§33.1 ConsistencyFinding`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsistencyFinding {
    /// Finding id.
    pub id: String,
    /// Category.
    pub category: ConsistencyCategory,
    /// Refs involved.
    pub refs: Vec<String>,
    /// Evidence ids.
    pub evidence: Vec<String>,
    /// Severity.
    pub severity: Severity,
    /// Confidence in percent.
    pub confidence: Option<u8>,
    /// Verification state.
    pub verification: ValidationState,
}

/// Category of a consistency finding.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsistencyCategory {
    /// Two statements cannot both hold.
    Contradiction,
    /// A referenced thing does not exist.
    MissingReference,
    /// A candidate was never reviewed.
    Unverified,
    /// Wording allows more than one reading.
    Ambiguity,
    /// Implementation looks older than the document.
    StaleImplementation,
}

/// Result of `document.verify_consistency` (`§33.1 ConsistencyReport`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsistencyReport {
    /// Findings, ordered by category then id.
    pub findings: Vec<ConsistencyFinding>,
    /// Refs that were analysed.
    pub analyzed_refs: Vec<String>,
    /// Refs that were skipped (with the reason in `skipped_reasons`).
    pub skipped_refs: Vec<String>,
    /// Why each skipped ref was skipped.
    pub skipped_reasons: Vec<String>,
    /// `complete` only when nothing was skipped or truncated.
    pub completeness: String,
    /// Snapshot the report was computed against.
    pub snapshot_id: String,
}

/// One change between two revisions (`§33.1 DocDiff`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocChange {
    /// Change kind.
    pub kind: DocChangeKind,
    /// Old block, when present.
    pub old: Option<DocumentBlock>,
    /// New block, when present.
    pub new: Option<DocumentBlock>,
    /// Evidence ids.
    pub evidence: Vec<String>,
}

/// Kind of document change.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocChangeKind {
    /// Only in the new revision.
    Added,
    /// Only in the old revision.
    Removed,
    /// Present in both with different text.
    Modified,
    /// Present in both with the same text at a different position.
    Moved,
}

/// Result of `document.compare_revisions` (`§33.1 DocDiff`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocDiff {
    /// Old revision label.
    pub from: String,
    /// New revision label.
    pub to: String,
    /// Changes.
    pub changes: Vec<DocChange>,
    /// Completeness.
    pub completeness: String,
}

/// Page request (§31.4). Kept local so the crate has no wire dependency.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", schemars(rename = "DocumentPageRequest"))]
pub struct PageRequest {
    /// Opaque cursor.
    pub cursor: Option<String>,
    /// Maximum items.
    pub limit: u32,
}

impl Default for PageRequest {
    fn default() -> Self {
        Self {
            cursor: None,
            limit: 50,
        }
    }
}

impl PageRequest {
    /// Largest page a response may return.
    pub const MAX_LIMIT: u32 = 200;

    /// Page request with a limit.
    pub fn new(limit: u32) -> Self {
        Self {
            cursor: None,
            limit,
        }
    }

    /// Validate the limit.
    pub fn validate(&self) -> Result<()> {
        if self.limit == 0 || self.limit > Self::MAX_LIMIT {
            return Err(DocumentError::InvalidArgument {
                field: "page.limit",
                reason: format!("must be in 1..={}", Self::MAX_LIMIT),
            });
        }
        Ok(())
    }

    /// Cursor offset.
    pub fn offset(&self) -> Result<usize> {
        match &self.cursor {
            None => Ok(0),
            Some(cursor) => cursor
                .parse::<usize>()
                .map_err(|_| DocumentError::CursorExpired {
                    cursor: cursor.clone(),
                }),
        }
    }
}

/// One page of results.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page<T> {
    /// Items.
    pub items: Vec<T>,
    /// Cursor for the next page.
    pub next_cursor: Option<String>,
}

impl<T> Page<T> {
    /// Build a page from the full ordered result set.
    pub fn of(items: Vec<T>, request: &PageRequest) -> Result<Self> {
        let offset = request.offset()?;
        if offset > items.len() {
            return Err(DocumentError::CursorExpired {
                cursor: request.cursor.clone().unwrap_or_default(),
            });
        }
        let limit = request.limit as usize;
        let mut page: Vec<T> = items.into_iter().skip(offset).take(limit + 1).collect();
        let next_cursor = if page.len() > limit {
            page.truncate(limit);
            Some((offset + limit).to_string())
        } else {
            None
        };
        Ok(Self {
            items: page,
            next_cursor,
        })
    }
}

/// Handle of an accepted ingest job.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", schemars(rename = "DocumentJobHandle"))]
pub struct JobHandle {
    /// Job id.
    pub job_id: String,
    /// Job kind.
    pub kind: String,
    /// Stage when the handle was produced.
    pub stage: String,
}

/// Live job view.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", schemars(rename = "DocumentJobView"))]
pub struct JobView {
    /// Job id.
    pub job_id: String,
    /// Job kind.
    pub kind: String,
    /// Stage.
    pub stage: String,
    /// Whether canonical data was committed.
    pub canonical_committed: bool,
    /// Reference to the parsed result, available after the commit.
    pub result_ref: Option<String>,
    /// Diagnostics.
    pub diagnostics: Vec<String>,
    /// Unparsed block ratio in percent when the document is partial.
    pub partial_ratio: Option<u8>,
}

/// Bookkeeping receipt of a mutation.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", schemars(rename = "DocumentChangeReceipt"))]
pub struct ChangeReceipt {
    /// Resource affected.
    pub resource_id: String,
    /// New etag.
    pub etag: String,
    /// Whether anything changed.
    pub changed: bool,
    /// Events a gateway must publish.
    pub events: Vec<String>,
}

// ---------------------------------------------------------------------------
// Requests / responses of the 14 operations
// ---------------------------------------------------------------------------

/// `document.ingest` (DAPI-01).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentIngest {
    /// Source description.
    pub source: DocumentSource,
    /// Parser profile the caller requires.
    pub parser_profile: String,
    /// Include images (they stay unparsed without OCR).
    pub include_images: bool,
    /// Must be `true`; a synchronous contract would hide job state.
    pub asynchronous: bool,
}

/// `document.get_ingest_job` (DAPI-02).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", schemars(rename = "DocumentGetJob"))]
pub struct GetJob {
    /// Job id.
    pub job_id: String,
}

/// `document.cancel_ingest_job` (DAPI-03).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", schemars(rename = "DocumentCancelJob"))]
pub struct CancelJob {
    /// Job id.
    pub job_id: String,
    /// Reason.
    pub reason: String,
}

/// `document.get_document` (DAPI-04).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetDocument {
    /// Document id.
    pub document_id: String,
    /// Whether blocks should be returned.
    pub include_blocks: bool,
    /// Page over the blocks.
    pub page: PageRequest,
}

/// Result of `document.get_document`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentView {
    /// Document id.
    pub document_id: String,
    /// Source description.
    pub source: DocumentSource,
    /// Blocks (a page when `include_blocks`).
    pub blocks: Page<DocumentBlock>,
    /// Completeness.
    pub completeness: String,
    /// Artifact etag for optimistic concurrency.
    pub etag: String,
}

/// Analysis manifest of a parsed document.
///
/// Not a wire operation: this is how a caller (or a test) reads what the parser
/// actually did ? parser identity, diagnostics and how much was left unparsed.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentManifest {
    /// Document id.
    pub document_id: String,
    /// Parser id.
    pub parser_id: String,
    /// Parser version.
    pub parser_version: String,
    /// Completeness.
    pub completeness: String,
    /// Blocks parsed.
    pub block_count: u32,
    /// Percentage of blocks that are not fully parsed.
    pub partial_ratio: u8,
    /// Diagnostics.
    pub diagnostics: Vec<ParseDiagnostic>,
    /// E-tag.
    pub etag: String,
    /// Published generation.
    pub generation: String,
}

/// `document.parse_preview` (DAPI-05): parse without persisting.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsePreview {
    /// Source description.
    pub source: DocumentSource,
    /// Parser profile.
    pub parser_profile: String,
    /// Maximum bytes to parse (bounded work).
    pub max_bytes: u64,
}

/// `document.search` (DAPI-06).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchDocument {
    /// Scope the search runs in.
    pub tenant_id: String,
    /// Project.
    pub project_id: String,
    /// Query string.
    pub query: String,
    /// Restrict to these documents.
    pub document_refs: Vec<String>,
    /// Restrict to these languages.
    pub languages: Vec<String>,
    /// Restrict to these block kinds.
    pub block_kinds: Vec<BlockKind>,
    /// Page.
    pub page: PageRequest,
}

/// `document.extract_requirements` (DAPI-07).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractRequirements {
    /// Document id.
    pub document_id: String,
    /// Restrict to these section (heading) block ids.
    pub section_refs: Vec<String>,
    /// Extractor profile.
    pub extractor_profile: String,
    /// Page.
    pub page: PageRequest,
}

/// `document.extract_relations` (DAPI-08).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractRelations {
    /// Document id.
    pub document_id: String,
    /// Scope the targets live in.
    pub target_tenant_id: String,
    /// Target project.
    pub target_project_id: String,
    /// Relation kinds to extract.
    pub relation_kinds: Vec<String>,
    /// Extractor profile.
    pub extractor_profile: String,
    /// Page.
    pub page: PageRequest,
}

/// `document.review_requirement` (DAPI-09).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewRequirement {
    /// Candidate id.
    pub candidate_id: String,
    /// `approve` or `reject`.
    pub decision: ReviewDecision,
    /// Reviewer rationale.
    pub rationale: String,
    /// Evidence supporting the decision; empty means the review is rejected.
    pub evidence_ids: Vec<String>,
    /// Candidate etag for optimistic concurrency.
    pub expected_etag: String,
}

/// Review decision.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    /// Accept the candidate.
    Approve,
    /// Reject the candidate.
    Reject,
}

/// Result of `document.review_requirement`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequirementReviewResult {
    /// The updated candidate.
    pub requirement: RequirementCandidate,
    /// Receipt of the change.
    pub receipt: ChangeReceipt,
}

/// `document.list_requirements` (DAPI-10).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListRequirements {
    /// Document id.
    pub document_id: String,
    /// Restrict to these statuses.
    pub statuses: Vec<RequirementStatus>,
    /// Page.
    pub page: PageRequest,
}

/// `document.verify_consistency` (DAPI-11).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyConsistency {
    /// Documents to include.
    pub document_refs: Vec<String>,
    /// Whether to compare against code (requires `code:read`).
    pub compare_code: bool,
    /// Maximum findings.
    pub max_findings: u32,
}

/// `document.compare_revisions` (DAPI-12).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentRevisionDiff {
    /// Old document id.
    pub from: String,
    /// New document id.
    pub to: String,
}

/// `document.invalidate_revision` (DAPI-13).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvalidateDocumentRevision {
    /// Document id.
    pub document_id: String,
    /// Reason.
    pub reason: String,
    /// Current etag.
    pub expected_etag: String,
}

/// `document.get_source_span` (DAPI-14).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetSourceSpan {
    /// Document id.
    pub document_id: String,
    /// Requested span.
    pub span: Span,
    /// Maximum bytes the caller accepts.
    pub max_bytes: u64,
}

/// Result of `document.get_source_span`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSpanView {
    /// Text, omitted when redacted or unavailable.
    pub text: Option<String>,
    /// Artifact the span belongs to.
    pub artifact_ref: String,
    /// The requested span.
    pub span: Span,
    /// Whether the content was withheld.
    pub redacted: bool,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why a document operation could not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentError {
    /// The media type is not supported (no OCR, no legacy `.doc`).
    UnsupportedMediaType {
        /// Offending media type.
        media_type: String,
    },
    /// The artifact is larger than the profile allows.
    SourceTooLarge {
        /// Artifact size.
        bytes: u64,
        /// Limit.
        limit: u64,
    },
    /// The artifact is missing or its hash does not match.
    SourceUnavailable {
        /// Artifact reference.
        artifact_ref: String,
    },
    /// The artifact could not be parsed.
    SourceParseFailed {
        /// Safe message.
        message: String,
    },
    /// The two revisions belong to different documents.
    SourceRevisionMismatch {
        /// Requested.
        requested: String,
        /// Actual.
        actual: String,
    },
    /// Malformed argument.
    InvalidArgument {
        /// Field.
        field: &'static str,
        /// Reason.
        reason: String,
    },
    /// Resource not found in the bound revision.
    NotFound {
        /// What was not found.
        resource: String,
    },
    /// The revision is stale.
    IndexStale {
        /// Revision.
        revision: String,
    },
    /// The index is not published yet.
    IndexNotReady {
        /// Revision.
        revision: String,
    },
    /// Unknown job.
    JobNotFound {
        /// Job id.
        job_id: String,
    },
    /// Cancellation arrived after the commit point.
    JobNotCancellable {
        /// Job id.
        job_id: String,
    },
    /// Page cursor is invalid.
    CursorExpired {
        /// Cursor.
        cursor: String,
    },
    /// The caller lacks a required scope.
    Forbidden {
        /// Missing scope.
        required_scope: String,
    },
    /// The request names a different tenant/project than the resource.
    CrossScopeDenied {
        /// Requested scope.
        requested: String,
        /// Owning scope.
        actual: String,
    },
    /// Optimistic concurrency mismatch.
    EtagMismatch {
        /// Expected.
        expected: String,
        /// Actual.
        actual: String,
    },
    /// A review needs evidence.
    EvidenceRequired {
        /// Candidate under review.
        candidate_id: String,
    },
    /// The candidate is not in a reviewable state.
    RequirementStateConflict {
        /// Candidate.
        candidate_id: String,
        /// Current status.
        status: RequirementStatus,
    },
    /// No extractor is available for the requested profile.
    ModelUnavailable {
        /// Profile.
        profile: String,
    },
    /// An idempotency key was reused with different arguments.
    IdempotencyConflict {
        /// The reused key.
        key: String,
    },
    /// The document source was already deleted (the tombstone exists).
    AlreadyDeleted {
        /// Document id.
        document_id: String,
    },
    /// A running job prevents deleting this document source.
    DeletionBlocked {
        /// Document id.
        document_id: String,
    },
    /// A consistency check exceeded its finding budget.
    ResourceLimit {
        /// Detail.
        detail: String,
    },
}

impl DocumentError {
    /// Registered v1.2.3 error code.
    pub fn code(&self) -> &'static str {
        match self {
            DocumentError::UnsupportedMediaType { .. } => "UNSUPPORTED_MEDIA_TYPE",
            DocumentError::SourceTooLarge { .. } => "SOURCE_TOO_LARGE",
            DocumentError::SourceUnavailable { .. } => "SOURCE_UNAVAILABLE",
            DocumentError::SourceParseFailed { .. } => "SOURCE_PARSE_FAILED",
            DocumentError::SourceRevisionMismatch { .. } => "SOURCE_REVISION_MISMATCH",
            DocumentError::InvalidArgument { .. } => "INVALID_ARGUMENT",
            DocumentError::NotFound { .. } => "NOT_FOUND",
            DocumentError::IndexStale { .. } => "INDEX_STALE",
            DocumentError::IndexNotReady { .. } => "INDEX_NOT_READY",
            DocumentError::JobNotFound { .. } => "JOB_NOT_FOUND",
            DocumentError::JobNotCancellable { .. } => "JOB_NOT_CANCELLABLE",
            DocumentError::CursorExpired { .. } => "CURSOR_EXPIRED",
            DocumentError::Forbidden { .. } => "FORBIDDEN",
            DocumentError::CrossScopeDenied { .. } => "CROSS_SCOPE_DENIED",
            DocumentError::EtagMismatch { .. } => "ETAG_MISMATCH",
            DocumentError::EvidenceRequired { .. } => "EVIDENCE_REQUIRED",
            DocumentError::RequirementStateConflict { .. } => "LINK_STATE_CONFLICT",
            DocumentError::ModelUnavailable { .. } => "MODEL_UNAVAILABLE",
            DocumentError::IdempotencyConflict { .. } => "IDEMPOTENCY_CONFLICT",
            DocumentError::AlreadyDeleted { .. } => "ALREADY_DELETED",
            DocumentError::DeletionBlocked { .. } => "DELETION_BLOCKED",
            DocumentError::ResourceLimit { .. } => "RESOURCE_LIMIT",
        }
    }
}

impl std::fmt::Display for DocumentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DocumentError::UnsupportedMediaType { media_type } => {
                write!(f, "unsupported media type `{media_type}`")
            }
            DocumentError::SourceTooLarge { bytes, limit } => {
                write!(f, "artifact is {bytes} bytes, limit is {limit}")
            }
            DocumentError::SourceUnavailable { artifact_ref } => {
                write!(f, "artifact {artifact_ref} is unavailable")
            }
            DocumentError::SourceParseFailed { message } => write!(f, "parse failed: {message}"),
            DocumentError::SourceRevisionMismatch { requested, actual } => {
                write!(f, "requested {requested} but the document is {actual}")
            }
            DocumentError::InvalidArgument { field, reason } => write!(f, "{field}: {reason}"),
            DocumentError::NotFound { resource } => write!(f, "{resource} not found"),
            DocumentError::IndexStale { revision } => write!(f, "{revision} is stale"),
            DocumentError::IndexNotReady { revision } => write!(f, "{revision} is not ready"),
            DocumentError::JobNotFound { job_id } => write!(f, "job {job_id} not found"),
            DocumentError::JobNotCancellable { job_id } => {
                write!(f, "job {job_id} cannot be cancelled after the commit point")
            }
            DocumentError::CursorExpired { cursor } => write!(f, "cursor {cursor} expired"),
            DocumentError::Forbidden { required_scope } => {
                write!(f, "missing scope {required_scope}")
            }
            DocumentError::CrossScopeDenied { requested, actual } => {
                write!(f, "scope {requested} cannot read {actual}")
            }
            DocumentError::EtagMismatch { expected, actual } => {
                write!(f, "etag mismatch: expected {expected}, found {actual}")
            }
            DocumentError::EvidenceRequired { candidate_id } => {
                write!(f, "review of {candidate_id} needs evidence")
            }
            DocumentError::RequirementStateConflict {
                candidate_id,
                status,
            } => write!(f, "{candidate_id} is {status:?} and cannot be reviewed"),
            DocumentError::ModelUnavailable { profile } => {
                write!(f, "no extractor available for `{profile}`")
            }
            DocumentError::IdempotencyConflict { key } => {
                write!(
                    f,
                    "idempotency key {key} was reused with different arguments"
                )
            }
            DocumentError::AlreadyDeleted { document_id } => {
                write!(f, "document {document_id} was already deleted")
            }
            DocumentError::DeletionBlocked { document_id } => {
                write!(
                    f,
                    "document {document_id} cannot be deleted while a job is running"
                )
            }
            DocumentError::ResourceLimit { detail } => write!(f, "resource limit: {detail}"),
        }
    }
}

impl std::error::Error for DocumentError {}

/// Result alias.
pub type Result<T> = std::result::Result<T, DocumentError>;

// ---------------------------------------------------------------------------
// Parity operations (CodeGraph gap closure, spec v1.2.3)
// ---------------------------------------------------------------------------

/// `document.list_doc_sources`: list every ingested document source.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListDocSources {
    /// Page.
    pub page: PageRequest,
}

/// Request: remove an ingested document source (`remove_doc_source` parity,
/// DAPI-15).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveDocSource {
    /// Document id to remove.
    pub document_id: String,
}

/// Receipt for a removed document source (DAPI-15).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocDeletionReceipt {
    /// The removed document.
    pub document_id: String,
    /// Blocks removed by this call.
    pub removed_blocks: u32,
    /// True when this call performed the deletion; false when the call
    /// replayed a previously recorded idempotent deletion.
    pub deleted: bool,
}

/// One ingested document source.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocSourceSummary {
    /// Document id.
    pub document_id: String,
    /// Original URI (informational).
    pub source_uri: String,
    /// Media type.
    pub media_type: String,
    /// Caller-provided revision label.
    pub revision: String,
    /// Language hint, when one was given.
    pub language_hint: Option<String>,
    /// Parser that produced the blocks.
    pub parser_id: String,
    /// Blocks in the index.
    pub block_count: u32,
    /// Whether the revision is stale.
    pub stale: bool,
}

/// `document.generate_architecture_doc`: render an architecture document
/// from code facts supplied through the [`ArchitectureFactsPort`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerateArchitectureDoc {
    /// Repository the document is generated from.
    pub repository_id: String,
    /// Revision (commit SHA or a registered alias such as `@cwd`).
    pub revision: String,
    /// Optional document title (defaults to `Architecture: <repo>`).
    pub title: Option<String>,
}

/// The rendered architecture document.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchitectureDoc {
    /// The rendered markdown.
    pub markdown: String,
    /// Where the facts came from (`code_graph`).
    pub source: String,
    /// Section titles in order.
    pub sections: Vec<String>,
}
