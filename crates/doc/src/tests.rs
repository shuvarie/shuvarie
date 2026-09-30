//! End-to-end conversion tests. Zip-based fixtures are synthesized in memory
//! with the `zip` writer (same crate the parsers use), keeping the suite
//! self-contained and deterministic.

use std::io::Write as _;

use crate::{ConvertError, SourceFormat, detect, to_markdown};

/// Builds a zip archive from name/content members.
fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, contents) in entries {
        writer
            .start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(contents).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

const DOC_MAIN_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

/// A docx body in the shape of the committed `report.docx` fixture: heading,
/// bold run, plain paragraphs, and a table.
fn document_xml() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{DOC_MAIN_NS}">
  <w:body>
    <w:p>
      <w:pPr><w:pStyle w:val="Heading1"/></w:pPr>
      <w:r><w:t>Quarterly Report</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>Revenue grew by </w:t></w:r>
      <w:r><w:rPr><w:b/></w:rPr><w:t>42 percent</w:t></w:r>
      <w:r><w:t> in Q3.</w:t></w:r>
    </w:p>
    <w:p><w:pPr><w:pStyle w:val="Heading_20_2"/></w:pPr><w:r><w:t>Regional sales</w:t></w:r></w:p>
    <w:tbl>
      <w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr>
      <w:tr>
        <w:trPr><w:tblHeader w:val="on"/></w:trPr>
        <w:tc><w:p><w:r><w:t>Product</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>Sales</w:t></w:r></w:p></w:tc>
      </w:tr>
      <w:tr>
        <w:tc><w:p><w:r><w:t>Anvil</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>1200</w:t></w:r></w:p></w:tc>
      </w:tr>
    </w:tbl>
    <w:p><w:hyperlink r:id="rId5"><w:r><w:t>the docs site</w:t></w:r></w:hyperlink></w:p>
    <w:sectPr><w:pgSz w:w="11906" w:h="16838"/></w:sectPr>
  </w:body>
</w:document>"#
    )
}

fn document_rels_xml() -> String {
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
  <Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://docs.shuvarie.dev/" TargetMode="External"/>
</Relationships>"#
        .to_string()
}

fn content_types_xml() -> String {
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
</Types>"#
        .to_string()
}

/// A minimal spreadsheet package using inline strings (no sharedStrings).
fn xlsx_bytes() -> Vec<u8> {
    let workbook = r#"<?xml version="1.0"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
    xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets>
</workbook>"#;
    let workbook_rels = r#"<?xml version="1.0"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>"#;
    let sheet = r#"<?xml version="1.0"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
    <row r="1"><c r="A1" t="inlineStr"><is><t>Product</t></is></c><c r="B1" t="inlineStr"><is><t>Sales</t></is></c></row>
    <row r="2"><c r="A2" t="inlineStr"><is><t>Anvil</t></is></c><c r="B2"><v>1200</v></c></row>
    <row r="3"><c r="A3" t="inlineStr"><is><t>Hammer</t></is></c><c r="B3"><v>3.5</v></c></row>
  </sheetData>
</worksheet>"#;
    let types = r#"<?xml version="1.0"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
  <Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
</Types>"#;
    let rels = r#"<?xml version="1.0"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#;
    zip_bytes(&[
        ("[Content_Types].xml", types.as_bytes()),
        ("_rels/.rels", rels.as_bytes()),
        ("xl/workbook.xml", workbook.as_bytes()),
        ("xl/_rels/workbook.xml.rels", workbook_rels.as_bytes()),
        ("xl/worksheets/sheet1.xml", sheet.as_bytes()),
    ])
}

const ODF_NS: &str = "urn:oasis:names:tc:opendocument:xmlns:office:1.0";
const TEXT_NS: &str = "urn:oasis:names:tc:opendocument:xmlns:text:1.0";
const TABLE_NS: &str = "urn:oasis:names:tc:opendocument:xmlns:table:1.0";
const DRAW_NS: &str = "urn:oasis:names:tc:opendocument:xmlns:drawing:1.0";

