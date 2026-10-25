//! PDF text-layer and DOCX parser tests (`P4-WP2`).
//!
//! Both fixtures are built in the test: the PDF with `lopdf` (so the xref table is
//! real) and the DOCX with `zip` (so it is a real OOXML container). That keeps the
//! repository free of binary blobs while still exercising the parsers end to end.
//!
//! The negative cases are the point of this file: without OCR an image region and
//! a page with no text layer must be reported, never silently treated as empty.

use std::io::{Cursor, Write};

use document_intelligence::domain::{
    BlockKind, DocCompleteness, DocumentError, DocumentSource, MediaFormat, ParseStatus,
};
use document_intelligence::parsers::{
    docx::DocxParser, pdf::PdfParser, DocumentParserPort, DocumentParsers, ParseLimits,
};
use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document, Object, Stream};

fn source(media_type: &str) -> DocumentSource {
    DocumentSource {
        source_uri: "file:///fixture".to_string(),
        media_type: media_type.to_string(),
        content_hash: "sha256:placeholder".to_string(),
        revision: "rev-1".to_string(),
        language_hint: Some("en".to_string()),
        artifact_ref: "artifact:fixture".to_string(),
    }
}

/// A one-page PDF whose text layer says `Hello world`.
fn text_pdf() -> Vec<u8> {
    let mut document = Document::with_version("1.5");
    let pages_id = document.new_object_id();
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let resources = dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    };
    let content = Content {
        operations: vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 24.into()]),
            Operation::new("Td", vec![20.into(), 100.into()]),
            Operation::new("Tj", vec![Object::string_literal("Hello world")]),
            Operation::new("ET", vec![]),
        ],
    };
    let content_id = document.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 200.into(), 200.into()],
        "Contents" => content_id,
        "Resources" => resources.clone(),
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1,
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);
    let mut buffer = Vec::new();
    document.save_to(&mut Cursor::new(&mut buffer)).unwrap();
    buffer
}

/// A one-page PDF whose page holds only an image (a scan): no text layer at all.
fn image_only_pdf() -> Vec<u8> {
    let mut document = Document::with_version("1.5");
    let pages_id = document.new_object_id();
    let image_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 1,
            "Height" => 1,
            "ColorSpace" => "DeviceGray",
            "BitsPerComponent" => 8,
        },
        vec![0u8],
    ));
    let resources = dictionary! {
        "XObject" => dictionary! { "Im1" => image_id },
    };
    let content = Content {
        operations: vec![
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![
                    200.into(),
                    0.into(),
                    0.into(),
                    200.into(),
                    0.into(),
                    0.into(),
                ],
            ),
            Operation::new("Do", vec!["Im1".into()]),
            Operation::new("Q", vec![]),
        ],
    };
    let content_id = document.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 200.into(), 200.into()],
        "Contents" => content_id,
        "Resources" => resources,
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1,
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);
    let mut buffer = Vec::new();
    document.save_to(&mut Cursor::new(&mut buffer)).unwrap();
    buffer
}

/// A DOCX with a heading, a paragraph, a numbered list item, a table and an image.
fn docx_bytes() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Overview</w:t></w:r></w:p>
    <w:p><w:r><w:t>The system MUST keep evidence.</w:t></w:r></w:p>
    <w:p><w:pPr><w:numPr/></w:pPr><w:r><w:t>first item</w:t></w:r></w:p>
    <w:tbl>
      <w:tr><w:tc><w:p><w:r><w:t>Name</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Kind</w:t></w:r></w:p></w:tc></w:tr>
      <w:tr><w:tc><w:p><w:r><w:t>a</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>b</w:t></w:r></w:p></w:tc></w:tr>
    </w:tbl>
    <w:p><w:r><w:drawing/></w:r></w:p>
  </w:body>
</w:document>"#;
    let mut buffer = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buffer);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("word/document.xml", options).unwrap();
        zip.write_all(document_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buffer.into_inner()
}

#[test]
fn pdf_text_layer_becomes_a_paragraph_with_its_text() {
    let bytes = text_pdf();
    let output = PdfParser::new()
        .parse(&source("application/pdf"), &bytes, &ParseLimits::default())
        .expect("pdf must parse");
    assert_eq!(output.document.parser_id, "ukma-pdf-textlayer");
    assert_eq!(output.document.completeness, DocCompleteness::Complete);
    let paragraph = output
        .document
        .blocks
        .iter()
        .find(|block| block.kind == BlockKind::Paragraph)
        .expect("paragraph block");
    let text = paragraph.text.clone().unwrap_or_default();
    assert!(text.contains("Hello"), "text: {text}");
    assert!(text.contains("world"), "text: {text}");
    assert_eq!(paragraph.parse_status, ParseStatus::Complete);
    assert_eq!(paragraph.span.line_start, 1);
}

