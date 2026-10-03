#!/usr/bin/env python3
"""Regenerate the binary document fixtures under tests/fixtures/.

Run from this directory with any Python 3:  python3 generate.py

Both fixtures are handcrafted from the file-format specifications so the
generator needs no external tools:

- report.docx: a minimal Office Open XML (WordprocessingML) package with a
  heading, a bold run, and a 2x2 table.
- plan.pdf: a single-page PDF with one line of extractable text; the xref
  table offsets are computed, so the file is spec-valid.

Zip member timestamps are pinned so regeneration is byte-for-byte
deterministic.
"""

import io
import zipfile


def docx() -> bytes:
    content_types = b"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>
"""
    rels = b"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>
"""
    document = b"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
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
    <w:p><w:r><w:t>Regional sales:</w:t></w:r></w:p>
    <w:tbl>
      <w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr>
      <w:tblGrid><w:gridCol w:w="4320"/><w:gridCol w:w="4320"/></w:tblGrid>
      <w:tr>
        <w:trPr><w:tblHeader w:val="on"/></w:trPr>
        <w:tc><w:tcPr/><w:p><w:r><w:t>Product</w:t></w:r></w:p></w:tc>
        <w:tc><w:tcPr/><w:p><w:r><w:t>Sales</w:t></w:r></w:p></w:tc>
      </w:tr>
      <w:tr>
        <w:tc><w:tcPr/><w:p><w:r><w:t>Anvil</w:t></w:r></w:p></w:tc>
        <w:tc><w:tcPr/><w:p><w:r><w:t>1200</w:t></w:r></w:p></w:tc>
      </w:tr>
    </w:tbl>
    <w:sectPr>
      <w:pgSz w:w="11906" w:h="16838"/>
      <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
    </w:sectPr>
  </w:body>
</w:document>
"""
    stamp = (2026, 1, 31, 0, 0, 0)
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w", zipfile.ZIP_DEFLATED) as archive:
        for name, payload in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/document.xml", document),
        ]:
            info = zipfile.ZipInfo(name, date_time=stamp)
            info.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(info, payload)
    return buffer.getvalue()


def escape_pdf_text(text: str) -> str:
    return text.replace("\\", r"\\").replace("(", r"\(").replace(")", r"\)")


def pdf(text: str) -> bytes:
    stream = f"BT /F1 24 Tf 72 720 Td ({escape_pdf_text(text)}) Tj ET\n".encode()
    objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
        b"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n" + stream + b"endstream",
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
    ]
    out = bytearray(b"%PDF-1.4\n")
    offsets = []
    for index, body in enumerate(objects, start=1):
        offsets.append(len(out))
        out += f"{index} 0 obj\n".encode()
        out += body
        out += b"\nendobj\n"
    xref = len(out)
    out += f"xref\n0 {len(objects) + 1}\n".encode()
    out += b"0000000000 65535 f \n"
    for offset in offsets:
        out += f"{offset:010d} 00000 n \n".encode()
    out += (
        f"trailer << /Size {len(objects) + 1} /Root 1 0 R >>\n"
        f"startxref\n{xref}\n%%EOF\n"
    ).encode()
    return bytes(out)


def main() -> None:
    with open("report.docx", "wb") as target:
        target.write(docx())
    with open("plan.pdf", "wb") as target:
        target.write(pdf("Shuvarie reads PDF documents without external parsers."))


if __name__ == "__main__":
    main()