fn odf_document(body: &str, mimetype: &str) -> Vec<u8> {
    let content = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content xmlns:office="{ODF_NS}" xmlns:text="{TEXT_NS}"
    xmlns:table="{TABLE_NS}" xmlns:draw="{DRAW_NS}" office:version="1.2">
  <office:body><office:text>{body}</office:text></office:body>
</office:document-content>"#
    );
    zip_bytes(&[
        ("mimetype", mimetype.as_bytes()),
        ("content.xml", content.as_bytes()),
    ])
}

/// An ODS for the calamine path (mimetype declares spreadsheet; the odt
/// walker rejects it).
fn ods_bytes() -> Vec<u8> {
    let content = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content xmlns:office="{ODF_NS}" xmlns:text="{TEXT_NS}"
    xmlns:table="{TABLE_NS}" office:version="1.2">
  <office:body>
    <office:spreadsheet>
      <table:table table:name="Inventory">
        <table:table-row><table:table-cell office:value-type="string"><text:p>Warehouse</text:p></table:table-cell><table:table-cell office:value-type="string"><text:p>Skus</text:p></table:table-cell></table:table-row>
        <table:table-row><table:table-cell office:value-type="string"><text:p>West</text:p></table:table-cell><table:table-cell office:value-type="float" office:value="17"><text:p>17</text:p></table:table-cell></table:table-row>
      </table:table>
    </office:spreadsheet>
  </office:body>
</office:document-content>"#
    );
    zip_bytes(&[
        (
            "mimetype",
            b"application/vnd.oasis.opendocument.spreadsheet",
        ),
        ("META-INF/manifest.xml", manifest_xml().as_bytes()),
        ("content.xml", content.as_bytes()),
    ])
}

/// The manifest calamine's ODS reader requires; the minimal legal form lists
/// the root entry and the content member.
fn manifest_xml() -> String {
    r#"<?xml version="1.0"?>
<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0" manifest:version="1.2">
  <manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.spreadsheet"/>
  <manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/>
</manifest:manifest>"#
        .to_string()
}

fn pptx_bytes() -> Vec<u8> {
    let presentation = r#"<?xml version="1.0"?>
<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
  <p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst>
</p:presentation>"#;
    let slide = r#"<?xml version="1.0"?>
<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
    xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <p:cSld><p:spTree>
    <p:sp><p:nvSpPr><p:cNvPr id="2" name="Title"/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr>
      <p:txBody><a:p><a:r><a:t>Deck Title</a:t></a:r></a:p></p:txBody>
    </p:sp>
    <p:sp><p:nvSpPr><p:cNvPr id="3" name="Body"/><p:nvPr><p:ph type="body"/></p:nvPr></p:nvSpPr>
      <p:txBody>
        <a:p><a:pPr><a:buChar char="-"/></a:pPr><a:r><a:t>Point one</a:t></a:r></a:p>
        <a:p><a:pPr lvl="0"/><a:r><a:t>Point two notes</a:t></a:r></a:p>
      </p:txBody>
    </p:sp>
  </p:spTree></p:cSld>
</p:sld>"#;
    let slide2 = r#"<?xml version="1.0"?>
<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
    xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <p:cSld><p:spTree>
    <p:sp><p:nvSpPr><p:cNvPr id="2" name="Title"/><p:nvPr><p:ph type="ctrTitle"/></p:nvPr></p:nvSpPr>
      <p:txBody><a:p><a:r><a:t>Closing</a:t></a:r></a:p></p:txBody>
    </p:sp>
  </p:spTree></p:cSld>
</p:sld>"#;
    zip_bytes(&[
        ("[Content_Types].xml", content_types_xml().as_bytes()),
        ("ppt/presentation.xml", presentation.as_bytes()),
        ("ppt/slides/slide1.xml", slide.as_bytes()),
        ("ppt/slides/slide2.xml", slide2.as_bytes()),
    ])
}