#[test]
fn pdf_image_only_page_is_unparsed_and_partial() {
    let bytes = image_only_pdf();
    let output = PdfParser::new()
        .parse(&source("application/pdf"), &bytes, &ParseLimits::default())
        .expect("pdf must parse");
    assert_eq!(
        output.document.completeness,
        DocCompleteness::Partial,
        "no OCR means no complete document"
    );
    let image = output
        .document
        .blocks
        .iter()
        .find(|block| block.kind == BlockKind::Image)
        .unwrap_or_else(|| panic!("image block; blocks={:?}", output.document.blocks));
    assert_eq!(image.parse_status, ParseStatus::Unparsed);
    assert!(image.text.is_none());
    assert!(output
        .document
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "OCR_NOT_IMPLEMENTED"));
}

#[test]
fn a_broken_pdf_is_a_typed_failure_not_an_empty_document() {
    let error = PdfParser::new()
        .parse(
            &source("application/pdf"),
            b"not a pdf at all",
            &ParseLimits::default(),
        )
        .unwrap_err();
    assert_eq!(error.code(), "SOURCE_PARSE_FAILED");
}

#[test]
fn docx_keeps_headings_lists_tables_and_reports_images() {
    let bytes = docx_bytes();
    let output = DocxParser::new()
        .parse(
            &source("application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
            &bytes,
            &ParseLimits::default(),
        )
        .expect("docx must parse");
    let kinds: Vec<BlockKind> = output
        .document
        .blocks
        .iter()
        .map(|block| block.kind)
        .collect();
    assert!(kinds.contains(&BlockKind::Heading), "kinds: {kinds:?}");
    assert!(kinds.contains(&BlockKind::Paragraph), "kinds: {kinds:?}");
    assert!(kinds.contains(&BlockKind::List), "kinds: {kinds:?}");
    assert!(kinds.contains(&BlockKind::Table), "kinds: {kinds:?}");
    assert!(kinds.contains(&BlockKind::TableCell), "kinds: {kinds:?}");
    assert!(kinds.contains(&BlockKind::Image), "kinds: {kinds:?}");

    let heading = output
        .document
        .blocks
        .iter()
        .find(|block| block.kind == BlockKind::Heading)
        .unwrap();
    assert_eq!(heading.text.as_deref(), Some("Overview"));
    assert_eq!(heading.level, Some(1));

    // the image is unparsed, so the document is partial
    assert_eq!(output.document.completeness, DocCompleteness::Partial);
    assert!(output
        .document
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "OCR_NOT_IMPLEMENTED"));

    // cells point at the table
    let table = output
        .document
        .blocks
        .iter()
        .find(|block| block.kind == BlockKind::Table)
        .unwrap();
    let cells: Vec<&str> = output
        .document
        .blocks
        .iter()
        .filter(|block| block.kind == BlockKind::TableCell)
        .filter_map(|block| block.parent_id.as_deref())
        .collect();
    assert!(cells.iter().all(|parent| *parent == table.id));
    assert_eq!(cells.len(), 4);
}

#[test]
fn a_zip_without_document_xml_is_a_typed_failure() {
    let mut buffer = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buffer);
        zip.start_file("other.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"nope").unwrap();
        zip.finish().unwrap();
    }
    let error = DocxParser::new()
        .parse(
            &source("application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
            &buffer.into_inner(),
            &ParseLimits::default(),
        )
        .unwrap_err();
    assert_eq!(error.code(), "SOURCE_PARSE_FAILED");
}

#[test]
fn unsupported_media_types_are_rejected_by_the_dispatcher() {
    let error = DocumentParsers::new()
        .parse(
            &source("application/msword"),
            b"legacy doc",
            &ParseLimits::default(),
        )
        .unwrap_err();
    assert_eq!(error.code(), "UNSUPPORTED_MEDIA_TYPE");
    assert!(matches!(error, DocumentError::UnsupportedMediaType { .. }));
    assert!(DocumentParsers::new().supports(MediaFormat::Markdown));
    assert!(DocumentParsers::new().supports(MediaFormat::Pdf));
    assert!(DocumentParsers::new().supports(MediaFormat::Docx));
}

#[test]
fn the_byte_limit_is_enforced_before_parsing() {
    let bytes = text_pdf();
    let error = DocumentParsers::new()
        .parse(
            &source("application/pdf"),
            &bytes,
            &ParseLimits {
                max_bytes: 16,
                include_images: true,
            },
        )
        .unwrap_err();
    assert_eq!(error.code(), "SOURCE_TOO_LARGE");
}