fn epub_bytes() -> Vec<u8> {
    let container = r#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#;
    let opf = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="id">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>Container Handbook</dc:title>
  </metadata>
  <manifest>
    <item id="ch1" href="chapter%20one.xhtml" media-type="application/xhtml+xml"/>
    <item id="ch2" href="chapter2.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine><itemref idref="ch1"/><itemref idref="ch2"/></spine>
</package>"#;
    let chapter1 = r#"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>c1</title></head>
<body><h1>Chapter One</h1><p>Chapter one body text.</p><ol><li>first</li><li>second</li></ol></body></html>"#;
    let chapter2 = r#"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml"><body><p>Chapter two body text.</p></body></html>"#;
    zip_bytes(&[
        ("mimetype", b"application/epub+zip"),
        ("META-INF/container.xml", container.as_bytes()),
        ("OEBPS/content.opf", opf.as_bytes()),
        ("OEBPS/chapter one.xhtml", chapter1.as_bytes()),
        ("OEBPS/chapter2.xhtml", chapter2.as_bytes()),
    ])
}
/// Fetches one member of an in-memory archive through the zip reader (for
/// detect() signature tests).
fn member_named(
    archive: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>,
    name: &str,
) -> Option<Vec<u8>> {
    crate::xml::member(archive, name)
}

#[test]
fn converts_docx_fixture() {
    let bytes = zip_bytes(&[
        ("[Content_Types].xml", b"<Types/>"),
        ("word/document.xml", document_xml().as_bytes()),
        (
            "word/_rels/document.xml.rels",
            document_rels_xml().as_bytes(),
        ),
    ]);
    assert_eq!(detect(&bytes, None), Some(SourceFormat::Docx));
    let markdown = to_markdown(SourceFormat::Docx, &bytes).unwrap();
    assert!(markdown.contains("# Quarterly Report"), "{markdown}");
    assert!(
        markdown.contains("Revenue grew by **42 percent** in Q3."),
        "{markdown}"
    );
    assert!(markdown.contains("## Regional sales"), "{markdown}");
    assert!(markdown.contains("| Product | Sales |"), "{markdown}");
    assert!(markdown.contains("| --- | --- |"), "{markdown}");
    assert!(markdown.contains("| Anvil | 1200 |"), "{markdown}");
    assert!(
        markdown.contains("[the docs site](https://docs.shuvarie.dev/)"),
        "{markdown}"
    );
}

#[test]
fn detects_docx_by_content_not_extension() {
    let bytes = zip_bytes(&[("word/document.xml", document_xml().as_bytes())]);
    assert_eq!(detect(&bytes, Some("dat")), Some(SourceFormat::Docx));
}

#[test]
fn xlsm_detected_by_macro_part() {
    let bytes = zip_bytes(&[
        ("[Content_Types].xml", b"<Types/>"),
        ("xl/workbook.xml", b"<workbook/>"),
        ("xl/vbaProject.bin", b""),
    ]);
    assert_eq!(detect(&bytes, None), Some(SourceFormat::Xlsm));
}

#[test]
fn xlsb_detected_by_workbook_bin() {
    let bytes = zip_bytes(&[("xl/workbook.bin", b"")]);
    assert_eq!(detect(&bytes, None), Some(SourceFormat::Xlsb));
}

#[test]
fn detects_epub_by_container() {
    let bytes = zip_bytes(&[("META-INF/container.xml", b"<container/>")]);
    assert_eq!(detect(&bytes, None), Some(SourceFormat::Epub));
}

#[test]
fn detects_odf_family_by_mimetype() {
    let bytes = zip_bytes(&[
        (
            "mimetype",
            b"application/vnd.oasis.opendocument.presentation",
        ),
        ("content.xml", b"<office:document-content/>"),
    ]);
    assert_eq!(detect(&bytes, None), Some(SourceFormat::Odp));
}

#[test]
fn converts_xlsx_with_inline_strings() {
    let bytes = xlsx_bytes();
    let markdown = to_markdown(SourceFormat::Xlsx, &bytes).unwrap();
    assert!(markdown.contains("## Sheet1"), "{markdown}");
    assert!(markdown.contains("| Product | Sales |"), "{markdown}");
    assert!(markdown.contains("| Anvil | 1200 |"), "{markdown}");
    assert!(markdown.contains("| Hammer | 3.5 |"), "{markdown}");
}

#[test]
fn converts_ods_via_calamine() {
    let bytes = ods_bytes();
    assert_eq!(detect(&bytes, None), Some(SourceFormat::Ods));
    let markdown = to_markdown(SourceFormat::Ods, &bytes).unwrap();
    assert!(markdown.contains("## Inventory"), "{markdown}");
    assert!(markdown.contains("| Warehouse | Skus |"), "{markdown}");
    assert!(markdown.contains("| West | 17 |"), "{markdown}");
}

#[test]
fn all_spreadsheet_members_can_be_read() {
    // Cross-check that the zip sniffing path in tests resolves members.
    let bytes = xlsx_bytes();
    let mut archive = crate::xml::open_zip(&bytes).unwrap();
    assert!(member_named(&mut archive, "xl/worksheets/sheet1.xml").is_some());
}

#[test]
fn converts_odt_with_heading_list_and_table() {
    let bytes = odf_document(
        concat!(
            "<text:h text:outline-level=\"1\">Quarterly Plan</text:h>",
            "<text:p>Ship the crate on time.</text:p>",
            "<text:list><text:list-item><text:p>Prepare artifacts</text:p></text:list-item></text:list>",
            concat!(
                "<table:table><table:table-row><table:table-cell><text:p>Region</text:p></table:table-cell>",
                "<table:table-cell><text:p>Units</text:p></table:table-cell></table:table-row>",
                "<table:table-row><table:table-cell><text:p>North</text:p></table:table-cell>",
                "<table:table-cell><text:p>40</text:p></table:table-cell></table:table-row></table:table>"
            )
        ),
        "application/vnd.oasis.opendocument.text",
    );
    assert_eq!(detect(&bytes, None), Some(SourceFormat::Odt));
    let markdown = to_markdown(SourceFormat::Odt, &bytes).unwrap();
    assert!(markdown.contains("# Quarterly Plan"), "{markdown}");
    assert!(markdown.contains("Ship the crate on time."), "{markdown}");
    assert!(markdown.contains("- Prepare artifacts"), "{markdown}");
    assert!(markdown.contains("| Region | Units |"), "{markdown}");
    assert!(markdown.contains("| North | 40 |"), "{markdown}");
}

#[test]
fn converts_odp_with_titles_and_rules() {
    let body = concat!(
        "<draw:page draw:name=\"page1\">",
        "<draw:frame presentation:class=\"title\"><draw:text-box><text:p>Deck Title</text:p></draw:text-box></draw:frame>",
        "<draw:frame presentation:class=\"outline\"><draw:text-box><text:p>Point one</text:p><text:p>Point two</text:p></draw:text-box></draw:frame>",
        "</draw:page>",
        "<draw:page draw:name=\"page2\">",
        "<draw:frame presentation:class=\"title\"><draw:text-box><text:p>Closing</text:p></draw:text-box></draw:frame>",
        "</draw:page>"
    );
    // The ODP parse needs its own body wrapper (office:presentation).
    let content = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content xmlns:office="{ODF_NS}" xmlns:text="{TEXT_NS}"
    xmlns:draw="{DRAW_NS}" xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0"
    office:version="1.2">
  <office:body><office:presentation>{body}</office:presentation></office:body>
</office:document-content>"#
    );
    let bytes = zip_bytes(&[
        (
            "mimetype",
            b"application/vnd.oasis.opendocument.presentation",
        ),
        ("content.xml", content.as_bytes()),
    ]);
    let markdown = to_markdown(SourceFormat::Odp, &bytes).unwrap();
    assert!(markdown.contains("## Deck Title"), "{markdown}");
    assert!(markdown.contains("Point one"), "{markdown}");
    assert!(markdown.contains("---"), "{markdown}");
    assert!(markdown.contains("## Closing"), "{markdown}");
}

#[test]
fn converts_pptx_with_titles_bullets_and_rules() {
    let bytes = pptx_bytes();
    assert_eq!(detect(&bytes, None), Some(SourceFormat::Pptx));
    let markdown = to_markdown(SourceFormat::Pptx, &bytes).unwrap();
    assert!(markdown.contains("## Deck Title"), "{markdown}");
    assert!(markdown.contains("- Point one"), "{markdown}");
    assert!(markdown.contains("Point two notes"), "{markdown}");
    assert!(markdown.contains("---"), "{markdown}");
    assert!(markdown.contains("## Closing"), "{markdown}");
}

#[test]
fn converts_epub_with_title_and_chapters() {
    let bytes = epub_bytes();
    assert_eq!(detect(&bytes, None), Some(SourceFormat::Epub));
    let markdown = to_markdown(SourceFormat::Epub, &bytes).unwrap();
    assert!(markdown.contains("# Container Handbook"), "{markdown}");
    assert!(markdown.contains("Chapter one body text."), "{markdown}");
    assert!(
        markdown.contains("2. second") || markdown.contains("- second"),
        "{markdown}"
    );
    assert!(markdown.contains("Chapter two body text."), "{markdown}");
}

#[test]
fn detects_rtf_by_prefix_and_csv_by_extension() {
    assert_eq!(detect(b"{\\rtf1\\ansi hi}", None), Some(SourceFormat::Rtf));
    let csv = b"id,city\n1,Reykjavik\n";
    assert_eq!(detect(csv, None), None);
    assert_eq!(detect(csv, Some("csv")), Some(SourceFormat::Csv));
    let csv_markdown = to_markdown(SourceFormat::Csv, csv).unwrap();
    assert!(csv_markdown.contains("| id | city |"), "{csv_markdown}");
}

#[test]
fn pdf_detection_prefers_offset_header() {
    let mut bytes = vec![0x00, 0x00];
    bytes.extend_from_slice(b"%PDF-1.4\nhello");
    assert_eq!(detect(&bytes, None), Some(SourceFormat::Pdf));
}

#[test]
fn rejects_plain_text_and_unknown_extensions() {
    assert_eq!(detect(b"plain text file", Some("txt")), None);
    assert_eq!(detect(b"plain text file", None), None);
    assert_eq!(
        detect(b"plain text file", Some("doc")),
        Some(SourceFormat::LegacyDoc)
    );
    assert_eq!(
        detect(b"plain text file", Some("ppt")),
        Some(SourceFormat::LegacyPpt)
    );
}

#[test]
fn legacy_formats_get_conversion_hints() {
    let doc = to_markdown(SourceFormat::LegacyDoc, b"legacy bytes").unwrap_err();
    assert!(matches!(doc, ConvertError::Unsupported(_)), "{doc:?}");
    assert!(
        doc.to_string().contains("soffice --convert-to docx"),
        "{doc}"
    );
    let ppt = to_markdown(SourceFormat::LegacyPpt, b"legacy bytes").unwrap_err();
    assert!(
        ppt.to_string().contains("soffice --convert-to pptx"),
        "{ppt}"
    );
}

#[test]
fn malformed_containers_report_the_format_part() {
    let mut junk = b"PK\x03\x04 not really a zip archive at all".to_vec();
    junk.truncate(40);
    assert_eq!(detect(&junk, Some("docx")), Some(SourceFormat::Docx));
    let error = to_markdown(SourceFormat::Docx, &junk).unwrap_err();
    assert!(
        matches!(
            error,
            ConvertError::Malformed {
                part: Some("zip"),
                ..
            }
        ),
        "{error:?}"
    );
}

#[test]
fn converts_rtf_runs() {
    let rtf =
        r"{\rtf1\ansi{\fonttbl\f0\fswiss Helvetica;}\f0\pard Voici du texte en {\b gras}.\par}";
    let markdown = to_markdown(SourceFormat::Rtf, rtf.as_bytes()).unwrap();
    assert!(
        markdown.contains("Voici du texte en **gras**."),
        "{markdown}"
    );
}

/// Assembles a spec-valid single- or multi-page PDF from page text strings
/// (empty strings produce a page with no extractable content).
fn synthetic_pdf(pages: &[&str]) -> Vec<u8> {
    let count = pages.len();
    let font_number = 3 + count * 2;
    let mut objects: Vec<Vec<u8>> = vec![b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(), {
        let kids: Vec<String> = (0..count)
            .map(|index| format!("{} 0 R", 3 + index * 2))
            .collect();
        format!(
            "<< /Type /Pages /Kids [{}] /Count {count} >>",
            kids.join(" ")
        )
        .into_bytes()
    }];
    for (index, text) in pages.iter().enumerate() {
        let page_number = 3 + index * 2;
        objects.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 {font_number} 0 R >> >> /Contents {} 0 R >>",
                page_number + 1
            )
            .into_bytes(),
        );
        let stream = if text.is_empty() {
            String::new()
        } else {
            format!("BT /F1 24 Tf 72 720 Td ({text}) Tj ET")
        };
        objects.push(
            format!(
                "<< /Length {} >>\nstream\n{stream}\nendstream",
                stream.len()
            )
            .into_bytes(),
        );
    }
    objects.push(
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_vec(),
    );
    let mut bytes: Vec<u8> = b"%PDF-1.4\n".to_vec();
    let mut offsets: Vec<usize> = Vec::new();
    for (index, body) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(b"\nendobj\n");
    }
    let xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer << /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    bytes
}

/// Opt-in smoke harness for files produced by REAL office tools (pandoc,
/// LibreOffice). Usage: generate files under /tmp/docsmoke, then run
/// `cargo test -p shuvarie-doc --lib -- --ignored smokes_real_tool_output`.
#[test]
#[ignore]
fn smokes_real_tool_output() {
    let dir = std::path::PathBuf::from("/tmp/docsmoke");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        eprintln!("skip: {dir:?} missing");
        return;
    };
    for entry in entries.filter_map(|entry| entry.ok()) {
        let path = entry.path();
        if path.extension().is_none() {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        let extension = path.extension().and_then(std::ffi::OsStr::to_str);
        let Some(format) = detect(&bytes, extension) else {
            println!(
                "=== {} skipped (no signature and unsupported extension) ===",
                path.display()
            );
            continue;
        };
        match to_markdown(format, &bytes) {
            Ok(markdown) => {
                assert!(
                    !markdown.trim().is_empty(),
                    "{}: empty result",
                    path.display()
                );
                let mut head = markdown.lines().take(14).collect::<Vec<_>>().join("\n");
                head.truncate(600);
                println!("=== {} ({format:?}) ===\n{head}", path.display());
            }
            Err(error) => panic!("{}: {error}", path.display()),
        }
    }
}

#[test]
fn all_scan_pages_report_needs_ocr() {
    let bytes = synthetic_pdf(&[""]);
    let error = to_markdown(SourceFormat::Pdf, &bytes).unwrap_err();
    assert!(
        matches!(
            error,
            ConvertError::NeedsOcr { ref pages, page_count: 1 } if pages == &vec![1]
        ),
        "{error:?}"
    );
}

#[test]
fn partially_empty_pages_get_flagged_but_text_survives() {
    let bytes = synthetic_pdf(&["First page has words.", ""]);
    let markdown = to_markdown(SourceFormat::Pdf, &bytes).unwrap();
    assert!(markdown.contains("First page has words."), "{markdown}");
    assert!(markdown.contains("[2] of 2 pages"), "{markdown}");
}

#[test]
fn encrypt_markers_classify_as_encrypted() {
    let mut bytes = synthetic_pdf(&["text"]);
    bytes.truncate(20); // break the xref so extraction fails
    bytes.extend_from_slice(b" /Encrypt junk");
    let error = to_markdown(SourceFormat::Pdf, &bytes).unwrap_err();
    assert!(matches!(error, ConvertError::Encrypted), "{error:?}");
}
